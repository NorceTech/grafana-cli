# tests/

## OVERVIEW

Black-box integration suite: every test spawns the compiled `gcli` binary against a wiremock Grafana. Own file because it is a distinct domain; unit tests stay in `#[cfg(test)]` modules under `src/`.

## WHERE TO LOOK

| Scenario                                                                                                                                                                                                                                    | File                    | IDs                            |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------- | ------------------------------ |
| profile add/list/use/set-token/remove, profile errors; `--token -`/`--client-secret -` secrets read from stdin                                                                                                                              | `config_cli.rs`         | s1, s28-s30                    |
| health; datasources (`--type`, table); `--check` (mixed health, table, listed-only, unchanged without it)                                                                                                                                   | `health_datasources.rs` | s2, s3, s31-s34                |
| logs search (defaults/limit/table/empty), labels/label-values (`--match`), invalid LogQL, metric-query matrix + `--step`, `logs instant`, `--follow` batches/boundary dedup/one-profile rule, out-of-range windows, a path-traversing label | `logs.rs`               | s4a-d, s5, s18, s31-s34        |
| metrics instant/range (+step, empty result, plugin and wrong-core-type datasources); discovery labels/label-values/series/exemplars (repeated `--match`, table, upstream error envelope)                                                    | `metrics.rs`            | s6, s31a-k                     |
| traces get (OTLP passthrough, table row per span incl. `status`, 404, a path-traversing id), search (+subsecond bounds), tags/tag-values (flatten/sort/scope, typed values)                                                                 | `traces.rs`             | s7, s21, s31                   |
| multi-profile fanout, exit codes, a bad window failing once; overrides rejected for several profiles; multi-profile table blocks and plain-JSONL logs (concurrency itself is a unit test in `src/fanout.rs`)                                | `fanout_e2e.rs`         | s8a-d,f, s17a-b, s19-s20       |
| basic auth; OAuth fetch/reuse/expiry/401 retry, parallel cache, remove forgets token; env/flag overrides; multi-element `token_command` argv; `--token -`/`--password -` from stdin                                                         | `auth.rs`               | s9, s10, s13-s16, s24, s26-s27 |
| output modes, error JSON schema (config/http/malformed/url/env-url), help/version/completions                                                                                                                                               | `ux.rs`                 | s11a-e, s12                    |
| `--timeout`: slow server fails as io, invalid duration is config, secret helper command timeout                                                                                                                                             | `timeout.rs`            | s22-s23, s25                   |
| broken pipe exits 0 (real `bash -c` pipe to `head -c 1`)                                                                                                                                                                                    | `pipe.rs`               | -                              |

Scenario IDs group by feature (s8 fanout, s11 ux, s18 windows, s21 traces tables, s22+ timeouts, s28+ profile stdin secrets, s31+ the discovery feature across datasources/logs/metrics/traces files) and can repeat across files; a new scenario takes the next ID in its feature group.

Shared harness, `common/mod.rs`:

- `gcli_command(args, envs)` / `spawn_gcli(args, envs)` / `spawn_gcli_with_stdin(args, envs, stdin)` (piped stdin, for secret flags given `-`) / `spawn_gcli_streaming(args, envs)` (tokio `Child`, for `--follow`): run `CARGO_BIN_EXE_gcli` from the manifest dir after `scrub_env` strips `GCLI_CONFIG_DIR`, `GCLI_CACHE_DIR` and every `GRAFANA_CLI_*` var, the one scrub list, shared with `pipe.rs`'s bash pipelines. Pass `("GCLI_CONFIG_DIR", dir)` through `envs`, plus `GCLI_CACHE_DIR` for OAuth tests (`auth.rs`) and for anything running `profile remove`, which forgets cached tokens (`config_cli.rs` `config_env` points both at one tempdir).
- `fixture_config(toml)`: `TempDir` holding `config.toml`; keep it bound until the spawn returns. `token_config(base_url)` is the common case: one token-auth profile "p" pointing at `base_url`.
- `stdout_lines(&out)`: every non-empty stdout line parsed as JSON.
- Mocks: `health_mock`, `health_auth_mock(authorization)`, `token_endpoint_mock(token, expires_in)` (POST `/token`), `datasources_json(body)` (GET `/api/datasources`), `loki_datasources_mock` (the one-Loki-datasource fixture).
- `write_transcript(name, cmd, &out)`, `write_transcript_multi(name, body)`, `transcript_block`: write `qa/transcripts/<name>` with command, stdout, stderr and exit code. `qa/` is gitignored and rewritten by every `cargo test`.

## CONVENTIONS

- Test shape: `#[tokio::test] async fn sN_<case>` -> `MockServer::start().await` -> mount mocks with `.expect(n)` -> `fixture_config` -> `spawn_gcli` -> assert the exit code with stderr in the failure message -> assert stdout JSON -> `server.verify().await` -> `write_transcript("sN_<case>.txt", "gcli ...", &out)`.
- A new scenario writes its own transcript file; `--follow` and other multi-command cases use `write_transcript_multi`.
- Shared harness helpers live in `common/mod.rs`, which keeps `#![allow(dead_code)]` because each test crate uses a different subset; bespoke mocks and multi-profile TOML builders stay in the file that needs them.
- Derive, don't trust: build expected wire params with `gcli_lib::timeutil` (`resolve_window`, `to_ns_string`, `to_epoch_secs`), or chrono `timestamp()` for Tempo's whole-second bounds, never hand-computed epochs (the `window_ns`/`ts_ns` helpers at the top of `logs.rs`, and their siblings in `metrics.rs`/`traces.rs`).
- Datasource calls are mocked on the full proxy path `/api/datasources/proxy/uid/<uid>/<api path>` plus `query_param` matchers; the `datasources_json` fixture decides which uid the CLI must resolve.
- JSON contracts are pinned by exact sorted key sets (`health_datasources.rs:84`, `ux.rs:67`), not by field presence.
- Fixtures return data out of order on purpose (`logs.rs:40`, `metrics.rs:49`) to pin CLI-side sorting.

## ANTI-PATTERNS

- Never let a test touch the developer's real config or token cache: always a `TempDir` through `GCLI_CONFIG_DIR`/`GCLI_CACHE_DIR`.
- Never commit `qa/transcripts/*` or review their churn as part of a diff.
- No wall-clock assertions and no sleeps: concurrency is proven structurally by the Barrier rendezvous in `run_executes_profiles_concurrently` (`src/fanout.rs`), never by timing the binary; `--follow` is tested against a mock server that gates its replies, not against real time.
- No real network or credentials: every profile URL points at the wiremock server.
