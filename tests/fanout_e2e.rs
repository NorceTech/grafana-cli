mod common;

use common::*;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn two_profile_toml(url_a: &str, url_b: &str) -> String {
    format!(
        "[profiles.a]\n\
         url = \"{url_a}\"\n\
         token = \"t\"\n\
         \n\
         [profiles.b]\n\
         url = \"{url_b}\"\n\
         token = \"t\"\n"
    )
}

async fn health_500(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"message": "boom"})))
        .expect(1)
        .mount(server)
        .await;
}

fn stderr_lines(out: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("stderr line is JSON"))
        .collect()
}

#[tokio::test]
async fn s8a_fanout_two_success() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    health_mock().expect(1).mount(&m1).await;
    health_mock().expect(1).mount(&m2).await;
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));

    let out = spawn_gcli(
        &["--profile", "a,b", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = stdout_lines(&out);
    assert_eq!(lines.len(), 2, "expected 2 tagged lines: {lines:?}");
    assert_eq!(lines[0]["profile"], "a");
    assert_eq!(lines[0]["data"]["version"], "11.4.0");
    assert_eq!(lines[1]["profile"], "b");
    assert_eq!(lines[1]["data"]["version"], "11.4.0");
    m1.verify().await;
    m2.verify().await;
    write_transcript("s8_fanout_two.txt", "gcli --profile a,b health", &out);
}

#[tokio::test]
async fn s8b_fanout_partial_failure() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    health_mock().expect(1).mount(&m1).await;
    health_500(&m2).await;
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));

    let out = spawn_gcli(
        &["--profile", "a,b", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "partial failure must still exit 0"
    );
    let stdout = stdout_lines(&out);
    assert_eq!(stdout.len(), 1, "only profile a succeeds: {stdout:?}");
    assert_eq!(stdout[0]["profile"], "a");

    let stderr = stderr_lines(&out);
    assert_eq!(stderr.len(), 1, "one error line for b: {stderr:?}");
    assert_eq!(stderr[0]["error"]["profile"], "b");
    assert_eq!(stderr[0]["error"]["kind"], "http");
    assert_eq!(stderr[0]["error"]["status"], 500);
    m1.verify().await;
    m2.verify().await;
    write_transcript("s8_fanout_partial.txt", "gcli --profile a,b health", &out);
}

#[tokio::test]
async fn s8c_fanout_all_failed() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    health_500(&m1).await;
    health_500(&m2).await;
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));

    let out = spawn_gcli(
        &["--profile", "a,b", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(1), "all failed must exit 1");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "stdout must be empty when all fail"
    );
    let stderr = stderr_lines(&out);
    assert_eq!(stderr.len(), 2, "two error lines: {stderr:?}");
    m1.verify().await;
    m2.verify().await;
    write_transcript(
        "s8_fanout_all_failed.txt",
        "gcli --profile a,b health",
        &out,
    );
}

#[tokio::test]
async fn s8d_all_profiles_matches_csv() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    health_mock().expect(1).mount(&m1).await;
    health_mock().expect(1).mount(&m2).await;
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));

    let out = spawn_gcli(
        &["--all-profiles", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(0));
    let lines = stdout_lines(&out);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["profile"], "a");
    assert_eq!(lines[1]["profile"], "b");
    assert_eq!(lines[0]["data"]["version"], "11.4.0");
    assert_eq!(lines[1]["data"]["version"], "11.4.0");
    m1.verify().await;
    m2.verify().await;
    write_transcript("s8d_all_profiles.txt", "gcli --all-profiles health", &out);
}

// URL and credential overrides name one instance and one identity: with
// several profiles selected they are a config error naming the override,
// and no profile's instance is contacted.
async fn assert_override_rejected(
    args: &[&str],
    extra_env: &[(&str, &str)],
    named: &str,
) -> std::process::Output {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    for server in [&m1, &m2] {
        health_mock().expect(0).mount(server).await;
    }
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));
    let mut env = vec![("GCLI_CONFIG_DIR", config.path().to_str().unwrap())];
    env.extend_from_slice(extra_env);

    let out = spawn_gcli(args, &env);

    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = stderr_lines(&out);
    assert_eq!(stderr.len(), 1, "one error line: {stderr:?}");
    assert_eq!(stderr[0]["error"]["kind"], "config");
    let message = stderr[0]["error"]["message"].as_str().unwrap();
    assert!(message.contains(named), "{message}");
    m1.verify().await;
    m2.verify().await;
    out
}

