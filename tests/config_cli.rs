mod common;

use common::*;

// profile remove also forgets cached tokens, so the cache dir points at the
// tempdir too and no test reaches the real token cache.
fn config_env(dir: &tempfile::TempDir) -> [(&str, &str); 2] {
    let dir = dir.path().to_str().unwrap();
    [("GCLI_CONFIG_DIR", dir), ("GCLI_CACHE_DIR", dir)]
}

fn load_config(dir: &tempfile::TempDir) -> gcli_lib::Config {
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).expect("config.toml");
    toml::from_str(&raw).expect("parse config.toml")
}

fn two_profile_toml() -> String {
    "current_profile = \"primary\"\n\
     [profiles.primary]\n\
     url = \"http://x\"\n\
     token = \"glsa_t\"\n\
     \n\
     [profiles.secondary]\n\
     url = \"http://y\"\n\
     token = \"glsa_2\"\n"
        .into()
}

fn stderr_json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_str(String::from_utf8_lossy(&out.stderr).trim()).expect("stderr json")
}

#[test]
fn s1_profile_add_writes_token_profile() {
    let dir = tempfile::tempdir().unwrap();
    let out = spawn_gcli(
        &[
            "profile", "add", "primary", "--url", "http://x", "--token", "glsa_t",
        ],
        &config_env(&dir),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("primary"), "no confirmation: {stdout}");

    let cfg = load_config(&dir);
    let p = cfg
        .profiles
        .get("primary")
        .expect("profile primary on disk");
    assert_eq!(p.url, "http://x");
    assert_eq!(p.token.as_deref(), Some("glsa_t"));
    write_transcript(
        "s1_profile_add.txt",
        "gcli profile add primary --url http://x --token glsa_t",
        &out,
    );
}

#[test]
fn s1_profile_add_oauth_writes_oauth_config() {
    let dir = tempfile::tempdir().unwrap();
    let out = spawn_gcli(
        &[
            "profile",
            "add",
            "oauthfull",
            "--url",
            "https://g.example.com",
            "--token-url",
            "https://idp/token",
            "--client-id",
            "cid",
            "--client-secret",
            "s",
            "--scope",
            "a,b",
            "--audience",
            "aud",
        ],
        &config_env(&dir),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cfg = load_config(&dir);
    let oauth = cfg
        .profiles
        .get("oauthfull")
        .expect("profile oauthfull on disk")
        .oauth
        .as_ref()
        .expect("oauth config on disk");
    assert_eq!(oauth.token_url, "https://idp/token");
    assert_eq!(oauth.client_id, "cid");
    assert_eq!(oauth.client_secret.as_deref(), Some("s"));
    assert_eq!(
        oauth.scopes.as_deref(),
        Some(["a".to_string(), "b".to_string()].as_slice())
    );
    assert_eq!(oauth.audience.as_deref(), Some("aud"));
}

#[test]
fn s1_profile_lifecycle_list_use_set_token_remove() {
    let dir = fixture_config(&two_profile_toml());
    let env = config_env(&dir);

    // list shows both profiles
    let out = spawn_gcli(&["profile", "list"], &env);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("primary"), "{stdout}");
    assert!(stdout.contains("secondary"), "{stdout}");

    // use flips current_profile on disk
    let out = spawn_gcli(&["profile", "use", "secondary"], &env);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        load_config(&dir).current_profile.as_deref(),
        Some("secondary")
    );

    // transcript: list after switching current profile
    let list2 = spawn_gcli(&["profile", "list"], &env);
    assert_eq!(list2.status.code(), Some(0));
    write_transcript(
        "s1_profile_lifecycle.txt",
        "gcli profile list (after profile use secondary)",
        &list2,
    );

    // set-token mutates on disk
    let out = spawn_gcli(
        &["profile", "set-token", "primary", "--token", "newtok"],
        &env,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        load_config(&dir).profiles["primary"].token.as_deref(),
        Some("newtok")
    );

    // remove deletes from disk and clears dangling current_profile
    let out = spawn_gcli(&["profile", "remove", "secondary"], &env);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg = load_config(&dir);
    assert!(!cfg.profiles.contains_key("secondary"));
    assert!(cfg.profiles.contains_key("primary"));
    assert!(cfg.current_profile.is_none());
}

