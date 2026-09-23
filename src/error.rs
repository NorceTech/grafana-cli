use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("http error {status}: {}", message.as_deref().unwrap_or(body.as_str()))]
    Http {
        status: u16,
        body: String,
        message: Option<String>,
    },
    #[error("invalid url: {0}")]
    Url(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    // The request never got a complete reply (connect, TLS, body read);
    // `source` keeps reqwest's error so callers can ask `is_timeout()`.
    #[error("request {path}: {source}")]
    Transport {
        path: String,
        source: reqwest::Error,
    },
    #[error("auth error ({kind}): {message}")]
    Auth { kind: String, message: String },
    #[error("config error: {message}")]
    Config { message: String },
    #[error("no datasource of type {wanted_type}; available: {}", candidates.join(", "))]
    NoDatasource {
        wanted_type: String,
        candidates: Vec<String>,
    },
    #[error("ambiguous datasource of type {wanted_type}: {}", candidates.join(", "))]
    AmbiguousDatasource {
        wanted_type: String,
        candidates: Vec<String>,
    },
}

/// Longest plain-text body kept as an `http` error message, in chars.
const MESSAGE_CHARS: usize = 500;

impl Error {
    /// A non-2xx reply. `message` is the upstream's own error text: the JSON
    /// `message` field (Grafana), else the JSON `error` field (Prometheus,
    /// Loki), else the plain-text body (Loki and Tempo answer in text),
    /// trimmed and cut to 500 chars; `None` only for an empty body.
    pub fn http(status: u16, body: String) -> Self {
        let json_text = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                ["message", "error"]
                    .into_iter()
                    .find_map(|key| v.get(key)?.as_str().map(str::to_string))
            });
        let message = json_text.or_else(|| {
            let text = body.trim();
            (!text.is_empty()).then(|| clip(text, MESSAGE_CHARS).to_string())
        });
        Self::Http {
            status,
            body,
            message,
        }
    }
}

/// `text` cut to at most `max` chars; the cut never splits a char.
fn clip(text: &str, max: usize) -> &str {
    text.char_indices()
        .nth(max)
        .map_or(text, |(end, _)| &text[..end])
}

pub fn emit_json_error(profile: Option<&str>, err: &Error) -> String {
    let (kind, status, message) = match err {
        Error::Http {
            status, message, ..
        } => ("http", Some(*status), message.clone()),
        Error::Url(_) => ("url", None, Some(err.to_string())),
        Error::Io(_) | Error::Transport { .. } => ("io", None, Some(err.to_string())),
        Error::Auth { .. } => ("auth", None, Some(err.to_string())),
        Error::Config { .. } => ("config", None, Some(err.to_string())),
        Error::NoDatasource { .. } => ("no_datasource", None, Some(err.to_string())),
        Error::AmbiguousDatasource { .. } => ("ambiguous_datasource", None, Some(err.to_string())),
    };
    serde_json::json!({
        "error": {
            "profile": profile,
            "kind": kind,
            "status": status,
            "message": message,
        }
    })
    .to_string()
}