#[tokio::test]
async fn s17a_csv_profiles_reject_a_flag_override() {
    let out = assert_override_rejected(
        &["--profile", "a,b", "--token", "x", "health"],
        &[],
        "--token",
    )
    .await;
    write_transcript(
        "s17a_csv_rejects_flag_override.txt",
        "gcli --profile a,b --token x health",
        &out,
    );
}

#[tokio::test]
async fn s17b_all_profiles_reject_an_env_override() {
    let out = assert_override_rejected(
        &["--all-profiles", "health"],
        &[("GRAFANA_CLI_URL", "http://127.0.0.1:1")],
        "GRAFANA_CLI_URL",
    )
    .await;
    write_transcript(
        "s17b_all_rejects_env_override.txt",
        "GRAFANA_CLI_URL=http://127.0.0.1:1 gcli --all-profiles health",
        &out,
    );
}

// A bad time window is the invocation's fault, not a profile's: it is one
// config error before fanout, exit 2, and no instance is contacted.
#[tokio::test]
async fn s8f_bad_window_fails_once_before_fanout() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    for server in [&m1, &m2] {
        datasources_json(json!([])).expect(0).mount(server).await;
    }
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));

    let out = spawn_gcli(
        &[
            "--profile",
            "a,b",
            "logs",
            "search",
            "{x=\"y\"}",
            "--since",
            "bogus",
        ],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );

    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = stderr_lines(&out);
    assert_eq!(stderr.len(), 1, "one error line: {stderr:?}");
    assert_eq!(stderr[0]["error"]["kind"], "config");
    m1.verify().await;
    m2.verify().await;
    write_transcript(
        "s8f_bad_window.txt",
        "gcli --profile a,b logs search '{x=\"y\"}' --since bogus",
        &out,
    );
}

// Table mode fans out as one block per profile, prefixed `profile: <name>`
// in selection order.
#[tokio::test]
async fn s19_fanout_table_prefixes_each_block_with_its_profile() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    health_mock().expect(1).mount(&m1).await;
    health_mock().expect(1).mount(&m2).await;
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));

    let out = spawn_gcli(
        &["--profile", "a,b", "health", "--output", "table"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (a, b) = (
        stdout.find("profile: a").unwrap_or_default(),
        stdout.find("profile: b").unwrap_or_default(),
    );
    assert!(a < b, "blocks out of order or missing: {stdout}");
    assert!(stdout.contains("version"), "health table body: {stdout}");
    assert!(!stdout.trim_start().starts_with('{'), "{stdout}");
    m1.verify().await;
    m2.verify().await;
    write_transcript(
        "s19_fanout_table.txt",
        "gcli --profile a,b health --output table",
        &out,
    );
}

// Log records fan out as plain JSONL tagged with their profile: no
// {"data":..,"profile":..} wrapper even with several profiles selected.
#[tokio::test]
async fn s20_fanout_logs_stay_plain_jsonl_tagged_with_their_profile() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    for server in [&m1, &m2] {
        loki_datasources_mock().expect(1).mount(server).await;
        Mock::given(method("GET"))
            .and(path(
                "/api/datasources/proxy/uid/loki1/loki/api/v1/query_range",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"resultType": "streams", "result": [
                    {"stream": {"job": "api"},
                     "values": [["1759372800000000000", "one line"]]}
                ]}
            })))
            .expect(1)
            .mount(server)
            .await;
    }
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));

    let out = spawn_gcli(
        &["--profile", "a,b", "logs", "search", "{job=\"api\"}"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let records = stdout_lines(&out);
    assert_eq!(records.len(), 2, "{records:?}");
    for (record, profile) in records.iter().zip(["a", "b"]) {
        assert_eq!(record["profile"], profile, "{record}");
        assert_eq!(record["line"], "one line", "{record}");
        assert_eq!(record["ts"], "2025-10-02T02:40:00.000Z", "{record}");
        assert!(record.get("data").is_none(), "wrapped record: {record}");
    }
    m1.verify().await;
    m2.verify().await;
    write_transcript(
        "s20_fanout_logs_jsonl.txt",
        "gcli --profile a,b logs search '{job=\"api\"}'",
        &out,
    );
}
