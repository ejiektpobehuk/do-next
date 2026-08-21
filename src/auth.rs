//! The cross-integration credential inventory: what you can sign in to, what
//! is configured, and where each credential lives.
//!
//! This module answers "what is my auth situation" for every integration at
//! once. Credential *use* — attaching headers, refreshing tokens — belongs to
//! [`crate::atlassian::auth`] and [`crate::gitlab::auth`]; this is the layer
//! above, and it owns no terminal so its rules stay testable.
//!
//! Two passes with deliberately different machinery:
//!
//! * The **offline probe** ([`probe`]) reads env vars, keyring entries, the
//!   credentials file and the OAuth token stores. It never runs a
//!   `credential_command` and never fails on a locked keyring, because a status
//!   display that hangs on a pinentry prompt or aborts on a locked store is
//!   worse than one that admits it cannot tell.
//! * The **verify pass** ([`verify_all`]) is the explicit, paid action: it goes
//!   to the network, so there running a command and failing loudly are exactly
//!   right.
//!
//! That split is why [`CredentialState::Command`] exists as its own state: for
//! a `credential_command` row, presence is genuinely unknowable offline.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::atlassian::auth::OAuthStore;
use crate::atlassian::oauth::ExtraScopes;
use crate::config::LoadedConfig;
use crate::config::credentials::{
    ATLASSIAN_CREDENTIALS_SECTION, ATLASSIAN_TOKEN_VARS, pick_env_var, stored_token_present,
};
use crate::config::types::{
    AtlassianConfig, Config, ResolvedGitlab, ResolvedGrafana, ResolvedTeam, SourceKind,
    StandupBackend,
};

/// Display names. The only correctly-cased per-integration labels elsewhere are
/// on `standup::Backend`, which has no Grafana variant and is about timeline
/// entries rather than credentials.
pub const ATLASSIAN: &str = "Atlassian";
pub const GITLAB: &str = "GitLab";
pub const GRAFANA: &str = "Grafana";

/// An Atlassian product a site is used for. Not the same thing as a
/// [`SourceKind`]: boards and backlogs are one product, and both Jira and
/// Confluence can be reached by a standup source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Product {
    Jira,
    Confluence,
    Boards,
}

impl Product {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Jira => "Jira",
            Self::Confluence => "Confluence",
            Self::Boards => "boards",
        }
    }
}

/// How a credential authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    OAuth,
    Token,
}

impl Method {
    const fn label(self) -> &'static str {
        match self {
            Self::OAuth => "OAuth",
            Self::Token => "token",
        }
    }
}

/// Where a credential actually came from — not where the config says to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    /// The variable that supplied it, so the display can name it.
    Env(&'static str),
    Keyring,
    File,
}

impl Store {
    const fn label(self) -> &'static str {
        match self {
            Self::Env(_) => "env",
            Self::Keyring => "keyring",
            Self::File => "file",
        }
    }
}

/// What a keyring lookup found. `Unavailable` is "could not tell" — a locked or
/// absent secret service — and is deliberately distinct from `Empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyringProbe {
    Found,
    Empty,
    Unavailable,
}

/// The offline verdict for one credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialState {
    /// A credential is there, and this is where it came from.
    Present {
        method: Method,
        store: Store,
        expires_at: Option<DateTime<Utc>>,
    },
    /// The config points at a store, but nothing is in it.
    Empty { method: Method, store: Store },
    /// A `credential_command` is configured. Offline we can say that much and
    /// no more: running it is a subprocess that may prompt.
    Command,
    /// Nothing configured and nothing stored.
    Missing,
    /// The probe itself could not tell.
    Unreadable { reason: &'static str },
}

/// Which config block a reconfigure writes into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotRef {
    /// The user's own `atlassian:` block.
    Primary,
    /// The user's own `confluence:` block — an override of the above.
    Override,
    /// A team's config file, named here so the row can say where to look.
    ///
    /// Not writable: a team config may live in a shared git repo that this
    /// tool has no business rewriting.
    Team(String),
}

/// One Atlassian site, and everything needed to sign in to it.
#[derive(Debug, Clone)]
pub struct AtlassianTarget {
    /// The effective connection config — the site's own, not the primary's.
    pub config: AtlassianConfig,
    /// Products the configured teams use this site for, sorted and deduped.
    pub products: Vec<Product>,
    /// Granular OAuth scopes *this site* needs. Per-site on purpose: a site
    /// used only for Confluence must not trigger a board consent screen.
    pub extra_scopes: ExtraScopes,
    pub slot: SlotRef,
}

/// One GitLab instance: the resolved connection to probe, and the target its
/// setup flow consumes.
///
/// Both are needed and neither subsumes the other — `TokenSetupTarget` carries
/// what the flow asks for (the OAuth app) but not the credential fields a probe
/// reads, and it is shared with the startup path, so it is left alone.
#[derive(Debug, Clone)]
pub struct GitlabTarget {
    pub config: crate::config::types::ResolvedGitlab,
    pub setup: crate::gitlab::TokenSetupTarget,
}

/// One Grafana `OnCall` instance. Same split as [`GitlabTarget`].
#[derive(Debug, Clone)]
pub struct GrafanaTarget {
    pub config: crate::config::types::ResolvedGrafana,
    pub setup: crate::grafana::TokenSetupTarget,
}

/// Which integration a row belongs to, carrying what its flow needs.
///
/// Every payload is boxed: inline, the largest variant is several times the
/// smallest, and the enum is rebuilt and moved on every menu pass.
#[derive(Debug, Clone)]
pub enum AuthKind {
    Atlassian(Box<AtlassianTarget>),
    Gitlab(Box<GitlabTarget>),
    Grafana(Box<GrafanaTarget>),
}

/// A coarse integration selector, for `do-next auth <integration>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Atlassian,
    Gitlab,
    Grafana,
}

impl AuthKind {
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Atlassian(_) => Kind::Atlassian,
            Self::Gitlab(_) => Kind::Gitlab,
            Self::Grafana(_) => Kind::Grafana,
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Self::Atlassian(_) => ATLASSIAN,
            Self::Gitlab(_) => GITLAB,
            Self::Grafana(_) => GRAFANA,
        }
    }
}

/// One thing the user can authenticate to: an integration plus the instance it
/// points at. One credential per row.
#[derive(Debug, Clone)]
pub struct AuthTarget {
    pub kind: AuthKind,
    /// The instance URL, shown in the menu.
    pub url: String,
    /// Teams that produced this row, in config order.
    pub team_ids: Vec<String>,
    pub state: CredentialState,
}

/// A row's stable identity, for caching verification results across passes.
///
/// Deliberately not the row index: the row set is rebuilt from a reloaded
/// config between passes, so an index can come to mean a different row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowId {
    kind: Kind,
    url: String,
}

impl AuthTarget {
    pub fn id(&self) -> RowId {
        RowId {
            kind: self.kind.kind(),
            url: normalize_url(&self.url),
        }
    }
}

/// Trailing slashes are cosmetic, so they must not split one instance into two
/// rows. Mirrors what `gitlab::oauth::keyring_key` already does for its keys.
fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

