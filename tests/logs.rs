mod common;

use common::*;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FROM: &str = "2026-08-28T14:00:00Z";
const TO: &str = "2026-08-28T15:00:00Z";

fn window_ns(from: &str, to: &str) -> (String, String) {
    // Self-derive expected nanosecond bounds with the same timeutil the CLI uses,
    // so the test never trusts hand arithmetic (task spec: derive, don't trust).
    let (start, end) =
        gcli_lib::timeutil::resolve_window(None, Some(from), Some(to)).expect("window");
    (
        gcli_lib::timeutil::to_ns_string(start),
        gcli_lib::timeutil::to_ns_string(end),
    )
}

fn ts_ns(rfc3339: &str) -> String {
    let dt = chrono::DateTime::parse_from_rfc3339(rfc3339).expect("fixture ts");
    gcli_lib::timeutil::to_ns_string(dt.with_timezone(&chrono::Utc))
}

fn token_config(base: &str) -> tempfile::TempDir {
    fixture_config(&format!(
        "current_profile = \"p\"\n[profiles.p]\nurl = \"{base}\"\ntoken = \"t\"\n"
    ))
}

fn loki_datasources_mock() -> Mock {
    datasources_json(json!([{
        "id": 1, "uid": "loki1", "name": "Loki", "type": "loki",
        "url": "/loki", "isDefault": true
    }]))
}

// 2 streams x 2 lines, timestamps interleaved OUT of order across streams.
fn streams_body() -> Value {
    json!({
        "status": "success",
        "data": {
            "resultType": "streams",
            "result": [
                {"stream": {"job": "api"},
                 "values": [[ts_ns("2026-08-28T14:59:00Z"), "api boom"],
                            [ts_ns("2026-08-28T14:05:00Z"), "api two"]]},
                {"stream": {"job": "web"},
                 "values": [[ts_ns("2026-08-28T14:20:30.250Z"), "web three"],
                            [ts_ns("2026-08-28T14:00:10Z"), "web one"]]}
            ]
        }
    })
}

fn run_logs(config: &tempfile::TempDir, args: &[&str]) -> std::process::Output {
    spawn_gcli(
        args,
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    )
}

fn stdout_lines(out: &std::process::Output) -> Vec<Value> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("stdout line is JSON"))
        .collect()
}

fn assert_search_records(records: &[Value]) {
    assert_eq!(records.len(), 4, "{records:?}");
    let expected = [
        ("2026-08-28T14:00:10.000Z", "web", "web one"),
        ("2026-08-28T14:05:00.000Z", "api", "api two"),
        ("2026-08-28T14:20:30.250Z", "web", "web three"),
        ("2026-08-28T14:59:00.000Z", "api", "api boom"),
    ];
    for (i, (ts, job, line)) in expected.iter().enumerate() {
        assert_eq!(records[i]["ts"], *ts, "record {i} ts");
        assert_eq!(records[i]["labels"]["job"], *job, "record {i} job");
        assert_eq!(records[i]["line"], *line, "record {i} line");
        assert_eq!(records[i]["profile"], "p", "record {i} profile");
        assert_eq!(records[i]["datasource"], "Loki", "record {i} datasource");
    }
}

