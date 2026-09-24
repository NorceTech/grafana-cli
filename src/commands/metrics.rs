use crate::Result;
use crate::client::GrafanaClient;
use crate::commands::prom;
use crate::commands::{Bounds, CommandOutput, QueryWindow, Windowed, resolve_ds};
use crate::datasource::Kind;
use crate::error::Error;
use crate::output::OutputMode;
use crate::timeutil::{default_step_secs, to_epoch_secs};
use chrono::{DateTime, Utc};
use serde_json::Value;

#[derive(clap::Subcommand, Clone, Debug)]
pub enum MetricsCmd {
    Instant(InstantArgs),
    Range(RangeArgs),
}

#[derive(clap::Args, Clone, Debug)]
pub struct InstantArgs {
    pub query: String,
    #[arg(long)]
    pub at: Option<String>,
    #[arg(long)]
    pub datasource: Option<String>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct RangeArgs {
    pub query: String,
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long)]
    pub step: Option<String>,
}

impl MetricsCmd {
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
        bounds: Bounds,
    ) -> Result<CommandOutput> {
        match self {
            Self::Instant(args) => instant(client, profile, mode, args).await,
            Self::Range(args) => range(client, profile, mode, Windowed { args, bounds }).await,
        }
    }
}

fn table(headers: [&str; 3], rows: Vec<Vec<String>>) -> CommandOutput {
    CommandOutput::Table {
        headers: headers.map(String::from).to_vec(),
        rows,
    }
}

pub async fn instant(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    args: &InstantArgs,
) -> Result<CommandOutput> {
    let ds = resolve_ds(client, Kind::Prometheus, args.datasource.as_deref()).await?;
    let mut pairs = vec![("query", args.query.clone())];
    if let Some(at) = args.at.as_deref() {
        let dt = DateTime::parse_from_rfc3339(at)
            .map_err(|_| Error::Config {
                message: format!("invalid RFC3339 timestamp (--at): '{at}'"),
            })?
            .with_timezone(&Utc);
        pairs.push(("time", format!("{}", to_epoch_secs(dt))));
    }
    let mut reply = client
        .proxy_request(&ds.uid, "api/v1/query", &pairs)
        .await?;
    let records = prom::flatten_vector(prom::prom_result(&mut reply, "vector")?);
    if mode == OutputMode::Table {
        return Ok(table(
            ["metric", "value", "ts"],
            prom::vector_rows(&records),
        ));
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}

pub async fn range(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    range: Windowed<'_, RangeArgs>,
) -> Result<CommandOutput> {
    let Windowed {
        args,
        bounds: Bounds { start, end },
    } = range;
    let ds = resolve_ds(client, Kind::Prometheus, args.window.datasource.as_deref()).await?;
    let step_secs = match args.step.as_deref() {
        Some(s) => {
            let secs = humantime::parse_duration(s)
                .map_err(|_| Error::Config {
                    message: format!("invalid step duration: '{s}'"),
                })?
                .as_secs();
            if secs == 0 {
                return Err(Error::Config {
                    message: format!("step '{s}' must be at least 1s"),
                });
            }
            secs
        }
        None => default_step_secs(start, end),
    };
    let pairs = [
        ("query", args.query.clone()),
        ("start", format!("{}", to_epoch_secs(start))),
        ("end", format!("{}", to_epoch_secs(end))),
        ("step", step_secs.to_string()),
    ];
    let mut reply = client
        .proxy_request(&ds.uid, "api/v1/query_range", &pairs)
        .await?;
    let records = prom::flatten_matrix(prom::prom_result(&mut reply, "matrix")?);
    if mode == OutputMode::Table {
        return Ok(table(
            ["metric", "timestamp", "value"],
            prom::matrix_rows(&records),
        ));
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}
