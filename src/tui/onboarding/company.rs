//! Company onboarding: join a company config repo (clone it if needed),
//! pick teams from its manifest catalog, and authenticate via the company's
//! shared OAuth app. Connection and OAuth values stay in the repo's manifest;
//! only the company reference and credential-storage choices are written to
//! the user's config.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::company as company_cfg;
use crate::config::company::CompanyManifest;
use crate::config::types::{Config, CompanyRef, JiraConfig, TeamConfig};
use crate::config::{LoadedConfig, extra_scopes_for};
use crate::jira::auth::OAuthStore;

use super::{
    StorageChoice, apply_token_storage, prompt, prompt_oauth_storage, prompt_token_storage,
    prompt_yes_no, run_multi_selection,
};

/// First-run "Join a company" branch: writes a fresh user config, then loads
/// it from disk so the manifest merge and team resolution run through the
/// normal pipeline.
pub(super) fn run_first_run_company_join() -> Result<LoadedConfig> {
    println!();
    let source = prompt("Company config repo (git URL or local path): ", None)?;
    let mut config = Config::default();
    join_company_into(&mut config, &source)?;
    crate::config::load()
}

/// `do-next company join <source>` for an existing install.
pub fn run_company_join_command(raw: &mut Config, source: &str) -> Result<()> {
    if let Some(existing) = &raw.company {
        let shown = existing.url.as_deref().unwrap_or(&existing.path);
        println!("A company is already configured: {shown}");
        println!("Joining a new one replaces it (your other settings are kept).");
        let replace = prompt_yes_no("Replace it? [y/N]: ", false)?;
        if !replace {
            println!("Keeping the current company.");
            return Ok(());
        }
        println!();
    }
    join_company_into(raw, source)?;
    println!();
    println!("All set — run do-next to start.");
    Ok(())
}

/// `do-next company teams`: re-run the team picker over the manifest catalog.
pub fn run_company_teams_command(raw: &mut Config) -> Result<()> {
    let Some(company) = raw.company.clone() else {
        bail!("No company configured. Run `do-next company join <url>` first.");
    };
    let dir = crate::config::expand_tilde(&company.path);
    let manifest = company_cfg::load_manifest(&dir)?;

    // Scopes granted for the current selection, to detect when the new
    // selection needs a re-authorization.
    let old_configs = load_selected_team_configs(&dir, &manifest, &company.teams);
    let old_extra = extra_scopes_for(&old_configs);

    let picked = pick_teams(&dir, &manifest, &company.teams)?;
    let new_extra = extra_scopes_for(picked.iter().map(|(_, tc)| tc));
    let ids: Vec<String> = picked.into_iter().map(|(id, _)| id).collect();

    if let Some(c) = raw.company.as_mut() {
        c.teams.clone_from(&ids);
    }
    write_user_config(raw)?;
    println!("Active company teams: {}", ids.join(", "));

    let effective = company_cfg::apply_company_defaults(&raw.jira, &manifest);
    let needs_more_scopes = (new_extra.confluence && !old_extra.confluence)
        || (new_extra.board && !old_extra.board);
    if needs_more_scopes && effective.auth_method.as_deref() == Some("oauth") {
        println!();
        println!("New teams need additional OAuth scopes — run `do-next auth` to re-authorize.");
    }
    Ok(())
}

/// Full join flow into an existing (possibly default) config: resolve the
/// source, parse the manifest, pick teams, authenticate, persist.
pub(super) fn join_company_into(config: &mut Config, source: &str) -> Result<()> {
    let (url, dir) = resolve_source(source)?;
    let manifest = company_cfg::load_manifest(&dir)?;
    println!();
    println!(
        "Found company config: {} ({})",
        manifest.name, manifest.jira.base_url
    );
    println!();

    let picked = pick_teams(&dir, &manifest, &[])?;
    let extra = extra_scopes_for(picked.iter().map(|(_, tc)| tc));
    let auth = run_company_auth(&manifest, extra)?;
    apply_auth_fields(config, &manifest, auth);

    config.company = Some(CompanyRef {
        url,
        path: dir.to_string_lossy().into_owned(),
        teams: picked.into_iter().map(|(id, _)| id).collect(),
    });
    write_user_config(config)
}

/// Best-effort load of the team configs behind a selection; entries that
/// fail to parse simply don't contribute (used only for scope comparison).
fn load_selected_team_configs(
    dir: &Path,
    manifest: &CompanyManifest,
    selected: &[String],
) -> Vec<TeamConfig> {
    let (refs, _) = company_cfg::company_team_refs(dir, manifest, selected);
    refs.iter()
        .filter_map(|r| {
            let path = crate::config::expand_tilde(&r.path)
                .join(r.file.as_deref().unwrap_or("do-next.json5"));
            crate::config::load_file(&path).ok()
        })
        .collect()
}