// ── Enumeration (pure) ──────────────────────────────────────────────────────

/// Everything about a target that does not require touching a credential store.
/// Split from [`AuthTarget`] so the enumeration rules can be tested without a
/// keyring.
#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub kind: AuthKind,
    pub url: String,
    pub team_ids: Vec<String>,
}

/// The part of a connection that decides *which credential* is used.
///
/// `default_project` is zeroed because it is a query concern: two teams on one
/// site with different default projects share a credential, and so should share
/// a row. Everything else — the store, the key, the command, the method — can
/// genuinely differ and then they are two credentials, not one.
fn credential_identity(c: &AtlassianConfig) -> AtlassianConfig {
    AtlassianConfig {
        base_url: normalize_url(&c.base_url),
        default_project: String::new(),
        ..c.clone()
    }
}

/// Atlassian sites across every team, primary first, deduped by credential
/// identity.
///
/// Attribution is per source kind, not per team: a Confluence source belongs to
/// `team.confluence`, while Jira and board sources belong to `team.atlassian`.
/// A team with Jira on one site and Confluence on another must not ask for
/// Confluence scopes on the Jira site.
#[allow(clippy::too_many_lines)]
fn atlassian_sites(config: &Config, teams: &[ResolvedTeam]) -> Vec<TargetSpec> {
    /// Fold one (site, products, team) observation into the accumulator.
    ///
    /// A free function taking `&mut Vec` rather than a closure: a closure that
    /// both searches and extends the same vector cannot satisfy the borrow
    /// checker without an index dance anyway.
    fn record(
        sites: &mut Vec<(AtlassianConfig, AtlassianTarget, Vec<String>)>,
        site: &AtlassianConfig,
        slot: &SlotRef,
        products: &[Product],
        team_id: Option<&str>,
    ) {
        let identity = credential_identity(site);
        let existing = sites.iter().position(|(id, _, _)| *id == identity);
        // `map_or_else` cannot express this: the fallback pushes onto the same
        // vector the closure would have to borrow.
        #[allow(clippy::option_if_let_else)]
        let i = if let Some(i) = existing {
            i
        } else {
            sites.push((
                identity,
                AtlassianTarget {
                    config: site.clone(),
                    products: Vec::new(),
                    extra_scopes: ExtraScopes::default(),
                    slot: slot.clone(),
                },
                Vec::new(),
            ));
            sites.len() - 1
        };
        let (_, target, team_ids) = &mut sites[i];
        for product in products {
            if !target.products.contains(product) {
                target.products.push(*product);
            }
            match product {
                Product::Confluence => target.extra_scopes.confluence = true,
                Product::Boards => target.extra_scopes.board = true,
                Product::Jira => {}
            }
        }
        if let Some(id) = team_id
            && !team_ids.iter().any(|t| t == id)
        {
            team_ids.push(id.to_string());
        }
    }

    // (credential identity, target, team ids) in discovery order.
    let mut sites: Vec<(AtlassianConfig, AtlassianTarget, Vec<String>)> = Vec::new();

    for team in teams {
        // Duty sources count throughout: `D` can splice them in at runtime,
        // but scopes are minted once, at sign-in.
        for source in team.sources_and_duty() {
            match source.kind {
                SourceKind::Jira => record(
                    &mut sites,
                    &team.atlassian,
                    &SlotRef::Primary,
                    &[Product::Jira],
                    Some(&team.id),
                ),
                SourceKind::Board | SourceKind::Backlog => record(
                    &mut sites,
                    &team.atlassian,
                    &SlotRef::Primary,
                    &[Product::Boards],
                    Some(&team.id),
                ),
                SourceKind::Confluence => record(
                    &mut sites,
                    &team.confluence,
                    &confluence_slot(config, team),
                    &[Product::Confluence],
                    Some(&team.id),
                ),
                SourceKind::Standup => {
                    // A standup reads whichever backends its filters include,
                    // which can span both sites.
                    let filters = source.standup.clone().unwrap_or_default();
                    if filters.includes(StandupBackend::Jira) {
                        record(
                            &mut sites,
                            &team.atlassian,
                            &SlotRef::Primary,
                            &[Product::Jira],
                            Some(&team.id),
                        );
                    }
                    if filters.includes(StandupBackend::ConfluenceTasks)
                        || filters.includes(StandupBackend::ConfluencePages)
                    {
                        record(
                            &mut sites,
                            &team.confluence,
                            &confluence_slot(config, team),
                            &[Product::Confluence],
                            Some(&team.id),
                        );
                    }
                }
                // GitLab authenticates with its own credential.
                SourceKind::Gitlab => {}
            }
        }
    }

    // A config with no teams — fresh or broken — still has a site to set up.
    if sites.is_empty() && !config.atlassian.base_url.is_empty() {
        record(
            &mut sites,
            &config.atlassian,
            &SlotRef::Primary,
            &[Product::Jira],
            None,
        );
    }

    sites
        .into_iter()
        .map(|(_, mut target, team_ids)| {
            target.products.sort_unstable();
            let url = target.config.base_url.clone();
            TargetSpec {
                kind: AuthKind::Atlassian(Box::new(target)),
                url,
                team_ids,
            }
        })
        .collect()
}

/// Which config block an override site's credentials would be written to.
///
/// A team-level `confluence` block is not writable from here — it may live in a
/// shared repo — so the row names the team instead of offering to rewrite it.
fn confluence_slot(config: &Config, team: &ResolvedTeam) -> SlotRef {
    if team.confluence == team.atlassian {
        // Not really an override: the same site, reached through the same
        // credential as Jira.
        return SlotRef::Primary;
    }
    if team.config.confluence.is_some() {
        return SlotRef::Team(team.id.clone());
    }
    if config.confluence.is_some() {
        return SlotRef::Override;
    }
    SlotRef::Primary
}

/// GitLab instances across every team, deduped by credential identity.
///
/// Deliberately not `gitlab::gitlab_api_urls`: that groups on `base_url` alone,
/// so two teams pointing at one instance with different `credential_key`s
/// collapse into a single group. Harmless for its own caller (the startup offer
/// only asks "is anything missing"), but a menu row must not show one state for
/// two credentials. Its grouping is left as-is; it feeds the startup path.
fn gitlab_instances(teams: &[ResolvedTeam]) -> Vec<TargetSpec> {
    let mut groups: Vec<(ResolvedGitlab, Vec<String>)> = Vec::new();
    for team in teams.iter().filter(|t| t.uses_gitlab()) {
        match groups.iter_mut().find(|(config, _)| *config == team.gitlab) {
            Some((_, team_ids)) => team_ids.push(team.id.clone()),
            None => groups.push((team.gitlab.clone(), vec![team.id.clone()])),
        }
    }
    groups
        .into_iter()
        .map(|(config, team_ids)| TargetSpec {
            url: config.base_url.clone(),
            team_ids: team_ids.clone(),
            kind: AuthKind::Gitlab(Box::new(GitlabTarget {
                setup: crate::gitlab::TokenSetupTarget {
                    base_url: config.base_url.clone(),
                    team_ids,
                    oauth_client_id: config.oauth_client_id.clone(),
                    oauth_client_secret: config.oauth_client_secret.clone(),
                },
                config,
            })),
        })
        .collect()
}

