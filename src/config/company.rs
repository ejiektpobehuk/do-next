//! Company config repo support: a git repo with a `company.json5` manifest
//! (Jira connection, shared OAuth app, defaults, team catalog) plus team
//! configs as subdirectories. The manifest is parsed here; selected teams are
//! synthesized into ordinary `TeamRef`s so the existing resolution pipeline
//! applies unchanged.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::types::{
    CompanyTeamSelection, ConfluenceConfig, GitlabConfig, GrafanaConfig, JiraConfig, TeamRef,
};

/// Manifest file name at the root of a company config repo.
pub const MANIFEST_FILE: &str = "company.json5";

/// Root of a company config repo: `company.json5`.
#[derive(Debug, Clone, Deserialize)]
pub struct CompanyManifest {
    /// Human-readable company name.
    pub name: String,
    pub jira: CompanyJira,
    /// Shared Atlassian OAuth app. Presence implies `auth_method: "oauth"`
    /// for users who haven't chosen an auth method themselves.
    pub oauth: Option<CompanyOAuth>,
    #[serde(default)]
    pub defaults: CompanyDefaults,
    /// Curated team catalog (explicit — the repo is not scanned).
    #[serde(default)]
    pub teams: Vec<CompanyTeamEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompanyJira {
    pub base_url: String,
    pub default_project: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompanyOAuth {
    pub client_id: String,
    /// Plaintext by design: an internal "public client" — the secret alone
    /// grants nothing without a user's browser consent.
    pub client_secret: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompanyDefaults {
    pub confluence: Option<ConfluenceConfig>,
    pub slack_team_id: Option<String>,
    pub open_slack_in_app: Option<bool>,
    /// Grafana `OnCall` connection defaults (e.g. the company's API URL).
    pub grafana: Option<GrafanaConfig>,
    /// GitLab connection defaults (e.g. the company's self-hosted instance).
    pub gitlab: Option<GitlabConfig>,
}

/// One entry in the manifest's team catalog.
#[derive(Debug, Clone, Deserialize)]
pub struct CompanyTeamEntry {
    pub id: String,
    /// Picker label; falls back to `id`.
    pub name: Option<String>,
    /// Picker subtitle.
    pub description: Option<String>,
    /// Directory relative to the repo root (default: `teams/<id>`).
    pub path: Option<String>,
    /// Config file name inside `path` (default: "do-next.json5").
    pub file: Option<String>,
    /// Preselected in the team picker.
    #[serde(default)]
    pub default: bool,
}

impl CompanyTeamEntry {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// Directory relative to the repo root.
    pub fn rel_path(&self) -> String {
        self.path
            .clone()
            .unwrap_or_else(|| format!("teams/{}", self.id))
    }
}

/// Parse and validate a manifest from its JSON5 content.
pub fn parse_manifest(content: &str) -> Result<CompanyManifest> {
    let manifest: CompanyManifest =
        json5::from_str(content).context("Failed to parse company manifest")?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &CompanyManifest) -> Result<()> {
    if manifest.name.trim().is_empty() {
        bail!("company manifest: `name` must not be empty");
    }
    if manifest.jira.base_url.trim().is_empty() {
        bail!("company manifest: `jira.base_url` must not be empty");
    }
    if let Some(oauth) = &manifest.oauth {
        if oauth.client_id.trim().is_empty() {
            bail!("company manifest: `oauth.client_id` must not be empty");
        }
        if oauth.client_secret.trim().is_empty() {
            bail!("company manifest: `oauth.client_secret` must not be empty");
        }
    }
    let mut seen = std::collections::HashSet::new();
    for team in &manifest.teams {
        if team.id.trim().is_empty() {
            bail!("company manifest: team ids must not be empty");
        }
        if !seen.insert(team.id.as_str()) {
            bail!("company manifest: duplicate team id '{}'", team.id);
        }
    }
    Ok(())
}

/// Read and parse `company.json5` from a clone directory.
pub fn load_manifest(clone_dir: &Path) -> Result<CompanyManifest> {
    let path = clone_dir.join(MANIFEST_FILE);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read company manifest: {}", path.display()))?;
    parse_manifest(&content).with_context(|| format!("in {}", path.display()))
}

/// Overlay company values onto the user's Jira config, filling only fields
/// the user left unset/empty. Precedence stays: team override > user config
/// > company manifest > built-in default.
pub fn apply_company_defaults(user: &JiraConfig, manifest: &CompanyManifest) -> JiraConfig {
    let mut jira = user.clone();
    if jira.base_url.is_empty() {
        jira.base_url.clone_from(&manifest.jira.base_url);
    }
    if jira.default_project.is_empty()
        && let Some(project) = &manifest.jira.default_project
    {
        jira.default_project.clone_from(project);
    }
    if let Some(oauth) = &manifest.oauth {
        if jira.auth_method.is_none() {
            jira.auth_method = Some("oauth".into());
        }
        if jira.oauth_client_id.is_none() {
            jira.oauth_client_id = Some(oauth.client_id.clone());
        }
        if jira.oauth_client_secret.is_none() {
            jira.oauth_client_secret = Some(oauth.client_secret.clone());
        }
    }
    jira
}

/// Synthesize `TeamRef`s for the selected catalog teams. Ids missing from the
/// catalog produce per-team error strings (non-fatal, like team load errors).
pub fn company_team_refs(
    clone_dir: &Path,
    manifest: &CompanyManifest,
    selected: &[CompanyTeamSelection],
) -> (Vec<TeamRef>, Vec<String>) {
    let mut refs = Vec::new();
    let mut errors = Vec::new();
    for selection in selected {
        let id = selection.id();
        if let Some(entry) = manifest.teams.iter().find(|t| t.id == id) {
            refs.push(TeamRef {
                id: entry.id.clone(),
                path: clone_dir
                    .join(entry.rel_path())
                    .to_string_lossy()
                    .into_owned(),
                file: entry.file.clone(),
                // Explicit either way: company backlog tabs are per-user opt-in.
                backlog: Some(selection.backlog()),
            });
        } else {
            errors.push(format!(
                "company team '{id}' is not in the manifest catalog — run `do-next company teams`"
            ));
        }
    }
    (refs, errors)
}

/// Derive a directory slug from a git URL:
/// `git@github.com:acme/do-next-config.git` → `do-next-config`.
pub fn slug_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or("");
    let mut slug = String::new();
    for c in last.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "company".into()
    } else {
        slug
    }
}

/// Managed clone location: `~/.config/do-next/company/<slug>`.
pub fn clone_dir_for(slug: &str) -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot determine config directory")?
        .join("do-next")
        .join("company")
        .join(slug))
}