pub fn exit_code(err: &Error) -> i32 {
    match err {
        Error::Http { .. } | Error::Url(_) | Error::Io(_) | Error::Transport { .. } => 1,
        Error::Auth { .. } | Error::Config { .. } => 2,
        Error::NoDatasource { .. } | Error::AmbiguousDatasource { .. } => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_error_http_full() {
        let e = Error::Http {
            status: 401,
            body: "raw body".into(),
            message: Some("unauthorized".into()),
        };
        let v: serde_json::Value =
            serde_json::from_str(&emit_json_error(Some("prod"), &e)).unwrap();
        assert_eq!(v["error"]["profile"], "prod");
        assert_eq!(v["error"]["kind"], "http");
        assert_eq!(v["error"]["status"], 401);
        assert_eq!(v["error"]["message"], "unauthorized");
    }

    fn json_message(e: &Error) -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_str(&emit_json_error(None, e)).unwrap();
        v["error"]["message"].clone()
    }

    #[test]
    fn json_error_http_plain_text_body_is_the_message() {
        // Tempo's reply to a fractional start, verbatim.
        let body = "invalid start: strconv.ParseInt: parsing \"1790195939.49\": invalid syntax\n";

        let e = Error::http(400, body.into());

        assert_eq!(json_message(&e), body.trim());
    }

    #[test]
    fn json_error_http_empty_body_has_null_message() {
        for body in ["", " \n"] {
            let e = Error::http(500, body.into());

            let v: serde_json::Value = serde_json::from_str(&emit_json_error(None, &e)).unwrap();
            assert!(v["error"]["profile"].is_null());
            assert_eq!(v["error"]["status"], 500);
            assert!(v["error"]["message"].is_null(), "{body:?}: {v}");
        }
    }

    #[test]
    fn json_error_http_prefers_the_json_message_field() {
        let e = Error::http(
            400,
            r#"{"message":"grafana says","error":"upstream says"}"#.into(),
        );

        assert_eq!(json_message(&e), "grafana says");
    }

    #[test]
    fn json_error_http_falls_back_to_the_json_error_field() {
        let body = r#"{"status":"error","errorType":"bad_data","error":"parse error"}"#;

        let e = Error::http(400, body.into());

        assert_eq!(json_message(&e), "parse error");
    }

    #[test]
    fn json_error_http_long_text_body_is_cut_to_500_chars() {
        // Two-byte chars: a byte-based cut at 500 would split one.
        let e = Error::http(502, "\u{e9}".repeat(600));

        assert_eq!(json_message(&e), "\u{e9}".repeat(500));
    }

    fn transport() -> Error {
        // An unparsable URL makes reqwest's builder fail without any I/O.
        let source = reqwest::Client::new().get("http://").build().unwrap_err();
        Error::Transport {
            path: "/api/health".into(),
            source,
        }
    }

    #[test]
    fn json_error_shape_is_exact() {
        for e in [
            transport(),
            Error::Url("x".into()),
            Error::Io(std::io::Error::other("io")),
            Error::Auth {
                kind: "oauth".into(),
                message: "no token".into(),
            },
            Error::Config {
                message: "bad".into(),
            },
            Error::NoDatasource {
                wanted_type: "loki".into(),
                candidates: vec!["p".into()],
            },
            Error::AmbiguousDatasource {
                wanted_type: "loki".into(),
                candidates: vec!["a".into(), "b".into()],
            },
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&emit_json_error(Some("p"), &e)).unwrap();
            let keys: Vec<&str> = v["error"]
                .as_object()
                .unwrap()
                .keys()
                .map(|k| k.as_str())
                .collect();
            assert_eq!(keys, ["kind", "message", "profile", "status"]);
            assert_eq!(v["error"]["profile"], "p");
            assert!(v["error"]["status"].is_null());
            assert!(v["error"]["message"].is_string());
        }
    }

    #[test]
    fn kinds_map_correctly() {
        let kind = |e: &Error| {
            serde_json::from_str::<serde_json::Value>(&emit_json_error(None, e)).unwrap()
                ["error"]["kind"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            kind(&Error::Http {
                status: 1,
                body: String::new(),
                message: None
            }),
            "http"
        );
        assert_eq!(kind(&Error::Url("u".into())), "url");
        assert_eq!(kind(&Error::Io(std::io::Error::other("e"))), "io");
        assert_eq!(kind(&transport()), "io");
        assert_eq!(
            kind(&Error::Auth {
                kind: "k".into(),
                message: "m".into()
            }),
            "auth"
        );
        assert_eq!(
            kind(&Error::Config {
                message: "m".into()
            }),
            "config"
        );
        assert_eq!(
            kind(&Error::NoDatasource {
                wanted_type: "t".into(),
                candidates: vec![]
            }),
            "no_datasource"
        );
        assert_eq!(
            kind(&Error::AmbiguousDatasource {
                wanted_type: "t".into(),
                candidates: vec![]
            }),
            "ambiguous_datasource"
        );
    }

    #[test]
    fn exit_codes() {
        assert_eq!(
            exit_code(&Error::Http {
                status: 404,
                body: String::new(),
                message: None
            }),
            1
        );
        assert_eq!(exit_code(&Error::Io(std::io::Error::other("e"))), 1);
        assert_eq!(exit_code(&transport()), 1);
        assert_eq!(
            exit_code(&Error::Auth {
                kind: "k".into(),
                message: "m".into()
            }),
            2
        );
        assert_eq!(
            exit_code(&Error::Config {
                message: "m".into()
            }),
            2
        );
    }
}
