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
