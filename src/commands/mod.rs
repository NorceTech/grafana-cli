use crate::client::{GrafanaClient, Reply};
use crate::output::{LogRecord, OutputMode};
use crate::{Error, Result};
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
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
    ) -> Result<CommandOutput> {
        match self {
            Self::Health => health::run(client, profile, mode).await,
            Self::Datasources(args) => datasources::run(client, profile, mode, args).await,
            Self::Logs { cmd } => cmd.run(client, profile, mode).await,
            Self::Metrics { cmd } => cmd.run(client, profile, mode).await,
            Self::Traces { cmd } => cmd.run(client, profile, mode).await,
        }
    }
}
