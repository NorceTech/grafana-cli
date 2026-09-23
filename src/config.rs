use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OAuthConfig {
    pub token_url: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Profile {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// Where `config.toml` lives: in `config_dir` (`GCLI_CONFIG_DIR`) when given,
/// else in the platform config directory.
pub fn config_path(config_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = config_dir {
        return Ok(dir.join("config.toml"));
    }
    let dirs =
        directories::ProjectDirs::from("", "", "grafana-cli").ok_or_else(|| Error::Config {
            message: "cannot determine config directory".into(),
        })?;
    Ok(dirs.config_dir().join("config.toml"))
}

impl Config {
    /// Reads `path`; a missing file is an empty config.
    pub fn load_from(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw).map_err(|e| Error::Config {
            message: format!("{}: {e}", path.display()),
        })
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self).map_err(|e| Error::Config {
            message: e.to_string(),
        })?;
        std::fs::write(path, raw)?;
        restrict_permissions(path);
        Ok(())
    }

    /// The profile used when neither `--profile` nor `--all-profiles` is given:
    /// `env_profile` (`GRAFANA_CLI_PROFILE`) if set, else `current_profile`.
    /// Returned as stored; the caller applies overrides.
    pub fn active_profile(&self, env_profile: Option<&str>) -> Result<(String, Profile)> {
        let selected = env_profile.or(self.current_profile.as_deref());
        let found = selected.and_then(|name| self.profiles.get(name).map(|p| (name, p)));
        let (name, profile) = found.ok_or_else(|| {
            let reason = match env_profile {
                Some(name) => format!("profile '{name}' not found"),
                None => "no profile selected; use `gcli profile use` or --profile".into(),
            };
            Error::Config {
                message: format!("{reason}; available: {}", self.available()),
            }
        })?;
        Ok((name.to_string(), profile.clone()))
    }

    pub fn add_profile(&mut self, name: &str, profile: Profile) -> Result<()> {
        validate_auth(&profile)?;
        self.profiles.insert(name.to_string(), profile);
        Ok(())
    }

    pub fn remove_profile(&mut self, name: &str) -> Result<()> {
        self.profiles.remove(name).ok_or_else(|| Error::Config {
            message: format!(
                "profile '{name}' not found; available: {}",
                self.available()
            ),
        })?;
        if self.current_profile.as_deref() == Some(name) {
            self.current_profile = None;
        }
        Ok(())
    }

    pub fn use_profile(&mut self, name: &str) -> Result<()> {
        if !self.profiles.contains_key(name) {
            return Err(Error::Config {
                message: format!(
                    "profile '{name}' not found; available: {}",
                    self.available()
                ),
            });
        }
        self.current_profile = Some(name.to_string());
        Ok(())
    }

    pub fn set_token(&mut self, name: &str, token: Option<&str>) -> Result<()> {
        let available = self.available();
        let profile = self.profiles.get_mut(name).ok_or_else(|| Error::Config {
            message: format!("profile '{name}' not found; available: {available}"),
        })?;
        if profile.username.is_some() {
            return Err(Error::Config {
                message: format!("profile '{name}' uses username/password auth; token not allowed"),
            });
        }
        match token {
            Some(t) => profile.token = Some(t.to_string()),
            None if profile.token_command.is_none() => {
                return Err(Error::Config {
                    message: format!("profile '{name}' has no token_command; token required"),
                });
            }
            None => profile.token = None,
        }
        Ok(())
    }

    /// Profile names for error messages, or `(none)`.
    pub(crate) fn available(&self) -> String {
        if self.profiles.is_empty() {
            "(none)".into()
        } else {
            self.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
        }
    }
}

// Best-effort chmod 0600 on a freshly written secret-bearing file. A chmod
// failure is non-fatal: the file already exists with the default umask mode.
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

