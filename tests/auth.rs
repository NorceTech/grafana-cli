mod common;

use common::*;
use serde_json::json;
use wiremock::Mock;

fn oauth_config_toml(base: &str) -> String {
    format!(
        "current_profile = \"p\"\n\
         [profiles.p]\n\
         url = \"{base}\"\n\
         [profiles.p.oauth]\n\
         token_url = \"{base}/token\"\n\
         client_id = \"cid\"\n\
         client_secret = \"sec\"\n"
    )
}

fn envs<'a>(
    config: &'a tempfile::TempDir,
    cache: &'a tempfile::TempDir,
) -> [(&'a str, &'a str); 2] {
    [
        ("GCLI_CONFIG_DIR", config.path().to_str().unwrap()),
        ("GCLI_CACHE_DIR", cache.path().to_str().unwrap()),
    ]
}

fn cache_file(cache: &tempfile::TempDir) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(cache.path().join("tokens.json")).expect("tokens.json"),
    )
    .expect("tokens.json json")
}

#[tokio::test]
async fn s9_basic_auth_header() {
    let server = wiremock::MockServer::start().await;
    health_auth_mock("Basic dTpw")
        .expect(1)
        .mount(&server)
        .await;

    let config = fixture_config(&format!(
        "current_profile = \"p\"\n\
         [profiles.p]\n\
         url = \"{}\"\n\
         username = \"u\"\n\
         password = \"p\"\n",
        server.uri()
    ));
    let cache = tempfile::tempdir().unwrap();
    let out = spawn_gcli(&["health"], &envs(&config, &cache));

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("11.4.0"), "{stdout}");
    server.verify().await;
    write_transcript("s9_basic_auth.txt", "gcli health", &out);
}

#[tokio::test]
async fn s10_oauth_fetch() {
    let server = wiremock::MockServer::start().await;
    token_endpoint_mock("tok1", 3600)
        .expect(1)
        .mount(&server)
        .await;
    health_auth_mock("Bearer tok1")
        .expect(1)
        .mount(&server)
        .await;

    let config = fixture_config(&oauth_config_toml(&server.uri()));
    let cache = tempfile::tempdir().unwrap();
    let out = spawn_gcli(&["health"], &envs(&config, &cache));

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("11.4.0"));
    server.verify().await;

    let now = chrono::Utc::now().timestamp();
    let entry = &cache_file(&cache)["p:cid"];
    assert_eq!(entry["token"], "tok1");
    let expires = entry["expires_at_unix"].as_i64().unwrap();
    assert!((expires - (now + 3600 - 60)).abs() <= 5, "{expires}");
    write_transcript("s10_oauth_fetch.txt", "gcli health", &out);
}

#[tokio::test]
async fn s10_oauth_reuse() {
    let server = wiremock::MockServer::start().await;
    token_endpoint_mock("tok1", 3600)
        .expect(1)
        .mount(&server)
        .await;
    health_auth_mock("Bearer tok1")
        .expect(2)
        .mount(&server)
        .await;

    let config = fixture_config(&oauth_config_toml(&server.uri()));
    let cache = tempfile::tempdir().unwrap();
    let mut last = None;
    for _ in 0..2 {
        last = Some(spawn_gcli(&["health"], &envs(&config, &cache)));
        assert!(
            last.as_ref().unwrap().status.success(),
            "{}",
            String::from_utf8_lossy(&last.as_ref().unwrap().stderr)
        );
    }
    // token endpoint hit exactly once across both runs; second run used the cache
    server.verify().await;
    write_transcript(
        "s10_oauth_reuse.txt",
        "gcli health (x2, cached token)",
        &last.unwrap(),
    );
}

#[tokio::test]
async fn s10_oauth_expiry() {
    let server = wiremock::MockServer::start().await;
    token_endpoint_mock("fresh", 3600)
        .expect(1)
        .mount(&server)
        .await;
    health_auth_mock("Bearer fresh")
        .expect(1)
        .mount(&server)
        .await;

    let config = fixture_config(&oauth_config_toml(&server.uri()));
    let cache = tempfile::tempdir().unwrap();
    let past = chrono::Utc::now().timestamp() - 1000;
    std::fs::write(
        cache.path().join("tokens.json"),
        json!({"p:cid": {"token": "stale", "expires_at_unix": past}}).to_string(),
    )
    .unwrap();

    let out = spawn_gcli(&["health"], &envs(&config, &cache));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("11.4.0"));
    server.verify().await;
    assert_eq!(cache_file(&cache)["p:cid"]["token"], "fresh");
    write_transcript("s10_oauth_expiry.txt", "gcli health (expired cache)", &out);
}