/// Clone a company repo with inherited stdio so interactive git auth
/// (ssh passphrase, https credentials) works during onboarding.
pub fn clone_repo(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let status = std::process::Command::new("git")
        .arg("clone")
        .arg(url)
        .arg(dest)
        .status();
    match status {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("git is not installed — it is required to clone the company config repo")
        }
        Err(e) => Err(e).context("Failed to run git clone"),
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("git clone failed ({s}); check the URL and your git access"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_MANIFEST: &str = r#"{
        // JSON5: comments and trailing commas allowed
        name: "Acme Corp",
        jira: { base_url: "https://acme.atlassian.net", default_project: "CORE" },
        oauth: { client_id: "cid-123", client_secret: "sec-456" },
        defaults: {
            confluence: { base_url: "https://acme.atlassian.net/wiki" },
            slack_team_id: "T0123",
            open_slack_in_app: false,
        },
        teams: [
            { id: "platform", name: "Platform Team", description: "Infra & tooling", default: true },
            { id: "billing", path: "squads/billing", file: "team.json5" },
        ],
    }"#;

    #[test]
    fn parses_full_manifest() {
        let m = parse_manifest(FULL_MANIFEST).expect("valid manifest");
        assert_eq!(m.name, "Acme Corp");
        assert_eq!(m.jira.base_url, "https://acme.atlassian.net");
        assert_eq!(m.jira.default_project.as_deref(), Some("CORE"));
        let oauth = m.oauth.as_ref().expect("oauth block");
        assert_eq!(oauth.client_id, "cid-123");
        assert_eq!(oauth.client_secret, "sec-456");
        assert_eq!(m.defaults.slack_team_id.as_deref(), Some("T0123"));
        assert_eq!(m.defaults.open_slack_in_app, Some(false));
        assert_eq!(m.teams.len(), 2);
        assert!(m.teams[0].default);
        assert_eq!(m.teams[0].display_name(), "Platform Team");
        assert!(!m.teams[1].default);
        assert_eq!(m.teams[1].display_name(), "billing");
    }

    #[test]
    fn parses_minimal_manifest() {
        let m = parse_manifest(r#"{ name: "Acme", jira: { base_url: "https://a.example" } }"#)
            .expect("valid manifest");
        assert!(m.oauth.is_none());
        assert!(m.teams.is_empty());
        assert!(m.defaults.confluence.is_none());
    }

    #[test]
    fn rejects_empty_name_and_base_url() {
        assert!(
            parse_manifest(r#"{ name: " ", jira: { base_url: "https://a.example" } }"#).is_err()
        );
        assert!(parse_manifest(r#"{ name: "Acme", jira: { base_url: "" } }"#).is_err());
        assert!(parse_manifest(r#"{ jira: { base_url: "https://a.example" } }"#).is_err());
    }

    #[test]
    fn rejects_partial_oauth_block() {
        let err = parse_manifest(
            r#"{ name: "Acme", jira: { base_url: "https://a.example" },
                 oauth: { client_id: "cid", client_secret: "" } }"#,
        )
        .expect_err("empty secret must be rejected");
        assert!(err.to_string().contains("client_secret"));
        assert!(
            parse_manifest(
                r#"{ name: "Acme", jira: { base_url: "https://a.example" },
                     oauth: { client_id: "", client_secret: "sec" } }"#,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_duplicate_and_empty_team_ids() {
        let dup = parse_manifest(
            r#"{ name: "Acme", jira: { base_url: "https://a.example" },
                 teams: [{ id: "x" }, { id: "x" }] }"#,
        )
        .expect_err("duplicate ids must be rejected");
        assert!(dup.to_string().contains("duplicate"));
        assert!(
            parse_manifest(
                r#"{ name: "Acme", jira: { base_url: "https://a.example" }, teams: [{ id: "" }] }"#,
            )
            .is_err()
        );
    }

    // ── apply_company_defaults ────────────────────────────────────────────

    fn manifest() -> CompanyManifest {
        parse_manifest(FULL_MANIFEST).expect("valid manifest")
    }

    #[test]
    fn empty_user_config_takes_all_company_values() {
        let jira = apply_company_defaults(&JiraConfig::default(), &manifest());
        assert_eq!(jira.base_url, "https://acme.atlassian.net");
        assert_eq!(jira.default_project, "CORE");
        assert_eq!(jira.auth_method.as_deref(), Some("oauth"));
        assert_eq!(jira.oauth_client_id.as_deref(), Some("cid-123"));
        assert_eq!(jira.oauth_client_secret.as_deref(), Some("sec-456"));
    }

    #[test]
    fn user_set_fields_win_over_company() {
        let user = JiraConfig {
            base_url: "https://mine.atlassian.net".into(),
            default_project: "MINE".into(),
            auth_method: Some("basic".into()),
            oauth_client_id: Some("my-cid".into()),
            oauth_client_secret: Some("my-sec".into()),
            ..Default::default()
        };
        let jira = apply_company_defaults(&user, &manifest());
        assert_eq!(jira.base_url, "https://mine.atlassian.net");
        assert_eq!(jira.default_project, "MINE");
        assert_eq!(jira.auth_method.as_deref(), Some("basic"));
        assert_eq!(jira.oauth_client_id.as_deref(), Some("my-cid"));
        assert_eq!(jira.oauth_client_secret.as_deref(), Some("my-sec"));
    }

    #[test]
    fn company_oauth_creds_fill_in_even_when_user_chose_oauth() {
        // User opted into oauth but relies on the company app for credentials.
        let user = JiraConfig {
            auth_method: Some("oauth".into()),
            ..Default::default()
        };
        let jira = apply_company_defaults(&user, &manifest());
        assert_eq!(jira.oauth_client_id.as_deref(), Some("cid-123"));
        assert_eq!(jira.oauth_client_secret.as_deref(), Some("sec-456"));
    }

    #[test]
    fn manifest_without_oauth_leaves_auth_untouched() {
        let m = parse_manifest(r#"{ name: "Acme", jira: { base_url: "https://a.example" } }"#)
            .expect("valid manifest");
        let jira = apply_company_defaults(&JiraConfig::default(), &m);
        assert_eq!(jira.auth_method, None);
        assert_eq!(jira.oauth_client_id, None);
    }

    // ── company_team_refs ─────────────────────────────────────────────────

    #[test]
    fn team_refs_use_defaults_and_explicit_paths() {
        let clone_dir = Path::new("/home/u/.config/do-next/company/acme");
        let (refs, errors) = company_team_refs(
            clone_dir,
            &manifest(),
            &["platform".into(), "billing".into()],
        );
        assert!(errors.is_empty());
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "platform");
        assert_eq!(
            refs[0].path,
            "/home/u/.config/do-next/company/acme/teams/platform"
        );
        assert_eq!(refs[0].file, None);
        // Shortcut selections opt out of the backlog tab explicitly.
        assert_eq!(refs[0].backlog, Some(false));
        assert_eq!(
            refs[1].path,
            "/home/u/.config/do-next/company/acme/squads/billing"
        );
        assert_eq!(refs[1].file.as_deref(), Some("team.json5"));
    }

    #[test]
    fn team_refs_carry_the_backlog_opt_in() {
        let clone_dir = Path::new("/tmp/acme");
        let (refs, errors) = company_team_refs(
            clone_dir,
            &manifest(),
            &[CompanyTeamSelection::new("platform", true)],
        );
        assert!(errors.is_empty());
        assert_eq!(refs[0].backlog, Some(true));
    }

    #[test]
    fn unknown_team_id_is_a_nonfatal_error() {
        let clone_dir = Path::new("/tmp/acme");
        let (refs, errors) =
            company_team_refs(clone_dir, &manifest(), &["gone".into(), "platform".into()]);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "platform");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("gone"));
    }

    // ── slug_from_url ─────────────────────────────────────────────────────

    #[test]
    fn slug_handles_common_git_url_forms() {
        for (url, expected) in [
            ("git@github.com:acme/do-next-config.git", "do-next-config"),
            ("https://github.com/acme/cfg.git", "cfg"),
            ("https://github.com/acme/cfg.git/", "cfg"),
            ("https://github.com/acme/Config_Repo", "config-repo"),
            ("ssh://git@host/team/repo", "repo"),
            ("weird*** ", "weird"),
            ("///", "company"),
            ("", "company"),
        ] {
            assert_eq!(slug_from_url(url), expected, "url: {url:?}");
        }
    }
}
