# tests/

## OVERVIEW

Black-box integration suite: every test spawns the compiled `gcli` binary against a wiremock Grafana. Own file because it scores 10 and is a distinct domain; unit tests stay in `#[cfg(test)]` modules under `src/`.

## WHERE TO LOOK

| Scenario                                                  | File                    | IDs    |
| --------------------------------------------------------- | ----------------------- | ------ |
| profile add/list/use/set-token/remove, profile errors     | `config_cli.rs`         | s1     |
| health; datasources (`--type`, table)                     | `health_datasources.rs` | s2, s3 |
| logs search, labels, label-values                         | `logs.rs`               | s4, s5 |
| metrics instant/range, epoch wire format                  | `metrics.rs`            | s6     |
| traces get/search/404                                     | `traces.rs`             | s7     |
| multi-profile fanout, exit codes, concurrency             | `fanout_e2e.rs`         | s8a-e  |
| basic auth header; OAuth fetch/reuse/expiry/401 retry     | `auth.rs`               | s9-s10 |
| output modes, error JSON schema, help/version/completions | `ux.rs`                 | s11-12 |
| broken pipe exits 0 (real `bash -c` pipe to `head -c 1`)  | `pipe.rs`               | -      |

Shared harness, `common/mod.rs`:

- `spawn_gcli(args, envs)`: runs `CARGO_BIN_EXE_gcli` from the manifest dir after stripping `GCLI_CONFIG_DIR`, `GCLI_CACHE_DIR` and every `GRAFANA_CLI_*` var. Pass `("GCLI_CONFIG_DIR", dir)` through `envs`, plus `GCLI_CACHE_DIR` for OAuth tests (`auth.rs:25`).
- `fixture_config(toml)`: `TempDir` holding `config.toml`; keep it bound until the spawn returns.
- Mocks: `health_mock`, `health_auth_mock(authorization)`, `token_endpoint_mock(token, expires_in)` (POST `/token`), `datasources_json(body)` (GET `/api/datasources`).
- `write_transcript(name, cmd, &out)`, `write_transcript_multi(name, body)`, `transcript_block`: write `qa/transcripts/<name>` with command, stdout, stderr and exit code. `qa/` is gitignored and rewritten by every `cargo test`.

## CONVENTIONS

- Test shape: `#[tokio::test] async fn sN_<case>` -> `MockServer::start().await` -> mount mocks with `.expect(n)` -> `fixture_config` -> `spawn_gcli` -> assert the exit code with stderr in the failure message -> assert stdout JSON -> `server.verify().await` -> `write_transcript("sN_<case>.txt", "gcli ...", &out)`.
- A new scenario takes the next scenario ID and writes its own transcript file.
- `common/mod.rs` is frozen (`pipe.rs:9`). New helpers (config TOML builders, bespoke mocks, `stdout_json`) go in the file that needs them; each file already carries its own `token_config`-style builder.
- Derive, don't trust: build expected wire params with `gcli_lib::timeutil` (`resolve_window`, `to_ns_string`, `to_epoch_secs`), or chrono `timestamp()` for Tempo's whole-second bounds, never hand-computed epochs (`logs.rs:11`, `metrics.rs:12`, `traces.rs:12`).
- Datasource calls are mocked on the full proxy path `/api/datasources/proxy/uid/<uid>/<api path>` plus `query_param` matchers; the `datasources_json` fixture decides which uid the CLI must resolve.
- JSON contracts are pinned by exact sorted key sets (`health_datasources.rs:76`, `ux.rs:71`), not by field presence.
- Fixtures return data out of order on purpose (`logs.rs:40`, `metrics.rs:49`) to pin CLI-side sorting.

## ANTI-PATTERNS

- Never let a test touch the developer's real config or token cache: always a `TempDir` through `GCLI_CONFIG_DIR`/`GCLI_CACHE_DIR`.
- Never commit `qa/transcripts/*` or review their churn as part of a diff.
- Do not tighten `PARALLELISM_BUDGET_MS` (`fanout_e2e.rs:9`: 450 ms for two profiles delayed 250 ms each). It is the suite's only wall-clock assertion and exists to prove concurrency.
- No real network or credentials: every profile URL points at the wiremock server.
