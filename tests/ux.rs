mod common;

use common::*;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn token_config(base: &str) -> tempfile::TempDir {
    fixture_config(&format!(
        "current_profile = \"p\"\n[profiles.p]\nurl = \"{base}\"\ntoken = \"t\"\n"
    ))
}

#[tokio::test]
async fn s11a_output_modes_json_and_table() {
    let server = MockServer::start().await;
    health_mock().mount(&server).await;
    let config = token_config(&server.uri());
    let env = [("GCLI_CONFIG_DIR", config.path().to_str().unwrap())];

    let json_out = spawn_gcli(&["health"], &env);
    assert_eq!(
        json_out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&json_out.stderr)
    );
    let parsed: Value = serde_json::from_str(String::from_utf8_lossy(&json_out.stdout).trim())
        .expect("piped health stdout must be valid JSON (auto-json)");
    assert_eq!(parsed["version"], "11.4.0");

    let table_out = spawn_gcli(&["--output", "table", "health"], &env);
    assert_eq!(table_out.status.code(), Some(0));
    let table_stdout = String::from_utf8_lossy(&table_out.stdout);
    assert!(
        table_stdout.contains("version"),
        "table health missing version row: {table_stdout}"
    );
    for header in ["key", "value"] {
        assert!(
            table_stdout.contains(header),
            "table health missing `{header}` header (not honoring table mode): {table_stdout}"
        );
    }
    assert!(
        !table_stdout.trim_start().starts_with('{'),
        "table mode must not emit raw JSON: {table_stdout}"
    );

    write_transcript_multi(
        "s11_output_modes.txt",
        &[
            transcript_block("gcli health", &json_out),
            transcript_block("gcli --output table health", &table_out),
        ]
        .concat(),
    );
}

