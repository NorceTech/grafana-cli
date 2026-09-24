use crate::Result;
use crate::client::GrafanaClient;
use crate::commands::{CommandOutput, resolve_ds};
use crate::datasource::Kind;
use crate::error::Error;
use crate::output::OutputMode;
use serde_json::Value;

use super::GetArgs;

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
                headers: ["service", "name", "status", "durationMillis"]
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
                // OTLP status codes, as the short form TraceQL uses; unset,
                // missing and unknown render as an empty cell.
                let status = match span.pointer("/status/code").and_then(Value::as_str) {
                    Some("STATUS_CODE_ERROR") => "error",
                    Some("STATUS_CODE_OK") => "ok",
                    _ => "",
                };
                spans.push((
                    start,
                    vec![
                        service.to_string(),
                        name.to_string(),
                        status.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            vec![
                "checkout".into(),
                "GET /cart".into(),
                "".into(),
                "1.5".into(),
            ],
            vec!["cart".into(), "SELECT".into(), "".into(), "2.5".into()],
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
            ["".to_string(), "GET /cart".into(), "".into(), "1.5".into()]
        );
    }

    #[test]
    fn span_rows_render_error_and_ok_status_codes() {
        let mut v = otlp_trace();
        // batches[1] holds the earlier span (GET /cart), batches[0] SELECT.
        v["batches"][1]["scopeSpans"][0]["spans"][0]["status"] =
            json!({"code": "STATUS_CODE_ERROR"});
        v["batches"][0]["scopeSpans"][0]["spans"][0]["status"] = json!({"code": "STATUS_CODE_OK"});

        let rows = span_rows(&v).unwrap();

        assert_eq!(rows[0], ["checkout", "GET /cart", "error", "1.5"]);
        assert_eq!(rows[1], ["cart", "SELECT", "ok", "2.5"]);
    }

    #[test]
    fn span_rows_unset_or_missing_status_renders_empty() {
        let mut v = otlp_trace();
        v["batches"][1]["scopeSpans"][0]["spans"][0]["status"] =
            json!({"code": "STATUS_CODE_UNSET"});
        v["batches"][0]["scopeSpans"][0]["spans"][0]["status"] = json!({});

        let rows = span_rows(&v).unwrap();

        assert_eq!(rows[0][2], "");
        assert_eq!(rows[1][2], "");
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