#[tokio::test]
async fn s10_oauth_401_retry() {
    let server = wiremock::MockServer::start().await;
    // FIFO precedence: first-mounted mock wins while it still matches
    token_endpoint_mock("tok1", 3600)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    token_endpoint_mock("tok2", 3600)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/health"))
        .and(wiremock::matchers::header("Authorization", "Bearer tok1"))
        .respond_with(wiremock::ResponseTemplate::new(401))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    health_auth_mock("Bearer tok2")
        .expect(1)
        .mount(&server)
        .await;

    let config = fixture_config(&oauth_config_toml(&server.uri()));
    let cache = tempfile::tempdir().unwrap();
    let out = spawn_gcli(&["health"], &envs(&config, &cache));

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("11.4.0"));
    server.verify().await;
    assert_eq!(cache_file(&cache)["p:cid"]["token"], "tok2");
    write_transcript(
        "s10_oauth_401_retry.txt",
        "gcli health (401 -> refresh -> retry)",
        &out,
    );
}

// GRAFANA_CLI_* override whichever profile was selected, --profile included,
// and a CLI flag still beats the env var (README: flags > env > config).
#[tokio::test]
async fn s13_env_token_overrides_selected_profile_and_flag_beats_env() {
    let server = wiremock::MockServer::start().await;
    health_auth_mock("Bearer envtok")
        .expect(1)
        .mount(&server)
        .await;
    health_auth_mock("Bearer flagtok")
        .expect(1)
        .mount(&server)
        .await;
    let config = fixture_config(&format!(
        "[profiles.p]\nurl = \"{}\"\ntoken = \"filetok\"\n",
        server.uri()
    ));
    let cache = tempfile::tempdir().unwrap();
    let [config_env, cache_env] = envs(&config, &cache);
    let env = [config_env, cache_env, ("GRAFANA_CLI_TOKEN", "envtok")];

    let via_env = spawn_gcli(&["--profile", "p", "health"], &env);
    let via_flag = spawn_gcli(&["--profile", "p", "--token", "flagtok", "health"], &env);

    for out in [&via_env, &via_flag] {
        assert!(
            out.status.success(),
            "exit={:?} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    server.verify().await;
    write_transcript_multi(
        "s13_env_overrides.txt",
        &(transcript_block("GRAFANA_CLI_TOKEN=envtok gcli --profile p health", &via_env)
            + &transcript_block(
                "GRAFANA_CLI_TOKEN=envtok gcli --profile p --token flagtok health",
                &via_flag,
            )),
    );
}

// GRAFANA_CLI_PROFILE beats current_profile, and GRAFANA_CLI_URL, _USERNAME and
// _PASSWORD replace the selected profile's url and basic credentials. Both file
// urls are dead, so only the env url can reach the mock, and only as u:p.
#[tokio::test]
async fn s14_env_selects_profile_and_overrides_url_and_basic_credentials() {
    let server = wiremock::MockServer::start().await;
    health_auth_mock("Basic dTpw")
        .expect(1)
        .mount(&server)
        .await;
    let config = fixture_config(
        "current_profile = \"other\"\n\
         [profiles.other]\n\
         url = \"http://127.0.0.1:1\"\n\
         token = \"othertok\"\n\
         [profiles.p]\n\
         url = \"http://127.0.0.1:1\"\n\
         username = \"fileuser\"\n\
         password = \"filepass\"\n",
    );
    let cache = tempfile::tempdir().unwrap();
    let [config_env, cache_env] = envs(&config, &cache);
    let uri = server.uri();
    let env = [
        config_env,
        cache_env,
        ("GRAFANA_CLI_PROFILE", "p"),
        ("GRAFANA_CLI_URL", uri.as_str()),
        ("GRAFANA_CLI_USERNAME", "u"),
        ("GRAFANA_CLI_PASSWORD", "p"),
    ];

    let out = spawn_gcli(&["health"], &env);

    assert!(
        out.status.success(),
        "exit={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    server.verify().await;
    write_transcript(
        "s14_env_profile_and_overrides.txt",
        "GRAFANA_CLI_PROFILE=p GRAFANA_CLI_URL=<mock> GRAFANA_CLI_USERNAME=u \
         GRAFANA_CLI_PASSWORD=p gcli health",
        &out,
    );
}
