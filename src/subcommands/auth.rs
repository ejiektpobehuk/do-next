//! `do-next auth [<integration>] [--status [--online]]`.
//!
//! With no argument this opens the interactive menu; `--status` is the
//! scriptable reader, in the shape of `check`'s output.
//!
//! One difference from `check` worth knowing: `check` deliberately touches no
//! credential store, which is what lets it run headless. `--status` cannot make
//! that promise — reading the stores is the whole question it answers — so it is
//! *harmless* instead: it never runs a `credential_command`, never propagates a
//! keyring failure, and never prints a secret. See [`crate::auth`].

use anyhow::{Result, bail};

use crate::auth::{self, AuthTarget, Kind, MenuLine};
use crate::config::LoadedConfig;

/// Which integration `do-next auth <integration>` picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Integration {
    /// The Atlassian site — Jira, Confluence and boards share one credential.
    ///
    /// `jira` and `confluence` are accepted as aliases: they name products on
    /// this site rather than credentials of their own, and people will keep
    /// typing them.
    #[value(alias = "jira", alias = "confluence")]
    Atlassian,
    Gitlab,
    Grafana,
}

impl Integration {
    const fn kind(self) -> Kind {
        match self {
            Self::Atlassian => Kind::Atlassian,
            Self::Gitlab => Kind::Gitlab,
            Self::Grafana => Kind::Grafana,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Atlassian => "atlassian",
            Self::Gitlab => "gitlab",
            Self::Grafana => "grafana",
        }
    }
}

/// Lowercase name for the `integration` column, matching `check`'s row labels.
const fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Atlassian => "atlassian",
        Kind::Gitlab => "gitlab",
        Kind::Grafana => "grafana",
    }
}

pub async fn run(
    loaded: &mut LoadedConfig,
    integration: Option<Integration>,
    status: bool,
    online: bool,
) -> Result<()> {
    if status {
        return report_status(loaded, online).await;
    }

    // A named integration that matches nothing is a mistake worth naming, not
    // an empty menu.
    if let Some(wanted) = integration {
        let configured = auth::enumerate_targets(&loaded.config, &loaded.teams);
        if !configured.iter().any(|s| s.kind.kind() == wanted.kind()) {
            let mut names: Vec<&str> = configured
                .iter()
                .map(|s| kind_label(s.kind.kind()))
                .collect();
            names.dedup();
            if names.is_empty() {
                bail!(
                    "no {} integration is configured (nothing is)",
                    wanted.label()
                );
            }
            bail!(
                "no {} integration is configured (configured: {})",
                wanted.label(),
                names.join(", ")
            );
        }
    }

    crate::tui::onboarding::menu::run_auth_menu(loaded, integration.map(Integration::kind)).await
}

/// Print the state of every integration and exit.
async fn report_status(loaded: &LoadedConfig, online: bool) -> Result<()> {
    let step = crate::startup::Step::start("reading credential stores");
    let targets = auth::targets(loaded);
    step.done(format!(
        "{} integration{}",
        targets.len(),
        if targets.len() == 1 { "" } else { "s" }
    ));

    let verified = if online {
        let step = crate::startup::Step::start("verifying credentials");
        let verified = auth::verify_all(&targets).await;
        let failures = verified.values().filter(|v| !v.is_ok()).count();
        if failures == 0 {
            step.done("every credential works");
        } else {
            step.warn(format!(
                "{failures} credential{} failed",
                if failures == 1 { "" } else { "s" }
            ));
        }
        verified
    } else {
        std::collections::HashMap::new()
    };

    let lines = auth::build_menu(&targets, &verified, chrono::Utc::now());
    print!("{}", status_table(&targets, &lines, online));

    if !online {
        println!("\n(offline — add `--online` to confirm each credential against its API)");
        return Ok(());
    }

    // A broken credential is a real problem, so `--online` reports it in the
    // exit code. "Not configured" is not: it is a legitimate answer to the
    // question asked, and failing on it would make this useless in a shell
    // chain.
    let failures = verified.values().filter(|v| !v.is_ok()).count();
    if failures > 0 {
        bail!(
            "{failures} credential{} did not work",
            if failures == 1 { "" } else { "s" }
        );
    }
    println!("\nevery credential works");
    Ok(())
}

