mod common;

use common::*;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FROM: &str = "2026-08-28T14:00:00Z";
const TO: &str = "2026-08-28T15:00:00Z";
const TRACEQL: &str = "{ resource.service.name = \"checkout\" }";

/// Tempo search bounds are whole epoch seconds (Tempo parses them with
/// strconv.ParseInt); derive them with chrono, independent of gcli.
fn epoch_secs(rfc3339: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .timestamp()
}

fn ds_mock() -> Mock {
    datasources_json(json!([
        {"id": 1, "uid": "tempo1", "name": "Tempo", "type": "tempo",
         "url": "/tempo", "isDefault": true},
        {"id": 2, "uid": "prom1", "name": "Prom", "type": "prometheus",
         "url": "/prom", "isDefault": false}
    ]))
}

/// The shape Tempo's `api/traces/{id}` answers with: OTLP `batches`, one per
/// resource, spans under `scopeSpans`, u64 nanos as strings. Two batches so a
/// table render shows one row per span with distinct services.
fn trace_body() -> Value {
    json!({"batches": [
        {"resource": {"attributes": [
            {"key": "service.name", "value": {"stringValue": "checkout"}}
        ]},
         "scopeSpans": [{"scope": {"name": "http"}, "spans": [
            {"traceId": "abc123", "spanId": "s1", "name": "GET /cart",
             "startTimeUnixNano": "1759372800000000000",
             "endTimeUnixNano": "1759372800001500000"}
         ]}]},
        {"resource": {"attributes": [
            {"key": "service.name", "value": {"stringValue": "cart"}}
        ]},
         "scopeSpans": [{"scope": {"name": "db"}, "spans": [
            {"traceId": "abc123", "spanId": "s2", "name": "SELECT items",
             "startTimeUnixNano": "1759372800000500000",
             "endTimeUnixNano": "1759372800003000000"}
         ]}]}
    ]})
}

fn search_body() -> Value {
    json!({
        "traces": [
            {
                "traceID": "t1",
                "rootServiceName": "checkout",
                "rootTraceName": "GET /cart",
                "startTimeUnixNano": "1759372800000000000",
                "durationMs": 42
            },
            {
                "traceID": "t2",
                "rootServiceName": "api",
                "rootTraceName": "POST /order",
                "startTimeUnixNano": "1759372800500000000",
                "durationMs": "7"
            }
        ],
        "metrics": {"inspectedTraces": 2}
    })
}

fn expected_search_records() -> Value {
    json!([
        {"traceID": "t1", "rootServiceName": "checkout", "rootTraceName": "GET /cart",
         "durationMs": 42, "start": "2025-10-02T02:40:00.000Z"},
        {"traceID": "t2", "rootServiceName": "api", "rootTraceName": "POST /order",
         "durationMs": "7", "start": "2025-10-02T02:40:00.500Z"}
    ])
}

fn assert_success(out: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout.into_owned()
}

#[tokio::test]
async fn s7_traces_get() {
    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    let body = trace_body();
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/tempo1/api/traces/abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["traces", "get", "abc123"], &[env]);

    let stdout = assert_success(&out);
    // lossless passthrough: stdout must equal the response body semantically
    let got: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(got, body, "{stdout}");
    server.verify().await;
    write_transcript("s7_traces_get.txt", "gcli traces get abc123", &out);
}

