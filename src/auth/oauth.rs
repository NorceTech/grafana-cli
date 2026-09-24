//! OAuth client-credentials tokens: fetch from the token endpoint, cache in
//! `tokens.json` under the profile's key, invalidate after a 401.

use super::OAuthSource;
use super::secret::exec_command;
use super::token_cache::{self, CacheEntry, TokenKey};
use crate::{Result, error::Error};

pub async fn get_oauth_token(source: &OAuthSource, http: &reqwest::Client) -> Result<String> {
    let config = &source.config;
    let key = TokenKey::new(&source.profile_name, config);
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
        // Saturating: expires_in comes from the endpoint and can be anything.
        expires_at_unix: now.saturating_add(expires_in).saturating_sub(60),
        token: token.clone(),
        key,
    };
    token_cache::store(path, entry)?;
    Ok(token)
}

pub fn invalidate(source: &OAuthSource) -> Result<()> {
    let key = TokenKey::new(&source.profile_name, &source.config);
    token_cache::remove(&source.cache_path, &key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OAuthConfig;
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

    #[tokio::test]
    async fn oauth_fresh_cache_skips_network() {
        let dir = cache_dir();
        // token_url unroutable: a network fetch would fail the test
        let cfg = oauth_config("http://127.0.0.1:1/token");
        let cached = CacheEntry {
            token: "cached".into(),
            expires_at_unix: chrono::Utc::now().timestamp() + 3000,
            key: TokenKey::new("p", &cfg),
        };
        token_cache::store(&dir.path().join("tokens.json"), cached).unwrap();
        let tok = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(tok, "cached");
    }

    // A token endpoint can name any expires_in; the expiry arithmetic must
    // saturate to the i64 second range instead of overflowing.
    #[tokio::test]
    async fn oauth_expires_in_overflow_saturates_instead_of_panicking() {
        let dir = cache_dir();

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "tok", "expires_in": i64::MAX
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = oauth_config(&format!("{}/token", server.uri()));
        let tok = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap();

        assert_eq!(tok, "tok");
        assert_eq!(
            read_cache_file(dir.path())["p:cid"]["expires_at_unix"],
            json!(i64::MAX - 60)
        );
        server.verify().await;
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
        let stale = CacheEntry {
            token: "stale".into(),
            expires_at_unix: chrono::Utc::now().timestamp() - 1000,
            key: TokenKey::new("p", &cfg),
        };
        token_cache::store(&dir.path().join("tokens.json"), stale).unwrap();

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

    // A client_secret_command's output is what reaches the token POST body,
    // exactly like a configured client_secret would.
    #[tokio::test]
    async fn oauth_client_secret_command_output_reaches_the_token_post_body() {
        let dir = cache_dir();

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/token"))
            .and(wiremock::matchers::body_string_contains(
                "client_secret=from-command",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "tok", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cfg = oauth_config(&format!("{}/token", server.uri()));
        cfg.client_secret = None;
        cfg.client_secret_command = Some(vec!["echo from-command".into()]);

        let tok = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap();

        assert_eq!(tok, "tok");
        server.verify().await;
    }

    // A failing client_secret_command is an oauth auth error, raised before
    // any token request is sent (token_url unroutable on purpose).
    #[tokio::test]
    async fn oauth_client_secret_command_failure_is_auth_kind_oauth() {
        let dir = cache_dir();
        let mut cfg = oauth_config("http://127.0.0.1:1/token");
        cfg.client_secret = None;
        cfg.client_secret_command = Some(vec!["exit 3".into()]);

        let e = get_oauth_token(&source(&dir, cfg), &reqwest::Client::new())
            .await
            .unwrap_err();

        assert!(
            matches!(&e, Error::Auth { kind, .. } if kind == "oauth"),
            "{e:?}"
        );
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

    // Profile "p" caches a token for one set of OAuth settings, then asks with
    // `change` applied: the token minted for the old settings must not be used.
    async fn assert_refetched_after(change: impl FnOnce(&mut OAuthConfig)) {
        let dir = cache_dir();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "tok", "expires_in": 3600
            })))
            .expect(2)
            .mount(&server)
            .await;
        let original = oauth_config(&format!("{}/token", server.uri()));
        let mut changed = original.clone();
        change(&mut changed);
        let http = reqwest::Client::new();
        get_oauth_token(&source(&dir, original), &http)
            .await
            .unwrap();

        get_oauth_token(&source(&dir, changed), &http)
            .await
            .unwrap();

        server.verify().await;
    }

    #[tokio::test]
    async fn cached_token_is_not_reused_for_another_token_url() {
        assert_refetched_after(|c| c.token_url = c.token_url.replace("/token", "/other")).await;
    }

    #[tokio::test]
    async fn cached_token_is_not_reused_for_another_audience() {
        assert_refetched_after(|c| c.audience = Some("other".into())).await;
    }

    #[tokio::test]
    async fn cached_token_is_not_reused_for_other_scopes() {
        assert_refetched_after(|c| c.scopes = Some(vec!["a".into()])).await;
    }

    #[tokio::test]
    async fn cached_token_is_not_reused_for_another_client_id() {
        assert_refetched_after(|c| c.client_id = "other".into()).await;
    }

    #[test]
    fn invalidate_removes_only_target_key() {
        let dir = cache_dir();
        let cfg = oauth_config("https://sso/token");
        for profile in ["p", "other"] {
            let entry = CacheEntry {
                token: "t".into(),
                expires_at_unix: 9,
                key: TokenKey::new(profile, &cfg),
            };
            token_cache::store(&dir.path().join("tokens.json"), entry).unwrap();
        }
        invalidate(&source(&dir, cfg)).unwrap();
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
