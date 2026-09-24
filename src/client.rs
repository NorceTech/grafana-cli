use crate::auth::{self, AuthProvider};
use crate::error::clip;
use crate::{Result, error::Error};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Datasource {
    pub uid: String,
    pub name: String,
    pub r#type: String,
    pub url: String,
    #[serde(rename = "isDefault")]
    pub is_default: bool,
}

/// Where a 2xx reply came from: what an error about its body names.
#[derive(Debug, Clone)]
pub struct ReplyMeta {
    pub path: String,
    pub status: u16,
    pub content_type: Option<String>,
}

/// A 2xx reply whose body parsed as JSON.
#[derive(Debug)]
pub struct Reply {
    pub meta: ReplyMeta,
    pub body: Value,
}

/// How much of a malformed body an `Error::Response` quotes, in chars.
const SNIPPET_CHARS: usize = 200;

impl ReplyMeta {
    /// `Error::Response` for this reply, whose `body` is not what `what` says:
    /// names the request path and content type and quotes the start of the
    /// body, so an SSO proxy's HTML login page is recognisable.
    pub fn malformed(&self, what: &str, body: &str) -> Error {
        let body = body.trim();
        let quoted = match clip(body, SNIPPET_CHARS) {
            "" => "(empty body)".to_string(),
            start if start.len() < body.len() => format!("{start}..."),
            whole => whole.to_string(),
        };
        Error::Response {
            status: self.status,
            message: format!(
                "{what} (GET {}, content-type {}): {quoted}",
                self.path,
                self.content_type.as_deref().unwrap_or("none"),
            ),
        }
    }
}

impl Reply {
    /// `Error::Response` for a body without the shape `what` describes.
    pub fn malformed(&self, what: &str) -> Error {
        self.meta.malformed(what, &self.body.to_string())
    }
}

/// A profile's base URL: an absolute `http`/`https` address. `GrafanaClient`
/// only ever requests from one of these, so a scheme-less or otherwise
/// unusable url is `Error::Url` when the profile is added or the client is
/// built, not a reqwest builder error at request time.
#[derive(Debug)]
pub struct BaseUrl(String);

impl BaseUrl {
    /// Parses `url`, rejecting anything but an absolute http(s) address, and
    /// keeps the normalized form; the trailing slash a root url gains is
    /// trimmed, as it always was.
    pub fn parse(url: &str) -> Result<Self> {
        let bad = |why: String| Error::Url(format!("'{url}': {why}"));
        let parsed = reqwest::Url::parse(url).map_err(|e| bad(e.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(bad("scheme must be http or https".into()));
        }
        Ok(Self(parsed.to_string().trim_end_matches('/').to_string()))
    }

    /// The normalized url, for callers that validated it earlier and now
    /// build a client from it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct GrafanaClient {
    base_url: BaseUrl,
    auth: AuthProvider,
    http: reqwest::Client,
}

impl GrafanaClient {
    /// `base_url` is validated by `BaseUrl::parse`: an unusable url is
    /// `Error::Url` here, not a reqwest builder error at request time.
    pub fn new(base_url: &str, auth: AuthProvider, http: reqwest::Client) -> Result<Self> {
        Ok(Self {
            base_url: BaseUrl::parse(base_url)?,
            auth,
            http,
        })
    }

    pub async fn health(&self) -> Result<Value> {
        Ok(self.get_json("/api/health", &[]).await?.body)
    }

    pub async fn datasources(&self) -> Result<Vec<Datasource>> {
        let reply = self.get_json("/api/datasources", &[]).await?;
        Vec::<Datasource>::deserialize(&reply.body)
            .map_err(|e| reply.malformed(&format!("unexpected datasource list: {e}")))
    }

