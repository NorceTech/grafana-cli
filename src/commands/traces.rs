use crate::client::{GrafanaClient, Reply};
use crate::commands::{Bounds, CommandOutput, QueryWindow, Windowed, cell, resolve_ds};
use crate::datasource::Kind;
use crate::output::OutputMode;
use crate::{Result, error::Error};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(clap::Subcommand, Clone, Debug)]
pub enum TracesCmd {
    Get(GetArgs),
    Search(SearchArgs),
    Tags(TagsArgs),
    TagValues(TagValuesArgs),
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

#[derive(clap::Args, Clone, Debug)]
pub struct TagsArgs {
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long, value_enum)]
    pub scope: Option<TagScope>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct TagValuesArgs {
    pub tag: String,
    #[command(flatten)]
    pub window: QueryWindow,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum TagScope {
    Resource,
    Span,
    Intrinsic,
    Event,
    Link,
    Instrumentation,
}

impl TagScope {
    /// Tempo's `scope` query parameter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resource => "resource",
            Self::Span => "span",
            Self::Intrinsic => "intrinsic",
            Self::Event => "event",
            Self::Link => "link",
            Self::Instrumentation => "instrumentation",
        }
    }
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
            Self::Tags(args) => tags(client, Windowed { args, bounds }).await,
            Self::TagValues(args) => tag_values(client, mode, Windowed { args, bounds }).await,
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

