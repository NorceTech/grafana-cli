use crate::output::LogRecord;
use serde_json::Value;

pub mod datasources;
pub mod health;
pub mod logs;
pub mod metrics;
pub mod profile;
pub mod traces;

pub enum CommandOutput {
    Json(Value),
    Logs(Vec<LogRecord>),
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Raw(Value),
}

/// Time range + datasource selection shared by every query subcommand.
#[derive(Clone)]
pub struct QueryWindow {
    pub since: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub datasource: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::GrafanaClient;
    use crate::config::Config;
    use crate::error::Error;

    fn client() -> &'static GrafanaClient {
        static CLIENT: std::sync::OnceLock<GrafanaClient> = std::sync::OnceLock::new();
        CLIENT.get_or_init(|| {
            GrafanaClient::new(
                "http://127.0.0.1:1".into(),
                crate::auth::AuthProvider::Token("t".into()),
                reqwest::Client::new(),
            )
        })
    }

    fn window() -> QueryWindow {
        QueryWindow {
            since: None,
            from: None,
            to: None,
            datasource: None,
        }
    }

    #[tokio::test]
    async fn query_commands_are_implemented() {
        // Dead-server client: every query command must attempt real work
        // (Io/Http error), never return the Unimplemented stub.
        let c = client();
        let w = window();
        let m = crate::output::OutputMode::Json;
        let results = vec![
            (
                "datasources",
                datasources::run(c, "p", m, Some("loki")).await,
            ),
            (
                "logs::search",
                logs::search(c, "p", m, "q", &w, 100, "backward").await,
            ),
            ("logs::labels", logs::labels(c, "p", &w).await),
            (
                "logs::label_values",
                logs::label_values(c, "p", "job", &w).await,
            ),
            (
                "metrics::instant",
                metrics::instant(c, "p", m, "up", None, None).await,
            ),
            (
                "metrics::range",
                metrics::range(c, "p", m, "up", &w, Some("15s")).await,
            ),
            ("traces::get", traces::get(c, "p", m, "abc", None).await),
            (
                "traces::search",
                traces::search(c, "p", m, "q", &w, 20).await,
            ),
        ];
        for (name, r) in results {
            assert!(
                !matches!(r, Err(Error::Unimplemented)),
                "{name} still returns Unimplemented"
            );
        }
    }

    #[test]
    fn profile_commands_are_implemented() {
        let mut config = Config::default();
        let args = profile::AddArgs {
            name: "n".into(),
            url: "http://x".into(),
            token: None,
            token_command: None,
            username: None,
            password: None,
            password_command: None,
            token_url: None,
            client_id: None,
            client_secret: None,
            client_secret_command: None,
            scope: vec![],
            audience: None,
        };
        for (name, r) in [
            ("add", profile::add(&mut config, &args)),
            (
                "list",
                profile::list(&config, crate::output::OutputMode::Json),
            ),
            ("use", profile::use_profile(&mut config, "n")),
            ("remove", profile::remove(&mut config, "n")),
            ("set_token", profile::set_token(&mut config, "n", "t")),
        ] {
            assert!(
                !matches!(r, Err(Error::Unimplemented)),
                "{name} still returns Unimplemented"
            );
        }
    }
}
