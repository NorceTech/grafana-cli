mod common;

use common::*;
use serde_json::{Value, json};

fn three_datasources() -> Value {
    json!([
        {"id": 1, "uid": "u-loki", "name": "Loki main", "type": "loki",
         "url": "http://loki", "isDefault": true},
        {"id": 2, "uid": "u-prom", "name": "Prom", "type": "prometheus",
         "url": "http://prom", "isDefault": false},
        {"id": 3, "uid": "u-tempo", "name": "Tempo", "type": "tempo",
         "url": "http://tempo", "isDefault": false},
    ])
}

fn stdout_json(out: &std::process::Output) -> Value {
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("stdout json")
}

#[tokio::test]
async fn s2_health_json() {
    let server = wiremock::MockServer::start().await;
    health_auth_mock("Bearer t").expect(1).mount(&server).await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["health"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = stdout_json(&out);
    assert_eq!(v["version"], "11.4.0");
    assert_eq!(v["database"], "ok");
    server.verify().await;
    write_transcript("s2_health.txt", "gcli health", &out);
}

#[tokio::test]
async fn s3_datasources_lists_all() {
    let server = wiremock::MockServer::start().await;
    datasources_json(three_datasources())
        .expect(1)
        .mount(&server)
        .await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["datasources"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = stdout_json(&out);
    let records = v.as_array().expect("json array");
    assert_eq!(records.len(), 3);
    for rec in records {
        let mut keys: Vec<&str> = rec
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["isDefault", "name", "type", "uid", "url"]);
    }
    assert_eq!(v[0]["uid"], "u-loki");
    assert_eq!(v[0]["type"], "loki");
    assert_eq!(v[0]["isDefault"], true);
    assert_eq!(v[1]["name"], "Prom");
    assert_eq!(v[2]["type"], "tempo");
    server.verify().await;
    write_transcript("s3_datasources.txt", "gcli datasources", &out);
}

#[tokio::test]
async fn s3_datasources_type_filter() {
    let server = wiremock::MockServer::start().await;
    datasources_json(three_datasources())
        .expect(1)
        .mount(&server)
        .await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["datasources", "--type", "loki"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = stdout_json(&out);
    let records = v.as_array().expect("json array");
    assert_eq!(records.len(), 1);
    assert_eq!(v[0]["type"], "loki");
    assert_eq!(v[0]["name"], "Loki main");
    assert_eq!(v[0]["uid"], "u-loki");
    server.verify().await;
    write_transcript(
        "s3_datasources_filter.txt",
        "gcli datasources --type loki",
        &out,
    );
}

#[tokio::test]
async fn s3_datasources_table_output() {
    let server = wiremock::MockServer::start().await;
    datasources_json(three_datasources())
        .expect(1)
        .mount(&server)
        .await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["datasources", "--output", "table"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // header row present
    for header in ["name", "type", "uid", "url", "default"] {
        assert!(stdout.contains(header), "missing header {header}: {stdout}");
    }
    // data rows present, not a json array
    assert!(stdout.contains("Loki main"), "{stdout}");
    assert!(stdout.contains("u-tempo"), "{stdout}");
    assert!(!stdout.trim_start().starts_with('['), "{stdout}");
    server.verify().await;
}
