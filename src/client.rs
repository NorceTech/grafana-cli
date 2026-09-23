use crate::auth::{self, AuthProvider};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Datasource {
    pub id: i64,
    pub uid: String,
    pub name: String,
    pub r#type: String,
    pub url: String,
    #[serde(rename = "isDefault")]
    pub is_default: bool,
}

pub struct GrafanaClient {
    base_url: String,
    auth: AuthProvider,
    http: reqwest::Client,
}

impl GrafanaClient {
    pub fn new(base_url: String, auth: AuthProvider, http: reqwest::Client) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth,
            http,
        }
    }

    pub async fn health(&self) -> Result<Value> {
        self.get_json("/api/health", &[]).await
    }

    pub async fn datasources(&self) -> Result<Vec<Datasource>> {
        let value = self.get_json("/api/datasources", &[]).await?;
        serde_json::from_value(value).map_err(|e| Error::Http {
            status: 200,
            body: String::new(),
            message: Some(format!("datasource parse: {e}")),
        })
    }

    pub async fn proxy_request(
        &self,
        uid: &str,
        path: &str,
        query_pairs: &[(&str, String)],
    ) -> Result<Value> {
        self.get_json(
            &format!("/api/datasources/proxy/uid/{uid}/{path}"),
            query_pairs,
        )
        .await
    }

    async fn authorize(&self, builder: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        match &self.auth {
            AuthProvider::Token(token) => Ok(builder.bearer_auth(token)),
            AuthProvider::Basic { user, pass } => Ok(builder.basic_auth(user, Some(pass))),
            AuthProvider::OAuth(source) => {
                let token = auth::get_oauth_token(source, &self.http).await?;
                Ok(builder.bearer_auth(token))
            }
        }
    }

    async fn get_json(&self, path: &str, query_pairs: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let mut oauth_attempt = 0;
        loop {
            let response = self
                .authorize(self.http.get(&url).query(query_pairs))
                .await?
                .send()
                .await
                .map_err(|source| transport(path, source))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|source| transport(path, source))?;
            if status.is_success() {
                return serde_json::from_str(&body).map_err(|e| Error::Http {
                    status: status.as_u16(),
                    body: body.clone(),
                    message: Some(format!("invalid JSON: {e}")),
                });
            }
            if status.as_u16() == 401
                && oauth_attempt == 0
                && matches!(self.auth, AuthProvider::OAuth(_))
            {
                if let AuthProvider::OAuth(source) = &self.auth {
                    auth::invalidate(source)?;
                }
                oauth_attempt += 1;
                continue;
            }
            return Err(Error::http(status.as_u16(), body));
        }
    }
}

