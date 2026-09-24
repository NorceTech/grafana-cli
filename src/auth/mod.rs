//! Profile authentication: which provider a profile resolves to, the OAuth
//! token machinery (`oauth`, with the `tokens.json` cache in `token_cache`)
//! and the secret helper commands (`secret`: a one-element `sh -c` line, a
//! multi-element argv).

pub mod oauth;
mod secret;
pub mod token_cache;

pub use oauth::{get_oauth_token, invalidate};

use crate::config::{OAuthConfig, Profile};
use crate::{Result, error::Error};
use std::path::Path;
use std::time::Duration;

/// Reads a secret given as `-` on a secret flag from `input`: everything up
/// to EOF, trimmed, and it must be non-empty after trimming. `flag` names the
/// flag in errors. The caller owns the reader, so tests feed a `Cursor` and
/// the binary feeds stdin.
pub fn secret_from_stdin(input: &mut impl std::io::Read, flag: &str) -> Result<String> {
    let mut raw = String::new();
    input.read_to_string(&mut raw)?;
    let secret = raw.trim();
    if secret.is_empty() {
        return Err(Error::Config {
            message: format!("{flag} was given '-' but stdin is empty"),
        });
    }
    Ok(secret.to_string())
}

#[derive(Debug, Clone)]
pub enum AuthProvider {
    Token(String),
    Basic { user: String, pass: String },
    OAuth(OAuthSource),
}

/// An OAuth client-credentials profile, the token cache file it uses and
/// the `--timeout` budget its `client_secret_command` runs with (the fetch
/// is deferred to request time, so the budget travels with the source).
#[derive(Debug, Clone)]
pub struct OAuthSource {
    pub profile_name: String,
    pub config: OAuthConfig,
    pub cache_path: std::path::PathBuf,
    pub secret_timeout: Duration,
}

/// `cache_dir` (`GCLI_CACHE_DIR`) locates the OAuth token cache; it is only
/// resolved for OAuth profiles. `timeout` bounds every `*_command` run.
pub async fn from_profile(
    profile_name: &str,
    profile: &Profile,
    cache_dir: Option<&Path>,
    timeout: Duration,
) -> Result<AuthProvider> {
    if let Some(config) = &profile.oauth {
        return Ok(AuthProvider::OAuth(OAuthSource {
            profile_name: profile_name.to_string(),
            config: config.clone(),
            cache_path: token_cache::path(cache_dir)?,
            secret_timeout: timeout,
        }));
    }
    if profile.token.is_some() || profile.token_command.is_some() {
        let token = match &profile.token {
            Some(t) => t.clone(),
            None => {
                secret::exec_command(
                    profile.token_command.as_deref().unwrap_or_default(),
                    "token_command",
                    timeout,
                )
                .await?
            }
        };
        return Ok(AuthProvider::Token(token));
    }
    if let Some(user) = &profile.username {
        let pass = match &profile.password {
            Some(p) => p.clone(),
            None => {
                secret::exec_command(
                    profile.password_command.as_deref().unwrap_or_default(),
                    "password_command",
                    timeout,
                )
                .await?
            }
        };
        return Ok(AuthProvider::Basic {
            user: user.clone(),
            pass,
        });
    }
    Err(Error::Config {
        message: format!("profile '{profile_name}' has no usable auth mode"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Generous for fast commands; the secret-command timeout has its own
    // tests in `secret`.
    const TIMEOUT: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn from_profile_token_literal() {
        let p = Profile {
            url: "u".into(),
            token: Some("tok".into()),
            ..Default::default()
        };
        assert!(matches!(
            from_profile("p", &p, None, TIMEOUT).await,
            Ok(AuthProvider::Token(t)) if t == "tok"
        ));
    }

    #[tokio::test]
    async fn from_profile_token_command() {
        let p = Profile {
            url: "u".into(),
            token_command: Some(vec!["echo tok".into()]),
            ..Default::default()
        };
        assert!(matches!(
            from_profile("p", &p, None, TIMEOUT).await,
            Ok(AuthProvider::Token(t)) if t == "tok"
        ));
    }

    #[tokio::test]
    async fn from_profile_token_command_failures() {
        let mk = |cmd: &[&str]| Profile {
            url: "u".into(),
            token_command: Some(cmd.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        };
        let e = from_profile("p", &mk(&["exit 3"]), None, TIMEOUT)
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Auth { kind, .. } if kind == "token_command"));
        let e = from_profile("p", &mk(&["printf ''"]), None, TIMEOUT)
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Auth { kind, .. } if kind == "token_command"));
    }

    #[tokio::test]
    async fn from_profile_basic() {
        let p = Profile {
            url: "u".into(),
            username: Some("usr".into()),
            password: Some("pw".into()),
            ..Default::default()
        };
        assert!(matches!(
            from_profile("p", &p, None, TIMEOUT).await,
            Ok(AuthProvider::Basic { user, pass }) if user == "usr" && pass == "pw"
        ));
    }

    #[tokio::test]
    async fn from_profile_basic_password_command() {
        let p = Profile {
            url: "u".into(),
            username: Some("usr".into()),
            password_command: Some(vec!["echo pw".into()]),
            ..Default::default()
        };
        assert!(matches!(
            from_profile("p", &p, None, TIMEOUT).await,
            Ok(AuthProvider::Basic { user, pass }) if user == "usr" && pass == "pw"
        ));
    }

    #[tokio::test]
    async fn from_profile_password_command_failure_kind() {
        let p = Profile {
            url: "u".into(),
            username: Some("usr".into()),
            password_command: Some(vec!["exit 1".into()]),
            ..Default::default()
        };
        let e = from_profile("p", &p, None, TIMEOUT).await.unwrap_err();
        assert!(matches!(e, Error::Auth { kind, .. } if kind == "password_command"));
    }

    #[tokio::test]
    async fn from_profile_oauth_caches_tokens_in_cache_dir() {
        let p = Profile {
            url: "u".into(),
            oauth: Some(OAuthConfig {
                token_url: "https://sso/token".into(),
                client_id: "cid".into(),
                client_secret: Some("sec".into()),
                client_secret_command: None,
                scopes: None,
                audience: None,
            }),
            ..Default::default()
        };
        let dir = Path::new("/cache");
        assert!(matches!(
            from_profile("prod", &p, Some(dir), TIMEOUT).await,
            Ok(AuthProvider::OAuth(s))
                if s.profile_name == "prod" && s.cache_path == dir.join("tokens.json")
        ));
    }

    #[tokio::test]
    async fn from_profile_no_auth_is_error() {
        let p = Profile {
            url: "u".into(),
            ..Default::default()
        };
        assert!(matches!(
            from_profile("p", &p, None, TIMEOUT).await,
            Err(Error::Config { .. })
        ));
    }

    fn stdin_secret(input: &str) -> Result<String> {
        secret_from_stdin(&mut std::io::Cursor::new(input), "--token")
    }

    #[test]
    fn stdin_secret_is_trimmed() {
        assert_eq!(stdin_secret("  tok \n").unwrap(), "tok");
    }

    #[test]
    fn stdin_secret_must_be_non_empty() {
        for input in ["", "\n", "  \t "] {
            let e = stdin_secret(input).unwrap_err();
            assert!(matches!(e, Error::Config { .. }), "{input:?}: {e:?}");
        }
    }
}
