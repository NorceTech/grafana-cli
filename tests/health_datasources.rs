mod common;

use common::*;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

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

// GET /api/datasources/uid/<uid>/health: the per-datasource health endpoint
// gcli's --check uses, pinned to the uid it must request.
fn ds_health_mock(uid: &str, status: u16, body: Value) -> Mock {
    Mock::given(method("GET"))
        .and(path(format!("/api/datasources/uid/{uid}/health")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
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

#[tokio::test]
async fn s31_datasources_check_mixed_health() {
    let server = wiremock::MockServer::start().await;
    datasources_json(three_datasources())
        .expect(1)
        .mount(&server)
        .await;
    ds_health_mock(
        "u-loki",
        200,
        json!({"status": "OK", "message": "Data source is working"}),
    )
    .expect(1)
    .mount(&server)
    .await;
    // A datasource type without health support: Grafana answers 4xx, the
    // command still succeeds.
    ds_health_mock(
        "u-prom",
        404,
        json!({"message": "Data source does not support health checks"}),
    )
    .expect(1)
    .mount(&server)
    .await;
    ds_health_mock(
        "u-tempo",
        200,
        json!({"status": "OK", "message": "Data source successfully connected."}),
    )
    .expect(1)
    .mount(&server)
    .await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["datasources", "--check"],
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
        assert_eq!(
            keys,
            [
                "health",
                "healthMessage",
                "isDefault",
                "name",
                "type",
                "uid",
                "url"
            ]
        );
    }
    // output order = datasource order, OK and ERROR mixed
    assert_eq!(v[0]["uid"], "u-loki");
    assert_eq!(v[0]["health"], "OK");
    assert_eq!(v[0]["healthMessage"], "Data source is working");
    assert_eq!(v[1]["uid"], "u-prom");
    assert_eq!(v[1]["health"], "ERROR");
    assert_eq!(
        v[1]["healthMessage"],
        "Data source does not support health checks"
    );
    assert_eq!(v[2]["uid"], "u-tempo");
    assert_eq!(v[2]["health"], "OK");
    assert_eq!(v[2]["healthMessage"], "Data source successfully connected.");
    // every health call went to its datasource's own uid path
    server.verify().await;
    write_transcript(
        "s31_datasources_check.txt",
        "gcli datasources --check",
        &out,
    );
}

#[tokio::test]
async fn s32_datasources_check_table_output() {
    let server = wiremock::MockServer::start().await;
    datasources_json(three_datasources())
        .expect(1)
        .mount(&server)
        .await;
    ds_health_mock(
        "u-loki",
        200,
        json!({"status": "OK", "message": "Data source is working"}),
    )
    .expect(1)
    .mount(&server)
    .await;
    ds_health_mock(
        "u-prom",
        500,
        json!({"message": "Data source does not support health checks"}),
    )
    .expect(1)
    .mount(&server)
    .await;
    ds_health_mock(
        "u-tempo",
        200,
        json!({"status": "OK", "message": "Data source successfully connected."}),
    )
    .expect(1)
    .mount(&server)
    .await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["datasources", "--check", "--output", "table"],
        &[("GCLI_CONFIG_DIR", config.path().to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for header in ["name", "type", "uid", "url", "default", "health", "message"] {
        assert!(stdout.contains(header), "missing header {header}: {stdout}");
    }
    assert!(stdout.contains("OK"), "{stdout}");
    assert!(stdout.contains("ERROR"), "{stdout}");
    assert!(stdout.contains("Data source is working"), "{stdout}");
    server.verify().await;
    write_transcript(
        "s32_datasources_check_table.txt",
        "gcli datasources --check --output table",
        &out,
    );
}

#[tokio::test]
async fn s33_datasources_check_only_listed_datasources() {
    // --type filters before --check: only u-loki is checked, and only its
    // health path is requested (no other health mock is mounted, so any
    // other call would leave an unmatched request).
    let server = wiremock::MockServer::start().await;
    datasources_json(three_datasources())
        .expect(1)
        .mount(&server)
        .await;
    ds_health_mock(
        "u-loki",
        200,
        json!({"status": "OK", "message": "Data source is working"}),
    )
    .expect(1)
    .mount(&server)
    .await;
    let config = token_config(&server.uri());

    let out = spawn_gcli(
        &["datasources", "--check", "--type", "loki"],
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
    assert_eq!(v[0]["uid"], "u-loki");
    assert_eq!(v[0]["health"], "OK");
    server.verify().await;
    write_transcript(
        "s33_datasources_check_filter.txt",
        "gcli datasources --check --type loki",
        &out,
    );
}

#[tokio::test]
async fn s34_datasources_without_check_is_unchanged() {
    // No --check: the exact five-key records, and no health endpoint
    // requested at all (no health mock is mounted).
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
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let expected = json!([
        {"name": "Loki main", "type": "loki", "uid": "u-loki",
         "url": "http://loki", "isDefault": true},
        {"name": "Prom", "type": "prometheus", "uid": "u-prom",
         "url": "http://prom", "isDefault": false},
        {"name": "Tempo", "type": "tempo", "uid": "u-tempo",
         "url": "http://tempo", "isDefault": false},
    ]);
    assert_eq!(stdout, expected.to_string());
    server.verify().await;
    write_transcript("s34_datasources_no_check.txt", "gcli datasources", &out);
}
