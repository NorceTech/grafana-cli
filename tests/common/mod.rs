#![allow(dead_code)]

use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

pub fn spawn_gcli(args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gcli"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("GCLI_CONFIG_DIR")
        .env_remove("GCLI_CACHE_DIR")
        .env_remove("GRAFANA_CLI_PROFILE")
        .env_remove("GRAFANA_CLI_URL")
        .env_remove("GRAFANA_CLI_TOKEN")
        .env_remove("GRAFANA_CLI_USERNAME")
        .env_remove("GRAFANA_CLI_PASSWORD")
        .envs(envs.iter().copied())
        .output()
        .expect("failed to spawn gcli")
}

pub fn fixture_config(toml_body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("config.toml"), toml_body).expect("write config.toml");
    dir
}

pub fn health_mock() -> Mock {
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "commit": "c1", "database": "ok", "version": "11.4.0"
        })))
}

pub fn health_auth_mock(authorization: &str) -> Mock {
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .and(wiremock::matchers::header("Authorization", authorization))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "commit": "c1", "database": "ok", "version": "11.4.0"
        })))
}

pub fn token_endpoint_mock(access_token: &str, expires_in: i64) -> Mock {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": access_token, "expires_in": expires_in, "token_type": "Bearer"
        })))
}

pub fn datasources_json(body: serde_json::Value) -> Mock {
    Mock::given(method("GET"))
        .and(path("/api/datasources"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
}

pub fn write_transcript(name: &str, cmd: &str, output: &std::process::Output) {
    write_transcript_multi(name, &transcript_block(cmd, output));
}

pub fn transcript_block(cmd: &str, output: &std::process::Output) -> String {
    format!(
        "$ {cmd}\n--- stdout ---\n{}--- stderr ---\n{}--- exit: {} ---\n",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        output.status.code().unwrap_or(-1),
    )
}

pub fn write_transcript_multi(name: &str, body: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qa/transcripts");
    std::fs::create_dir_all(&dir).expect("create transcripts dir");
    std::fs::write(dir.join(name), body).expect("write transcript");
}