fn transport(path: &str, source: reqwest::Error) -> Error {
    Error::Transport {
        path: path.to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(base: &str, auth: AuthProvider) -> GrafanaClient {
        GrafanaClient::new(base.to_string(), auth, reqwest::Client::new())
    }

    async fn mount_health_ok(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"commit": "c1", "database": "ok", "version": "11.4.0"})),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn health_happy_path() {
        let server = MockServer::start().await;
        mount_health_ok(&server).await;
        let out = client(&server.uri(), AuthProvider::Token("t".into()))
            .health()
            .await
            .unwrap();
        assert_eq!(out["version"], "11.4.0");
    }

    #[tokio::test]
    async fn trailing_slash_base_url_trimmed() {
        let server = MockServer::start().await;
        mount_health_ok(&server).await;
        let base = format!("{}/", server.uri());
        let out = client(&base, AuthProvider::Token("t".into()))
            .health()
            .await
            .unwrap();
        assert_eq!(out["database"], "ok");
    }

    #[tokio::test]
    async fn bearer_token_applied() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .and(wiremock::matchers::header("Authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "1"})))
            .expect(1)
            .mount(&server)
            .await;
        client(&server.uri(), AuthProvider::Token("tok".into()))
            .health()
            .await
            .unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn basic_auth_applied() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .and(wiremock::matchers::header("Authorization", "Basic dTpw"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "1"})))
            .expect(1)
            .mount(&server)
            .await;
        client(
            &server.uri(),
            AuthProvider::Basic {
                user: "u".into(),
                pass: "p".into(),
            },
        )
        .health()
        .await
        .unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn grafana_error_message_surfaced() {
        let server = MockServer::start().await;
        let body = json!({"message": "bad LogQL"}).to_string();
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .respond_with(ResponseTemplate::new(400).set_body_string(body.clone()))
            .mount(&server)
            .await;
        let err = client(&server.uri(), AuthProvider::Token("t".into()))
            .health()
            .await
            .unwrap_err();
        match err {
            Error::Http {
                status,
                body: got_body,
                message,
            } => {
                assert_eq!(status, 400);
                assert_eq!(body, got_body);
                assert_eq!(message.as_deref(), Some("bad LogQL"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn non_json_success_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>nope</html>"))
            .mount(&server)
            .await;
        let err = client(&server.uri(), AuthProvider::Token("t".into()))
            .health()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Http { status: 200, .. }), "{err:?}");
    }

    #[tokio::test]
    async fn unreachable_server_is_a_transport_error_keeping_its_cause() {
        // Port 1 is privileged and unused: the connection is refused.
        let err = client("http://127.0.0.1:1", AuthProvider::Token("t".into()))
            .health()
            .await
            .unwrap_err();

        assert!(
            matches!(&err, Error::Transport { path, source } if path == "/api/health" && source.is_connect()),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn token_auth_401_fails_fast() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let err = client(&server.uri(), AuthProvider::Token("t".into()))
            .health()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Http { status: 401, .. }));
        server.verify().await;
    }

    #[tokio::test]
    async fn oauth_401_invalidates_refetches_retries_once() {
        let dir = tempfile::tempdir().unwrap();

        let server = MockServer::start().await;
        // FIFO precedence: token endpoint returns tok1 once, then tok2
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "tok1", "expires_in": 3600})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "tok2", "expires_in": 3600})),
            )
            .expect(1)
            .mount(&server)
            .await;
        // API: 401 for tok1 once, then 200 for tok2
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .and(wiremock::matchers::header("Authorization", "Bearer tok1"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .and(wiremock::matchers::header("Authorization", "Bearer tok2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "1"})))
            .expect(1)
            .mount(&server)
            .await;

        let auth = AuthProvider::OAuth(auth::OAuthSource {
            profile_name: "p".into(),
            config: crate::OAuthConfig {
                token_url: format!("{}/token", server.uri()),
                client_id: "cid".into(),
                client_secret: Some("sec".into()),
                client_secret_command: None,
                scopes: None,
                audience: None,
            },
            cache_path: dir.path().join("tokens.json"),
        });
        let out = client(&server.uri(), auth).health().await.unwrap();
        assert_eq!(out["version"], "1");
        server.verify().await;
    }

    #[tokio::test]
    async fn datasources_parse_camel_case() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/datasources"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": 1, "uid": "u1", "name": "Loki", "type": "loki",
                "url": "/loki", "isDefault": true
            }, {
                "id": 2, "uid": "u2", "name": "Prom", "type": "prometheus",
                "url": "/prom", "isDefault": false
            }])))
            .mount(&server)
            .await;
        let list = client(&server.uri(), AuthProvider::Token("t".into()))
            .datasources()
            .await
            .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].r#type, "loki");
        assert!(list[0].is_default);
        assert!(!list[1].is_default);
        assert_eq!(list[1].uid, "u2");
    }

    #[tokio::test]
    async fn proxy_request_builds_path_and_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/datasources/proxy/uid/u1/loki/api/v1/labels"))
            .and(query_param("since", "123"))
            .and(query_param("query", "{job=\"api\"}"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "success"})))
            .expect(1)
            .mount(&server)
            .await;
        let out = client(&server.uri(), AuthProvider::Token("t".into()))
            .proxy_request(
                "u1",
                "loki/api/v1/labels",
                &[
                    ("since", "123".to_string()),
                    ("query", "{job=\"api\"}".to_string()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(out["status"], "success");
        server.verify().await;
    }
}
