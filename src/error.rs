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
    #[error("auth error ({kind}): {message}")]
    Auth { kind: String, message: String },
    #[error("config error: {message}")]
    Config { message: String },
    #[error("not implemented")]
    Unimplemented,
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

pub fn emit_json_error(profile: Option<&str>, err: &Error) -> String {
    let (kind, status, message) = match err {
        Error::Http {
            status, message, ..
        } => ("http", Some(*status), message.clone()),
        Error::Url(_) => ("url", None, Some(err.to_string())),
        Error::Io(_) => ("io", None, Some(err.to_string())),
        Error::Auth { .. } => ("auth", None, Some(err.to_string())),
        Error::Config { .. } => ("config", None, Some(err.to_string())),
        Error::Unimplemented => ("unimplemented", None, Some(err.to_string())),
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
        Error::Http { .. } | Error::Url(_) | Error::Io(_) => 1,
        Error::Auth { .. } | Error::Config { .. } => 2,
        Error::NoDatasource { .. } | Error::AmbiguousDatasource { .. } => 2,
        Error::Unimplemented => 3,
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

    #[test]
    fn json_error_http_null_message() {
        let e = Error::Http {
            status: 500,
            body: "oops".into(),
            message: None,
        };
        let v: serde_json::Value = serde_json::from_str(&emit_json_error(None, &e)).unwrap();
        assert!(v["error"]["profile"].is_null());
        assert_eq!(v["error"]["status"], 500);
        assert!(v["error"]["message"].is_null());
    }

    #[test]
    fn json_error_shape_is_exact() {
        for e in [
            Error::Unimplemented,
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
        assert_eq!(kind(&Error::Unimplemented), "unimplemented");
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
        assert_eq!(exit_code(&Error::Unimplemented), 3);
    }
}
