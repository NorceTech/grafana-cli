use crate::secret_file::{create_private_dir, open_private, write_secret};
use crate::{Error, OAuthConfig, Result};
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

/// Everything that identifies an OAuth access token. A cached token is only
/// reused for an equal key, so a profile re-added or edited with other OAuth
/// settings never picks up a token minted for the old ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TokenKey {
    profile: String,
    client_id: String,
    token_url: String,
    audience: Option<String>,
    scopes: Option<Vec<String>>,
}

impl TokenKey {
    pub(crate) fn new(profile: &str, config: &OAuthConfig) -> Self {
        Self {
            profile: profile.to_string(),
            client_id: config.client_id.clone(),
            token_url: config.token_url.clone(),
            audience: config.audience.clone(),
            scopes: config.scopes.clone(),
        }
    }

    /// Where the entry sits in tokens.json: `<profile>:<client_id>`, as it
    /// always has; the entry itself carries the full key.
    fn index(&self) -> String {
        format!("{}:{}", self.profile, self.client_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CacheEntry {
    pub(crate) token: String,
    pub(crate) expires_at_unix: i64,
    pub(crate) key: TokenKey,
}

/// The token cached for exactly `key` if it is still valid at `now`.
pub(crate) fn fresh_token(path: &Path, key: &TokenKey, now: i64) -> Option<String> {
    read(path)
        .remove(&key.index())
        .filter(|entry| entry.key == *key && entry.expires_at_unix > now)
        .map(|entry| entry.token)
}

pub(crate) fn store(path: &Path, entry: CacheEntry) -> Result<()> {
    update(path, |cache| {
        cache.insert(entry.key.index(), entry);
        true
    })
}

pub(crate) fn remove(path: &Path, key: &TokenKey) -> Result<()> {
    update(path, |cache| cache.remove(&key.index()).is_some())
}

/// Drops every token cached for `profile`. Without a cache file there is
/// nothing to forget, and nothing is created.
pub fn forget_profile(path: &Path, profile: &str) -> Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    update(path, |cache| {
        let before = cache.len();
        cache.retain(|_, entry| entry.key.profile != profile);
        cache.len() != before
    })
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

// A missing or unreadable cache, including one written before entries
// carried their key, is empty: every token is fetched afresh.
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

    fn key(profile: &str) -> TokenKey {
        TokenKey {
            profile: profile.into(),
            client_id: "cid".into(),
            token_url: "https://sso/token".into(),
            audience: None,
            scopes: None,
        }
    }

    fn entry(profile: &str) -> CacheEntry {
        CacheEntry {
            token: "t".into(),
            expires_at_unix: 9,
            key: key(profile),
        }
    }

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

        store(&path, entry("p")).unwrap();

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

        store(&cache_dir.join("tokens.json"), entry("p")).unwrap();

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
                        store(path, entry(&format!("p{thread}-{i}"))).unwrap();
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

    #[test]
    fn forget_profile_drops_only_that_profiles_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        store(&path, entry("p")).unwrap();
        let second_client = TokenKey {
            client_id: "cid2".into(),
            ..key("p")
        };
        store(
            &path,
            CacheEntry {
                key: second_client,
                ..entry("p")
            },
        )
        .unwrap();
        store(&path, entry("q")).unwrap();

        forget_profile(&path, "p").unwrap();

        assert_eq!(read(&path).into_keys().collect::<Vec<_>>(), ["q:cid"]);
    }

    #[test]
    fn forget_profile_without_a_cache_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("cache");

        forget_profile(&cache_dir.join("tokens.json"), "p").unwrap();

        assert!(!cache_dir.exists());
    }
}
