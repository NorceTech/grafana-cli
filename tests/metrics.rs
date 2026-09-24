mod common;

use chrono::SecondsFormat;
use common::*;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FROM: &str = "2026-08-28T14:00:00Z";
const TO: &str = "2026-08-28T15:00:00Z";

/// Self-derive the exact epoch-seconds wire string the CLI must send.
fn epoch_secs(rfc3339: &str) -> String {
    let dt = chrono::DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .with_timezone(&chrono::Utc);
    format!("{}", gcli_lib::timeutil::to_epoch_secs(dt))
}

fn ds_mock() -> Mock {
    // prom1 is the only prometheus ds (and default) despite a loki sibling.
    datasources_json(json!([
        {"id": 1, "uid": "prom1", "name": "Prom", "type": "prometheus",
         "url": "/prom", "isDefault": true},
        {"id": 2, "uid": "loki1", "name": "Loki", "type": "loki",
         "url": "/loki", "isDefault": false}
    ]))
}

fn vector_body() -> Value {
    json!({
        "status": "success",
        "data": {"resultType": "vector", "result": [
            {"metric": {"__name__": "up", "job": "node"}, "value": [1759372800, "1"]}
        ]}
    })
}

fn matrix_body() -> Value {
    // zeta listed first on purpose: output must order series by label string.
    // zeta samples deliberately unsorted: output must sort by ts.
    json!({
        "status": "success",
        "data": {"resultType": "matrix", "result": [
            {"metric": {"job": "zeta"}, "values": [[1759372800, "5"], [1759372700, "4"]]},
            {"metric": {"job": "api"}, "values": [[1759372700, "1"], [1759372800, "2"]]}
        ]}
    })
}

