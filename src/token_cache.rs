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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string(cache).map_err(|e| Error::Auth {
        kind: "oauth".into(),
        message: format!("cache serialize: {e}"),
    })?;
    std::fs::write(path, raw)?;
    restrict_permissions(path);
    Ok(())
}

// Best-effort chmod 0600 on the token cache; a chmod failure is non-fatal.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn cache_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        store(
            &path,
            "p:cid",
            CacheEntry {
                token: "t".into(),
                expires_at_unix: 9,
            },
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "tokens.json must be chmod 0600");
    }
}
