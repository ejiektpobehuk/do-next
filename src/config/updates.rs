use std::path::Path;
use std::process::Command;

use crate::config::types::ResolvedTeam;

/// Check if a team's config repo has upstream updates available.
/// Returns `Some(message)` if the team is behind upstream, `None` otherwise.
pub fn check_team_update(team: &ResolvedTeam) -> Option<String> {
    let path = super::expand_tilde(&team.path);
    if !path.join(".git").exists() {
        return None;
    }

    // Fetch latest refs from remote (silent, best-effort)
    let _ = Command::new("git")
        .args(["fetch", "--quiet"])
        .current_dir(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Compare local HEAD with upstream tracking branch
    let local = git_rev_parse(&path, "HEAD")?;
    let upstream = git_rev_parse(&path, "@{u}")?;

    if local == upstream {
        return None;
    }

    // Check if local is behind (upstream has commits local doesn't)
    let behind = Command::new("git")
        .args(["rev-list", "--count", &format!("{local}..{upstream}")])
        .current_dir(&path)
        .output()
        .ok()?;
    let count = String::from_utf8_lossy(&behind.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or(0);

    if count > 0 {
        Some(format!(
            "team '{}' config has {} update{} — git pull in {}",
            team.id,
            count,
            if count == 1 { "" } else { "s" },
            team.path,
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
