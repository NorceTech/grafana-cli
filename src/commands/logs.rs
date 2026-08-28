use crate::client::{Datasource, GrafanaClient};
use crate::commands::{CommandOutput, QueryWindow};
use crate::datasource;
use crate::output::{LogRecord, OutputMode};
use crate::timeutil::{resolve_window, to_ns_string};
use crate::{Error, Result};
use chrono::{DateTime, SecondsFormat};
use serde_json::Value;
use std::collections::BTreeMap;

const LOKI: &str = "loki";

fn parse_error(what: &str) -> Error {
    Error::Http {
        status: 200,
        body: String::new(),
        message: Some(format!("loki response parse: {what}")),
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
    let ds = datasource::resolve(&all, LOKI, window.datasource.as_deref())?;
    Ok((to_ns_string(start), to_ns_string(end), ds.clone()))
}

fn string_array(value: &Value) -> Result<Vec<String>> {
    let data = value
        .get("data")
        .ok_or_else(|| parse_error("missing data array"))?;
    serde_json::from_value(data.clone()).map_err(|e| parse_error(&format!("data: {e}")))
}

pub async fn search(
    client: &GrafanaClient,
    profile: &str,
    mode: OutputMode,
    query: &str,
    window: &QueryWindow,
    limit: u64,
    direction: &str,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, window).await?;
    let value = client
        .proxy_request(
            &ds.uid,
            "loki/api/v1/query_range",
            &[
                ("query", query.to_string()),
                ("start", start),
                ("end", end),
                ("limit", limit.to_string()),
                ("direction", direction.to_string()),
            ],
        )
        .await?;
    let streams = value
        .pointer("/data/result")
        .and_then(Value::as_array)
        .ok_or_else(|| parse_error("missing data.result array"))?;

    // Keep the raw ns alongside the record: primary sort key must be true
    // (nanosecond) chronology, not the millisecond-truncated ts string.
    let mut records: Vec<(i64, LogRecord)> = Vec::new();
    for stream in streams {
        let labels: BTreeMap<String, String> = serde_json::from_value(
            stream
                .get("stream")
                .cloned()
                .ok_or_else(|| parse_error("stream missing stream map"))?,
        )
        .map_err(|e| parse_error(&format!("stream labels: {e}")))?;
        let entries = stream
            .get("values")
            .and_then(Value::as_array)
            .ok_or_else(|| parse_error("stream missing values array"))?;
        for entry in entries {
            let pair = entry
                .as_array()
                .filter(|p| p.len() >= 2)
                .ok_or_else(|| parse_error("entry is not a [ts, line] pair"))?;
            let ns = pair[0]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| parse_error("entry timestamp is not a ns integer"))?;
            records.push((
                ns,
                LogRecord {
                    profile: profile.to_string(),
                    datasource: ds.name.clone(),
                    ts: DateTime::from_timestamp_nanos(ns)
                        .to_rfc3339_opts(SecondsFormat::Millis, true),
                    labels: labels.clone(),
                    line: pair[1].as_str().unwrap_or_default().to_string(),
                },
            ));
        }
    }
    records.sort_by(|a, b| (a.0, a.1.line.as_str()).cmp(&(b.0, b.1.line.as_str())));
    let records: Vec<LogRecord> = records.into_iter().map(|(_, r)| r).collect();
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
    let value = client
        .proxy_request(
            &ds.uid,
            "loki/api/v1/labels",
            &[("start", start), ("end", end)],
        )
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &value,
    )?)))
}

pub async fn label_values(
    client: &GrafanaClient,
    _profile: &str,
    label: &str,
    window: &QueryWindow,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, window).await?;
    let value = client
        .proxy_request(
            &ds.uid,
            &format!("loki/api/v1/label/{label}/values"),
            &[("start", start), ("end", end)],
        )
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &value,
    )?)))
}