/// Render the status table.
///
/// Returns a `String` rather than printing, so the column arithmetic is
/// testable — the one place this departs from `check`'s inline `println!` style.
#[must_use]
fn status_table(targets: &[AuthTarget], lines: &[MenuLine], online: bool) -> String {
    // Only the target rows; the menu's separator and actions are UI.
    let rows: Vec<(&'static str, &MenuLine)> = lines
        .iter()
        .filter_map(|line| match line.action {
            auth::MenuAction::Target(i) => Some((kind_label(targets[i].kind.kind()), line)),
            _ => None,
        })
        .collect();

    if rows.is_empty() {
        return "no integrations are configured\n".to_string();
    }

    // The last column carries the products for a site and the teams for an
    // instance — different facts, but each is "what this credential is for".
    let detail = |line: &MenuLine| -> String {
        line.products
            .clone()
            .or_else(|| line.teams.clone())
            .unwrap_or_default()
    };

    let headers = ("integration", "instance", "state", "for");
    let w_kind = width(rows.iter().map(|(k, _)| *k).chain([headers.0]));
    let w_url = width(rows.iter().map(|(_, l)| l.url.as_str()).chain([headers.1]));
    let w_state = width(
        rows.iter()
            .map(|(_, l)| l.state.as_str())
            .chain([headers.2]),
    );

    let mut out = String::new();
    let header = format!(
        "{:<w_kind$}  {:<w_url$}  {:<w_state$}  {}",
        headers.0, headers.1, headers.2, headers.3
    );
    out.push_str(header.trim_end());
    out.push('\n');

    for (kind, line) in &rows {
        let mut row = format!(
            "{:<w_kind$}  {:<w_url$}  {:<w_state$}  {}",
            kind,
            line.url,
            line.state,
            detail(line)
        );
        if online {
            let verdict = line.verified.as_deref().unwrap_or("");
            row = format!("{}  {verdict}", row.trim_end());
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// Column width in characters, so a non-ASCII value still lines up.
fn width<'a>(values: impl Iterator<Item = &'a str>) -> usize {
    values.map(|v| v.chars().count()).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{
        AtlassianTarget, AuthKind, CredentialState, Product, SlotRef, VerifyOutcome,
    };
    use crate::config::types::AtlassianConfig;

    fn atlassian_target(url: &str, products: Vec<Product>) -> AuthTarget {
        AuthTarget {
            kind: AuthKind::Atlassian(Box::new(AtlassianTarget {
                config: AtlassianConfig {
                    base_url: url.into(),
                    ..Default::default()
                },
                products,
                extra_scopes: crate::atlassian::oauth::ExtraScopes::default(),
                slot: SlotRef::Primary,
            })),
            url: url.into(),
            team_ids: vec!["core".into()],
            state: CredentialState::Missing,
        }
    }

    fn table_of(targets: &[AuthTarget], online: bool) -> String {
        let lines = crate::auth::build_menu(targets, &std::collections::HashMap::new(), now());
        status_table(targets, &lines, online)
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
            .expect("valid")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn the_table_aligns_its_columns_around_the_widest_value() {
        let targets = vec![
            atlassian_target("https://a.net", vec![Product::Jira]),
            atlassian_target(
                "https://an-extremely-long-atlassian-site-name.example.com",
                vec![Product::Confluence],
            ),
        ];
        let table = table_of(&targets, false);
        let widths: Vec<usize> = table
            .lines()
            .map(|l| l.find("not configured").unwrap_or(0))
            .collect();
        // Every data row starts its state column at the same offset.
        assert_eq!(widths[1], widths[2], "state column must line up:\n{table}");
        assert!(table.starts_with("integration  instance"));
    }

    #[test]
    fn no_row_carries_trailing_whitespace() {
        // The table is meant to be piped; padding out to a fixed width would
        // show up as ragged trailing spaces.
        let targets = vec![atlassian_target("https://a.net", vec![Product::Jira])];
        for line in table_of(&targets, false).lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace in {line:?}");
        }
    }

    #[test]
    fn an_empty_config_says_so_rather_than_printing_a_bare_header() {
        assert_eq!(
            status_table(&[], &[], false),
            "no integrations are configured\n"
        );
    }

    #[test]
    fn a_site_row_shows_its_products_in_the_last_column() {
        let targets = vec![atlassian_target(
            "https://a.net",
            vec![Product::Jira, Product::Confluence],
        )];
        let table = table_of(&targets, false);
        assert!(
            table.contains("Jira \u{b7} Confluence"),
            "products missing from:\n{table}"
        );
        assert!(
            !table.contains("teams:"),
            "a site names products, not teams:\n{table}"
        );
    }

    #[test]
    fn every_integration_has_a_lowercase_column_label() {
        // The column mirrors check's row labels, which are lowercase.
        for kind in [Kind::Atlassian, Kind::Gitlab, Kind::Grafana] {
            let label = kind_label(kind);
            assert_eq!(label, label.to_lowercase());
            assert!(!label.is_empty());
        }
    }

    #[test]
    fn the_jira_and_confluence_aliases_both_select_the_atlassian_row() {
        use clap::ValueEnum;
        for alias in ["atlassian", "jira", "confluence"] {
            let parsed = Integration::from_str(alias, true)
                .unwrap_or_else(|e| panic!("`{alias}` should parse: {e}"));
            assert_eq!(parsed.kind(), Kind::Atlassian, "for `{alias}`");
        }
        assert_eq!(
            Integration::from_str("gitlab", true)
                .expect("parses")
                .kind(),
            Kind::Gitlab
        );
    }

    #[test]
    fn a_verified_column_appears_only_with_online() {
        let targets = vec![atlassian_target("https://a.net", vec![Product::Jira])];
        let mut verified = std::collections::HashMap::new();
        verified.insert(targets[0].id(), VerifyOutcome::Ok("Vlad Petrov".into()));
        let lines = crate::auth::build_menu(&targets, &verified, now());

        assert!(status_table(&targets, &lines, true).contains("Vlad Petrov"));
        assert!(
            !status_table(&targets, &lines, false).contains("Vlad Petrov"),
            "offline output must not claim a verification happened"
        );
    }
}
