#![allow(dead_code)]

use serde_json::Value;
use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

/// Env vars scrubbed from every spawn of the binary under test: the two test
/// dir vars plus every `GRAFANA_CLI_*` override, so no test inherits the
/// developer's real configuration or environment.
const SCRUBBED_ENV: &[&str] = &[
    "GCLI_CONFIG_DIR",
    "GCLI_CACHE_DIR",
    "GRAFANA_CLI_PROFILE",
    "GRAFANA_CLI_URL",
    "GRAFANA_CLI_TOKEN",
    "GRAFANA_CLI_USERNAME",
    "GRAFANA_CLI_PASSWORD",
];

/// `cmd` with the scrubbed env vars removed. The single owner of the scrub
/// list: `gcli_command`/`spawn_gcli` and the bash pipelines in `pipe.rs`.
pub fn scrub_env(mut cmd: Command) -> Command {
    for var in SCRUBBED_ENV {
        cmd.env_remove(var);
    }
    cmd
}

/// The `gcli` binary under test, ready to run: args, manifest dir, scrubbed
/// environment plus `envs`.
pub fn gcli_command(args: &[&str], envs: &[(&str, &str)]) -> Command {
    let mut cmd = scrub_env(Command::new(env!("CARGO_BIN_EXE_gcli")));
    cmd.args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .envs(envs.iter().copied());
    cmd
}

pub fn spawn_gcli(args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    gcli_command(args, envs)
        .output()
        .expect("failed to spawn gcli")
}

/// Config with one token-auth profile "p" pointing at `base_url`.
pub fn token_config(base_url: &str) -> tempfile::TempDir {
    fixture_config(&format!(
        "current_profile = \"p\"\n[profiles.p]\nurl = \"{base_url}\"\ntoken = \"t\"\n"
    ))
}

/// Every non-empty stdout line parsed as JSON.
pub fn stdout_lines(out: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("stdout line is JSON"))
        .collect()
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

/// The sole Loki datasource `loki1`, default, for query-command tests.
pub fn loki_datasources_mock() -> Mock {
    datasources_json(serde_json::json!([{
        "id": 1, "uid": "loki1", "name": "Loki", "type": "loki",
        "url": "/loki", "isDefault": true
    }]))
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
