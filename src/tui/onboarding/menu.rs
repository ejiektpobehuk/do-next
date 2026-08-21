//! The `do-next auth` menu: every integration and instance you can sign in to,
//! each showing what its credential state is, looping until you are done.
//!
//! The loop rebuilds and re-probes its rows on every pass rather than mutating
//! what it has. Anything that configures a credential writes the on-disk config
//! *and* an external store, and the next probe reads the merged view — so a
//! flow that sets `credential_store: "keyring"` would otherwise leave the
//! resolved config stale and report the token it just stored as missing.
//!
//! Errors are shown and the loop continues: for an explicit `do-next auth` the
//! user is right there and can retry, and aborting the process over one bad
//! token would strand the other rows.

use anyhow::Result;
use std::collections::HashMap;

use crate::auth::{self, AuthKind, AuthTarget, Kind, MenuAction, MenuLine, RowId, VerifyOutcome};
use crate::config::LoadedConfig;

use super::{SelectRow, is_cancelled, run_menu};

/// Run the interactive auth menu.
///
/// `filter` restricts the rows to one integration, for `do-next auth <name>`.
/// When it leaves exactly one row, that row runs directly and the menu never
/// appears — that mode is explicitly "skip the menu".
pub async fn run_auth_menu(loaded: &mut LoadedConfig, filter: Option<Kind>) -> Result<()> {
    let mut verified: HashMap<RowId, VerifyOutcome> = HashMap::new();
    let mut cursor = 0;

    // A single filtered row: go straight in, then leave.
    if filter.is_some() {
        let targets = collect(loaded, filter);
        if targets.len() == 1 {
            return dispatch_and_report(&targets[0], loaded).await;
        }
    }

    loop {
        let targets = collect(loaded, filter);
        if targets.is_empty() {
            println!("No integrations are configured.");
            return Ok(());
        }

        let lines = auth::build_menu(&targets, &verified, chrono::Utc::now());
        let rows = to_rows(&lines, terminal_width());

        let Some(choice) = run_menu("Integrations", &rows, cursor)? else {
            return Ok(());
        };
        cursor = choice;

        match lines[choice].action {
            MenuAction::Done => return Ok(()),
            MenuAction::VerifyAll => {
                verified = verify_and_report(&targets).await;
            }
            MenuAction::Target(i) => {
                let id = targets[i].id();
                match dispatch(&targets[i], loaded).await {
                    Ok(()) => {
                        // The credential changed, so any earlier verdict is a
                        // lie about the credential that is there now.
                        verified.remove(&id);
                    }
                    Err(e) if is_cancelled(&e) => {}
                    Err(e) => eprintln!("error: {e:#}"),
                }
            }
            // Separators are never returned as a choice.
            MenuAction::None => {}
        }
    }
}

fn collect(loaded: &LoadedConfig, filter: Option<Kind>) -> Vec<AuthTarget> {
    let step = crate::startup::Step::start("reading credential stores");
    let mut targets = auth::targets(loaded);
    if let Some(kind) = filter {
        targets.retain(|t| t.kind.kind() == kind);
    }
    step.done(format!(
        "{} integration{}",
        targets.len(),
        if targets.len() == 1 { "" } else { "s" }
    ));
    targets
}

/// Run one row's flow, then reload so the next probe sees what it wrote.
async fn dispatch(target: &AuthTarget, loaded: &mut LoadedConfig) -> Result<()> {
    match &target.kind {
        AuthKind::Atlassian(site) => {
            super::run_auth_reset(&site.config, &mut loaded.raw, &site.slot, site.extra_scopes)?;
        }
        AuthKind::Gitlab(gitlab) => {
            super::gitlab::configure_gitlab_token(&gitlab.setup, &mut loaded.raw).await?;
        }
        AuthKind::Grafana(grafana) => {
            super::grafana::configure_grafana_token(&grafana.setup, &mut loaded.raw).await?;
        }
    }

    reload(loaded)?;

    // Say who the credential belongs to, which the Atlassian flow has never
    // done: `myself()` existed all along but nothing called it, so a mistyped
    // token was only discovered later, from somewhere else. The GitLab and
    // Grafana flows already greet the user themselves.
    if matches!(target.kind, AuthKind::Atlassian(_)) {
        let fresh = collect(loaded, Some(Kind::Atlassian));
        if let Some(same) = fresh.iter().find(|t| t.id() == target.id()) {
            match auth::verify(same).await {
                VerifyOutcome::Ok(who) => println!("Authenticated as {who}."),
                VerifyOutcome::Failed(why) => {
                    eprintln!("warning: the credential was stored but did not work: {why}");
                }
            }
        }
    }
    Ok(())
}