#[tokio::test]
async fn s7_traces_search() {
    let (start, end) = (epoch_secs(FROM).to_string(), epoch_secs(TO).to_string());

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/tempo1/api/search"))
        // q compared after percent-decoding: exact TraceQL string
        .and(query_param("q", TRACEQL))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .and(query_param("limit", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "traces", "search", TRACEQL, "--from", FROM, "--to", TO, "--limit", "5",
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    let records: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(records, expected_search_records(), "{stdout}");
    server.verify().await;
    write_transcript(
        "s7_traces_search.txt",
        "gcli traces search '{ resource.service.name = \"checkout\" }' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --limit 5",
        &out,
    );
}

// The shape Tempo's `api/v2/search/tags` answers with: scopes out of
// alphabetical order, a duplicated intrinsic tag to pin de-duplication.
// Generic values only, never the captured instance's names.
fn tags_body() -> Value {
    json!({"scopes": [
        {"name": "span", "tags": ["http.method", "db.name"]},
        {"name": "resource", "tags": ["service.name", "deployment.environment"]},
        {"name": "intrinsic", "tags": ["status", "duration", "name", "status"]},
        {"name": "event", "tags": ["exception.type"]}
    ], "metrics": {"inspectedBytes": "7414172"}})
}

// Scopes flattened to TraceQL names (bare intrinsics), sorted, de-duplicated.
fn expected_tag_names() -> Value {
    json!([
        "duration",
        "event.exception.type",
        "name",
        "resource.deployment.environment",
        "resource.service.name",
        "span.db.name",
        "span.http.method",
        "status"
    ])
}

// The shape Tempo's `api/v2/search/tag/<tag>/values` answers with: typed
// values out of order to pin CLI-side sorting.
fn tag_values_body() -> Value {
    json!({"tagValues": [
        {"type": "string", "value": "checkout"},
        {"type": "int", "value": "200"},
        {"type": "string", "value": "cart"},
        {"type": "keyword", "value": "api"}
    ], "metrics": {"inspectedBytes": "2395279"}})
}

fn expected_tag_values() -> Value {
    json!([
        {"type": "int", "value": "200"},
        {"type": "keyword", "value": "api"},
        {"type": "string", "value": "cart"},
        {"type": "string", "value": "checkout"}
    ])
}

// Every `--since` window has sub-second bounds. Live Tempo rejects a fractional
// start/end with 400 "invalid start: strconv.ParseInt", so the start is floored
// and the end ceiled to keep the requested window covered.
#[tokio::test]
async fn s7_traces_search_subsecond_bounds_send_whole_seconds() {
    const FROM_FRAC: &str = "2026-08-28T14:00:00.500Z";
    const TO_FRAC: &str = "2026-08-28T15:00:00.250Z";
    let start = epoch_secs(FROM_FRAC).to_string();
    let end = (epoch_secs(TO_FRAC) + 1).to_string();

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/tempo1/api/search"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "traces", "search", TRACEQL, "--from", FROM_FRAC, "--to", TO_FRAC,
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    let records: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(records, expected_search_records(), "{stdout}");
    server.verify().await;
    write_transcript(
        "s7_traces_search_subsecond.txt",
        "gcli traces search '{ resource.service.name = \"checkout\" }' --from 2026-08-28T14:00:00.500Z --to 2026-08-28T15:00:00.250Z",
        &out,
    );
}

#[tokio::test]
async fn s7_traces_404() {
    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/tempo1/api/traces/deadbeef",
        ))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"message": "trace not found"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["traces", "get", "deadbeef"], &[env]);

    assert_eq!(out.status.code(), Some(1), "expected exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("trace not found"), "{stderr}");
    server.verify().await;
    write_transcript("s7_traces_404.txt", "gcli traces get deadbeef", &out);
}

// Table mode renders one row per span from the OTLP batches.
#[tokio::test]
async fn s21_traces_get_table_renders_one_row_per_span() {
    let server = MockServer::start().await;
    ds_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/tempo1/api/traces/abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(trace_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["traces", "get", "abc123", "--output", "table"], &[env]);

    let stdout = assert_success(&out);
    for cell in ["GET /cart", "SELECT items", "checkout", "cart"] {
        assert!(stdout.contains(cell), "{cell} missing: {stdout}");
    }
    for header in ["service", "name", "status", "durationMillis"] {
        assert!(stdout.contains(header), "{header} missing: {stdout}");
    }
    assert!(!stdout.trim_start().starts_with('{'), "{stdout}");
    server.verify().await;
    write_transcript(
        "s21_traces_get_table.txt",
        "gcli traces get abc123 --output table",
        &out,
    );
}

