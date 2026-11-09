//! Writing files that hold secrets (tokens, OAuth credentials).
//!
//! `std::fs::write` followed by `set_permissions(0o600)` leaves a window in
//! which the secret sits on disk with the umask's default mode, usually
//! world-readable. It also never tightens a file that already exists with
//! loose permissions until after the new secret is in it.
//!
//! [`write_secret_file`] closes both gaps: the content goes into a fresh
//! sibling file that is *created* with mode 0600, and is then renamed over
//! the target. The secret is never readable by anyone else, and a crash
//! mid-write leaves the old file intact instead of a truncated one.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Atomically replace `path` with `contents`, owner-only (0600) on Unix.
///
/// Creates the parent directory if needed.
pub fn write_secret_file(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    let file_name = path
        .file_name()
        .with_context(|| format!("Not a file path: {}", path.display()))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(file_name);
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);

    let result =
        write_new_private(&tmp, contents.as_ref()).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.with_context(|| format!("Failed to write {}", path.display()))
}

/// Create `path` (which must not exist; a stale leftover from a crashed run
/// is removed first) with owner-only permissions, write, and flush to disk.
fn write_new_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = match opts.open(path) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(path)?;
            opts.open(path)?
        }
        other => other?,
    };
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_contents_and_creates_parent_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/tokens.json5");
        write_secret_file(&path, "secret").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "secret");
    }

    #[test]
    fn replaces_existing_content_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tokens.json5");
        write_secret_file(&path, "a much longer old secret").expect("first write");
        write_secret_file(&path, "new").expect("second write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "new");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("tokens.json5")]);
    }

    #[cfg(unix)]
    #[test]
    fn file_is_owner_only_even_when_it_existed_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tokens.json5");
        std::fs::write(&path, "old").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        write_secret_file(&path, "new").expect("write");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_temp_file_from_a_crashed_run_does_not_block_the_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tokens.json5");
        let stale = dir
            .path()
            .join(format!(".tokens.json5.tmp-{}", std::process::id()));
        std::fs::write(&stale, "leftover").expect("seed stale");

        write_secret_file(&path, "new").expect("write");

        assert_eq!(std::fs::read_to_string(&path).expect("read"), "new");
        assert!(!stale.exists());
    }
}