/// Rejects a tag unfit for `api/v2/search/tag/{tag}/values` before it becomes
/// a path segment; see `validate_trace_id` for why. TraceQL tag names are
/// dotted scope-qualified names (`resource.service.name`), bare intrinsics
/// (`status`) and colon event names (`event:name`), so the class is
/// `[A-Za-z0-9_.:-]` — and a bare `.` or `..` is what reqwest resolves.
pub(crate) fn validate_tag(tag: &str) -> Result<()> {
    let valid = !matches!(tag, "" | "." | "..")
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-' | b'_'));
    if valid {
        return Ok(());
    }
    Err(Error::Config {
        message: format!(
            "invalid tag: '{tag}' (expected letters, digits, '.', ':', '-' or '_', and not '.' or '..')"
        ),
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

pub async fn search(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    search: Windowed<'_, SearchArgs>,
) -> Result<CommandOutput> {
    let Windowed { args, bounds } = search;
    let ds = resolve_ds(client, Kind::Tempo, args.window.datasource.as_deref()).await?;
    let (start, end) = tempo_secs(bounds);
    let pairs = [
        ("q", args.query.clone()),
        ("start", start),
        ("end", end),
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

/// Window bounds as Tempo's whole epoch seconds: Tempo parses start/end with
/// strconv.ParseInt, so a fraction is a 400. The start is floored and the end
/// ceiled to keep the requested window covered.
fn tempo_secs(Bounds { start, end }: Bounds) -> (String, String) {
    let end_secs = end.timestamp() + i64::from(end.timestamp_subsec_nanos() > 0);
    (start.timestamp().to_string(), end_secs.to_string())
}

/// The TraceQL-ready names in an `api/v2/search/tags` reply: every tag
/// qualified by its scope (`span.http.method`, `resource.service.name`),
/// except intrinsics, which TraceQL addresses bare (`status`, `duration`).
/// Sorted and de-duplicated for stable output. The v2 API is used because
/// v1 `api/search/tags` returns protobuf text mangled into strings.
fn traceql_tag_names(reply: &Reply) -> Result<Vec<String>> {
    let scopes = reply
        .body
        .get("scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| reply.malformed("no scopes array"))?;
    let mut names = Vec::new();
    for scope in scopes {
        let name = scope
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| reply.malformed("scope without a name"))?;
        let tags = scope
            .get("tags")
            .and_then(Value::as_array)
            .ok_or_else(|| reply.malformed("scope without a tags array"))?;
        for tag in tags {
            let tag = tag
                .as_str()
                .ok_or_else(|| reply.malformed("tag is not a string"))?;
            if name == "intrinsic" {
                names.push(tag.to_string());
            } else {
                names.push(format!("{name}.{tag}"));
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

pub async fn tags(client: &GrafanaClient, window: Windowed<'_, TagsArgs>) -> Result<CommandOutput> {
    let Windowed { args, bounds } = window;
    let ds = resolve_ds(client, Kind::Tempo, args.window.datasource.as_deref()).await?;
    let (start, end) = tempo_secs(bounds);
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if let Some(scope) = args.scope {
        pairs.push(("scope", scope.as_str().to_string()));
    }
    pairs.push(("start", start));
    pairs.push(("end", end));
    let reply = client
        .proxy_request(&ds.uid, "api/v2/search/tags", &pairs)
        .await?;
    // Discovery names are query-building input, not rows to scan, so they are
    // a JSON array in every output mode, like logs labels.
    Ok(CommandOutput::Json(Value::Array(
        traceql_tag_names(&reply)?
            .into_iter()
            .map(Value::String)
            .collect(),
    )))
}

// A typed tag value: the type decides TraceQL quoting (strings need quotes,
// ints and durations do not), so it travels with the value.
#[derive(Clone, Debug, Deserialize)]
struct TagValue {
    r#type: String,
    value: String,
}

/// The typed values in an `api/v2/search/tag/{tag}/values` reply, sorted by
/// type then value for stable output whatever the upstream order.
fn typed_tag_values(reply: &Reply) -> Result<Vec<TagValue>> {
    let values = reply
        .body
        .get("tagValues")
        .and_then(Value::as_array)
        .ok_or_else(|| reply.malformed("no tagValues array"))?;
    let mut values: Vec<TagValue> = values
        .iter()
        .cloned()
        .map(|v| {
            TagValue::deserialize(v).map_err(|e| reply.malformed(&format!("tagValues entry: {e}")))
        })
        .collect::<Result<_>>()?;
    values.sort_by(|a, b| (&a.r#type, &a.value).cmp(&(&b.r#type, &b.value)));
    Ok(values)
}

pub async fn tag_values(
    client: &GrafanaClient,
    mode: OutputMode,
    window: Windowed<'_, TagValuesArgs>,
) -> Result<CommandOutput> {
    let Windowed { args, bounds } = window;
    let ds = resolve_ds(client, Kind::Tempo, args.window.datasource.as_deref()).await?;
    let (start, end) = tempo_secs(bounds);
    let reply = client
        .proxy_request(
            &ds.uid,
            &format!("api/v2/search/tag/{}/values", args.tag),
            &[("start", start), ("end", end)],
        )
        .await?;
    let values = typed_tag_values(&reply)?;
    if mode == OutputMode::Table {
        let rows = values
            .iter()
            .map(|v| vec![v.r#type.clone(), v.value.clone()])
            .collect();
        return Ok(CommandOutput::Table {
            headers: ["type", "value"].map(String::from).to_vec(),
            rows,
        });
    }
    Ok(CommandOutput::Json(Value::Array(
        values
            .into_iter()
            .map(|v| json!({"type": v.r#type, "value": v.value}))
            .collect(),
    )))
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
    fn traceql_shaped_tags_are_valid() {
        for tag in [
            "resource.service.name",
            "span.http.status_code",
            "status",
            "duration",
            "event:exception.type",
            "instrumentation:name",
            "k8s-namespace",
            "tag_with_underscore",
        ] {
            let result = validate_tag(tag);

            assert!(result.is_ok(), "{tag}: {result:?}");
        }
    }

    #[test]
    fn other_tags_are_config_errors_naming_the_tag() {
        for tag in ["", ".", "..", "bad tag", "tag/../other", "tag?", "t\u{e4}g"] {
            let result = validate_tag(tag);

            assert!(
                matches!(&result, Err(Error::Config { message }) if message.contains(&format!("'{tag}'"))),
                "{tag}: {result:?}"
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

    #[test]
    fn tag_names_flatten_sort_and_dedup() {
        // Scopes out of alphabetical order, one intrinsic tag duplicated.
        let reply = Reply::ok(json!({"scopes": [
            {"name": "span", "tags": ["http.method", "db.name"]},
            {"name": "intrinsic", "tags": ["status", "duration", "name", "status"]},
            {"name": "resource", "tags": ["service.name"]}
        ]}));

        let names = traceql_tag_names(&reply).unwrap();

        assert_eq!(
            names,
            [
                "duration",
                "name",
                "resource.service.name",
                "span.db.name",
                "span.http.method",
                "status"
            ]
        );
    }

    #[test]
    fn tags_reply_of_the_wrong_shape_is_a_response_error() {
        for body in [
            json!({"tagNames": []}),
            json!({"scopes": [{"tags": []}]}),
            json!({"scopes": [{"name": "span"}]}),
            json!({"scopes": [{"name": "span", "tags": [1]}]}),
        ] {
            let result = traceql_tag_names(&Reply::ok(body));

            assert!(
                matches!(result, Err(Error::Response { status: 200, .. })),
                "{result:?}"
            );
        }
    }

    #[test]
    fn tag_values_sort_by_type_then_value() {
        let reply = Reply::ok(json!({"tagValues": [
            {"type": "string", "value": "checkout"},
            {"type": "int", "value": "200"},
            {"type": "string", "value": "cart"}
        ]}));

        let values = typed_tag_values(&reply).unwrap();

        let pairs: Vec<(&str, &str)> = values
            .iter()
            .map(|v| (v.r#type.as_str(), v.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("int", "200"), ("string", "cart"), ("string", "checkout")]
        );
    }

    #[test]
    fn tag_values_reply_of_the_wrong_shape_is_a_response_error() {
        for body in [
            json!({"values": []}),
            json!({"tagValues": [{"type": "string"}]}),
            json!({"tagValues": [{"value": "x"}]}),
            json!({"tagValues": ["x"]}),
        ] {
            let result = typed_tag_values(&Reply::ok(body));

            assert!(
                matches!(result, Err(Error::Response { status: 200, .. })),
                "{result:?}"
            );
        }
    }
}
