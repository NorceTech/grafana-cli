use crate::secret_file::write_secret;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    let mut cache = read(path);
    cache.insert(key.to_string(), entry);
    write(path, &cache)
}

pub(crate) fn remove(path: &Path, key: &str) {
    let mut cache = read(path);
    if cache.remove(key).is_some() {
        let _ = write(path, &cache);
    }
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
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["tokens.json"], "temp file left behind");
    }
}
