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

fn config(base: &str) -> tempfile::TempDir {
    fixture_config(&format!(
        "current_profile = \"p\"\n\
         [profiles.p]\n\
         url = \"{base}\"\n\
         token = \"t\"\n"
    ))
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

    let cfg = config(&server.uri());
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

    let cfg = config(&server.uri());
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
        // 1h window -> ceil(3600/1000) = 4 (default_step_secs)
        .and(query_param("step", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(matrix_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = config(&server.uri());
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

    let cfg = config(&server.uri());
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

    let cfg = config(&server.uri());
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

    let cfg = config(&server.uri());
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

    let cfg = config(&server.uri());
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

// expected RFC3339 rendering used across wire commands (logs consistency)
#[test]
fn epoch_formatting_contract() {
    let dt = chrono::DateTime::parse_from_rfc3339("2026-08-28T15:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let secs = gcli_lib::timeutil::to_epoch_secs(dt);
    assert_eq!(format!("{secs}"), epoch_secs("2026-08-28T15:00:00Z"));
    assert!(!format!("{secs}").contains('.'));
    assert_eq!(
        dt.to_rfc3339_opts(SecondsFormat::Millis, true),
        "2026-08-28T15:00:00.000Z"
    );
}
