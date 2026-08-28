mod common;

use common::*;
use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Raw `bash -c` spawn so we can observe PIPESTATUS[0] of the gcli process in a
// `gcli ... | head` pipeline. Kept local to this file (common/mod.rs is frozen).
fn bash_pipe(script_body: &str, envs: &[(&str, &str)]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_gcli");
    // `head -c 1` closes the read end after one byte; exit with gcli's status.
    let script = format!("'{bin}' {script_body} | head -c 1; exit ${{PIPESTATUS[0]}}");
    Command::new("bash")
        .arg("-c")
        .arg(script)
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
        .expect("failed to spawn bash pipeline")
}

fn token_config(base: &str) -> tempfile::TempDir {
    fixture_config(&format!(
        "current_profile = \"p\"\n[profiles.p]\nurl = \"{base}\"\ntoken = \"t\"\n"
    ))
}

// ~2000 datasource entries => serialized JSON well over 128KB, which overflows
// the 64KB pipe buffer and forces a write to fail once `head` closes the reader.
fn many_datasources() -> serde_json::Value {
    let list: Vec<serde_json::Value> = (0..2000)
        .map(|i| {
            serde_json::json!({
                "id": i,
                "uid": format!("uid-{i:06}-padding-padding-padding"),
                "name": format!("datasource-number-{i:06}-with-a-long-name"),
                "type": "loki",
                "url": format!("http://loki-{i:06}.example.internal.cluster.local"),
                "isDefault": false,
            })
        })
        .collect();
    serde_json::Value::Array(list)
}

#[tokio::test]
async fn pipe_data_path_broken_pipe_is_clean_exit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/datasources"))
        .respond_with(ResponseTemplate::new(200).set_body_json(many_datasources()))
        .mount(&server)
        .await;
    let config = token_config(&server.uri());
    let env = [("GCLI_CONFIG_DIR", config.path().to_str().unwrap())];

    let out = bash_pipe("datasources --output json", &env);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "gcli panicked on broken pipe (data path): stderr={stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "broken pipe on data path must be a clean exit 0; stderr={stderr}"
    );
}

#[test]
fn pipe_completions_broken_pipe_is_clean_exit() {
    let out = bash_pipe("completions bash", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "gcli panicked on broken pipe (completions): stderr={stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "broken pipe on completions must be a clean exit 0; stderr={stderr}"
    );
}