    pub async fn proxy_request(
        &self,
        uid: &str,
        path: &str,
        query_pairs: &[(&str, String)],
    ) -> Result<Reply> {
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

    async fn get_json(&self, path: &str, query_pairs: &[(&str, String)]) -> Result<Reply> {
        let url = format!("{}{path}", self.base_url.0);
        let mut oauth_attempt = 0;
        loop {
            let response = self
                .authorize(self.http.get(&url).query(query_pairs))
                .await?
                .send()
                .await
                .map_err(|source| transport(path, source))?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let body = response
                .text()
                .await
                .map_err(|source| transport(path, source))?;
            if status.is_success() {
                let meta = ReplyMeta {
                    path: path.to_string(),
                    status: status.as_u16(),
                    content_type,
                };
                return match serde_json::from_str(&body) {
                    Ok(json) => Ok(Reply { meta, body: json }),
                    Err(e) => Err(meta.malformed(&format!("invalid JSON: {e}"), &body)),
                };
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

#[cfg(test)]
impl Reply {
    /// A 200 JSON reply, for tests of the code that reshapes replies.
    pub(crate) fn ok(body: Value) -> Self {
        Self {
            meta: ReplyMeta {
                path: "/api/datasources/proxy/uid/u1/test".into(),
                status: 200,
                content_type: Some("application/json".into()),
            },
            body,
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
        GrafanaClient::new(base, auth, reqwest::Client::new()).expect("fixture url")
    }

    #[test]
    fn base_urls_accept_absolute_http_addresses() {
        for url in [
            "http://grafana.example.com",
            "https://grafana.example.com",
            "https://grafana.example.com:3000",
            "https://grafana.example.com/grafana/",
        ] {
            BaseUrl::parse(url).expect(url);
        }
    }

    #[test]
    fn base_urls_reject_anything_else_as_a_url_error() {
        for url in [
            "grafana.example.com",
            "localhost:3000",
            "ftp://grafana.example.com",
            "http://",
            "not a url",
        ] {
            let err = BaseUrl::parse(url).unwrap_err();

            assert!(matches!(err, Error::Url(_)), "{url}: {err:?}");
            assert!(err.to_string().contains(url), "{url}: {err}");
        }
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
    async fn non_json_success_is_a_response_error() {
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
        assert!(
            matches!(err, Error::Response { status: 200, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn html_login_page_is_recognisable_in_the_message() {
        // An SSO proxy in front of Grafana answering with its login page.
        let page = format!(
            "<!DOCTYPE html><html><head><title>Sign in</title></head><body>{}TAIL</body></html>",
            "x".repeat(300)
        );
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/datasources"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(page, "text/html; charset=utf-8"))
            .mount(&server)
            .await;

        let err = client(&server.uri(), AuthProvider::Token("t".into()))
            .datasources()
            .await
            .unwrap_err();

        let Error::Response { status, message } = &err else {
            panic!("{err:?}");
        };
        assert_eq!(*status, 200);
        assert!(message.contains("text/html"), "content type: {message}");
        assert!(message.contains("/api/datasources"), "path: {message}");
        assert!(
            message.contains("<title>Sign in</title>"),
            "body start: {message}"
        );
        assert!(
            !message.contains("TAIL"),
            "body beyond the snippet: {message}"
        );
    }

    #[tokio::test]
    async fn datasource_list_of_unexpected_shape_is_a_response_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/datasources"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"datasources": []})))
            .mount(&server)
            .await;

        let err = client(&server.uri(), AuthProvider::Token("t".into()))
            .datasources()
            .await
            .unwrap_err();

        assert!(
            matches!(&err, Error::Response { status: 200, message } if message.contains("/api/datasources")),
            "{err:?}"
        );
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
    async fn refused_connection_is_named_in_the_message() {
        let err = client("http://127.0.0.1:1", AuthProvider::Token("t".into()))
            .health()
            .await
            .unwrap_err();

        // reqwest's own text ("error sending request for url ...") is the same
        // for a refusal, a DNS failure and a bad certificate; the OS error at
        // the end of the source chain tells them apart.
        let message = err.to_string();
        assert!(message.contains("Connection refused"), "{message}");
    }

    // The shared client's read_timeout bounds a request against a server that
    // sends nothing: a Transport error keeping reqwest's timeout cause, with
    // a message that names the path and the timeout.
    #[tokio::test]
    async fn read_timeout_bounds_a_server_that_answers_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"version": "1"}))
                    .set_delay(std::time::Duration::from_secs(5)),
            )
            .mount(&server)
            .await;
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_millis(100))
            .build()
            .unwrap();

        let err = GrafanaClient::new(&server.uri(), AuthProvider::Token("t".into()), http)
            .expect("fixture url")
            .health()
            .await
            .unwrap_err();

        assert!(
            matches!(&err, Error::Transport { path, source } if path == "/api/health" && source.is_timeout()),
            "{err:?}"
        );
        assert!(err.to_string().contains("timed out"), "{err}");
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
            config: crate::config::OAuthConfig {
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
                "uid": "u1", "name": "Loki", "type": "loki",
                "url": "/loki", "isDefault": true
            }, {
                "uid": "u2", "name": "Prom", "type": "prometheus",
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
        assert_eq!(out.body["status"], "success");
        server.verify().await;
    }
}
