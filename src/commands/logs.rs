use crate::Result;
use crate::client::{Datasource, GrafanaClient, Reply};
use crate::commands::{CommandOutput, QueryWindow, success_data};
use crate::datasource::{self, Kind};
use crate::output::{LogRecord, OutputMode};
use crate::timeutil::{resolve_window, to_ns_string};
use chrono::{DateTime, SecondsFormat};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(clap::Subcommand, Clone, Debug)]
pub enum LogsCmd {
    Search(SearchArgs),
    Labels {
        #[command(flatten)]
        window: QueryWindow,
    },
    LabelValues {
        label: String,
        #[command(flatten)]
        window: QueryWindow,
    },
}

#[derive(clap::Args, Clone, Debug)]
pub struct SearchArgs {
    pub query: String,
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long, default_value_t = 100)]
    pub limit: u64,
    #[arg(long, value_enum, default_value = "backward")]
    pub direction: Direction,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Direction {
    Forward,
    Backward,
}

impl Direction {
    /// Loki's `direction` query parameter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
        }
    }
}

impl LogsCmd {
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
    ) -> Result<CommandOutput> {
        match self {
            Self::Search(args) => search(client, profile, mode, args).await,
            Self::Labels { window } => labels(client, profile, window).await,
            Self::LabelValues { label, window } => {
                label_values(client, profile, label, window).await
            }
        }
    }
}

/// Window bounds (ns strings) + the resolved Loki datasource for a query.
async fn loki_context(
    client: &GrafanaClient,
    window: &QueryWindow,
) -> Result<(String, String, Datasource)> {
    let (start, end) = resolve_window(
        window.since.as_deref(),
        window.from.as_deref(),
        window.to.as_deref(),
    )?;
    let all = client.datasources().await?;
    let ds = datasource::resolve(&all, Kind::Loki, window.datasource.as_deref())?;
    Ok((to_ns_string(start), to_ns_string(end), ds.clone()))
}

/// The names in a `labels` or `label/{label}/values` reply.
fn string_array(reply: &Reply) -> Result<Vec<String>> {
    let data = success_data(reply)?.ok_or_else(|| reply.malformed("no data array"))?;
    Vec::<String>::deserialize(data).map_err(|e| reply.malformed(&format!("data: {e}")))
}

pub async fn search(
    client: &GrafanaClient,
    profile: &str,
    mode: OutputMode,
    args: &SearchArgs,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, &args.window).await?;
    let reply = client
        .proxy_request(
            &ds.uid,
            "loki/api/v1/query_range",
            &[
                ("query", args.query.clone()),
                ("start", start),
                ("end", end),
                ("limit", args.limit.to_string()),
                ("direction", args.direction.as_str().to_string()),
            ],
        )
        .await?;
    let records = log_records(&reply, profile, &ds.name)?;
    if mode == OutputMode::Table {
        let rows = records
            .iter()
            .map(|r| vec![r.ts.clone(), r.line.clone(), labels_kv(&r.labels)])
            .collect();
        return Ok(CommandOutput::Table {
            headers: ["ts", "line", "labels"].map(String::from).to_vec(),
            rows,
        });
    }
    Ok(CommandOutput::Logs(records))
}

