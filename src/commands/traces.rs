use crate::Result;
use crate::client::{Datasource, GrafanaClient};
use crate::commands::{CommandOutput, QueryWindow};
use crate::datasource;
use crate::output::OutputMode;
use crate::timeutil::{resolve_window, to_epoch_secs};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

async fn resolve_ds(client: &GrafanaClient, flag: Option<&str>) -> Result<Datasource> {
    let all = client.datasources().await?;
    Ok(datasource::resolve(&all, "tempo", flag)?.clone())
}

fn cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn nanos_to_rfc3339(s: &str) -> Option<String> {
    let ns: i64 = s.parse().ok()?;
    DateTime::<Utc>::from_timestamp(
        ns.div_euclid(1_000_000_000),
        ns.rem_euclid(1_000_000_000) as u32,
    )
    .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub async fn get(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    trace_id: &str,
    datasource: Option<&str>,
) -> Result<CommandOutput> {
    let ds = resolve_ds(client, datasource).await?;
    let value = client
        .proxy_request(&ds.uid, &format!("api/traces/{trace_id}"), &[])
        .await?;
    if mode == OutputMode::Table {
        if let Some(rows) = span_rows(&value) {
            return Ok(CommandOutput::Table {
                headers: ["service", "name", "durationMillis"]
                    .map(String::from)
                    .to_vec(),
                rows,
            });
        }
        eprintln!("note: unrecognized trace structure; printing raw JSON");
    }
    // Lossless passthrough; main renders Raw exactly like Json (compact single line).
    Ok(CommandOutput::Raw(value))
}

/// Best-effort: None on ANY shape surprise -> caller falls back to raw JSON.
fn span_rows(v: &Value) -> Option<Vec<Vec<String>>> {
    let spans = v.get("spans")?.as_array()?;
    if spans.is_empty() {
        return None;
    }
    let mut rows = Vec::new();
    for s in spans {
        let service = s
            .pointer("/resource/service.name")
            .or_else(|| s.get("service.name"))
            .and_then(Value::as_str)?;
        let name = s.get("name").and_then(Value::as_str)?;
        let nanos = s.get("durationNanos").and_then(|d| {
            d.as_i64()
                .or_else(|| d.as_str().and_then(|x| x.parse::<i64>().ok()))
        })?;
        rows.push(vec![
            service.to_string(),
            name.to_string(),
            format!("{}", nanos as f64 / 1e6),
        ]);
    }
    Some(rows)
}

pub async fn search(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    query: &str,
    window: &QueryWindow,
    limit: u64,
) -> Result<CommandOutput> {
    let ds = resolve_ds(client, window.datasource.as_deref()).await?;
    let (start, end) = resolve_window(
        window.since.as_deref(),
        window.from.as_deref(),
        window.to.as_deref(),
    )?;
    let pairs = [
        ("q", query.to_string()),
        ("start", format!("{}", to_epoch_secs(start))),
        ("end", format!("{}", to_epoch_secs(end))),
        ("limit", limit.to_string()),
    ];
    let body = client.proxy_request(&ds.uid, "api/search", &pairs).await?;
    let traces = body
        .get("traces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut records = Vec::new();
    let mut rows = Vec::new();
    for t in &traces {
        let start = t
            .get("startTimeUnixNano")
            .and_then(Value::as_str)
            .and_then(nanos_to_rfc3339);
        if t.get("startTimeUnixNano").is_some() && start.is_none() {
            eprintln!(
                "note: trace {} has unparseable startTimeUnixNano",
                t.get("traceID").and_then(Value::as_str).unwrap_or("?")
            );
        }
        let record = json!({
            "traceID": t.get("traceID").cloned().unwrap_or(Value::Null),
            "rootServiceName": t.get("rootServiceName").cloned().unwrap_or(Value::Null),
            "rootTraceName": t.get("rootTraceName").cloned().unwrap_or(Value::Null),
            "durationMs": t.get("durationMs").cloned().unwrap_or(Value::Null),
            "start": start,
        });
        rows.push(vec![
            cell(&record["traceID"]),
            cell(&record["rootServiceName"]),
            cell(&record["rootTraceName"]),
            cell(&record["durationMs"]),
            cell(&record["start"]),
        ]);
        records.push(record);
    }
    if mode == OutputMode::Table {
        return Ok(CommandOutput::Table {
            headers: [
                "traceID",
                "rootServiceName",
                "rootTraceName",
                "durationMs",
                "start",
            ]
            .map(String::from)
            .to_vec(),
            rows,
        });
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanos_string_to_rfc3339_millis() {
        assert_eq!(
            nanos_to_rfc3339("1759372800000000000").as_deref(),
            Some("2025-10-02T02:40:00.000Z")
        );
        assert_eq!(
            nanos_to_rfc3339("1759372800500000000").as_deref(),
            Some("2025-10-02T02:40:00.500Z")
        );
        assert_eq!(nanos_to_rfc3339("junk"), None);
        assert_eq!(nanos_to_rfc3339(""), None);
    }

    #[test]
    fn span_rows_extract_or_none() {
        let v = json!({"traceID": "t", "spans": [
            {"resource": {"service.name": "checkout"}, "name": "GET /cart", "durationNanos": 1500000},
            {"resource": {"service.name": "cart"}, "name": "SELECT", "durationNanos": "2500000"}
        ]});
        assert_eq!(
            span_rows(&v),
            Some(vec![
                vec!["checkout".into(), "GET /cart".into(), "1.5".into()],
                vec!["cart".into(), "SELECT".into(), "2.5".into()]
            ])
        );
        // missing duration -> fallback
        assert_eq!(span_rows(&json!({"spans": [{"name": "x"}]})), None);
        // no spans key -> fallback
        assert_eq!(span_rows(&json!({"traceID": "t"})), None);
        // empty spans -> fallback
        assert_eq!(span_rows(&json!({"spans": []})), None);
    }
}
