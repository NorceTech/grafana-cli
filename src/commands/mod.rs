use crate::client::{GrafanaClient, Reply};
use crate::output::{LogRecord, OutputMode};
use crate::timeutil::resolve_window;
use crate::{Error, Result};
use chrono::{DateTime, Utc};
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

// Time range + datasource selection shared by every query subcommand.
// Clap-derived items take `//` comments only: clap turns `///` into help text.
#[derive(clap::Args, Clone, Debug)]
pub struct QueryWindow {
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub from: Option<String>,
    #[arg(long)]
    pub to: Option<String>,
    #[arg(long)]
    pub datasource: Option<String>,
}

/// The start and end a `QueryWindow` resolved to, once per invocation.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// A windowed command's arguments and the bounds their window resolved to:
/// commands read `bounds`, never `--since`/`--from`/`--to` themselves.
#[derive(Debug)]
pub struct Windowed<'a, A> {
    pub args: &'a A,
    pub bounds: Bounds,
}

/// `data` of a reply in the envelope Prometheus and Loki share,
/// `{"status": "success" | "error", "data": ..., "error": ...}`. A
/// `"status": "error"` reply is the upstream's own error (kind `http`), any
/// other status a malformed reply; `None` is a success without `data`.
fn success_data(reply: &Reply) -> Result<Option<&Value>> {
    match reply.body.get("status").and_then(Value::as_str) {
        Some("success") => Ok(reply.body.get("data")),
        Some("error") => Err(Error::http(reply.meta.status, reply.body.to_string())),
        _ => Err(reply.malformed("no \"status\": \"success\" or \"error\"")),
    }
}

// The subcommands that run once per selected profile; the bin flattens them
// into its top-level `Commands`.
#[derive(clap::Subcommand, Clone, Debug)]
pub enum Query {
    Health,
    Datasources(datasources::DatasourcesArgs),
    Logs {
        #[command(subcommand)]
        cmd: logs::LogsCmd,
    },
    Metrics {
        #[command(subcommand)]
        cmd: metrics::MetricsCmd,
    },
    Traces {
        #[command(subcommand)]
        cmd: traces::TracesCmd,
    },
}

impl Query {
    /// Resolves the time window once, before fanout: a bad `--since`,
    /// `--from` or `--to` is one config error instead of one per profile, and
    /// every profile queries the same bounds.
    pub fn resolve(&self) -> Result<Resolved> {
        let window = self.window();
        let (start, end) = resolve_window(
            window.and_then(|w| w.since.as_deref()),
            window.and_then(|w| w.from.as_deref()),
            window.and_then(|w| w.to.as_deref()),
        )?;
        Ok(Resolved {
            query: self.clone(),
            bounds: Bounds { start, end },
        })
    }

    fn window(&self) -> Option<&QueryWindow> {
        use logs::LogsCmd;
        use metrics::MetricsCmd;
        use traces::TracesCmd;
        match self {
            Self::Logs {
                cmd: LogsCmd::Search(args),
            } => Some(&args.window),
            Self::Logs {
                cmd: LogsCmd::Labels { window } | LogsCmd::LabelValues { window, .. },
            } => Some(window),
            Self::Metrics {
                cmd: MetricsCmd::Range(args),
            } => Some(&args.window),
            Self::Traces {
                cmd: TracesCmd::Search(args),
            } => Some(&args.window),
            Self::Health
            | Self::Datasources(_)
            | Self::Metrics {
                cmd: MetricsCmd::Instant(_),
            }
            | Self::Traces {
                cmd: TracesCmd::Get(_),
            } => None,
        }
    }
}

/// A `Query` with its time window resolved. Commands without a window get the
/// default (the last hour) and ignore it.
#[derive(Debug)]
pub struct Resolved {
    query: Query,
    bounds: Bounds,
}

impl Resolved {
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
    ) -> Result<CommandOutput> {
        let bounds = self.bounds;
        match &self.query {
            Query::Health => health::run(client, profile, mode).await,
            Query::Datasources(args) => datasources::run(client, profile, mode, args).await,
            Query::Logs { cmd } => cmd.run(client, profile, mode, bounds).await,
            Query::Metrics { cmd } => cmd.run(client, profile, mode, bounds).await,
            Query::Traces { cmd } => cmd.run(client, profile, mode, bounds).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_window_is_a_config_error() {
        let query = Query::Logs {
            cmd: logs::LogsCmd::Labels {
                window: QueryWindow {
                    since: Some("bogus".into()),
                    from: None,
                    to: None,
                    datasource: None,
                },
            },
        };

        let result = query.resolve();

        assert!(matches!(result, Err(Error::Config { .. })), "{result:?}");
    }
}