/// The records of a `query_range` reply for `profile` and `datasource`.
fn log_records(reply: &Reply, profile: &str, datasource: &str) -> Result<Vec<LogRecord>> {
    let streams = success_data(reply)?
        .and_then(|data| data.get("result"))
        .and_then(Value::as_array)
        .ok_or_else(|| reply.malformed("no data.result array"))?;

    // Keep the raw ns alongside the record: primary sort key must be true
    // (nanosecond) chronology, not the millisecond-truncated ts string.
    let mut records: Vec<(i64, LogRecord)> = Vec::new();
    for stream in streams {
        let labels: BTreeMap<String, String> = serde_json::from_value(
            stream
                .get("stream")
                .cloned()
                .ok_or_else(|| reply.malformed("stream without a stream label map"))?,
        )
        .map_err(|e| reply.malformed(&format!("stream labels: {e}")))?;
        let entries = stream
            .get("values")
            .and_then(Value::as_array)
            .ok_or_else(|| reply.malformed("stream without a values array"))?;
        for entry in entries {
            let pair = entry
                .as_array()
                .filter(|p| p.len() >= 2)
                .ok_or_else(|| reply.malformed("entry is not a [ts, line] pair"))?;
            let ns = pair[0]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| reply.malformed("entry timestamp is not integer nanoseconds"))?;
            records.push((
                ns,
                LogRecord {
                    profile: profile.to_string(),
                    datasource: datasource.to_string(),
                    ts: DateTime::from_timestamp_nanos(ns)
                        .to_rfc3339_opts(SecondsFormat::Millis, true),
                    labels: labels.clone(),
                    line: pair[1].as_str().unwrap_or_default().to_string(),
                },
            ));
        }
    }
    records.sort_by(|a, b| (a.0, a.1.line.as_str()).cmp(&(b.0, b.1.line.as_str())));
    Ok(records.into_iter().map(|(_, r)| r).collect())
}

fn labels_kv(labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn json_string_array(names: Vec<String>) -> Value {
    Value::Array(names.into_iter().map(Value::String).collect())
}

pub async fn labels(
    client: &GrafanaClient,
    _profile: &str,
    window: &QueryWindow,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, window).await?;
    let reply = client
        .proxy_request(
            &ds.uid,
            "loki/api/v1/labels",
            &[("start", start), ("end", end)],
        )
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &reply,
    )?)))
}

pub async fn label_values(
    client: &GrafanaClient,
    _profile: &str,
    label: &str,
    window: &QueryWindow,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, window).await?;
    let reply = client
        .proxy_request(
            &ds.uid,
            &format!("loki/api/v1/label/{label}/values"),
            &[("start", start), ("end", end)],
        )
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &reply,
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use serde_json::json;

    fn records(body: Value) -> Result<Vec<LogRecord>> {
        log_records(&Reply::ok(body), "p", "Loki")
    }

    fn streams(result: Value) -> Value {
        json!({"status": "success", "data": {"resultType": "streams", "result": result}})
    }

    #[test]
    fn entries_sort_by_nanosecond_timestamp_then_line() {
        // One millisecond, entries 1 ns apart, newest listed first.
        let body = streams(json!([
            {"stream": {"job": "api"},
             "values": [["1787925600000000002", "b"], ["1787925600000000001", "c"]]},
            {"stream": {"job": "web"}, "values": [["1787925600000000001", "a"]]}
        ]));

        let got = records(body).unwrap();

        let lines: Vec<&str> = got.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["a", "c", "b"]);
    }

    #[test]
    fn reply_without_data_is_a_response_error() {
        let result = records(json!({"status": "success"}));

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }

    #[test]
    fn stream_with_non_array_values_is_a_response_error() {
        let body = streams(json!([{"stream": {"job": "api"}, "values": "api boom"}]));

        let result = records(body);

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }

    #[test]
    fn entry_with_non_integer_ns_is_a_response_error() {
        // Fractional seconds, not Loki's integer nanoseconds.
        let body = streams(json!([
            {"stream": {"job": "api"}, "values": [["1787925600.5", "api boom"]]}
        ]));

        let result = records(body);

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }

    #[test]
    fn error_status_is_http_with_the_upstream_error() {
        let body = json!({"status": "error", "errorType": "bad_data", "error": "parse error"});

        let result = records(body);

        assert!(
            matches!(&result, Err(Error::Http { status: 200, message: Some(m), .. }) if m == "parse error"),
            "{result:?}"
        );
    }

    #[test]
    fn label_reply_with_non_string_names_is_a_response_error() {
        let reply = Reply::ok(json!({"status": "success", "data": [1, 2]}));

        let result = string_array(&reply);

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }
}
