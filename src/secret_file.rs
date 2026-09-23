use crate::Result;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

/// Replaces `path` with `bytes` atomically, readable only by the owner. The
/// only way `config.toml` and `tokens.json` are written.
///
/// The bytes go to a fresh sibling temp file created with mode 0600 (unix),
/// which is fsynced and renamed over `path`: readers see the old or the new
/// file, never a partial one, and the secret never exists under a wider mode.
/// Missing parent directories are created with `create_private_dir`. The temp
/// file is removed when the write or the rename fails.
pub(crate) fn write_secret(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let temp = temp_sibling(path);
    let mut file = open_private(OpenOptions::new().write(true).create_new(true), &temp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| std::fs::rename(&temp, path)) {
        // Report the write error; a failed cleanup only leaves an owner-only
        // temp file behind.
        let _ = std::fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(())
}

/// Creates `dir` and its missing parents 0700 (unix); directories that already
/// exist keep their mode.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(dir)
}

/// Opens `path` with `options`; a file it creates is 0600 (unix).
pub(crate) fn open_private(options: &mut OpenOptions, path: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

/// `.<name>.<pid>.<nanos>.<n>.tmp` beside `path`: the pid and counter keep
/// concurrent writers apart, the timestamp keeps a file left by a crashed
/// writer whose pid was later reused from colliding with `create_new`.
fn temp_sibling(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}.{nanos}.{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_0644_target_with_an_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_secret(&path, b"new").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();

        write_secret(&dir.path().join("secret"), b"new").unwrap();

        assert_eq!(names(dir.path()), ["secret"]);
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("secret");

        write_secret(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn removes_the_temp_file_when_the_rename_fails() {
        // A directory at the target path makes the final rename fail.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        std::fs::create_dir(&path).unwrap();

        let result = write_secret(&path, b"new");

        assert!(matches!(result, Err(crate::Error::Io(_))), "{result:?}");
        assert_eq!(names(dir.path()), ["secret"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_reader_of_the_old_file_keeps_its_complete_content() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        std::fs::write(&path, "old").unwrap();
        let mut reader = std::fs::File::open(&path).unwrap();

        write_secret(&path, b"new").unwrap();

        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        assert_eq!(old, "old");
    }
}
