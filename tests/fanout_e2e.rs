mod common;

use common::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PARALLELISM_BUDGET_MS: u128 = 450;

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
        .mount(server)
        .await;
}

fn stdout_lines(out: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("stdout line is JSON"))
        .collect()
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
    health_mock().mount(&m1).await;
    health_mock().mount(&m2).await;
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
    write_transcript("s8_fanout_two.txt", "gcli --profile a,b health", &out);
}

#[tokio::test]
async fn s8b_fanout_partial_failure() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    health_mock().mount(&m1).await;
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
    health_mock().mount(&m1).await;
    health_mock().mount(&m2).await;
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
}

#[tokio::test]
async fn s8e_fanout_runs_in_parallel() {
    let m1 = MockServer::start().await;
    let m2 = MockServer::start().await;
    let delayed = || {
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(250))
                    .set_body_json(json!({"version": "11.4.0"})),
            )
    };
    delayed().mount(&m1).await;
    delayed().mount(&m2).await;
    let config = fixture_config(&two_profile_toml(&m1.uri(), &m2.uri()));

    let start = Instant::now();
    let out = spawn_gcli(
        &["--profile", "a,b", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    let elapsed = start.elapsed().as_millis();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        elapsed < PARALLELISM_BUDGET_MS,
        "two 250ms profiles took {elapsed}ms; serial would exceed 500ms \
         (budget {PARALLELISM_BUDGET_MS}ms)"
    );
}
