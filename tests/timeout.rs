mod common;

use common::*;
use serde_json::Value;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// --timeout bounds the request: a server that answers long after the budget
// fails as kind io with a message naming the timeout, instead of hanging
// until the server answers. Nothing is asserted about elapsed wall-clock
// time; only the outcome of giving up early is pinned.
#[tokio::test]
async fn s22_timeout_fails_a_slow_server_as_io() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"version": "11.4.0"}))
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["--timeout", "200ms", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(
        out.stdout.is_empty(),
        "stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
    assert_eq!(err["error"]["kind"], "io");
    assert_eq!(err["error"]["profile"], "p");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("timed out"),
        "message must name the timeout: {stderr}"
    );
    write_transcript(
        "s22_timeout_slow_server.txt",
        "gcli --timeout 200ms health (server answers after 5s)",
        &out,
    );
}

// An unparsable --timeout is bad input: one config error, exit 2, and the
// server is never contacted.
#[tokio::test]
async fn s23_invalid_timeout_is_a_config_error() {
    let server = MockServer::start().await;
    health_mock().expect(0).mount(&server).await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["--timeout", "nonsense", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr={stderr}");
    let err: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
    assert_eq!(err["error"]["kind"], "config");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("nonsense"),
        "{stderr}"
    );
    server.verify().await;
    write_transcript(
        "s23_invalid_timeout.txt",
        "gcli --timeout nonsense health",
        &out,
    );
}

// The same --timeout bounds secret helper commands: a token_command that
// never finishes is killed and reported as an auth error naming the command
// and the timeout, without waiting for the command to end on its own.
#[tokio::test]
async fn s25_timeout_bounds_secret_helper_commands() {
    let server = MockServer::start().await;
    health_mock().expect(0).mount(&server).await;
    let config = fixture_config(&format!(
        "current_profile = \"p\"\n\
         [profiles.p]\n\
         url = \"{}\"\n\
         token_command = [\"sleep\", \"5\"]\n",
        server.uri()
    ));

    let out = spawn_gcli(
        &["--timeout", "300ms", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr={stderr}");
    let err: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
    assert_eq!(err["error"]["kind"], "auth");
    let message = err["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("sleep 5"), "{stderr}");
    assert!(message.contains("timed out"), "{stderr}");
    server.verify().await;
    write_transcript(
        "s25_timeout_helper_command.txt",
        "gcli --timeout 300ms health (token_command = [\"sleep\", \"5\"])",
        &out,
    );
}