#[tokio::test]
async fn s11b_error_schema_config_and_http() {
    let empty = fixture_config("");
    let missing = spawn_gcli(
        &["--profile", "missing", "health"],
        &[("GCLI_CONFIG_DIR", empty.path().to_str().unwrap())],
    );
    assert_eq!(missing.status.code(), Some(2), "config error must exit 2");
    let cfg_err: Value =
        serde_json::from_str(String::from_utf8_lossy(&missing.stderr).trim()).expect("stderr JSON");
    let err_obj = cfg_err["error"].as_object().expect("error object");
    let mut keys: Vec<&str> = err_obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["kind", "message", "profile", "status"]);
    assert_eq!(cfg_err["error"]["profile"], "missing");
    assert_eq!(cfg_err["error"]["kind"], "config");
    assert!(cfg_err["error"]["status"].is_null());
    assert!(cfg_err["error"]["message"].is_string());

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"message": "nope"})))
        .mount(&server)
        .await;
    let config = token_config(&server.uri());
    let http = spawn_gcli(
        &["health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(http.status.code(), Some(1), "http error must exit 1");
    let http_err: Value =
        serde_json::from_str(String::from_utf8_lossy(&http.stderr).trim()).expect("stderr JSON");
    assert_eq!(http_err["error"]["kind"], "http");
    assert_eq!(http_err["error"]["status"], 500);
    assert_eq!(http_err["error"]["profile"], "p");

    write_transcript_multi(
        "s11_error_schema.txt",
        &[
            transcript_block("gcli --profile missing health", &missing),
            transcript_block("gcli health (http 500)", &http),
        ]
        .concat(),
    );
}

// An SSO proxy in front of Grafana answering 200 with its HTML login page is
// kind "response" with the reply's status, and the message shows what came
// back instead of JSON.
#[tokio::test]
async fn s11c_error_schema_malformed_reply() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<!DOCTYPE html><html><head><title>Sign in</title></head><body></body></html>",
            "text/html; charset=utf-8",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
    assert_eq!(err["error"]["kind"], "response");
    assert_eq!(err["error"]["status"], 200);
    assert_eq!(err["error"]["profile"], "p");
    let message = err["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("text/html"), "{message}");
    assert!(message.contains("<title>Sign in</title>"), "{message}");
    server.verify().await;
    write_transcript(
        "s11c_malformed_reply.txt",
        "gcli health (200 text/html login page)",
        &out,
    );
}

// kind "url": a profile url, here via --url, that is not an absolute
// http(s) address is rejected before anything is requested.
#[tokio::test]
async fn s11d_error_schema_invalid_url() {
    let config = fixture_config(
        "current_profile = \"p\"\n[profiles.p]\nurl = \"http://127.0.0.1:1\"\ntoken = \"t\"\n",
    );

    let out = spawn_gcli(
        &["--url", "not-a-url", "health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    let err: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
    assert_eq!(err["error"]["kind"], "url");
    assert_eq!(err["error"]["profile"], "p");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not-a-url"),
        "{stderr}"
    );
    write_transcript("s11d_invalid_url.txt", "gcli --url not-a-url health", &out);
}

// GRAFANA_CLI_URL gets the same check: a non-http scheme is a url error and
// the profile's own instance is never contacted.
#[tokio::test]
async fn s11e_env_url_with_a_foreign_scheme_is_rejected() {
    let server = MockServer::start().await;
    health_mock().expect(0).mount(&server).await;
    let config = token_config(&server.uri());
    let uri = "ftp://grafana.example.com".to_string();
    let env = [
        ("GCLI_CONFIG_DIR", config.path().to_str().unwrap()),
        ("GRAFANA_CLI_URL", uri.as_str()),
    ];

    let out = spawn_gcli(&["health"], &env);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    let err: Value = serde_json::from_str(stderr.trim()).expect("stderr JSON");
    assert_eq!(err["error"]["kind"], "url");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("ftp"),
        "{stderr}"
    );
    server.verify().await;
    write_transcript(
        "s11e_env_url_scheme.txt",
        "GRAFANA_CLI_URL=ftp://grafana.example.com gcli health",
        &out,
    );
}

const HELP_TARGETS: &[&[&str]] = &[
    &[],
    &["profile"],
    &["profile", "add"],
    &["profile", "list"],
    &["profile", "use"],
    &["profile", "remove"],
    &["profile", "set-token"],
    &["health"],
    &["datasources"],
    &["logs"],
    &["logs", "search"],
    &["logs", "labels"],
    &["logs", "label-values"],
    &["metrics"],
    &["metrics", "instant"],
    &["metrics", "range"],
    &["traces"],
    &["traces", "get"],
    &["traces", "search"],
    &["completions"],
];

#[test]
fn s12_help_version_completions() {
    for target in HELP_TARGETS {
        let mut args = target.to_vec();
        args.push("--help");
        let out = spawn_gcli(&args, &[]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "`{} --help` exit != 0",
            target.join(" ")
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("Usage:"),
            "`{} --help` missing Usage:",
            target.join(" ")
        );
    }

    let version = spawn_gcli(&["--version"], &[]);
    assert_eq!(version.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&version.stdout).contains("0.1.0"),
        "--version missing version string"
    );

    let expectations: [(&str, &str); 3] = [
        ("bash", "_gcli"),
        ("zsh", "#compdef gcli"),
        ("fish", "complete -c gcli"),
    ];
    let mut blocks = vec![transcript_block(
        "gcli --help",
        &spawn_gcli(&["--help"], &[]),
    )];
    for (shell, token) in expectations {
        let out = spawn_gcli(&["completions", shell], &[]);
        assert_eq!(out.status.code(), Some(0), "completions {shell} exit != 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!stdout.trim().is_empty(), "completions {shell} empty");
        if shell == "fish" {
            assert!(stdout.contains(token), "fish completions missing `{token}`");
        } else {
            assert!(
                stdout.starts_with(token),
                "{shell} completions must start with `{token}`, got: {}",
                stdout.lines().next().unwrap_or("")
            );
        }
        blocks.push(transcript_block(&format!("gcli completions {shell}"), &out));
    }
    write_transcript_multi("s12_help_completions.txt", &blocks.concat());
}
