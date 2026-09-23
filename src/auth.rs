use crate::token_cache::{self, CacheEntry};
use crate::{Error, OAuthConfig, Profile, Result};
use std::path::{Path, PathBuf};

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
    pub cache_path: PathBuf,
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
            None => exec_command(
                profile.token_command.as_deref().unwrap_or_default(),
                "token_command",
            )?,
        };
        return Ok(AuthProvider::Token(token));
    }
    if let Some(user) = &profile.username {
        let pass = match &profile.password {
            Some(p) => p.clone(),
            None => exec_command(
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

pub async fn get_oauth_token(source: &OAuthSource, http: &reqwest::Client) -> Result<String> {
    let config = &source.config;
    let key = format!("{}:{}", source.profile_name, config.client_id);
    let path = &source.cache_path;
    let now = chrono::Utc::now().timestamp();
    if let Some(token) = token_cache::fresh_token(path, &key, now) {
        return Ok(token);
    }

    let auth_error = |message: String| Error::Auth {
        kind: "oauth".into(),
        message,
    };
    let secret = match (&config.client_secret, &config.client_secret_command) {
        (Some(s), _) => Some(s.clone()),
        (None, Some(cmd)) => Some(exec_command(cmd, "oauth")?),
        _ => None,
    };
    let mut form: Vec<(String, String)> = vec![
        ("grant_type".into(), "client_credentials".into()),
        ("client_id".into(), config.client_id.clone()),
    ];
    if let Some(secret) = secret {
        form.push(("client_secret".into(), secret));
    }
    if let Some(scopes) = &config.scopes {
        form.push(("scope".into(), scopes.join(" ")));
    }
    if let Some(audience) = &config.audience {
        form.push(("audience".into(), audience.clone()));
    }

    let response = http
        .post(&config.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| auth_error(format!("token request failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| auth_error(format!("token response read failed: {e}")))?;
    if !status.is_success() {
        return Err(auth_error(format!(
            "token endpoint returned {}: {body}",
            status.as_u16()
        )));
    }
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| auth_error(format!("token response not JSON: {e}")))?;
    let token = value
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| auth_error("no access_token in token response".into()))?
        .to_string();
    let expires_in = value
        .get("expires_in")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(3600);

    let entry = CacheEntry {
        expires_at_unix: now + expires_in - 60,
        token: token.clone(),
    };
    token_cache::store(path, &key, entry)?;
    Ok(token)
}

pub fn invalidate(source: &OAuthSource) -> Result<()> {
    let key = format!("{}:{}", source.profile_name, source.config.client_id);
    token_cache::remove(&source.cache_path, &key)
}

pub(crate) fn exec_command(cmd: &[String], kind: &str) -> Result<String> {
    let joined = cmd.join(" ");
    let auth_error = |message: String| Error::Auth {
        kind: kind.into(),
        message,
    };
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&joined)
        .output()
        .map_err(|e| auth_error(format!("spawn '{joined}': {e}")))?;
    if !output.status.success() {
        return Err(auth_error(format!(
            "'{joined}' exited with {}",
            output.status.code().unwrap_or(-1)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(auth_error(format!("'{joined}' produced empty output")));
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn oauth_config(token_url: &str) -> OAuthConfig {
        OAuthConfig {
            token_url: token_url.into(),
            client_id: "cid".into(),
            client_secret: Some("sec".into()),
            client_secret_command: None,
            scopes: Some(vec!["a".into(), "b".into()]),
            audience: Some("aud".into()),
        }
    }

    fn cache_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // Profile "p" with `config`, caching its tokens in `dir`/tokens.json.
    fn source(dir: &tempfile::TempDir, config: OAuthConfig) -> OAuthSource {
        OAuthSource {
            profile_name: "p".into(),
            config,
            cache_path: dir.path().join("tokens.json"),
        }
    }

    fn write_cache_file(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("tokens.json"), body).unwrap();
    }

    fn read_cache_file(dir: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("tokens.json")).unwrap()).unwrap()
    }

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
            oauth: Some(oauth_config("https://sso/token")),
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

    #[tokio::test]
    async fn oauth_fresh_cache_skips_network() {
        let dir = cache_dir();
        let expires = chrono::Utc::now().timestamp() + 3000;
        write_cache_file(
            dir.path(),
            &json!({"p:cid": {"token": "cached", "expires_at_unix": expires}}).to_string(),
        );
        // token_url unroutable: a network fetch would fail the test
        let cfg = oauth_config("http://127.0.0.1:1/token");
        let tok = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(tok, "cached");
    }

    #[tokio::test]
    async fn oauth_fetch_caches_with_expiry_margin() {
        let dir = cache_dir();

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fresh", "expires_in": 3600, "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = oauth_config(&format!("{}/token", server.uri()));
        let tok = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(tok, "fresh");

        let cache = read_cache_file(dir.path());
        let entry = &cache["p:cid"];
        assert_eq!(entry["token"], "fresh");
        let now = chrono::Utc::now().timestamp();
        let expires = entry["expires_at_unix"].as_i64().unwrap();
        assert!((expires - (now + 3600 - 60)).abs() <= 5, "{expires}");
        server.verify().await;
    }

    #[tokio::test]
    async fn oauth_corrupt_cache_is_cold() {
        let dir = cache_dir();
        write_cache_file(dir.path(), "not json at all");

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "again", "expires_in": 60
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = oauth_config(&format!("{}/token", server.uri()));
        let tok = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(tok, "again");
        assert_eq!(read_cache_file(dir.path())["p:cid"]["token"], "again");
    }

    #[tokio::test]
    async fn oauth_expired_cache_refetches() {
        let dir = cache_dir();
        let past = chrono::Utc::now().timestamp() - 1000;
        write_cache_file(
            dir.path(),
            &json!({"p:cid": {"token": "stale", "expires_at_unix": past}}).to_string(),
        );

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "refreshed", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = oauth_config(&format!("{}/token", server.uri()));
        let tok = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(tok, "refreshed");
    }

    #[tokio::test]
    async fn oauth_public_client_omits_secret() {
        let dir = cache_dir();

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .and(wiremock::matchers::body_string_contains("client_secret"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "pubtok", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cfg = oauth_config(&format!("{}/token", server.uri()));
        cfg.client_secret = None;
        let tok = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(tok, "pubtok");
        server.verify().await;
    }

    #[tokio::test]
    async fn oauth_fetch_error_is_auth_kind() {
        let dir = cache_dir();

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let cfg = oauth_config(&format!("{}/token", server.uri()));
        let e = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap_err();
        match e {
            Error::Auth { kind, .. } => assert_eq!(kind, "oauth"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn invalidate_removes_only_target_key() {
        let dir = cache_dir();
        write_cache_file(
            dir.path(),
            &json!({
                "p:cid": {"token": "t", "expires_at_unix": 9},
                "other:cid": {"token": "t2", "expires_at_unix": 9}
            })
            .to_string(),
        );
        invalidate(&source(&dir, oauth_config("https://sso/token"))).unwrap();
        let cache = read_cache_file(dir.path());
        assert!(cache.get("p:cid").is_none());
        assert!(cache.get("other:cid").is_some());
    }

    #[test]
    fn invalidate_missing_file_is_noop() {
        let dir = cache_dir();
        invalidate(&source(&dir, oauth_config("https://sso/token"))).unwrap();
        assert!(!dir.path().join("tokens.json").exists());
    }

    #[test]
    fn reqwest_form_encoding_is_rfc_form_urlencoded() {
        let req = reqwest::Client::new()
            .post("http://x/")
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", "a b&c=d"),
                ("scope", "a b"),
            ])
            .build()
            .unwrap();
        let body = req.body().unwrap().as_bytes().unwrap();
        assert_eq!(
            std::str::from_utf8(body).unwrap(),
            "grant_type=client_credentials&client_id=a+b%26c%3Dd&scope=a+b"
        );
    }
}
