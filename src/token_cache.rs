use crate::secret_file::{create_private_dir, open_private, write_secret};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Where the OAuth token cache lives: `tokens.json` in `cache_dir`
/// (`GCLI_CACHE_DIR`) when given, else in the platform cache directory.
pub fn path(cache_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = cache_dir {
        return Ok(dir.join("tokens.json"));
    }
    let dirs =
        directories::ProjectDirs::from("", "", "grafana-cli").ok_or_else(|| Error::Config {
            message: "cannot determine cache directory".into(),
        })?;
    Ok(dirs.cache_dir().join("tokens.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CacheEntry {
    pub(crate) token: String,
    pub(crate) expires_at_unix: i64,
}

/// The token cached under `key` if it is still valid at `now`.
pub(crate) fn fresh_token(path: &Path, key: &str, now: i64) -> Option<String> {
    read(path)
        .remove(key)
        .filter(|entry| entry.expires_at_unix > now)
        .map(|entry| entry.token)
}

pub(crate) fn store(path: &Path, key: &str, entry: CacheEntry) -> Result<()> {
    update(path, |cache| {
        cache.insert(key.to_string(), entry);
        true
    })
}

pub(crate) fn remove(path: &Path, key: &str) -> Result<()> {
    update(path, |cache| cache.remove(key).is_some())
}

/// Read-modify-write of the cache under an exclusive lock, so concurrent
/// writers never drop each other's entries: an in-process mutex serializes
/// OAuth profiles fetching in parallel, and a file lock on the
/// `tokens.json.lock` sidecar serializes gcli processes. `modify` edits the
/// entries and returns whether anything changed; only then is the file
/// rewritten. Plain reads need no lock: the file is replaced atomically.
fn update(
    path: &Path,
    modify: impl FnOnce(&mut BTreeMap<String, CacheEntry>) -> bool,
) -> Result<()> {
    static IN_PROCESS: Mutex<()> = Mutex::new(());
    // The mutex guards no data, so a panicked holder leaves nothing broken.
    let _in_process = IN_PROCESS.lock().unwrap_or_else(PoisonError::into_inner);
    let _across_processes = lock_sidecar(path)?;
    let mut cache = read(path);
    if modify(&mut cache) {
        write(path, &cache)?;
    }
    Ok(())
}

/// Locks `<path>.lock` exclusively, creating it if needed; dropping the
/// returned file releases the lock.
fn lock_sidecar(path: &Path) -> Result<File> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    if let Some(dir) = lock_path.parent() {
        create_private_dir(dir)?;
    }
    let file = open_private(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false),
        &lock_path,
    )?;
    file.lock()?;
    Ok(file)
}

fn read(path: &Path) -> BTreeMap<String, CacheEntry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write(path: &Path, cache: &BTreeMap<String, CacheEntry>) -> Result<()> {
    let raw = serde_json::to_vec(cache).map_err(|e| Error::Auth {
        kind: "oauth".into(),
        message: format!("cache serialize: {e}"),
    })?;
    write_secret(path, &raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Storing replaces tokens.json instead of rewriting it in place: a reader
    // that opened the old file keeps its complete bytes, the new file is
    // owner-only from the start, and no temp file is left behind.
    #[cfg(unix)]
    #[test]
    fn store_replaces_a_0644_cache_atomically_with_an_owner_only_file() {
        use std::io::Read;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut reader = std::fs::File::open(&path).unwrap();

        store(
            &path,
            "p:cid",
            CacheEntry {
                token: "t".into(),
                expires_at_unix: 9,
            },
        )
        .unwrap();

        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        assert_eq!(old, "{}", "rewritten in place");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "tokens.json must be 0600");
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["tokens.json", "tokens.json.lock"],
            "temp file left behind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn store_creates_the_cache_directory_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("cache");

        store(
            &cache_dir.join("tokens.json"),
            "p:cid",
            CacheEntry {
                token: "t".into(),
                expires_at_unix: 9,
            },
        )
        .unwrap();

        let mode = std::fs::metadata(&cache_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "cache dir is {mode:o}");
    }

    // Concurrent OAuth profiles store their tokens at the same time; each
    // read-modify-write must see the others' entries.
    #[test]
    fn concurrent_stores_keep_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        std::thread::scope(|s| {
            for thread in 0..4 {
                let path = &path;
                s.spawn(move || {
                    for i in 0..10 {
                        let entry = CacheEntry {
                            token: "t".into(),
                            expires_at_unix: 9,
                        };
                        store(path, &format!("p{thread}-{i}:cid"), entry).unwrap();
                    }
                });
            }
        });

        assert_eq!(read(&path).len(), 40);
    }

    // Another gcli process takes the same exclusive lock on tokens.json.lock
    // before its own read-modify-write, so an update must hold it throughout.
    #[test]
    fn update_holds_the_sidecar_lock_while_modifying() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let mut other_process = None;

        update(&path, |_| {
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.path().join("tokens.json.lock"))
                .unwrap();
            other_process = Some(lock.try_lock());
            false
        })
        .unwrap();

        assert!(
            matches!(other_process, Some(Err(std::fs::TryLockError::WouldBlock))),
            "{other_process:?}"
        );
    }
}