#[test]
fn s1_profile_errors() {
    let dir = fixture_config(&two_profile_toml());
    let env = config_env(&dir);

    // use unknown profile -> exit 2, config-kind JSON error on stderr
    let out = spawn_gcli(&["profile", "use", "missing"], &env);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stderr_json(&out)["error"]["kind"], "config");
    write_transcript("s1_profile_errors.txt", "gcli profile use missing", &out);

    // add without auth -> validation error via add_profile, exit 2, nothing saved
    let dir2 = tempfile::tempdir().unwrap();
    let out = spawn_gcli(
        &["profile", "add", "bad", "--url", "http://x"],
        &config_env(&dir2),
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stderr_json(&out)["error"]["kind"], "config");
    assert!(!dir2.path().join("config.toml").exists());
}

// A secret flag value of `-` reads the secret from stdin, so a profile's
// token never appears on the command line: the piped value, trimmed, is what
// lands in config.toml.
#[test]
fn s28_profile_add_token_dash_reads_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let out = spawn_gcli_with_stdin(
        &["profile", "add", "p", "--url", "http://x", "--token", "-"],
        &config_env(&dir),
        "glsa_from_stdin\n",
    );

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        load_config(&dir).profiles["p"].token.as_deref(),
        Some("glsa_from_stdin")
    );
    write_transcript(
        "s28_add_token_dash.txt",
        "echo glsa_from_stdin | gcli profile add p --url http://x --token -",
        &out,
    );
}

#[test]
fn s29_set_token_dash_reads_stdin() {
    let dir = fixture_config(&two_profile_toml());

    let out = spawn_gcli_with_stdin(
        &["profile", "set-token", "primary", "--token", "-"],
        &config_env(&dir),
        "rotated-from-stdin\n",
    );

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        load_config(&dir).profiles["primary"].token.as_deref(),
        Some("rotated-from-stdin")
    );
    write_transcript(
        "s29_set_token_dash.txt",
        "echo rotated-from-stdin | gcli profile set-token primary --token -",
        &out,
    );
}

// The convention covers the OAuth client secret too: profile add stores the
// piped value in the oauth table.
#[test]
fn s30_profile_add_client_secret_dash_reads_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let out = spawn_gcli_with_stdin(
        &[
            "profile",
            "add",
            "oauthp",
            "--url",
            "https://g.example.com",
            "--token-url",
            "https://idp/token",
            "--client-id",
            "cid",
            "--client-secret",
            "-",
        ],
        &config_env(&dir),
        "s3cret-from-stdin\n",
    );

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg = load_config(&dir);
    let oauth = cfg
        .profiles
        .get("oauthp")
        .expect("profile oauthp on disk")
        .oauth
        .as_ref()
        .expect("oauth config on disk");
    assert_eq!(oauth.client_secret.as_deref(), Some("s3cret-from-stdin"));
    write_transcript(
        "s30_add_client_secret_dash.txt",
        "echo s3cret-from-stdin | gcli profile add oauthp --url https://g.example.com \\
  --token-url https://idp/token --client-id cid --client-secret -",
        &out,
    );
}

// A profile url that is not an absolute http(s) address would only surface
// later as a reqwest builder error; profile add rejects it as kind url, exit
// 1, and saves nothing.
#[test]
fn s1_profile_add_rejects_a_url_without_a_scheme() {
    let dir = tempfile::tempdir().unwrap();
    let out = spawn_gcli(
        &[
            "profile",
            "add",
            "bad",
            "--url",
            "grafana.example.com",
            "--token",
            "t",
        ],
        &config_env(&dir),
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout={} stderr={stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(stderr_json(&out)["error"]["kind"], "url");
    assert!(
        stderr_json(&out)["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("grafana.example.com"),
        "{stderr}"
    );
    assert!(!dir.path().join("config.toml").exists());
    write_transcript(
        "s1_profile_add_bad_url.txt",
        "gcli profile add bad --url grafana.example.com --token t",
        &out,
    );
}
