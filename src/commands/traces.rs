use crate::Result;
use crate::client::{Datasource, GrafanaClient};
use crate::commands::{CommandOutput, QueryWindow};
use crate::datasource;
use crate::output::OutputMode;
use crate::timeutil::resolve_window;
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

/// Span table from Tempo's `api/traces/{id}` JSON: OTLP `batches` (or the
/// standard `resourceSpans`), one per resource, spans under `scopeSpans`, u64
/// nanos as strings. Rows are sorted by span start. Best-effort: None on ANY
/// shape surprise -> caller falls back to raw JSON.
fn span_rows(v: &Value) -> Option<Vec<Vec<String>>> {
    let batches = v
        .get("batches")
        .or_else(|| v.get("resourceSpans"))?
        .as_array()?;
    let nanos = |span: &Value, key: &str| span.get(key)?.as_str()?.parse::<u64>().ok();
    let mut spans: Vec<(u64, Vec<String>)> = Vec::new();
    for batch in batches {
        let service = batch
            .pointer("/resource/attributes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|a| a.get("key").and_then(Value::as_str) == Some("service.name"))
            .and_then(|a| a.pointer("/value/stringValue"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        for scope in batch.get("scopeSpans")?.as_array()? {
            for span in scope.get("spans")?.as_array()? {
                let name = span.get("name").and_then(Value::as_str)?;
                let start = nanos(span, "startTimeUnixNano")?;
                let end = nanos(span, "endTimeUnixNano")?;
                spans.push((
                    start,
                    vec![
                        service.to_string(),
                        name.to_string(),
                        format!("{}", end.saturating_sub(start) as f64 / 1e6),
                    ],
                ));
            }
        }
    }
    if spans.is_empty() {
        return None;
    }
    spans.sort_by_key(|(start, _)| *start);
    Some(spans.into_iter().map(|(_, row)| row).collect())
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
    // Tempo parses start/end with strconv.ParseInt: whole seconds only, a
    // fraction is a 400. Floor the start and ceil the end to cover the window.
    let end_secs = end.timestamp() + i64::from(end.timestamp_subsec_nanos() > 0);
    let pairs = [
        ("q", query.to_string()),
        ("start", start.timestamp().to_string()),
        ("end", end_secs.to_string()),
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

    /// Shape of Tempo's `api/traces/{id}` (captured live): OTLP `batches`, one
    /// per resource, spans under `scopeSpans`, u64 nanos as strings. Batches
    /// are listed out of start order on purpose.
    fn otlp_trace() -> Value {
        json!({"batches": [
            {"resource": {"attributes": [
                {"key": "k8s.namespace.name", "value": {"stringValue": "shop"}},
                {"key": "service.name", "value": {"stringValue": "cart"}}
            ]},
             "scopeSpans": [{"scope": {"name": "db"}, "spans": [
                {"traceId": "t", "spanId": "s2", "name": "SELECT",
                 "startTimeUnixNano": "1759372800000500000", "endTimeUnixNano": "1759372800003000000"}
             ]}]},
            {"resource": {"attributes": [
                {"key": "service.name", "value": {"stringValue": "checkout"}}
            ]},
             "scopeSpans": [{"scope": {"name": "http"}, "spans": [
                {"traceId": "t", "spanId": "s1", "name": "GET /cart",
                 "startTimeUnixNano": "1759372800000000000", "endTimeUnixNano": "1759372800001500000"}
             ]}]}
        ]})
    }

    #[test]
    fn span_rows_from_tempo_otlp_sorted_by_start() {
        let expected = Some(vec![
            vec!["checkout".into(), "GET /cart".into(), "1.5".into()],
            vec!["cart".into(), "SELECT".into(), "2.5".into()],
        ]);
        assert_eq!(span_rows(&otlp_trace()), expected);
        // OTLP's standard top-level key parses the same way.
        let standard = json!({"resourceSpans": otlp_trace()["batches"].clone()});
        assert_eq!(span_rows(&standard), expected);
    }

    #[test]
    fn span_rows_missing_service_renders_empty_cell() {
        let mut v = otlp_trace();
        v["batches"][1]["resource"]["attributes"] = json!([]);
        assert_eq!(
            span_rows(&v).unwrap()[0],
            ["".to_string(), "GET /cart".into(), "1.5".into()]
        );
    }

    #[test]
    fn span_rows_none_on_shape_surprise() {
        // span without an end time -> fallback
        let mut v = otlp_trace();
        v["batches"][0]["scopeSpans"][0]["spans"][0]
            .as_object_mut()
            .unwrap()
            .remove("endTimeUnixNano");
        assert_eq!(span_rows(&v), None);
        // no batches/resourceSpans (e.g. a flat `spans` list) -> fallback
        assert_eq!(span_rows(&json!({"traceID": "t", "spans": []})), None);
        // no spans at all -> fallback
        assert_eq!(span_rows(&json!({"batches": []})), None);
    }
}
