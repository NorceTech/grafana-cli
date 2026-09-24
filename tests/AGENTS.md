# tests/

## OVERVIEW

Black-box integration suite: every test spawns the compiled `gcli` binary against a wiremock Grafana. Own file because it scores 10 and is a distinct domain; unit tests stay in `#[cfg(test)]` modules under `src/`.

## WHERE TO LOOK

| Scenario                                                                                                                                                    | File                    | IDs              |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------- | ---------------- |
| profile add/list/use/set-token/remove, profile errors                                                                                                       | `config_cli.rs`         | s1               |
| health; datasources (`--type`, table)                                                                                                                       | `health_datasources.rs` | s2, s3           |
| logs search, labels, label-values; a path-traversing label rejected; out-of-range time windows                                                              | `logs.rs`               | s4, s5, s18      |
| metrics instant/range, epoch wire format                                                                                                                    | `metrics.rs`            | s6               |
| traces get/search/404; a path-traversing trace id rejected                                                                                                  | `traces.rs`             | s7               |
| multi-profile fanout, exit codes, a bad window failing once; overrides rejected for several profiles (concurrency itself is a unit test in `src/fanout.rs`) | `fanout_e2e.rs`         | s8a-d,f, s17a-b  |
| basic auth; OAuth fetch/reuse/expiry/401 retry, parallel cache, remove forgets token; env/flag overrides                                                    | `auth.rs`               | s9, s10, s13-s16 |
| output modes, error JSON schema, help/version/completions                                                                                                   | `ux.rs`                 | s11-12           |
| broken pipe exits 0 (real `bash -c` pipe to `head -c 1`)                                                                                                    | `pipe.rs`               | -                |

Shared harness, `common/mod.rs`:

- `gcli_command(args, envs)` / `spawn_gcli(args, envs)`: run `CARGO_BIN_EXE_gcli` from the manifest dir after `scrub_env` strips `GCLI_CONFIG_DIR`, `GCLI_CACHE_DIR` and every `GRAFANA_CLI_*` var — the one scrub list, shared with `pipe.rs`'s bash pipelines. Pass `("GCLI_CONFIG_DIR", dir)` through `envs`, plus `GCLI_CACHE_DIR` for OAuth tests (`auth.rs`) and for anything running `profile remove`, which forgets cached tokens (`config_cli.rs` `config_env` points both at one tempdir).
- `fixture_config(toml)`: `TempDir` holding `config.toml`; keep it bound until the spawn returns. `token_config(base_url)` is the common case: one token-auth profile "p" pointing at `base_url`.
- `stdout_lines(&out)`: every non-empty stdout line parsed as JSON.
- Mocks: `health_mock`, `health_auth_mock(authorization)`, `token_endpoint_mock(token, expires_in)` (POST `/token`), `datasources_json(body)` (GET `/api/datasources`).
- `write_transcript(name, cmd, &out)`, `write_transcript_multi(name, body)`, `transcript_block`: write `qa/transcripts/<name>` with command, stdout, stderr and exit code. `qa/` is gitignored and rewritten by every `cargo test`.

## CONVENTIONS

- Test shape: `#[tokio::test] async fn sN_<case>` -> `MockServer::start().await` -> mount mocks with `.expect(n)` -> `fixture_config` -> `spawn_gcli` -> assert the exit code with stderr in the failure message -> assert stdout JSON -> `server.verify().await` -> `write_transcript("sN_<case>.txt", "gcli ...", &out)`.
- A new scenario takes the next scenario ID and writes its own transcript file.
- Shared harness helpers live in `common/mod.rs`, which keeps `#![allow(dead_code)]` because each test crate uses a different subset; bespoke mocks and multi-profile TOML builders stay in the file that needs them.
- Derive, don't trust: build expected wire params with `gcli_lib::timeutil` (`resolve_window`, `to_ns_string`, `to_epoch_secs`), or chrono `timestamp()` for Tempo's whole-second bounds, never hand-computed epochs (`logs.rs:11`, `metrics.rs:12`, `traces.rs:12`).
- Datasource calls are mocked on the full proxy path `/api/datasources/proxy/uid/<uid>/<api path>` plus `query_param` matchers; the `datasources_json` fixture decides which uid the CLI must resolve.
- JSON contracts are pinned by exact sorted key sets (`health_datasources.rs:76`, `ux.rs:71`), not by field presence.
- Fixtures return data out of order on purpose (`logs.rs:40`, `metrics.rs:49`) to pin CLI-side sorting.

## ANTI-PATTERNS

- Never let a test touch the developer's real config or token cache: always a `TempDir` through `GCLI_CONFIG_DIR`/`GCLI_CACHE_DIR`.
- Never commit `qa/transcripts/*` or review their churn as part of a diff.
- No wall-clock assertions: concurrency is proven structurally by the Barrier rendezvous in `run_executes_profiles_concurrently` (`src/fanout.rs`), never by timing the binary.
- No real network or credentials: every profile URL points at the wiremock server.