fn sorted_records() -> Value {
    json!([
        {"metric": {"job": "api"}, "value": "1", "ts": 1759372700},
        {"metric": {"job": "api"}, "value": "2", "ts": 1759372800},
        {"metric": {"job": "zeta"}, "value": "4", "ts": 1759372700},
        {"metric": {"job": "zeta"}, "value": "5", "ts": 1759372800}
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
async fn s6_metrics_instant() {
    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/query"))
        .and(query_param("query", "up"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["metrics", "instant", "up"], &[env]);

    let stdout = assert_success(&out);
    let records: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        records,
        json!([{"metric": {"__name__": "up", "job": "node"}, "value": "1", "ts": 1759372800}]),
        "{stdout}"
    );

    // no --at: request must carry query= but NO time= param
    for r in server.received_requests().await.unwrap() {
        if r.url.path().ends_with("/api/v1/query") {
            let q = r.url.query().unwrap_or("");
            assert!(q.contains("query=up"), "missing query param: {q}");
            assert!(!q.contains("time="), "unexpected time param: {q}");
        }
    }
    server.verify().await;
    write_transcript("s6_metrics_instant.txt", "gcli metrics instant up", &out);
}

#[tokio::test]
async fn s6_metrics_instant_at() {
    let at = "2026-08-28T15:00:00Z";
    let expected_time = epoch_secs(at);

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/query"))
        .and(query_param("query", "up"))
        .and(query_param("time", expected_time.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["metrics", "instant", "up", "--at", at], &[env]);

    let stdout = assert_success(&out);
    let records: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(records[0]["value"], "1", "{stdout}");
    server.verify().await;
    write_transcript(
        "s6_metrics_instant_at.txt",
        "gcli metrics instant up --at 2026-08-28T15:00:00Z",
        &out,
    );
}

#[tokio::test]
async fn s6_metrics_range() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/query_range"))
        .and(query_param("query", "up"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        // 1h window -> max(15, ceil(3600/1000)) = 15 (default_step_secs)
        .and(query_param("step", "15"))
        .respond_with(ResponseTemplate::new(200).set_body_json(matrix_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &["metrics", "range", "up", "--from", FROM, "--to", TO],
        &[env],
    );

    let stdout = assert_success(&out);
    let records: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(records, sorted_records(), "{stdout}");
    server.verify().await;
    write_transcript(
        "s6_metrics_range.txt",
        "gcli metrics range up --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

#[tokio::test]
async fn s6_metrics_range_step() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/query_range"))
        .and(query_param("query", "up"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .and(query_param("step", "30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(matrix_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics", "range", "up", "--from", FROM, "--to", TO, "--step", "30s",
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    let records: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(records, sorted_records(), "{stdout}");
    server.verify().await;
    write_transcript(
        "s6_metrics_range_step.txt",
        "gcli metrics range up --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --step 30s",
        &out,
    );
}

#[tokio::test]
async fn s6_metrics_range_empty_result_is_exit_0() {
    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/query_range"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": {"resultType": "matrix", "result": []}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &["metrics", "range", "up", "--from", FROM, "--to", TO],
        &[env],
    );

    let stdout = assert_success(&out);
    assert_eq!(stdout.trim(), "[]", "{stdout}");
    server.verify().await;
}

// An explicit --datasource of another core type (Loki for a metrics query) is
// a config error naming both types, and nothing is proxied.
#[tokio::test]
async fn s6_metrics_other_core_type_datasource_is_a_config_error() {
    let server = MockServer::start().await;
    ds_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path_regex("^/api/datasources/proxy/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_body()))
        .expect(0)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    // loki1 is the uid of the Loki datasource in ds_mock.
    let out = spawn_gcli(
        &["metrics", "instant", "up", "--datasource", "loki1"],
        &[env],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr={stderr}");
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err: Value = serde_json::from_str(stderr.trim()).expect("one stderr JSON line");
    assert_eq!(err["error"]["kind"], "config");
    let message = err["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("loki") && message.contains("prometheus"),
        "{message}"
    );
    server.verify().await;
    write_transcript(
        "s6_metrics_other_core_type_datasource.txt",
        "gcli metrics instant up --datasource loki1",
        &out,
    );
}

// An explicit --datasource of a plugin type (VictoriaMetrics and other
// Prometheus-compatible plugins have their own) is queried anyway, with one
// note on stderr saying so.
#[tokio::test]
async fn s6_metrics_plugin_datasource_is_queried_with_a_note() {
    let server = MockServer::start().await;
    datasources_json(json!([
        {"id": 1, "uid": "prom1", "name": "Prom", "type": "prometheus",
         "url": "/prom", "isDefault": true},
        {"id": 2, "uid": "vm1", "name": "VM", "type": "victoriametrics-metrics-datasource",
         "url": "/vm", "isDefault": false}
    ]))
    .mount(&server)
    .await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/vm1/api/v1/query"))
        .and(query_param("query", "up"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vector_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["metrics", "instant", "up", "--datasource", "VM"], &[env]);

    let stdout = assert_success(&out);
    let records: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(records[0]["value"], "1", "{stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "one note: {stderr}");
    assert!(lines[0].starts_with("note: "), "{stderr}");
    for fact in ["'VM'", "victoriametrics-metrics-datasource", "prometheus"] {
        assert!(lines[0].contains(fact), "{fact} missing: {stderr}");
    }
    server.verify().await;
    write_transcript(
        "s6_metrics_plugin_datasource.txt",
        "gcli metrics instant up --datasource VM",
        &out,
    );
}

// A run rejected before any request: one config error naming `value`.
fn assert_config_error_naming(out: &std::process::Output, value: &str) {
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
    assert!(message.contains(value), "{message}");
}

async fn assert_no_requests(server: &MockServer) {
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
}

#[tokio::test]
async fn s31a_metrics_labels_sends_repeated_match() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/labels"))
        .and(query_param("match[]", "up"))
        .and(query_param("match[]", "{job=\"api\"}"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"status": "success", "data": ["__name__", "instance", "job"]}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics",
            "labels",
            "--match",
            "up",
            "--match",
            "{job=\"api\"}",
            "--from",
            FROM,
            "--to",
            TO,
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    assert_eq!(
        stdout.trim(),
        "[\"__name__\",\"instance\",\"job\"]",
        "{stdout}"
    );
    server.verify().await;
    write_transcript(
        "s31a_metrics_labels.txt",
        "gcli metrics labels --match up --match '{job=\"api\"}' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

// No --match: the request carries only start/end, and --output table is
// ignored — labels always render as a JSON array, like logs labels.
#[tokio::test]
async fn s31b_metrics_labels_without_match_is_always_json() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/labels"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "success", "data": ["__name__", "job"]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics", "labels", "--from", FROM, "--to", TO, "--output", "table",
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    assert_eq!(stdout.trim(), "[\"__name__\",\"job\"]", "{stdout}");
    for r in server.received_requests().await.unwrap() {
        if r.url.path().ends_with("/api/v1/labels") {
            let q = r.url.query().unwrap_or("");
            assert!(!q.contains("match"), "unexpected match param: {q}");
        }
    }
    server.verify().await;
    write_transcript(
        "s31b_metrics_labels_without_match.txt",
        "gcli metrics labels --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --output table",
        &out,
    );
}

// __name__ is a valid label name: label-values on it lists metric names.
#[tokio::test]
async fn s31c_metrics_label_values() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/prom1/api/v1/label/__name__/values",
        ))
        .and(query_param("match[]", "up"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"status": "success", "data": ["http_request_duration_seconds_bucket", "up"]}),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics",
            "label-values",
            "__name__",
            "--match",
            "up",
            "--from",
            FROM,
            "--to",
            TO,
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    assert_eq!(
        stdout.trim(),
        "[\"http_request_duration_seconds_bucket\",\"up\"]",
        "{stdout}"
    );
    server.verify().await;
    write_transcript(
        "s31c_metrics_label_values.txt",
        "gcli metrics label-values __name__ --match up --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

// The label becomes a path segment of api/v1/label/{label}/values; only
// Prometheus label names reach the path: anything else is one config error
// and zero requests, including the datasource listing.
#[tokio::test]
async fn s31d_metrics_label_values_rejects_a_path_traversing_name() {
    const TRAVERSAL: &str = "../../../../../../../../../api/admin/users?";
    let server = MockServer::start().await;
    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());

    let out = spawn_gcli(
        &[
            "metrics",
            "label-values",
            TRAVERSAL,
            "--from",
            FROM,
            "--to",
            TO,
        ],
        &[env],
    );

    assert_config_error_naming(&out, TRAVERSAL);
    assert_no_requests(&server).await;
    write_transcript(
        "s31d_metrics_label_values_path_traversal.txt",
        "gcli metrics label-values '../../../../../../../../../api/admin/users?' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

fn series_body() -> Value {
    // node listed before api on purpose: output must order series by their
    // label string.
    json!({
        "status": "success",
        "data": [
            {"__name__": "up", "job": "node", "instance": "node-1:9100"},
            {"__name__": "up", "job": "api", "instance": "api-1:8080"}
        ]
    })
}

#[tokio::test]
async fn s31e_metrics_series() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/series"))
        .and(query_param("match[]", "up"))
        .and(query_param("match[]", "{job=\"api\"}"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(series_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics",
            "series",
            "--match",
            "up",
            "--match",
            "{job=\"api\"}",
            "--from",
            FROM,
            "--to",
            TO,
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    let series: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        series,
        json!([
            {"__name__": "up", "job": "api", "instance": "api-1:8080"},
            {"__name__": "up", "job": "node", "instance": "node-1:9100"}
        ]),
        "{stdout}"
    );
    server.verify().await;
    write_transcript(
        "s31e_metrics_series.txt",
        "gcli metrics series --match up --match '{job=\"api\"}' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

// Table mode: one row per series, a single "series" column holding the
// {k="v", ...} label string.
#[tokio::test]
async fn s31f_metrics_series_table() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/series"))
        .and(query_param("match[]", "up"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(series_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics", "series", "--match", "up", "--from", FROM, "--to", TO, "--output", "table",
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    assert!(stdout.contains("series"), "{stdout}");
    assert!(
        stdout.contains("{__name__=\"up\", instance=\"api-1:8080\", job=\"api\"}"),
        "{stdout}"
    );
    assert!(
        stdout.contains("{__name__=\"up\", instance=\"node-1:9100\", job=\"node\"}"),
        "{stdout}"
    );
    server.verify().await;
    write_transcript(
        "s31f_metrics_series_table.txt",
        "gcli metrics series --match up --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --output table",
        &out,
    );
}

// clap enforces at least one --match: a usage error, exit 2, no request.
#[tokio::test]
async fn s31g_metrics_series_requires_a_match() {
    let server = MockServer::start().await;
    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());

    let out = spawn_gcli(&["metrics", "series", "--from", FROM, "--to", TO], &[env]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr={stderr}");
    assert!(stderr.contains("--match"), "{stderr}");
    assert_no_requests(&server).await;
}

fn exemplars_body() -> Value {
    // cart listed before api and each group's exemplars unsorted on purpose:
    // output must order by (series label string, ts).
    // t-api-legacy exercises the traceID spelling of the trace label.
    json!({
        "status": "success",
        "data": [
            {"seriesLabels": {"__name__": "http_request_duration_seconds_bucket", "service": "cart"},
             "exemplars": [
                 {"labels": {"trace_id": "t-cart-2"}, "value": "0.42", "timestamp": 1759372860.5},
                 {"labels": {"trace_id": "t-cart-1"}, "value": "0.11", "timestamp": 1759372800.25}
             ]},
            {"seriesLabels": {"__name__": "http_request_duration_seconds_bucket", "service": "api"},
             "exemplars": [
                 {"labels": {"trace_id": "t-api-1"}, "value": "0.05", "timestamp": 1759372900.0},
                 {"labels": {"traceID": "t-api-legacy"}, "value": "0.07", "timestamp": 1759372700.0}
             ]}
        ]
    })
}

fn exemplar_metric(service: &str) -> Value {
    json!({"__name__": "http_request_duration_seconds_bucket", "service": service})
}

#[tokio::test]
async fn s31h_metrics_exemplars() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/prom1/api/v1/query_exemplars",
        ))
        .and(query_param("query", "traces_spanmetrics_latency_bucket"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(exemplars_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics",
            "exemplars",
            "traces_spanmetrics_latency_bucket",
            "--from",
            FROM,
            "--to",
            TO,
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    let records: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        records,
        json!([
            {"metric": exemplar_metric("api"), "labels": {"traceID": "t-api-legacy"},
             "value": "0.07", "ts": 1759372700.0},
            {"metric": exemplar_metric("api"), "labels": {"trace_id": "t-api-1"},
             "value": "0.05", "ts": 1759372900.0},
            {"metric": exemplar_metric("cart"), "labels": {"trace_id": "t-cart-1"},
             "value": "0.11", "ts": 1759372800.25},
            {"metric": exemplar_metric("cart"), "labels": {"trace_id": "t-cart-2"},
             "value": "0.42", "ts": 1759372860.5}
        ]),
        "{stdout}"
    );
    server.verify().await;
    write_transcript(
        "s31h_metrics_exemplars.txt",
        "gcli metrics exemplars traces_spanmetrics_latency_bucket --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

// Table mode: columns metric, traceID (labels.trace_id, else labels.traceID,
// else empty), value, ts.
#[tokio::test]
async fn s31i_metrics_exemplars_table() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/prom1/api/v1/query_exemplars",
        ))
        .and(query_param("query", "traces_spanmetrics_latency_bucket"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(exemplars_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics",
            "exemplars",
            "traces_spanmetrics_latency_bucket",
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
    for fragment in [
        "traceID",
        "t-api-legacy",
        "t-cart-2",
        "0.42",
        "1759372860.5",
        "{__name__=\"http_request_duration_seconds_bucket\", service=\"api\"}",
    ] {
        assert!(stdout.contains(fragment), "{fragment} missing: {stdout}");
    }
    server.verify().await;
    write_transcript(
        "s31i_metrics_exemplars_table.txt",
        "gcli metrics exemplars traces_spanmetrics_latency_bucket --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --output table",
        &out,
    );
}

// The production capture answers an empty exemplars window with data: [].
#[tokio::test]
async fn s31j_metrics_exemplars_empty_data_is_an_empty_array() {
    let (start, end) = (epoch_secs(FROM), epoch_secs(TO));

    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/prom1/api/v1/query_exemplars",
        ))
        .and(query_param("query", "traces_spanmetrics_latency_bucket"))
        .and(query_param("start", start.as_str()))
        .and(query_param("end", end.as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"status": "success", "data": []})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(
        &[
            "metrics",
            "exemplars",
            "traces_spanmetrics_latency_bucket",
            "--from",
            FROM,
            "--to",
            TO,
        ],
        &[env],
    );

    let stdout = assert_success(&out);
    assert_eq!(stdout.trim(), "[]", "{stdout}");
    server.verify().await;
}

// A "status": "error" envelope is the upstream's own error: kind http with
// the upstream text, exactly like metrics instant/range.
#[tokio::test]
async fn s31k_metrics_labels_upstream_error_envelope() {
    let server = MockServer::start().await;
    ds_mock().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/prom1/api/v1/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error", "errorType": "bad_data", "error": "parse error"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = token_config(&server.uri());
    let env = ("GCLI_CONFIG_DIR", cfg.path().to_str().unwrap());
    let out = spawn_gcli(&["metrics", "labels", "--from", FROM, "--to", TO], &[env]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    let error: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
    assert_eq!(error["error"]["kind"], "http");
    assert_eq!(error["error"]["status"], 200);
    assert_eq!(error["error"]["message"], "parse error");
    server.verify().await;
    write_transcript(
        "s31k_metrics_labels_upstream_error.txt",
        "gcli metrics labels --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

// expected RFC3339 rendering used across wire commands (logs consistency)
#[test]
fn epoch_formatting_contract() {
    let dt = chrono::DateTime::parse_from_rfc3339("2026-08-28T15:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let secs = gcli_lib::timeutil::to_epoch_secs(dt);
    // literal pin: deriving it with to_epoch_secs again would compare the
    // function with itself and pass even if the encoder drifted
    assert_eq!(format!("{secs}"), "1787929200");
    assert!(!format!("{secs}").contains('.'));
    assert_eq!(
        dt.to_rfc3339_opts(SecondsFormat::Millis, true),
        "2026-08-28T15:00:00.000Z"
    );
}
