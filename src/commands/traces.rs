use crate::client::GrafanaClient;
use crate::commands::{Bounds, CommandOutput, QueryWindow, Windowed, cell, resolve_ds};
use crate::datasource::Kind;
use crate::output::OutputMode;
use crate::{Result, error::Error};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

#[derive(clap::Subcommand, Clone, Debug)]
pub enum TracesCmd {
    Get(GetArgs),
    Search(SearchArgs),
}

#[derive(clap::Args, Clone, Debug)]
pub struct GetArgs {
    pub trace_id: String,
    #[arg(long)]
    pub datasource: Option<String>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct SearchArgs {
    pub query: String,
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long, default_value_t = 20)]
    pub limit: u64,
}

impl TracesCmd {
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
        bounds: Bounds,
    ) -> Result<CommandOutput> {
        match self {
            Self::Get(args) => get(client, profile, mode, args).await,
            Self::Search(args) => search(client, profile, mode, Windowed { args, bounds }).await,
        }
    }
}

fn nanos_to_rfc3339(s: &str) -> Option<String> {
    let ns: i64 = s.parse().ok()?;
    // rem_euclid(1_000_000_000) is in 0..1_000_000_000, always inside u32, so
    // the narrowing conversion cannot fail; try_from states that instead of a
    // truncating `as`.
    let subsec_nanos = u32::try_from(ns.rem_euclid(1_000_000_000)).ok()?;
    DateTime::<Utc>::from_timestamp(ns.div_euclid(1_000_000_000), subsec_nanos)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// Rejects a trace id Tempo would not accept, 1 to 32 hex digits, before it
/// becomes a segment of `api/traces/{id}`: reqwest resolves `..` in a URL, so
/// an unchecked id walks out of the datasource proxy into any Grafana API,
/// sent with the profile's credentials.
pub(crate) fn validate_trace_id(id: &str) -> Result<()> {
    if (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(Error::Config {
        message: format!("invalid trace id: '{id}' (expected 1 to 32 hex digits)"),
    })
}

pub async fn get(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    args: &GetArgs,
) -> Result<CommandOutput> {
    let ds = resolve_ds(client, Kind::Tempo, args.datasource.as_deref()).await?;
    let value = client
        .proxy_request(&ds.uid, &format!("api/traces/{}", args.trace_id), &[])
        .await?
        .body;
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
    // Lossless passthrough: the upstream JSON is handed to the renderer as-is.
    Ok(CommandOutput::Json(value))
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
    search: Windowed<'_, SearchArgs>,
) -> Result<CommandOutput> {
    let Windowed {
        args,
        bounds: Bounds { start, end },
    } = search;
    let ds = resolve_ds(client, Kind::Tempo, args.window.datasource.as_deref()).await?;
    // Tempo parses start/end with strconv.ParseInt: whole seconds only, a
    // fraction is a 400. Floor the start and ceil the end to cover the window.
    let end_secs = end.timestamp() + i64::from(end.timestamp_subsec_nanos() > 0);
    let pairs = [
        ("q", args.query.clone()),
        ("start", start.timestamp().to_string()),
        ("end", end_secs.to_string()),
        ("limit", args.limit.to_string()),
    ];
    let mut reply = client.proxy_request(&ds.uid, "api/search", &pairs).await?;
    // `traces` is taken out of the body instead of cloned: it is the whole
    // result, and nothing else reads the reply.
    let traces = match reply.body.pointer_mut("/traces").map(Value::take) {
        Some(Value::Array(traces)) => traces,
        _ => Vec::new(),
    };

    let mut records = Vec::new();
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
        records.push(json!({
            "traceID": t.get("traceID").cloned().unwrap_or(Value::Null),
            "rootServiceName": t.get("rootServiceName").cloned().unwrap_or(Value::Null),
            "rootTraceName": t.get("rootTraceName").cloned().unwrap_or(Value::Null),
            "durationMs": t.get("durationMs").cloned().unwrap_or(Value::Null),
            "start": start,
        }));
    }
    if mode == OutputMode::Table {
        let rows = records
            .iter()
            .map(|r| {
                vec![
                    cell(&r["traceID"]),
                    cell(&r["rootServiceName"]),
                    cell(&r["rootTraceName"]),
                    cell(&r["durationMs"]),
                    cell(&r["start"]),
                ]
            })
            .collect();
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
    fn trace_ids_of_1_to_32_hex_digits_are_valid() {
        for id in ["a", "abc123", "0123456789abcdefABCDEF0123456789"] {
            let result = validate_trace_id(id);

            assert!(result.is_ok(), "{id}: {result:?}");
        }
    }

    #[test]
    fn other_trace_ids_are_config_errors_naming_the_id() {
        let too_long = "a".repeat(33);
        for id in [
            "", &too_long, "xyz", "abc/123", "..", "%2e%2e", "abc 123", "\u{e9}",
        ] {
            let result = validate_trace_id(id);

            assert!(
                matches!(&result, Err(Error::Config { message }) if message.contains(&format!("'{id}'"))),
                "{id}: {result:?}"
            );
        }
    }

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