// The trace id becomes a path segment and reqwest resolves `..`, so an
// unchecked id walks out of the Tempo proxy into Grafana's own API with the
// profile's credentials. It is a config error naming the id, and Grafana
// receives no request at all.
#[tokio::test]
async fn s7_traces_get_rejects_a_path_traversing_trace_id() {
    const TRAVERSAL: &str = "../../../../../../../api/admin/users";
    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/admin/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"login": "admin"}])))
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["traces", "get", TRAVERSAL], &[env]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout={} stderr={stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
    assert_eq!(error["error"]["kind"], "config");
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains(TRAVERSAL), "{message}");
    let requests: Vec<String> = server
        .received_requests()
        .await
        .expect("request recording")
        .iter()
        .map(|request| request.url.to_string())
        .collect();
    assert!(
        requests.is_empty(),
        "requests reached Grafana: {requests:?}"
    );
    write_transcript(
        "s7_traces_get_path_traversal.txt",
        "gcli traces get '../../../../../../../api/admin/users'",
        &out,
    );
}

// `traces tags` flattens every scope to TraceQL names, sorted and
// de-duplicated, and sends no `scope` parameter when the flag is absent.
#[tokio::test]
async fn s31_traces_tags_flatten_sort_and_dedup() {
    let (start, end) = (epoch_secs(FROM).to_string(), epoch_secs(TO).to_string());

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/tempo1/api/v2/search/tags"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .and(query_param_is_missing("scope"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tags_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["traces", "tags", "--from", FROM, "--to", TO], &[env]);

    let stdout = assert_success(&out);
    let names: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(names, expected_tag_names(), "{stdout}");
    server.verify().await;
    write_transcript(
        "s31_traces_tags.txt",
        "gcli traces tags --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

// `--scope` rides along as the `scope` query parameter and the reply's scopes
// (here just one) still flatten with their own prefix.
#[tokio::test]
async fn s31_traces_tags_scope_filters_the_wire_parameter() {
    let (start, end) = (epoch_secs(FROM).to_string(), epoch_secs(TO).to_string());

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/tempo1/api/v2/search/tags"))
        .and(query_param("scope", "resource"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "scopes": [{"name": "resource", "tags": ["service.name", "deployment.environment"]}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "traces", "tags", "--scope", "resource", "--from", FROM, "--to", TO,
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    let names: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        names,
        json!(["resource.deployment.environment", "resource.service.name"]),
        "{stdout}"
    );
    server.verify().await;
    write_transcript(
        "s31_traces_tags_scope.txt",
        "gcli traces tags --scope resource --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

// Tag names are discovery output for building TraceQL queries, not rows to
// scan: like logs labels, the JSON array is the output in every mode.
#[tokio::test]
async fn s31_traces_tags_stay_json_in_table_mode() {
    let (start, end) = (epoch_secs(FROM).to_string(), epoch_secs(TO).to_string());

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/tempo1/api/v2/search/tags"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(tags_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "traces", "tags", "--from", FROM, "--to", TO, "--output", "table",
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    let names: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(names, expected_tag_names(), "{stdout}");
    server.verify().await;
    write_transcript(
        "s31_traces_tags_table_mode.txt",
        "gcli traces tags --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --output table",
        &out,
    );
}

// Tag values keep their Tempo type beside the value: TraceQL quotes strings
// but not ints or durations, so the pair is the output. Sorted by type, then
// value, for stable output whatever the upstream order.
#[tokio::test]
async fn s31_traces_tag_values_typed_json_sorted() {
    let (start, end) = (epoch_secs(FROM).to_string(), epoch_secs(TO).to_string());

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/tempo1/api/v2/search/tag/resource.service.name/values",
        ))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(tag_values_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "traces",
            "tag-values",
            "resource.service.name",
            "--from",
            FROM,
            "--to",
            TO,
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    let values: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(values, expected_tag_values(), "{stdout}");
    server.verify().await;
    write_transcript(
        "s31_traces_tag_values.txt",
        "gcli traces tag-values resource.service.name --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

#[tokio::test]
async fn s31_traces_tag_values_table_columns_type_and_value() {
    let (start, end) = (epoch_secs(FROM).to_string(), epoch_secs(TO).to_string());

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/tempo1/api/v2/search/tag/resource.service.name/values",
        ))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(tag_values_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "traces",
            "tag-values",
            "resource.service.name",
            "--from",
            FROM,
            "--to",
            TO,
            "--output",
            "table",
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    // comfy-table's first line is the top border; the header text is second.
    let header = stdout.lines().nth(1).unwrap_or_default();
    for header_cell in ["type", "value"] {
        assert!(
            header.contains(header_cell),
            "{header_cell} missing: {stdout}"
        );
    }
    for cell in ["string", "int", "keyword", "checkout", "200", "cart", "api"] {
        assert!(stdout.contains(cell), "{cell} missing: {stdout}");
    }
    assert!(!stdout.trim_start().starts_with('['), "{stdout}");
    server.verify().await;
    write_transcript(
        "s31_traces_tag_values_table.txt",
        "gcli traces tag-values resource.service.name --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --output table",
        &out,
    );
}

// The tag becomes a path segment and reqwest resolves `..`, so an unfit tag
// is a config error naming it and Grafana receives no request at all.
#[tokio::test]
async fn s31_traces_tag_values_rejects_unfit_tags() {
    let server = MockServer::start().await;
    ds_mock().mount(&server).await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    for tag in ["..", ".", "", "bad tag", "tag/../other", "t\u{e4}g", "tag?"] {
        let out = spawn_gcli(&["traces", "tag-values", tag], &[env]);

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "tag '{tag}': stdout={} stderr={stderr}",
            String::from_utf8_lossy(&out.stdout)
        );
        let error: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
        assert_eq!(error["error"]["kind"], "config", "tag '{tag}': {stderr}");
        let message = error["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains(tag), "tag '{tag}': {message}");
    }
    let requests: Vec<String> = server
        .received_requests()
        .await
        .expect("request recording")
        .iter()
        .map(|request| request.url.to_string())
        .collect();
    assert!(
        requests.is_empty(),
        "requests reached Grafana: {requests:?}"
    );
    write_transcript(
        "s31_traces_tag_values_invalid.txt",
        "gcli traces tag-values ..",
        &spawn_gcli(&["traces", "tag-values", ".."], &[env]),
    );
}

// The span table renders OTLP status codes: error/ok between name and
// duration, unset and missing status as an empty cell.
#[tokio::test]
async fn s31_traces_get_table_renders_span_status() {
    let server = MockServer::start().await;
    ds_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/tempo1/api/traces/abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"batches": [
            {"resource": {"attributes": [
                {"key": "service.name", "value": {"stringValue": "checkout"}}
            ]},
             "scopeSpans": [{"spans": [
                {"spanId": "s1", "name": "GET /cart",
                 "startTimeUnixNano": "1759372800000000000",
                 "endTimeUnixNano": "1759372800001500000",
                 "status": {"code": "STATUS_CODE_ERROR"}},
                {"spanId": "s2", "name": "POST /pay",
                 "startTimeUnixNano": "1759372800002000000",
                 "endTimeUnixNano": "1759372800005000000",
                 "status": {"code": "STATUS_CODE_OK"}}
             ]}]},
            {"resource": {"attributes": [
                {"key": "service.name", "value": {"stringValue": "cart"}}
            ]},
             "scopeSpans": [{"spans": [
                {"spanId": "s3", "name": "SELECT items",
                 "startTimeUnixNano": "1759372800000500000",
                 "endTimeUnixNano": "1759372800003000000",
                 "status": {"code": "STATUS_CODE_UNSET"}},
                {"spanId": "s4", "name": "cache.get",
                 "startTimeUnixNano": "1759372800003500000",
                 "endTimeUnixNano": "1759372800004000000"}
             ]}]}
        ]})))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["traces", "get", "abc123", "--output", "table"], &[env]);

    let stdout = assert_success(&out);
    // comfy-table's first line is the top border; the header text is second.
    let header = stdout.lines().nth(1).unwrap_or_default();
    let position = |needle: &str| header.find(needle).unwrap_or(usize::MAX);
    assert!(
        position("name") < position("status") && position("status") < position("durationMillis"),
        "status column not between name and durationMillis: {header}"
    );
    assert!(stdout.contains("error"), "{stdout}");
    let pay_row = stdout
        .lines()
        .find(|line| line.contains("POST /pay"))
        .unwrap_or_default();
    assert!(pay_row.contains("ok"), "{stdout}");
    server.verify().await;
    write_transcript(
        "s31_traces_get_table_status.txt",
        "gcli traces get abc123 --output table",
        &out,
    );
}
