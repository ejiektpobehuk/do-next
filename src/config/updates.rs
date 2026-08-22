use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::types::ResolvedTeam;

/// Deadline for the startup `git fetch`. A config repo hosted behind a VPN
/// that happens to be off does not fail fast — the TCP connect sits there
/// until the kernel gives up, minutes later — so the check needs a deadline of
/// its own. Short, because the user is waiting for the TUI to open.
pub const STARTUP_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Deadline for the in-TUI check. Nobody is blocked on it, so a slow-but-alive
/// remote gets more room than at startup.
pub const BACKGROUND_FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// How often the fetch is checked for completion while waiting out the timeout.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Keeps a one-line failure reason readable inside a startup verdict.
const MAX_REASON_LEN: usize = 100;

/// Whether the ref state we compared against is current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// The fetch succeeded — upstream refs are current.
    Fetched,
    /// The remote could not be reached (VPN off, DNS, auth, timeout). The
    /// carried string is a short reason for the user; the comparison below
    /// used whatever the last successful fetch left on disk.
    Unreachable(String),
}

/// How far behind upstream a config repo is, and whether that number is
/// based on freshly fetched refs.
#[derive(Debug, Clone)]
pub struct UpdateStatus {
    pub behind: u32,
    pub freshness: Freshness,
}

impl UpdateStatus {
    /// The reason the remote could not be reached, if it could not.
    pub fn unreachable_reason(&self) -> Option<&str> {
        match &self.freshness {
            Freshness::Fetched => None,
            Freshness::Unreachable(reason) => Some(reason),
        }
    }
}

/// Check every distinct config repo (teams are grouped by git repo root, so a
/// company repo holding several team subdirectories is fetched once) and
/// report a warning per repo that is behind upstream.
///
/// A repo whose remote is unreachable is only mentioned when the refs we
/// already have say updates are pending; an unreachable remote on an
/// up-to-date checkout is not worth a banner.
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
            let status = check_repo(&root, BACKGROUND_FETCH_TIMEOUT)?;
            (status.behind > 0).then(|| format_update_message(&root, &status, &team_ids))
        })
        .collect()
}

fn format_update_message(root: &Path, status: &UpdateStatus, team_ids: &[String]) -> String {
    let count = status.behind;
    let stale = status
        .unreachable_reason()
        .map_or_else(String::new, |reason| {
            format!(" [as of the last fetch: {reason}]")
        });
    format!(
        "config repo {} has {} update{} (teams: {}) — git pull{}",
        root.display(),
        count,
        if count == 1 { "" } else { "s" },
        team_ids.join(", "),
        stale,
    )
}

/// Fetch upstream refs — giving up after `timeout` — and count how many
/// commits the repo is behind. `None` when the path is not a repo, has no
/// upstream, or git is unusable; those are not update problems and have
/// nothing to report.
pub fn check_repo(repo: &Path, timeout: Duration) -> Option<UpdateStatus> {
    let freshness = fetch(repo, timeout);

    // Compare local HEAD with the upstream tracking branch. Works offline:
    // when the fetch above failed, this reports what the last one brought in.
    let local = git_rev_parse(repo, "HEAD")?;
    let upstream = git_rev_parse(repo, "@{u}")?;
    let behind = if local == upstream {
        0
    } else {
        // Count commits upstream has that local doesn't.
        let out = Command::new("git")
            .args(["rev-list", "--count", &format!("{local}..{upstream}")])
            .current_dir(repo)
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<u32>()
            .unwrap_or(0)
    };
    Some(UpdateStatus { behind, freshness })
}

/// Fetch upstream refs, best-effort and never longer than `timeout`.
///
/// Force non-interactive: ssh's passphrase/host-key prompts go to /dev/tty,
/// not stdio, so silencing pipes is not enough — `BatchMode=yes` makes ssh
/// fail instead of prompting, and `GIT_TERMINAL_PROMPT=0` blocks git's own
/// HTTPS credential prompts. `ConnectTimeout` and the HTTP low-speed limit
/// let git itself give up on a dead remote; the wall-clock deadline below is
/// the backstop for everything they miss.
fn fetch(repo: &Path, timeout: Duration) -> Freshness {
    let secs = timeout.as_secs().max(1);
    let ssh_command = std::env::var("GIT_SSH_COMMAND").unwrap_or_else(|_| "ssh".to_string());
    let child = Command::new("git")
        .args([
            "-c",
            "http.lowSpeedLimit=1",
            "-c",
            &format!("http.lowSpeedTime={secs}"),
            "fetch",
            "--quiet",
        ])
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            format!("{ssh_command} -o BatchMode=yes -o ConnectTimeout={secs}"),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Freshness::Unreachable("git is not installed".into());
        }
        Err(e) => return Freshness::Unreachable(format!("cannot run git fetch: {e}")),
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Freshness::Fetched,
            Ok(Some(_)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read;
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return Freshness::Unreachable(summarize_git_error(&stderr));
            }
            Err(e) => return Freshness::Unreachable(format!("git fetch failed: {e}")),
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Leaves the ssh/https child to be reaped by its own
                    // ConnectTimeout; git itself is gone either way.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Freshness::Unreachable(format!("no answer within {secs}s"));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