/// A local directory containing `company.json5` is used in place; anything
/// else is treated as a git URL and cloned into the managed location.
fn resolve_source(source: &str) -> Result<(Option<String>, PathBuf)> {
    let expanded = crate::config::expand_tilde(source);
    if expanded.join(company_cfg::MANIFEST_FILE).is_file() {
        return Ok((None, expanded));
    }
    if expanded.is_dir() {
        bail!(
            "{} exists but contains no {}",
            expanded.display(),
            company_cfg::MANIFEST_FILE
        );
    }

    let slug = company_cfg::slug_from_url(source);
    let dest = company_cfg::clone_dir_for(&slug)?;
    if dest.join(".git").exists() {
        println!("A clone already exists at {}.", dest.display());
        let reuse = prompt_yes_no("Use it? [Y/n]: ", true)?;
        if !reuse {
            bail!(
                "Remove {} (or pass a local path directly) and retry",
                dest.display()
            );
        }
    } else {
        println!("Cloning {source} into {} …", dest.display());
        company_cfg::clone_repo(source, &dest)?;
    }
    Ok((Some(source.to_string()), dest))
}

/// Multi-select over the manifest catalog. Entries whose config fails to
/// parse are shown but can't be selected. Returns the picked ids with their
/// already-parsed team configs (used for scope computation).
fn pick_teams(
    dir: &Path,
    manifest: &CompanyManifest,
    preselected_ids: &[String],
) -> Result<Vec<(String, TeamConfig)>> {
    if manifest.teams.is_empty() {
        bail!(
            "the company manifest has no teams in its catalog — \
             ask the config repo maintainer to add some"
        );
    }

    let label_width = manifest
        .teams
        .iter()
        .map(|t| t.display_name().chars().count())
        .max()
        .unwrap_or(0);

    let mut labels = Vec::new();
    let mut descriptions = Vec::new();
    let mut tags = Vec::new();
    let mut preselected = Vec::new();
    let mut selectable = Vec::new();
    let mut configs: Vec<Option<TeamConfig>> = Vec::new();

    for entry in &manifest.teams {
        let config_path = dir
            .join(entry.rel_path())
            .join(entry.file.as_deref().unwrap_or("do-next.json5"));
        let loaded: Result<TeamConfig> = crate::config::load_file(&config_path);
        labels.push(format!("{:<label_width$}", entry.display_name()));
        descriptions.push(entry.description.clone().unwrap_or_default());
        let ok = loaded.is_ok();
        tags.push(if ok {
            String::new()
        } else {
            "  [config error]".into()
        });
        selectable.push(ok);
        let wanted = if preselected_ids.is_empty() {
            entry.default
        } else {
            preselected_ids.contains(&entry.id)
        };
        preselected.push(wanted && ok);
        configs.push(loaded.ok());
    }

    let picked = run_multi_selection(
        "Which teams do you want on your board?",
        &labels,
        &descriptions,
        &tags,
        &preselected,
        &selectable,
    )?;

    Ok(picked
        .into_iter()
        .map(|i| {
            let config = configs[i].take().expect("unselectable rows can't be picked");
            (manifest.teams[i].id.clone(), config)
        })
        .collect())
}

/// Authenticate against the company's Jira. With a shared OAuth app the only
/// question is token storage; without one, fall back to the personal API
/// token flow. Returns the credential fields to persist in the user config.
fn run_company_auth(
    manifest: &CompanyManifest,
    extra: crate::jira::oauth::ExtraScopes,
) -> Result<JiraConfig> {
    let mut jira = JiraConfig::default();
    if let Some(oauth) = &manifest.oauth {
        println!(
            "Authenticating with {} via {}'s shared OAuth app.",
            manifest.jira.base_url, manifest.name
        );
        println!();
        let storage = prompt_oauth_storage(None)?;
        let store = match storage {
            StorageChoice::Keyring => OAuthStore::Keyring,
            _ => OAuthStore::File,
        };
        crate::jira::oauth::run_oauth_flow(&oauth.client_id, &oauth.client_secret, store, extra)?;
        if matches!(storage, StorageChoice::Keyring) {
            jira.credential_store = Some("keyring".into());
        }
    } else {
        println!("The company manifest defines no OAuth app — using a personal API token.");
        println!();
        let storage = prompt_token_storage(None, None)?;
        let email = prompt("Jira account email: ", None)?;
        let config_dir = dirs::config_dir()
            .context("Cannot determine config directory")?
            .join("do-next");
        std::fs::create_dir_all(&config_dir)?;
        // The keyring entry key defaults to the base URL at resolution time;
        // storage must use the same base so the entry matches. The value is
        // not persisted — the manifest provides it at load time.
        jira.base_url.clone_from(&manifest.jira.base_url);
        apply_token_storage(&storage, &mut jira, &config_dir)?;
        jira.base_url = String::new();
        jira.email = Some(email);
    }
    Ok(jira)
}

/// Land the join flow's credential choices in the user config. With a shared
/// OAuth app the manifest implies `auth_method: "oauth"` at load time, so
/// stale token-auth fields are cleared rather than copied over.
fn apply_auth_fields(config: &mut Config, manifest: &CompanyManifest, auth: JiraConfig) {
    config.jira.auth_method = None;
    config.jira.credential_store = auth.credential_store;
    if manifest.oauth.is_some() {
        config.jira.credential_command = None;
        config.jira.email = None;
    } else {
        config.jira.email = auth.email;
        config.jira.credential_command = auth.credential_command;
    }
}

fn write_user_config(config: &Config) -> Result<()> {
    let path = crate::config::user_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, json5::to_string(config)?)?;
    println!("Config written to {}", path.display());
    Ok(())
}
