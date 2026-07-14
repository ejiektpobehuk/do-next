use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::types::ResolvedTeam;

/// Check every distinct config repo (teams are grouped by git repo root, so a
/// company repo holding several team subdirectories is fetched once) and
/// report a warning per repo that is behind upstream.
pub fn check_updates(teams: &[ResolvedTeam]) -> Vec<String> {
    // BTreeMap for deterministic warning order.
    let mut by_root: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for team in teams {
        let path = super::expand_tilde(&team.path);
        if let Some(root) = git_toplevel(&path) {
            by_root.entry(root).or_default().push(team.id.clone());
        }
    }
    by_root
        .into_iter()
        .filter_map(|(root, team_ids)| {
            let behind = fetch_behind_count(&root)?;
            (behind > 0).then(|| format_update_message(&root, behind, &team_ids))
        })
        .collect()
}

fn format_update_message(root: &Path, count: u32, team_ids: &[String]) -> String {
    format!(
        "config repo {} has {} update{} (teams: {}) — git pull",
        root.display(),
        count,
        if count == 1 { "" } else { "s" },
        team_ids.join(", "),
    )
}

/// Fetch upstream refs and count how many commits the repo is behind.
/// `None` when the path is not a repo, has no upstream, or git fails.
pub fn fetch_behind_count(repo: &Path) -> Option<u32> {
    // Fetch latest refs from remote (silent, best-effort).
    // Force non-interactive: ssh's passphrase/host-key prompts go to /dev/tty,
    // not stdio, so silencing pipes is not enough — BatchMode=yes makes ssh
    // fail instead of prompting, and GIT_TERMINAL_PROMPT=0 blocks git's own
    // HTTPS credential prompts.
    let ssh_command = std::env::var("GIT_SSH_COMMAND").unwrap_or_else(|_| "ssh".to_string());
    let _ = Command::new("git")
        .args(["fetch", "--quiet"])
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", format!("{ssh_command} -o BatchMode=yes"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Compare local HEAD with upstream tracking branch
    let local = git_rev_parse(repo, "HEAD")?;
    let upstream = git_rev_parse(repo, "@{u}")?;
    if local == upstream {
        return Some(0);
    }

    // Count commits upstream has that local doesn't.
    let behind = Command::new("git")
        .args(["rev-list", "--count", &format!("{local}..{upstream}")])
        .current_dir(repo)
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&behind.stdout)
            .trim()
            .parse::<u32>()
            .unwrap_or(0),
    )
}

/// Fast-forward pull with inherited stdio so the user sees git's own output
/// and interactive auth works. Used by the startup company-update prompt.
pub fn pull_ff_only(repo: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(repo)
        .status();
    match status {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("git is not installed")
        }
        Err(e) => Err(e).context("Failed to run git pull"),
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("git pull --ff-only failed ({s})"),
    }
}

/// The repo root containing `path`, or `None` if it's not inside a git repo.
/// Resolves team subdirectories of a shared company clone to one common root.
fn git_toplevel(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        Some(PathBuf::from(
            String::from_utf8_lossy(&output.stdout).trim(),
        ))
    } else {
        None
    }
}

fn git_rev_parse(repo_path: &Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_message_names_repo_teams_and_count() {
        let msg = format_update_message(
            Path::new("/home/u/.config/do-next/company/acme"),
            3,
            &["platform".into(), "billing".into()],
        );
        assert_eq!(
            msg,
            "config repo /home/u/.config/do-next/company/acme has 3 updates \
             (teams: platform, billing) — git pull"
        );
        let one = format_update_message(Path::new("/tmp/r"), 1, &["personal".into()]);
        assert!(one.contains("has 1 update "), "singular form: {one}");
    }
}