/// Boil git's stderr down to one short line fit for a startup verdict.
fn summarize_git_error(stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(|l| l.trim().trim_start_matches("fatal: "))
        .find(|l| !l.is_empty())
        .unwrap_or("git fetch failed");
    if line.chars().count() > MAX_REASON_LEN {
        let short: String = line.chars().take(MAX_REASON_LEN).collect();
        format!("{short}…")
    } else {
        line.to_string()
    }
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
    use std::assert_matches;

    fn fetched(behind: u32) -> UpdateStatus {
        UpdateStatus {
            behind,
            freshness: Freshness::Fetched,
        }
    }

    #[test]
    fn update_message_names_repo_teams_and_count() {
        let msg = format_update_message(
            Path::new("/home/u/.config/do-next/company/acme"),
            &fetched(3),
            &["platform".into(), "billing".into()],
        );
        assert_eq!(
            msg,
            "config repo /home/u/.config/do-next/company/acme has 3 updates \
             (teams: platform, billing) — git pull"
        );
        let one = format_update_message(Path::new("/tmp/r"), &fetched(1), &["personal".into()]);
        assert!(one.contains("has 1 update "), "singular form: {one}");
    }

    #[test]
    fn update_message_marks_a_stale_comparison() {
        let status = UpdateStatus {
            behind: 2,
            freshness: Freshness::Unreachable("no answer within 5s".into()),
        };
        let msg = format_update_message(Path::new("/tmp/r"), &status, &["platform".into()]);
        assert!(
            msg.ends_with("[as of the last fetch: no answer within 5s]"),
            "stale note missing: {msg}"
        );
    }

    #[test]
    fn unreachable_reason_only_for_failed_fetches() {
        assert_eq!(fetched(0).unreachable_reason(), None);
        let stale = UpdateStatus {
            behind: 0,
            freshness: Freshness::Unreachable("VPN?".into()),
        };
        assert_eq!(stale.unreachable_reason(), Some("VPN?"));
    }

    #[test]
    fn git_error_summary_takes_the_first_meaningful_line() {
        assert_eq!(
            summarize_git_error(
                "\nfatal: Could not read from remote repository.\n\nPlease make sure...\n"
            ),
            "Could not read from remote repository."
        );
        assert_eq!(summarize_git_error(""), "git fetch failed");
        assert_eq!(summarize_git_error("   \n \n"), "git fetch failed");
    }

    #[test]
    fn git_error_summary_is_truncated() {
        let long = "x".repeat(MAX_REASON_LEN + 20);
        let summary = summarize_git_error(&long);
        assert_eq!(summary.chars().count(), MAX_REASON_LEN + 1);
        assert!(summary.ends_with('\u{2026}'));
    }

    #[test]
    fn a_non_repo_path_has_nothing_to_report() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(check_repo(dir.path(), Duration::from_secs(1)).is_none());
    }

    #[test]
    fn a_repo_without_an_upstream_has_nothing_to_report() {
        let dir = tempfile::tempdir().expect("temp dir");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("run git");
        };
        git(&["init", "--quiet"]);
        git(&[
            "-c",
            "user.email=t@e",
            "-c",
            "user.name=T",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "init",
        ]);
        // No remote configured: the fetch fails, and `@{u}` does not resolve.
        assert!(check_repo(dir.path(), Duration::from_secs(2)).is_none());
    }

    #[test]
    fn an_unreachable_remote_times_out_instead_of_hanging() {
        let dir = tempfile::tempdir().expect("temp dir");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git must be installed to run this test");
        };
        git(&["init", "--quiet"]);
        // 198.51.100.0/24 is TEST-NET-2: reserved and unroutable, so the
        // connect hangs exactly like a VPN-only host with the VPN off. Over
        // https git has no connect-phase timeout of its own — this is the
        // case only the wall-clock deadline can end.
        git(&["remote", "add", "origin", "https://198.51.100.1/cfg.git"]);

        let timeout = Duration::from_secs(2);
        let started = Instant::now();
        let freshness = fetch(dir.path(), timeout);
        assert_matches!(
            freshness,
            Freshness::Unreachable(_),
            "expected an unreachable verdict"
        );
        assert!(
            started.elapsed() < timeout * 3,
            "the fetch must give up promptly, took {:?}",
            started.elapsed()
        );
    }
}