/// Grafana `OnCall` instances, deduped by credential identity. Same reasoning as
/// [`gitlab_instances`].
fn grafana_instances(teams: &[ResolvedTeam]) -> Vec<TargetSpec> {
    let mut groups: Vec<(ResolvedGrafana, Vec<String>)> = Vec::new();
    for grafana in teams
        .iter()
        .filter_map(|t| t.grafana.as_ref().map(|g| (t, g)))
    {
        let (team, config) = grafana;
        match groups.iter_mut().find(|(c, _)| credential_peer(c, config)) {
            Some((_, team_ids)) => team_ids.push(team.id.clone()),
            None => groups.push((config.clone(), vec![team.id.clone()])),
        }
    }
    groups
        .into_iter()
        .map(|(config, team_ids)| TargetSpec {
            url: config.oncall_api_url.clone(),
            team_ids: team_ids.clone(),
            kind: AuthKind::Grafana(Box::new(GrafanaTarget {
                setup: crate::grafana::TokenSetupTarget {
                    oncall_api_url: config.oncall_api_url.clone(),
                    instance_url: config.instance_url.clone(),
                    team_ids,
                },
                config,
            })),
        })
        .collect()
}

/// Whether two Grafana connections share a credential.
///
/// `ResolvedGrafana` also carries per-team view settings (the schedule, the
/// on-duty source list), which have nothing to do with which token is used, so
/// comparing whole values would split one credential across several rows.
fn credential_peer(a: &ResolvedGrafana, b: &ResolvedGrafana) -> bool {
    a.oncall_api_url.trim_end_matches('/') == b.oncall_api_url.trim_end_matches('/')
        && a.credential_key == b.credential_key
        && a.credential_store == b.credential_store
        && a.credential_command == b.credential_command
}

/// Every target the user could sign in to, in a stable order: Atlassian sites
/// first, then GitLab and Grafana instances in team order.
#[must_use]
pub fn enumerate_targets(config: &Config, teams: &[ResolvedTeam]) -> Vec<TargetSpec> {
    let mut specs = atlassian_sites(config, teams);
    specs.extend(gitlab_instances(teams));
    specs.extend(grafana_instances(teams));
    specs
}

// ── Probing (impure, but never fatal) ───────────────────────────────────────

/// Whether a keyring entry exists, with every failure folded into
/// `Unavailable`.
///
/// A locked or missing secret service must not fail a status listing — the
/// other rows are still worth showing. Deliberately not
/// `credentials::keyring_lookup`, which hard-errors with a fix-it message
/// appropriate to a credential path but not to a display.
fn keyring_probe(key: &str) -> KeyringProbe {
    match keyring::Entry::new("do-next", key) {
        Err(e) => {
            log::debug!("auth: keyring unavailable for key={key}: {e}");
            KeyringProbe::Unavailable
        }
        Ok(entry) => match entry.get_password() {
            Ok(_) => KeyringProbe::Found,
            Err(keyring::Error::NoEntry) => KeyringProbe::Empty,
            Err(e) => {
                log::debug!("auth: keyring probe failed for key={key}: {e}");
                KeyringProbe::Unavailable
            }
        },
    }
}

/// The token chain's verdict, from facts already gathered.
///
/// Order mirrors `resolve_atlassian_api_token` / `resolve_gitlab_token` /
/// `resolve_grafana_token` exactly: env, then command, then keyring, then file.
/// Pure, so every integration's precedence is testable without touching the
/// environment or a keyring — the same reason `gitlab_auth_source` is pure.
///
/// `env` is the *name* of the variable that supplied a token, if any — only the
/// name, because the value is a secret this module has no reason to handle.
///
/// `store_is_keyring` is its own parameter rather than inferred from `keyring`
/// because resolution only consults the keyring when the config points at it.
/// An entry the config does not name is genuinely unused, and reporting it as
/// present would be a lie.
#[must_use]
pub const fn token_state(
    env: Option<&'static str>,
    command: Option<&str>,
    store_is_keyring: bool,
    keyring: KeyringProbe,
    file_has_token: bool,
) -> CredentialState {
    if let Some(name) = env {
        return CredentialState::Present {
            method: Method::Token,
            store: Store::Env(name),
            expires_at: None,
        };
    }
    if command.is_some() {
        return CredentialState::Command;
    }
    if store_is_keyring {
        match keyring {
            KeyringProbe::Found => {
                return CredentialState::Present {
                    method: Method::Token,
                    store: Store::Keyring,
                    expires_at: None,
                };
            }
            KeyringProbe::Unavailable => {
                return CredentialState::Unreadable {
                    reason: "keyring unreadable",
                };
            }
            // Fall through: resolution would try the file next.
            KeyringProbe::Empty => {}
        }
    }
    if file_has_token {
        return CredentialState::Present {
            method: Method::Token,
            store: Store::File,
            expires_at: None,
        };
    }
    if store_is_keyring {
        return CredentialState::Empty {
            method: Method::Token,
            store: Store::Keyring,
        };
    }
    CredentialState::Missing
}

/// Why an OAuth token store came back empty: genuinely nothing stored, or a
/// keyring we could not read.
fn unreadable_or_missing(keyring_key: &str) -> CredentialState {
    unreadable_or_missing_from(keyring_probe(keyring_key))
}

/// The decision half of [`unreadable_or_missing`], split out so it is testable
/// without a keyring.
const fn unreadable_or_missing_from(probe: KeyringProbe) -> CredentialState {
    match probe {
        KeyringProbe::Unavailable => CredentialState::Unreadable {
            reason: "keyring unreadable",
        },
        KeyringProbe::Found | KeyringProbe::Empty => CredentialState::Missing,
    }
}

const fn oauth_store(store: &OAuthStore) -> Store {
    match store {
        OAuthStore::Keyring => Store::Keyring,
        OAuthStore::File => Store::File,
    }
}

/// Read the stores and decide this target's state. Never runs a
/// `credential_command`; never propagates a keyring failure.
#[must_use]
pub fn probe(spec: &TargetSpec) -> CredentialState {
    match &spec.kind {
        AuthKind::Atlassian(target) => probe_atlassian(&target.config),
        AuthKind::Gitlab(target) => probe_gitlab(&target.config),
        AuthKind::Grafana(target) => probe_grafana(&target.config),
    }
}

