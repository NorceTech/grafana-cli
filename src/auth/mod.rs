//! Profile authentication: which provider a profile resolves to, the OAuth
//! token machinery (`oauth`, with the `tokens.json` cache in `token_cache`)
//! and the `sh -c` secret commands (`secret`).

pub mod oauth;
mod secret;
pub mod token_cache;

pub use oauth::{get_oauth_token, invalidate};

use crate::config::{OAuthConfig, Profile};
use crate::{Result, error::Error};
use std::path::Path;

#[derive(Debug, Clone)]
pub enum AuthProvider {
    Token(String),
    Basic { user: String, pass: String },
    OAuth(OAuthSource),
}

/// An OAuth client-credentials profile and the token cache file it uses.
#[derive(Debug, Clone)]
pub struct OAuthSource {
    pub profile_name: String,
    pub config: OAuthConfig,
    pub cache_path: std::path::PathBuf,
}

/// `cache_dir` (`GCLI_CACHE_DIR`) locates the OAuth token cache; it is only
/// resolved for OAuth profiles.
pub fn from_profile(
    profile_name: &str,
    profile: &Profile,
    cache_dir: Option<&Path>,
) -> Result<AuthProvider> {
    if let Some(config) = &profile.oauth {
        return Ok(AuthProvider::OAuth(OAuthSource {
            profile_name: profile_name.to_string(),
            config: config.clone(),
            cache_path: token_cache::path(cache_dir)?,
        }));
    }
    if profile.token.is_some() || profile.token_command.is_some() {
        let token = match &profile.token {
            Some(t) => t.clone(),
            None => secret::exec_command(
                profile.token_command.as_deref().unwrap_or_default(),
                "token_command",
            )?,
        };
        return Ok(AuthProvider::Token(token));
    }
    if let Some(user) = &profile.username {
        let pass = match &profile.password {
            Some(p) => p.clone(),
            None => secret::exec_command(
                profile.password_command.as_deref().unwrap_or_default(),
                "password_command",
            )?,
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

    #[test]
    fn from_profile_token_literal() {
        let p = Profile {
            url: "u".into(),
            token: Some("tok".into()),
            ..Default::default()
        };
        assert!(matches!(from_profile("p", &p, None), Ok(AuthProvider::Token(t)) if t == "tok"));
    }

    #[test]
    fn from_profile_token_command() {
        let p = Profile {
            url: "u".into(),
            token_command: Some(vec!["echo tok".into()]),
            ..Default::default()
        };
        assert!(matches!(from_profile("p", &p, None), Ok(AuthProvider::Token(t)) if t == "tok"));
    }

    #[test]
    fn from_profile_token_command_failures() {
        let mk = |cmd: &[&str]| Profile {
            url: "u".into(),
            token_command: Some(cmd.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        };
        let e = from_profile("p", &mk(&["exit 3"]), None).unwrap_err();
        assert!(matches!(e, Error::Auth { kind, .. } if kind == "token_command"));
        let e = from_profile("p", &mk(&["printf ''"]), None).unwrap_err();
        assert!(matches!(e, Error::Auth { kind, .. } if kind == "token_command"));
    }

    #[test]
    fn from_profile_basic() {
        let p = Profile {
            url: "u".into(),
            username: Some("usr".into()),
            password: Some("pw".into()),
            ..Default::default()
        };
        assert!(
            matches!(from_profile("p", &p, None), Ok(AuthProvider::Basic { user, pass }) if user == "usr" && pass == "pw")
        );
    }

    #[test]
    fn from_profile_basic_password_command() {
        let p = Profile {
            url: "u".into(),
            username: Some("usr".into()),
            password_command: Some(vec!["echo pw".into()]),
            ..Default::default()
        };
        assert!(
            matches!(from_profile("p", &p, None), Ok(AuthProvider::Basic { user, pass }) if user == "usr" && pass == "pw")
        );
    }

    #[test]
    fn from_profile_password_command_failure_kind() {
        let p = Profile {
            url: "u".into(),
            username: Some("usr".into()),
            password_command: Some(vec!["exit 1".into()]),
            ..Default::default()
        };
        let e = from_profile("p", &p, None).unwrap_err();
        assert!(matches!(e, Error::Auth { kind, .. } if kind == "password_command"));
    }

    #[test]
    fn from_profile_oauth_caches_tokens_in_cache_dir() {
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
            from_profile("prod", &p, Some(dir)),
            Ok(AuthProvider::OAuth(s))
                if s.profile_name == "prod" && s.cache_path == dir.join("tokens.json")
        ));
    }

    #[test]
    fn from_profile_no_auth_is_error() {
        let p = Profile {
            url: "u".into(),
            ..Default::default()
        };
        assert!(matches!(
            from_profile("p", &p, None),
            Err(Error::Config { .. })
        ));
    }
}