/// `do-next auth <integration>` with a single match: run it, report, done.
async fn dispatch_and_report(target: &AuthTarget, loaded: &mut LoadedConfig) -> Result<()> {
    match dispatch(target, loaded).await {
        Ok(()) => Ok(()),
        Err(e) if is_cancelled(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

fn reload(loaded: &mut LoadedConfig) -> Result<()> {
    let step = crate::startup::Step::start("reloading config");
    *loaded = crate::config::load()?;
    step.done("config reloaded");
    Ok(())
}

/// Verify every row, print the block, and hand back the verdicts to cache.
async fn verify_and_report(targets: &[AuthTarget]) -> HashMap<RowId, VerifyOutcome> {
    let step = crate::startup::Step::start(format!("verifying {} integrations", targets.len()));
    let verified = auth::verify_all(targets).await;
    let failures = verified.values().filter(|v| !v.is_ok()).count();
    if failures == 0 {
        step.done("every credential works");
    } else {
        step.warn(format!(
            "{failures} credential{} failed",
            if failures == 1 { "" } else { "s" }
        ));
    }

    println!();
    let name_width = targets
        .iter()
        .map(|t| t.kind.name().chars().count())
        .max()
        .unwrap_or(0);
    for target in targets {
        let Some(outcome) = verified.get(&target.id()) else {
            continue;
        };
        // The full text here; the row tag gets a truncated one line.
        let detail = match outcome {
            VerifyOutcome::Ok(who) => format!("\u{2713} {who}"),
            VerifyOutcome::Failed(why) => format!("\u{2717} {why}"),
        };
        println!(
            "  {:<name_width$}  {}  {detail}",
            target.kind.name(),
            target.url
        );
    }
    println!();
    verified
}

/// Map the model's rows onto the prompt layer's, truncated to fit.
fn to_rows(lines: &[MenuLine], width: usize) -> Vec<SelectRow> {
    let fitted = auth::fit_columns(lines, width);
    fitted
        .iter()
        .map(|line| {
            if !line.selectable {
                return SelectRow::separator();
            }
            // Action rows (Verify all, Done) are a bare label.
            if line.url.is_empty() && line.state.is_empty() {
                return SelectRow::new(line.name, "", "");
            }
            let tag = line.verified.as_ref().map_or_else(
                || format!("[{}]", line.state),
                |verdict| format!("[{}]  {verdict}", line.state),
            );
            let mut row = SelectRow::new(line.name, line.url.clone(), tag);
            if let Some(teams) = &line.teams {
                row.tag = format!("{}   {teams}", row.tag);
            }
            if let Some(products) = &line.products {
                row = row.with_sublabel(products.clone());
            }
            row
        })
        .collect()
}

/// Usable width, with a floor so a failed query or a very narrow terminal still
/// produces something rather than truncating everything to nothing.
fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map_or(80, |(cols, _)| usize::from(cols))
        .max(40)
}

#[cfg(test)]
mod tests {
    use super::to_rows;
    use crate::auth::{MenuAction, MenuLine};

    fn target_line(state: &str) -> MenuLine {
        MenuLine {
            name: crate::auth::ATLASSIAN,
            url: "https://acme.atlassian.net".into(),
            state: state.into(),
            products: Some("Jira \u{b7} Confluence".into()),
            teams: None,
            verified: None,
            action: MenuAction::Target(0),
            selectable: true,
        }
    }

    fn action_line(name: &'static str, action: MenuAction) -> MenuLine {
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

    #[test]
    fn a_site_row_shows_its_state_bracketed_and_its_products_beneath() {
        let rows = to_rows(&[target_line("OAuth \u{b7} keyring")], 120);
        assert_eq!(rows[0].label, crate::auth::ATLASSIAN);
        assert_eq!(rows[0].description, "https://acme.atlassian.net");
        assert_eq!(rows[0].tag, "[OAuth \u{b7} keyring]");
        assert_eq!(rows[0].sublabel.as_deref(), Some("Jira \u{b7} Confluence"));
        assert!(rows[0].selectable);
    }

    #[test]
    fn a_verified_row_keeps_both_the_state_and_the_verdict() {
        let mut line = target_line("OAuth \u{b7} keyring");
        line.verified = Some("\u{2713} Vlad Petrov".into());
        let rows = to_rows(&[line], 120);
        assert_eq!(rows[0].tag, "[OAuth \u{b7} keyring]  \u{2713} Vlad Petrov");
    }

    #[test]
    fn an_instance_row_appends_its_team_list_to_the_tag() {
        let mut line = target_line("token \u{b7} file");
        line.products = None;
        line.teams = Some("teams: core, infra".into());
        let rows = to_rows(&[line], 200);
        assert!(
            rows[0].tag.ends_with("teams: core, infra"),
            "got {}",
            rows[0].tag
        );
        assert_eq!(rows[0].sublabel, None);
    }

    #[test]
    fn the_action_rows_stay_bare_labels_and_the_separator_stays_inert() {
        let lines = vec![
            MenuLine {
                selectable: false,
                ..action_line("", MenuAction::None)
            },
            action_line("Verify all (network)", MenuAction::VerifyAll),
            action_line("Done", MenuAction::Done),
        ];
        let rows = to_rows(&lines, 120);
        assert!(!rows[0].selectable, "the separator must not be choosable");
        assert_eq!(rows[1].label, "Verify all (network)");
        assert_eq!(rows[1].tag, "", "an action row carries no state tag");
        assert!(rows[1].selectable);
        assert_eq!(rows[2].label, "Done");
    }

    #[test]
    fn a_narrow_terminal_still_yields_one_row_per_line() {
        // The redraw moves up by a fixed count, so the mapping must not add or
        // drop rows no matter how little room there is.
        let lines = vec![
            target_line("token \u{b7} file"),
            action_line("Done", MenuAction::Done),
        ];
        for width in [40, 60, 80, 200] {
            assert_eq!(
                to_rows(&lines, width).len(),
                lines.len(),
                "at width {width}"
            );
        }
    }
}