fn probe_atlassian(config: &AtlassianConfig) -> CredentialState {
    if config.auth_method.as_deref() == Some("oauth") {
        // `load_oauth_tokens` already swallows keyring errors, so this cannot
        // hang or hard-fail the listing.
        return match crate::atlassian::oauth::load_oauth_tokens() {
            Ok(Some(creds)) => CredentialState::Present {
                method: Method::OAuth,
                store: oauth_store(&creds.store),
                expires_at: Some(creds.expires_at),
            },
            // `load_oauth_tokens` folds a locked keyring into "no tokens",
            // which is right for a credential path and wrong here: reporting a
            // store we could not read as "not configured" is a lie the user
            // would act on. So when nothing turned up, ask why.
            Ok(None) => unreadable_or_missing(crate::atlassian::oauth::KEYRING_INDEX_KEY),
            Err(e) => {
                log::debug!("auth: reading Atlassian OAuth tokens failed: {e:#}");
                CredentialState::Unreadable {
                    reason: "token store unreadable",
                }
            }
        };
    }

    let key = config
        .credential_key
        .as_deref()
        .unwrap_or(&config.base_url)
        .to_string();
    token_state(
        pick_env_var(&ATLASSIAN_TOKEN_VARS).map(|(name, _)| name),
        config.credential_command.as_deref(),
        config.credential_store.as_deref() == Some("keyring"),
        keyring_probe(&key),
        stored_token_present(ATLASSIAN_CREDENTIALS_SECTION),
    )
}

fn probe_gitlab(config: &crate::config::types::ResolvedGitlab) -> CredentialState {
    // `DO_NEXT_GITLAB_TOKEN` outranks even an `auth_method: "oauth"` config —
    // see `gitlab_auth_source`, where taking the token from the environment
    // while letting a config flag pick the header is the bug that had `glab`
    // sending PATs as bearer tokens. The probe must mirror that order, or it
    // would name a credential resolution is not going to use.
    if let Some((name, _)) = pick_env_var(&["DO_NEXT_GITLAB_TOKEN"]) {
        return CredentialState::Present {
            method: Method::Token,
            store: Store::Env(name),
            expires_at: None,
        };
    }
    if config.uses_oauth() {
        return match crate::gitlab::oauth::load_oauth_tokens(&config.base_url) {
            Ok(Some(creds)) => CredentialState::Present {
                method: Method::OAuth,
                store: oauth_store(&creds.store),
                expires_at: Some(creds.expires_at),
            },
            Ok(None) => unreadable_or_missing(&crate::gitlab::oauth::keyring_key(&config.base_url)),
            Err(e) => {
                log::debug!("auth: reading GitLab OAuth tokens failed: {e:#}");
                CredentialState::Unreadable {
                    reason: "token store unreadable",
                }
            }
        };
    }
    let key = config
        .credential_key
        .clone()
        .unwrap_or_else(|| config.base_url.clone());
    token_state(
        pick_env_var(&["DO_NEXT_GITLAB_TOKEN"]).map(|(name, _)| name),
        config.credential_command.as_deref(),
        config.credential_store.as_deref() == Some("keyring"),
        keyring_probe(&key),
        stored_token_present("gitlab"),
    )
}

fn probe_grafana(config: &crate::config::types::ResolvedGrafana) -> CredentialState {
    let key = config
        .credential_key
        .clone()
        .unwrap_or_else(|| config.oncall_api_url.clone());
    token_state(
        pick_env_var(&["DO_NEXT_GRAFANA_TOKEN"]).map(|(name, _)| name),
        config.credential_command.as_deref(),
        config.credential_store.as_deref() == Some("keyring"),
        keyring_probe(&key),
        stored_token_present("grafana"),
    )
}

/// Enumerate and probe every target.
#[must_use]
pub fn targets(loaded: &LoadedConfig) -> Vec<AuthTarget> {
    enumerate_targets(&loaded.config, &loaded.teams)
        .into_iter()
        .map(|spec| {
            let state = probe(&spec);
            AuthTarget {
                kind: spec.kind,
                url: spec.url,
                team_ids: spec.team_ids,
                state,
            }
        })
        .collect()
}

// ── Rendering (pure) ────────────────────────────────────────────────────────

/// The state as displayed, without brackets. The menu wraps it; `--status`
/// prints it bare.
///
/// `now` is a parameter so the expiry wording is deterministic under test.
#[must_use]
pub fn state_label(state: &CredentialState, now: DateTime<Utc>) -> String {
    match state {
        CredentialState::Present {
            method,
            store,
            expires_at,
        } => {
            let base = format!("{} \u{b7} {}", method.label(), store.label());
            match expires_at {
                Some(at) => format!("{base}{}", expiry_suffix(*at, now)),
                None => base,
            }
        }
        CredentialState::Empty { store, .. } => match store {
            Store::Keyring => "keyring empty".to_string(),
            Store::File => "file has no token".to_string(),
            // A credential cannot be "configured but empty" in the
            // environment: an unset variable is simply absent.
            Store::Env(_) => "not configured".to_string(),
        },
        CredentialState::Command => "token \u{b7} command".to_string(),
        CredentialState::Missing => "not configured".to_string(),
        CredentialState::Unreadable { reason } => (*reason).to_string(),
    }
}

/// Expiry is worth surfacing because nothing else in the tool reports it, and
/// "signed in but the refresh token died" is exactly the failure a status
/// display exists to catch. Only mentioned when it is close or past.
fn expiry_suffix(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let remaining = at.signed_duration_since(now);
    if remaining <= chrono::Duration::zero() {
        return " \u{b7} expired".to_string();
    }
    if remaining >= chrono::Duration::hours(24) {
        return String::new();
    }
    let minutes = remaining.num_minutes();
    if minutes < 60 {
        format!(" \u{b7} expires in {minutes}m")
    } else {
        format!(" \u{b7} expires in {}h", remaining.num_hours())
    }
}

/// One rendered row, shared by the menu and the `--status` table so the two can
/// never disagree about what a row says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuLine {
    pub name: &'static str,
    pub url: String,
    pub state: String,
    /// The Atlassian product list, rendered under the row.
    pub products: Option<String>,
    /// `teams: a, b` for instance-scoped rows.
    pub teams: Option<String>,
    /// Filled in by a verify pass.
    pub verified: Option<String>,
    pub action: MenuAction,
    pub selectable: bool,
}

/// What choosing a row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Index into the target list.
    Target(usize),
    VerifyAll,
    Done,
    /// A separator — never returned as a choice.
    None,
}

/// Build the display rows for a set of targets, plus the trailing actions.
///
/// Pure: the menu maps these to its own row type, and `--status` renders the
/// same values, so the two views cannot drift.
#[must_use]
pub fn build_menu(
    targets: &[AuthTarget],
    verified: &HashMap<RowId, VerifyOutcome>,
    now: DateTime<Utc>,
) -> Vec<MenuLine> {
    let mut lines: Vec<MenuLine> = targets
        .iter()
        .enumerate()
        .map(|(i, target)| MenuLine {
            name: target.kind.name(),
            url: target.url.clone(),
            state: state_label(&target.state, now),
            products: match &target.kind {
                AuthKind::Atlassian(a) if !a.products.is_empty() => Some(
                    a.products
                        .iter()
                        .map(|p| p.label())
                        .collect::<Vec<_>>()
                        .join(" \u{b7} "),
                ),
                _ => None,
            },
            // Only instance-scoped rows carry a team list; an Atlassian site
            // names its products instead.
            teams: match &target.kind {
                AuthKind::Atlassian(_) => None,
                _ if target.team_ids.is_empty() => None,
                _ => Some(format!("teams: {}", target.team_ids.join(", "))),
            },
            verified: verified.get(&target.id()).map(VerifyOutcome::label),
            action: MenuAction::Target(i),
            selectable: true,
        })
        .collect();

    lines.push(MenuLine {
        name: "",
        url: String::new(),
        state: String::new(),
        products: None,
        teams: None,
        verified: None,
        action: MenuAction::None,
        selectable: false,
    });
    lines.push(action_line("Verify all (network)", MenuAction::VerifyAll));
    lines.push(action_line("Done", MenuAction::Done));
    lines
}

