use crate::Result;
use crate::client::GrafanaClient;
use crate::commands::prom;
use crate::commands::{CommandOutput, resolve_ds};
use crate::datasource::Kind;
use crate::error::Error;
use crate::output::OutputMode;
use crate::timeutil::to_ns_string;
use chrono::DateTime;
use serde_json::Value;

use super::InstantArgs;

/// A LogQL metric query evaluated at one instant: Loki's `query` endpoint,
/// with `time` in nanoseconds (Loki's wire unit, unlike Prometheus's epoch
/// seconds) and omitted without `--at` so Loki evaluates at now. The vector
/// result is flattened like `metrics instant`; a log query here is rejected
/// by Loki with a plain-text 400 that surfaces as the error message.
pub async fn instant(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    args: &InstantArgs,
) -> Result<CommandOutput> {
    let ds = resolve_ds(client, Kind::Loki, args.datasource.as_deref()).await?;
    let mut pairs = vec![("query", args.query.clone())];
    if let Some(at) = args.at.as_deref() {
        let dt = DateTime::parse_from_rfc3339(at)
            .map_err(|_| Error::Config {
                message: format!("invalid RFC3339 timestamp (--at): '{at}'"),
            })?
            .with_timezone(&chrono::Utc);
        pairs.push(("time", to_ns_string(dt)?));
    }
    let mut reply = client
        .proxy_request(&ds.uid, "loki/api/v1/query", &pairs)
        .await?;
    let records = prom::flatten_vector(prom::prom_result(&mut reply, "vector")?);
    if mode == OutputMode::Table {
        return Ok(CommandOutput::Table {
            headers: ["metric", "value", "ts"].map(String::from).to_vec(),
            rows: prom::vector_rows(&records),
        });
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}