#[tokio::test]
async fn s4a_logs_search_jsonl_defaults() {
    let (start_ns, end_ns) = window_ns(FROM, TO);
    // Exact-literal pin (self-derived above; chrono is the source of truth).
    assert_eq!(start_ns, "1787925600000000000");
    assert_eq!(end_ns, "1787929200000000000");

    let server = MockServer::start().await;
    loki_datasources_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki1/loki/api/v1/query_range",
        ))
        .and(query_param("query", "{job=~\"api|web\"}"))
        .and(query_param("start", start_ns.as_str()))
        .and(query_param("end", end_ns.as_str()))
        .and(query_param("limit", "100"))
        .and(query_param("direction", "backward"))
        .respond_with(ResponseTemplate::new(200).set_body_json(streams_body()))
        .expect(1)
        .mount(&server)
        .await;

    let config = token_config(&server.uri());
    let out = run_logs(
        &config,
        &[
            "logs",
            "search",
            "{job=~\"api|web\"}",
            "--from",
            FROM,
            "--to",
            TO,
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let records = stdout_lines(&out);
    assert_search_records(&records);
    server.verify().await;
    write_transcript(
        "s4_logs_search.txt",
        "gcli logs search '{job=~\"api|web\"}' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

#[tokio::test]
async fn s4a_logs_search_limit_and_direction_forward() {
    let (start_ns, end_ns) = window_ns(FROM, TO);
    let server = MockServer::start().await;
    loki_datasources_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki1/loki/api/v1/query_range",
        ))
        .and(query_param("query", "{job=\"api\"}"))
        .and(query_param("start", start_ns.as_str()))
        .and(query_param("end", end_ns.as_str()))
        .and(query_param("limit", "5"))
        .and(query_param("direction", "forward"))
        .respond_with(ResponseTemplate::new(200).set_body_json(streams_body()))
        .expect(1)
        .mount(&server)
        .await;

    let config = token_config(&server.uri());
    let out = run_logs(
        &config,
        &[
            "logs",
            "search",
            "{job=\"api\"}",
            "--from",
            FROM,
            "--to",
            TO,
            "--limit",
            "5",
            "--direction",
            "forward",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    server.verify().await;
}

// Table mode: logs search renders a Table{headers:["ts","line","labels"]}; the
// "ts" header and a line fragment must appear in the rendered table.
#[tokio::test]
async fn s4b_logs_search_table() {
    let (start_ns, end_ns) = window_ns(FROM, TO);
    let server = MockServer::start().await;
    loki_datasources_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki1/loki/api/v1/query_range",
        ))
        .and(query_param("query", "{job=~\"api|web\"}"))
        .and(query_param("start", start_ns.as_str()))
        .and(query_param("end", end_ns.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(streams_body()))
        .expect(1)
        .mount(&server)
        .await;

    let config = token_config(&server.uri());
    let out = run_logs(
        &config,
        &[
            "logs",
            "search",
            "{job=~\"api|web\"}",
            "--from",
            FROM,
            "--to",
            TO,
            "--output",
            "table",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ts"), "{stdout}");
    assert!(stdout.contains("web one"), "{stdout}");
    server.verify().await;
    write_transcript(
        "s4_logs_search_table.txt",
        "gcli logs search '{job=~\"api|web\"}' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --output table",
        &out,
    );
}

#[tokio::test]
async fn s4c_logs_search_empty_result() {
    let (start_ns, end_ns) = window_ns(FROM, TO);
    let server = MockServer::start().await;
    loki_datasources_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki1/loki/api/v1/query_range",
        ))
        .and(query_param("query", "{job=\"nope\"}"))
        .and(query_param("start", start_ns.as_str()))
        .and(query_param("end", end_ns.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": {"resultType": "streams", "result": []}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config = token_config(&server.uri());
    let out = run_logs(
        &config,
        &[
            "logs",
            "search",
            "{job=\"nope\"}",
            "--from",
            FROM,
            "--to",
            TO,
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "expected no stdout lines"
    );
    server.verify().await;
    write_transcript(
        "s4_logs_empty.txt",
        "gcli logs search '{job=\"nope\"}' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

#[tokio::test]
async fn s5_logs_labels() {
    let (start_ns, end_ns) = window_ns(FROM, TO);
    let server = MockServer::start().await;
    loki_datasources_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/datasources/proxy/uid/loki1/loki/api/v1/labels"))
        .and(query_param("start", start_ns.as_str()))
        .and(query_param("end", end_ns.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "success", "data": ["app", "level"]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let config = token_config(&server.uri());
    let out = run_logs(&config, &["logs", "labels", "--from", FROM, "--to", TO]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "[\"app\",\"level\"]"
    );
    server.verify().await;
    write_transcript(
        "s5_logs_labels.txt",
        "gcli logs labels --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

#[tokio::test]
async fn s5_logs_label_values() {
    let (start_ns, end_ns) = window_ns(FROM, TO);
    let server = MockServer::start().await;
    loki_datasources_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki1/loki/api/v1/label/app/values",
        ))
        .and(query_param("start", start_ns.as_str()))
        .and(query_param("end", end_ns.as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"status": "success", "data": ["web"]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let config = token_config(&server.uri());
    let out = run_logs(
        &config,
        &["logs", "label-values", "app", "--from", FROM, "--to", TO],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[\"web\"]");
    server.verify().await;
    write_transcript(
        "s5_logs_label_values.txt",
        "gcli logs label-values app --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

#[tokio::test]
async fn s4_logs_invalid_logql_surfaces_grafana_message() {
    let server = MockServer::start().await;
    loki_datasources_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki1/loki/api/v1/query_range",
        ))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "status": "error", "message": "parse error: bad LogQL"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config = token_config(&server.uri());
    let out = run_logs(
        &config,
        &["logs", "search", "{broken", "--from", FROM, "--to", TO],
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr: Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stderr)).expect("stderr JSON");
    assert_eq!(stderr["error"]["kind"], "http");
    assert_eq!(stderr["error"]["status"], 400);
    assert!(
        stderr["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("parse error"),
        "{stderr}"
    );
    server.verify().await;
    write_transcript(
        "s4_logs_invalid_logql.txt",
        "gcli logs search '{broken' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}

// Loki (like Tempo) answers a bad query in plain text, not JSON: that text is
// the error message instead of null.
#[tokio::test]
async fn s4_logs_plain_text_error_is_the_message() {
    let server = MockServer::start().await;
    loki_datasources_mock().expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/api/datasources/proxy/uid/loki1/loki/api/v1/query_range",
        ))
        .respond_with(
            ResponseTemplate::new(400).set_body_raw("parse error at line 1, col 5\n", "text/plain"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let config = token_config(&server.uri());
    let out = run_logs(
        &config,
        &["logs", "search", "{job=", "--from", FROM, "--to", TO],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    let error: Value = serde_json::from_str(&stderr).expect("stderr JSON");
    assert_eq!(error["error"]["kind"], "http");
    assert_eq!(error["error"]["status"], 400);
    assert_eq!(error["error"]["message"], "parse error at line 1, col 5");
    server.verify().await;
    write_transcript(
        "s4_logs_plain_text_error.txt",
        "gcli logs search '{job=' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z",
        &out,
    );
}