const fn action_line(name: &'static str, action: MenuAction) -> MenuLine {
    MenuLine {
        name,
        url: String::new(),
        state: String::new(),
        products: None,
        teams: None,
        verified: None,
        action,
        selectable: true,
    }
}

/// Truncate the variable-width columns so each row fits on one line.
///
/// Required, not cosmetic: the menu's redraw moves the cursor up by a fixed
/// number of lines, so a row that wraps corrupts the display. Existing menus
/// never hit this because their labels are short constants; these rows carry a
/// URL, a state and a team list.
///
/// Dropped in order of what a reader can most afford to lose: the team list
/// first, then the URL is ellipsized.
#[must_use]
pub fn fit_columns(lines: &[MenuLine], width: usize) -> Vec<MenuLine> {
    // What every row spends before the URL: indent, name column, gutters.
    let name_width = lines
        .iter()
        .filter(|l| l.selectable && !l.url.is_empty())
        .map(|l| l.name.chars().count())
        .max()
        .unwrap_or(0);
    let state_width = lines
        .iter()
        .map(|l| l.state.chars().count() + 2) // the brackets
        .max()
        .unwrap_or(0);
    // "  > " + name + "   " + url + "   " + [state]
    let fixed = 4 + name_width + 3 + 3 + state_width;

    lines
        .iter()
        .map(|line| {
            let mut line = line.clone();
            if line.url.is_empty() {
                return line;
            }
            let verdict = line.verified.as_ref().map_or(0, |v| v.chars().count() + 2);
            let teams = line.teams.as_ref().map_or(0, |t| t.chars().count() + 3);
            let url_len = line.url.chars().count();

            if fixed + url_len + verdict + teams <= width {
                return line;
            }
            // The team list is the most expendable: it repeats what `check`
            // already prints, and the row is identified by its URL.
            line.teams = None;
            if fixed + url_len + verdict <= width {
                return line;
            }
            let room = width.saturating_sub(fixed + verdict);
            line.url = ellipsize(&line.url, room);
            line
        })
        .collect()
}

/// Shorten to at most `max` characters, marking the cut.
fn ellipsize(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "\u{2026}".to_string();
    }
    let kept: String = s.chars().take(max - 1).collect();
    format!("{kept}\u{2026}")
}

// ── Verification (network) ──────────────────────────────────────────────────

/// The result of confirming one credential against its API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Who the credential belongs to.
    Ok(String),
    Failed(String),
}

impl VerifyOutcome {
    /// A short annotation for the row's tag. The full error is printed above.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Ok(who) => format!("\u{2713} {who}"),
            Self::Failed(why) => format!("\u{2717} {}", truncate(why, 40)),
        }
    }

    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }
}