fn validate_auth(profile: &Profile) -> Result<()> {
    let token_mode = profile.token.is_some() || profile.token_command.is_some();
    let basic_mode = profile.username.is_some()
        && (profile.password.is_some() || profile.password_command.is_some());
    let oauth_mode = profile.oauth.is_some();
    match (token_mode as u8) + (basic_mode as u8) + (oauth_mode as u8) {
        1 => Ok(()),
        0 => Err(Error::Config {
            message: "profile needs exactly one auth mode: --token/--token-command, \
                      --username with --password/--password-command (username required \
                      for basic auth), or oauth"
                .into(),
        }),
        _ => Err(Error::Config {
            message: "profile has multiple auth modes; use exactly one".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_profile() -> Profile {
        Profile {
            url: "https://g.example.com".into(),
            token: Some("tok".into()),
            ..Default::default()
        }
    }

    fn basic_profile() -> Profile {
        Profile {
            url: "https://g.example.com".into(),
            username: Some("u".into()),
            password: Some("p".into()),
            ..Default::default()
        }
    }

    fn oauth_profile() -> Profile {
        Profile {
            url: "https://g.example.com".into(),
            oauth: Some(OAuthConfig {
                token_url: "https://sso/token".into(),
                client_id: "cid".into(),
                client_secret: Some("sec".into()),
                client_secret_command: None,
                scopes: Some(vec!["scope1".into()]),
                audience: None,
            }),
            ..Default::default()
        }
    }

    fn sample_config() -> Config {
        let mut c = Config::default();
        c.add_profile("a", token_profile()).unwrap();
        c.add_profile("b", basic_profile()).unwrap();
        c.current_profile = Some("a".into());
        c
    }

    #[test]
    fn add_profile_requires_exactly_one_auth_mode() {
        let mut c = Config::default();
        assert!(c.add_profile("t", token_profile()).is_ok());
        assert!(c.add_profile("b", basic_profile()).is_ok());
        assert!(c.add_profile("o", oauth_profile()).is_ok());

        let none = Profile {
            url: "u".into(),
            ..Default::default()
        };
        let err = c.add_profile("n", none).unwrap_err();
        assert!(matches!(err, Error::Config { .. }));

        let both = Profile {
            token: Some("t".into()),
            oauth: oauth_profile().oauth,
            ..token_profile()
        };
        assert!(matches!(
            c.add_profile("x", both),
            Err(Error::Config { .. })
        ));
    }

    #[test]
    fn token_command_counts_as_token_mode() {
        let mut c = Config::default();
        let p = Profile {
            url: "u".into(),
            token_command: Some(vec!["cat".into(), "tokenfile".into()]),
            ..Default::default()
        };
        assert!(c.add_profile("tc", p).is_ok());
    }

    #[test]
    fn password_command_counts_as_basic_mode() {
        let mut c = Config::default();
        let p = Profile {
            url: "u".into(),
            username: Some("u".into()),
            password_command: Some(vec!["op".into(), "read".into()]),
            ..Default::default()
        };
        assert!(c.add_profile("pc", p).is_ok());
    }

    #[test]
    fn basic_auth_requires_username() {
        let mut c = Config::default();
        // password_command without username is no longer a valid basic mode:
        // auth::from_profile needs the username to build Basic, so such a
        // profile would save then fail at query time. Reject it up front.
        let pw_cmd_only = Profile {
            url: "u".into(),
            password_command: Some(vec!["op".into(), "read".into()]),
            ..Default::default()
        };
        let err = c.add_profile("x", pw_cmd_only).unwrap_err();
        assert!(matches!(err, Error::Config { .. }));

        let pw_only = Profile {
            url: "u".into(),
            password: Some("p".into()),
            ..Default::default()
        };
        assert!(matches!(
            c.add_profile("y", pw_only),
            Err(Error::Config { .. })
        ));

        let user_only = Profile {
            url: "u".into(),
            username: Some("u".into()),
            ..Default::default()
        };
        assert!(matches!(
            c.add_profile("z", user_only),
            Err(Error::Config { .. })
        ));

        assert!(c.add_profile("ok_pw", basic_profile()).is_ok());
        let ok_cmd = Profile {
            url: "u".into(),
            username: Some("u".into()),
            password_command: Some(vec!["op".into(), "read".into()]),
            ..Default::default()
        };
        assert!(c.add_profile("ok_cmd", ok_cmd).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        sample_config().save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "config.toml must be chmod 0600");
    }

    #[test]
    fn remove_use_set_token_ops() {
        let mut c = sample_config();
        assert!(c.remove_profile("nope").is_err());
        c.remove_profile("b").unwrap();
        assert!(!c.profiles.contains_key("b"));

        assert!(c.use_profile("nope").is_err());
        c.use_profile("a").unwrap();
        assert_eq!(c.current_profile.as_deref(), Some("a"));

        c.set_token("a", Some("newtok")).unwrap();
        assert_eq!(c.profiles["a"].token.as_deref(), Some("newtok"));
        assert!(matches!(
            c.set_token("nope", Some("t")),
            Err(Error::Config { .. })
        ));
    }

    #[test]
    fn set_token_rejects_username_profiles() {
        let mut c = sample_config();
        assert!(matches!(
            c.set_token("b", Some("t")),
            Err(Error::Config { .. })
        ));
    }

    #[test]
    fn set_token_none_requires_token_command() {
        let mut c = sample_config();
        assert!(c.set_token("a", None).is_err());
        c.profiles.get_mut("a").unwrap().token_command = Some(vec!["cat".into(), "f".into()]);
        c.set_token("a", None).unwrap();
        assert_eq!(c.profiles["a"].token, None);
    }

    #[test]
    fn roundtrip_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let c = sample_config();
        c.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), c);
    }

    #[test]
    fn load_missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Config::load_from(&dir.path().join("config.toml")).unwrap();
        assert_eq!(loaded, Config::default());
    }

    #[test]
    fn load_invalid_toml_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not [valid toml").unwrap();
        assert!(matches!(
            Config::load_from(&path),
            Err(Error::Config { .. })
        ));
    }

    #[test]
    fn active_profile_prefers_env_profile_and_returns_it_as_stored() {
        let (name, profile) = sample_config().active_profile(Some("b")).unwrap();
        assert_eq!(name, "b");
        assert_eq!(profile, basic_profile());
    }

    #[test]
    fn active_profile_falls_back_to_current_profile() {
        let (name, profile) = sample_config().active_profile(None).unwrap();
        assert_eq!(name, "a");
        assert_eq!(profile, token_profile());
    }

    #[test]
    fn active_profile_unknown_env_profile_lists_available() {
        let err = sample_config().active_profile(Some("zzz")).unwrap_err();
        assert!(
            matches!(&err, Error::Config { message }
                if message == "profile 'zzz' not found; available: a, b"),
            "{err:?}"
        );
    }

    #[test]
    fn active_profile_without_any_profile_is_config_error() {
        let err = Config::default().active_profile(None).unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
    }

    #[test]
    fn missing_current_profile_errors_with_available_list() {
        let mut c = sample_config();
        c.current_profile = Some("gone".into());
        let err = c.active_profile(None).unwrap_err();
        match err {
            Error::Config { message } => {
                assert!(message.contains("a") && message.contains("b"), "{message}");
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }
}