fn truncate(s: &str, max: usize) -> String {
    // First line only: an anyhow chain is multi-line and a tag is one line.
    let line = s.lines().next().unwrap_or(s);
    if line.chars().count() <= max {
        return line.to_string();
    }
    let kept: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

/// Confirm one credential against its API and report who it belongs to.
///
/// This is the paid action, and unlike [`probe`] it deliberately *does* run a
/// `credential_command` and *does* surface a locked keyring as a failure: the
/// user asked for the truth, so the cost and the loud error are the point.
pub async fn verify(target: &AuthTarget) -> VerifyOutcome {
    let result = match &target.kind {
        AuthKind::Atlassian(a) => verify_atlassian(a).await,
        AuthKind::Gitlab(g) => verify_gitlab(&g.config).await,
        AuthKind::Grafana(g) => verify_grafana(&g.config).await,
    };
    match result {
        Ok(who) => VerifyOutcome::Ok(who),
        Err(e) => VerifyOutcome::Failed(format!("{e:#}")),
    }
}

async fn verify_atlassian(target: &AtlassianTarget) -> anyhow::Result<String> {
    // A site used only for Confluence has no Jira to ask: `/myself` would
    // either 404 or prove something about a product nobody reads here.
    if target.products == [Product::Confluence] {
        let auth = crate::config::credentials::resolve_confluence_auth(&target.config)?;
        let client = crate::confluence::ConfluenceClient::new(&target.config.base_url, auth)?;
        let account_id = client.account_id().await?;
        return Ok(account_id);
    }
    let auth = crate::config::credentials::resolve_atlassian_auth(&target.config)?;
    let client = crate::jira::JiraClient::new(target.config.base_url.clone(), auth)?;
    let me = client.myself().await?;
    Ok(me.display().to_string())
}

async fn verify_gitlab(config: &ResolvedGitlab) -> anyhow::Result<String> {
    let Some(auth) = crate::config::credentials::resolve_gitlab_auth(config)? else {
        anyhow::bail!("no GitLab credential is configured");
    };
    let user = crate::gitlab::validate_auth(&config.base_url, auth).await?;
    Ok(format!("{} (@{})", user.display(), user.username))
}

async fn verify_grafana(config: &ResolvedGrafana) -> anyhow::Result<String> {
    let Some(token) = crate::config::credentials::resolve_grafana_token(config)? else {
        anyhow::bail!("no Grafana OnCall token is configured");
    };
    let user = crate::grafana::validate_token(&config.oncall_api_url, token).await?;
    Ok(user
        .username
        .or(user.email)
        .unwrap_or_else(|| format!("user {}", user.id)))
}

/// Verify every target concurrently, keyed by row identity.
///
/// Concurrent because the clients carry generous timeouts — Grafana's is a hard
/// 10s, GitLab's 60s — so a serial pass over a handful of rows has an
/// unpleasant worst case for no reason.
pub async fn verify_all(targets: &[AuthTarget]) -> HashMap<RowId, VerifyOutcome> {
    let outcomes = futures::future::join_all(targets.iter().map(verify)).await;
    targets.iter().map(AuthTarget::id).zip(outcomes).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{
        OnDutyMode, ResolvedGitlab, ScheduleSelector, SourceConfig, StandupFilters, TeamConfig,
    };

    /// A Grafana connection with only the credential-relevant fields set.
    /// `ResolvedGrafana` has no `Default` — `ScheduleSelector` has none.
    fn grafana_config(oncall_api_url: &str) -> ResolvedGrafana {
        ResolvedGrafana {
            oncall_api_url: oncall_api_url.into(),
            instance_url: None,
            schedule: ScheduleSelector::Name("primary".into()),
            mode: OnDutyMode::default(),
            on_duty_sources: Vec::new(),
            credential_command: None,
            credential_store: None,
            credential_key: None,
        }
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid timestamp")
            .with_timezone(&Utc)
    }

    fn site(base_url: &str) -> AtlassianConfig {
        AtlassianConfig {
            base_url: base_url.into(),
            default_project: "PROJ".into(),
            ..Default::default()
        }
    }

    fn source(id: &str, kind: SourceKind) -> SourceConfig {
        SourceConfig {
            id: id.into(),
            kind,
            ..Default::default()
        }
    }

    fn team(id: &str, atlassian: AtlassianConfig, sources: Vec<SourceConfig>) -> ResolvedTeam {
        ResolvedTeam {
            id: id.into(),
            path: "/tmp".into(),
            config: TeamConfig {
                sources: sources.clone(),
                ..Default::default()
            },
            confluence: atlassian.clone(),
            atlassian,
            open_slack_in_app: true,
            slack_team_id: None,
            grafana: None,
            gitlab: ResolvedGitlab::default(),
            normal_sources: sources,
            on_duty: false,
        }
    }

    fn atlassian_of(spec: &TargetSpec) -> &AtlassianTarget {
        match &spec.kind {
            AuthKind::Atlassian(t) => t,
            _ => panic!("expected an Atlassian target"),
        }
    }

    // ── Enumeration ─────────────────────────────────────────────────────

    #[test]
    fn two_teams_on_one_site_collapse_into_one_row() {
        let teams = vec![
            team(
                "core",
                site("https://acme.atlassian.net"),
                vec![source("a", SourceKind::Jira)],
            ),
            team(
                "infra",
                site("https://acme.atlassian.net"),
                vec![source("b", SourceKind::Jira)],
            ),
        ];
        let specs = atlassian_sites(&Config::default(), &teams);
        assert_eq!(specs.len(), 1, "one credential, one row");
        assert_eq!(specs[0].team_ids, vec!["core", "infra"], "in config order");
    }

    #[test]
    fn a_trailing_slash_does_not_split_a_site_in_two() {
        let teams = vec![
            team(
                "a",
                site("https://acme.atlassian.net"),
                vec![source("s", SourceKind::Jira)],
            ),
            team(
                "b",
                site("https://acme.atlassian.net/"),
                vec![source("s", SourceKind::Jira)],
            ),
        ];
        assert_eq!(atlassian_sites(&Config::default(), &teams).len(), 1);
    }

    #[test]
    fn a_different_credential_key_on_one_url_is_a_separate_row() {
        // Same site, two genuinely different credentials: one row would show
        // one state for both.
        let mut other = site("https://acme.atlassian.net");
        other.credential_key = Some("second".into());
        let teams = vec![
            team(
                "a",
                site("https://acme.atlassian.net"),
                vec![source("s", SourceKind::Jira)],
            ),
            team("b", other, vec![source("s", SourceKind::Jira)]),
        ];
        assert_eq!(atlassian_sites(&Config::default(), &teams).len(), 2);
    }

    #[test]
    fn a_differing_default_project_still_shares_one_row() {
        // default_project is a query concern, not a credential one.
        let mut other = site("https://acme.atlassian.net");
        other.default_project = "OTHER".into();
        let teams = vec![
            team(
                "a",
                site("https://acme.atlassian.net"),
                vec![source("s", SourceKind::Jira)],
            ),
            team("b", other, vec![source("s", SourceKind::Jira)]),
        ];
        assert_eq!(atlassian_sites(&Config::default(), &teams).len(), 1);
    }

    #[test]
    fn jira_and_confluence_on_different_sites_split_their_scopes() {
        // The whole point of per-site scopes: the Jira site must not be asked
        // to consent to Confluence access it is never used for.
        let mut t = team(
            "core",
            site("https://jira.acme.net"),
            vec![
                source("i", SourceKind::Jira),
                source("c", SourceKind::Confluence),
            ],
        );
        t.confluence = site("https://wiki.other.net");
        let specs = atlassian_sites(&Config::default(), &[t]);
        assert_eq!(specs.len(), 2);

        let jira = atlassian_of(&specs[0]);
        assert_eq!(jira.products, vec![Product::Jira]);
        assert!(
            !jira.extra_scopes.confluence,
            "the Jira site needs no Confluence scope"
        );

        let wiki = atlassian_of(&specs[1]);
        assert_eq!(wiki.products, vec![Product::Confluence]);
        assert!(wiki.extra_scopes.confluence);
        assert!(!wiki.extra_scopes.board);
    }

    #[test]
    fn boards_and_backlogs_ask_for_the_board_scope() {
        let t = team(
            "core",
            site("https://acme.atlassian.net"),
            vec![
                source("b", SourceKind::Board),
                source("k", SourceKind::Backlog),
            ],
        );
        let specs = atlassian_sites(&Config::default(), &[t]);
        let target = atlassian_of(&specs[0]);
        assert_eq!(target.products, vec![Product::Boards]);
        assert!(target.extra_scopes.board);
        assert!(!target.extra_scopes.confluence);
    }

    #[test]
    fn on_duty_sources_count_toward_products_and_scopes() {
        // `D` can splice them in at runtime, but scopes are minted at sign-in.
        let mut t = team(
            "core",
            site("https://acme.atlassian.net"),
            vec![source("j", SourceKind::Jira)],
        );
        t.grafana = Some(ResolvedGrafana {
            on_duty_sources: vec![source("c", SourceKind::Confluence)],
            ..grafana_config("https://g")
        });
        let specs = atlassian_sites(&Config::default(), &[t]);
        let target = atlassian_of(&specs[0]);
        assert!(
            target.extra_scopes.confluence,
            "an on-duty Confluence source still needs the scope"
        );
        assert!(target.products.contains(&Product::Confluence));
    }

    #[test]
    fn a_standup_asks_only_for_the_backends_it_includes() {
        let mut src = source("s", SourceKind::Standup);
        src.standup = Some(StandupFilters {
            include: Some(vec![StandupBackend::ConfluencePages]),
            ..Default::default()
        });
        let t = team("core", site("https://acme.atlassian.net"), vec![src]);
        let specs = atlassian_sites(&Config::default(), &[t]);
        let target = atlassian_of(&specs[0]);
        assert!(target.extra_scopes.confluence);
        assert_eq!(
            target.products,
            vec![Product::Confluence],
            "a pages-only standup does not make this a Jira site"
        );
    }

    #[test]
    fn gitlab_sources_produce_no_atlassian_row() {
        let t = team(
            "core",
            site("https://acme.atlassian.net"),
            vec![source("m", SourceKind::Gitlab)],
        );
        assert!(atlassian_sites(&Config::default(), &[t]).is_empty());
    }

    #[test]
    fn a_config_with_no_teams_still_offers_its_site() {
        // A fresh or broken install must still have something to configure.
        let config = Config {
            atlassian: site("https://acme.atlassian.net"),
            ..Default::default()
        };
        let specs = atlassian_sites(&config, &[]);
        assert_eq!(specs.len(), 1);
        assert!(specs[0].team_ids.is_empty());
    }

    #[test]
    fn a_config_with_no_site_at_all_offers_nothing() {
        assert!(atlassian_sites(&Config::default(), &[]).is_empty());
    }

    // ── token_state precedence ──────────────────────────────────────────

    #[test]
    fn an_env_token_outranks_every_stored_credential() {
        let state = token_state(
            Some("DO_NEXT_ATLASSIAN_API_TOKEN"),
            Some("pass show x"),
            true,
            KeyringProbe::Found,
            true,
        );
        assert_eq!(
            state,
            CredentialState::Present {
                method: Method::Token,
                store: Store::Env("DO_NEXT_ATLASSIAN_API_TOKEN"),
                expires_at: None
            }
        );
    }

    #[test]
    fn a_credential_command_is_reported_without_being_run() {
        // Offline we can say it is configured and no more: running it is a
        // subprocess that may block on a pinentry prompt.
        assert_eq!(
            token_state(None, Some("pass show x"), true, KeyringProbe::Found, true),
            CredentialState::Command
        );
    }

    #[test]
    fn a_keyring_entry_the_config_does_not_name_is_not_used() {
        // Resolution only consults the keyring when credential_store says so,
        // so reporting the entry as present would be a lie.
        assert_eq!(
            token_state(None, None, false, KeyringProbe::Found, false),
            CredentialState::Missing
        );
    }

    #[test]
    fn a_named_keyring_entry_is_present() {
        assert_eq!(
            token_state(None, None, true, KeyringProbe::Found, false),
            CredentialState::Present {
                method: Method::Token,
                store: Store::Keyring,
                expires_at: None
            }
        );
    }

    #[test]
    fn an_unreadable_keyring_is_not_the_same_as_an_empty_one() {
        assert_eq!(
            token_state(None, None, true, KeyringProbe::Unavailable, false),
            CredentialState::Unreadable {
                reason: "keyring unreadable"
            }
        );
        assert_eq!(
            token_state(None, None, true, KeyringProbe::Empty, false),
            CredentialState::Empty {
                method: Method::Token,
                store: Store::Keyring
            }
        );
    }

    #[test]
    fn an_empty_keyring_falls_through_to_the_file_as_resolution_does() {
        assert_eq!(
            token_state(None, None, true, KeyringProbe::Empty, true),
            CredentialState::Present {
                method: Method::Token,
                store: Store::File,
                expires_at: None
            }
        );
    }

    #[test]
    fn nothing_anywhere_is_missing() {
        assert_eq!(
            token_state(None, None, false, KeyringProbe::Empty, false),
            CredentialState::Missing
        );
    }

    #[test]
    fn an_env_token_beats_an_oauth_config_for_gitlab() {
        // resolve_gitlab_auth gives DO_NEXT_GITLAB_TOKEN precedence over even
        // an `auth_method: "oauth"` config, on purpose: that is the glab bug
        // guard. A probe that reported OAuth here would name a credential
        // resolution is not going to use.
        //
        // Exercised through token_state's shape rather than the process
        // environment; probe_gitlab's own env check short-circuits first.
        let with_env = token_state(
            Some("DO_NEXT_GITLAB_TOKEN"),
            None,
            true,
            KeyringProbe::Found,
            false,
        );
        assert!(
            matches!(
                with_env,
                CredentialState::Present {
                    store: Store::Env("DO_NEXT_GITLAB_TOKEN"),
                    ..
                }
            ),
            "got {with_env:?}"
        );
    }

    #[test]
    fn an_empty_oauth_store_says_unreadable_only_when_it_could_not_be_read() {
        // load_oauth_tokens folds a locked keyring into "no tokens". Reporting
        // that as "not configured" tells the user to sign in again when the
        // real problem is a locked store, so the two are separated here.
        assert_eq!(
            unreadable_or_missing_from(KeyringProbe::Unavailable),
            CredentialState::Unreadable {
                reason: "keyring unreadable"
            }
        );
        assert_eq!(
            unreadable_or_missing_from(KeyringProbe::Empty),
            CredentialState::Missing
        );
        assert_eq!(
            unreadable_or_missing_from(KeyringProbe::Found),
            CredentialState::Missing,
            "an entry that exists but yielded no tokens is genuinely empty"
        );
    }

    // ── Labels ──────────────────────────────────────────────────────────

    #[test]
    fn state_labels_name_the_method_and_the_store() {
        let now = utc("2026-08-21T12:00:00Z");
        let present = |store| CredentialState::Present {
            method: Method::OAuth,
            store,
            expires_at: None,
        };
        assert_eq!(
            state_label(&present(Store::Keyring), now),
            "OAuth \u{b7} keyring"
        );
        assert_eq!(state_label(&present(Store::File), now), "OAuth \u{b7} file");
        assert_eq!(
            state_label(
                &CredentialState::Present {
                    method: Method::Token,
                    store: Store::Env("DO_NEXT_GITLAB_TOKEN"),
                    expires_at: None
                },
                now
            ),
            "token \u{b7} env"
        );
        assert_eq!(
            state_label(&CredentialState::Command, now),
            "token \u{b7} command"
        );
        assert_eq!(
            state_label(&CredentialState::Missing, now),
            "not configured"
        );
        assert_eq!(
            state_label(
                &CredentialState::Empty {
                    method: Method::Token,
                    store: Store::Keyring
                },
                now
            ),
            "keyring empty"
        );
        assert_eq!(
            state_label(
                &CredentialState::Unreadable {
                    reason: "keyring unreadable"
                },
                now
            ),
            "keyring unreadable"
        );
    }

    #[test]
    fn expiry_is_mentioned_only_when_it_is_close_or_past() {
        let now = utc("2026-08-21T12:00:00Z");
        let at = |s: &str| CredentialState::Present {
            method: Method::OAuth,
            store: Store::Keyring,
            expires_at: Some(utc(s)),
        };
        assert_eq!(
            state_label(&at("2026-08-23T12:00:00Z"), now),
            "OAuth \u{b7} keyring",
            "two days out is not worth saying"
        );
        assert_eq!(
            state_label(&at("2026-08-21T12:42:00Z"), now),
            "OAuth \u{b7} keyring \u{b7} expires in 42m"
        );
        assert_eq!(
            state_label(&at("2026-08-21T20:00:00Z"), now),
            "OAuth \u{b7} keyring \u{b7} expires in 8h"
        );
        assert_eq!(
            state_label(&at("2026-08-21T11:00:00Z"), now),
            "OAuth \u{b7} keyring \u{b7} expired"
        );
    }

    // ── Menu construction ───────────────────────────────────────────────

    fn target(kind: AuthKind, url: &str, team_ids: &[&str]) -> AuthTarget {
        AuthTarget {
            kind,
            url: url.into(),
            team_ids: team_ids.iter().map(|s| (*s).to_string()).collect(),
            state: CredentialState::Missing,
        }
    }

    fn atlassian_kind(products: Vec<Product>) -> AuthKind {
        AuthKind::Atlassian(Box::new(AtlassianTarget {
            config: site("https://acme.atlassian.net"),
            products,
            extra_scopes: ExtraScopes::default(),
            slot: SlotRef::Primary,
        }))
    }

    fn grafana_kind(url: &str) -> AuthKind {
        AuthKind::Grafana(Box::new(GrafanaTarget {
            config: grafana_config(url),
            setup: crate::grafana::TokenSetupTarget {
                oncall_api_url: url.into(),
                instance_url: None,
                team_ids: Vec::new(),
            },
        }))
    }

    #[test]
    fn the_menu_ends_with_a_separator_verify_and_done() {
        let now = utc("2026-08-21T12:00:00Z");
        let targets = vec![target(
            atlassian_kind(vec![Product::Jira]),
            "https://a",
            &[],
        )];
        let lines = build_menu(&targets, &HashMap::new(), now);

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].action, MenuAction::Target(0));
        assert_eq!(lines[1].action, MenuAction::None);
        assert!(!lines[1].selectable, "the separator is not choosable");
        assert_eq!(lines[2].action, MenuAction::VerifyAll);
        assert_eq!(lines[3].action, MenuAction::Done);
    }

    #[test]
    fn target_actions_stay_aligned_with_the_target_list() {
        let now = utc("2026-08-21T12:00:00Z");
        let targets = vec![
            target(atlassian_kind(vec![Product::Jira]), "https://a", &[]),
            target(grafana_kind("https://g"), "https://g", &["core"]),
        ];
        let lines = build_menu(&targets, &HashMap::new(), now);
        for (i, line) in lines.iter().enumerate().take(targets.len()) {
            assert_eq!(line.action, MenuAction::Target(i));
            assert_eq!(line.url, targets[i].url);
        }
    }

    #[test]
    fn an_atlassian_row_lists_products_and_an_instance_row_lists_teams() {
        let now = utc("2026-08-21T12:00:00Z");
        let targets = vec![
            target(
                atlassian_kind(vec![Product::Jira, Product::Confluence, Product::Boards]),
                "https://acme.atlassian.net",
                &["core"],
            ),
            target(grafana_kind("https://g"), "https://g", &["core", "infra"]),
        ];
        let lines = build_menu(&targets, &HashMap::new(), now);

        assert_eq!(
            lines[0].products.as_deref(),
            Some("Jira \u{b7} Confluence \u{b7} boards")
        );
        assert_eq!(
            lines[0].teams, None,
            "a site names its products, not its teams"
        );

        assert_eq!(lines[1].products, None);
        assert_eq!(lines[1].teams.as_deref(), Some("teams: core, infra"));
    }

    #[test]
    fn a_verified_row_carries_its_verdict() {
        let now = utc("2026-08-21T12:00:00Z");
        let targets = vec![target(grafana_kind("https://g"), "https://g", &[])];
        let mut verified = HashMap::new();
        verified.insert(targets[0].id(), VerifyOutcome::Ok("Vlad Petrov".into()));

        let lines = build_menu(&targets, &verified, now);
        assert_eq!(lines[0].verified.as_deref(), Some("\u{2713} Vlad Petrov"));
    }

    #[test]
    fn a_row_id_ignores_a_trailing_slash_so_a_verdict_survives_a_reload() {
        let a = target(grafana_kind("https://g/"), "https://g/", &[]);
        let b = target(grafana_kind("https://g"), "https://g", &[]);
        assert_eq!(a.id(), b.id());
    }

    #[test]
    fn rows_of_different_kinds_on_one_url_have_different_ids() {
        let a = target(atlassian_kind(vec![]), "https://same", &[]);
        let b = target(grafana_kind("https://same"), "https://same", &[]);
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn a_failure_annotation_is_one_truncated_line() {
        // anyhow chains are multi-line; a row tag is not.
        let outcome = VerifyOutcome::Failed(
            "401 Unauthorized\nCaused by: the token was revoked last Tuesday".into(),
        );
        let label = outcome.label();
        assert!(!label.contains('\n'));
        assert_eq!(label, "\u{2717} 401 Unauthorized");
        assert!(!outcome.is_ok());

        let long = VerifyOutcome::Failed("x".repeat(80));
        assert!(long.label().chars().count() <= 42, "got {}", long.label());
        assert!(long.label().ends_with('\u{2026}'));
    }
    // ── Column fitting ──────────────────────────────────────────────────

    fn line(url: &str, state: &str, teams: Option<&str>) -> MenuLine {
        MenuLine {
            name: GRAFANA,
            url: url.into(),
            state: state.into(),
            products: None,
            teams: teams.map(str::to_string),
            verified: None,
            action: MenuAction::Target(0),
            selectable: true,
        }
    }

    #[test]
    fn a_row_that_already_fits_is_left_alone() {
        let lines = vec![line(
            "https://g.acme.com",
            "token \u{b7} file",
            Some("teams: core"),
        )];
        let fitted = fit_columns(&lines, 200);
        assert_eq!(fitted[0].url, lines[0].url);
        assert_eq!(fitted[0].teams, lines[0].teams);
    }

    #[test]
    fn the_team_list_is_dropped_before_the_url_is_cut() {
        let lines = vec![line(
            "https://oncall-prod-eu-west-0.grafana.net/oncall",
            "token \u{b7} file",
            Some("teams: core, infra, platform"),
        )];
        let fitted = fit_columns(&lines, 80);
        assert_eq!(fitted[0].teams, None, "teams go first");
        assert_eq!(
            fitted[0].url, lines[0].url,
            "the URL identifies the row, so it survives longer"
        );
    }

    #[test]
    fn a_very_narrow_terminal_ellipsizes_the_url() {
        let lines = vec![line(
            "https://oncall-prod-eu-west-0.grafana.net/oncall",
            "token \u{b7} file",
            Some("teams: core"),
        )];
        let fitted = fit_columns(&lines, 45);
        assert!(fitted[0].url.ends_with('\u{2026}'), "got {}", fitted[0].url);
        assert!(
            fitted[0].url.chars().count() < lines[0].url.chars().count(),
            "must actually be shorter"
        );
    }

    #[test]
    fn fitting_never_touches_the_action_rows() {
        // "Verify all" and "Done" have no URL and must survive untouched at
        // any width, or the menu loses its exits.
        let lines = build_menu(
            &[target(grafana_kind("https://g"), "https://g", &["core"])],
            &HashMap::new(),
            utc("2026-08-21T12:00:00Z"),
        );
        let fitted = fit_columns(&lines, 20);
        assert_eq!(fitted.len(), lines.len());
        assert_eq!(fitted[lines.len() - 1].action, MenuAction::Done);
        assert_eq!(fitted[lines.len() - 2].action, MenuAction::VerifyAll);
    }

    #[test]
    fn ellipsize_degrades_gracefully_at_tiny_widths() {
        assert_eq!(ellipsize("abcdef", 6), "abcdef");
        assert_eq!(ellipsize("abcdef", 3), "ab\u{2026}");
        assert_eq!(ellipsize("abcdef", 1), "\u{2026}");
        assert_eq!(ellipsize("abcdef", 0), "\u{2026}");
    }
}
