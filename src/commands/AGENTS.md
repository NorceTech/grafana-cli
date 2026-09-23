# src/commands/

## OVERVIEW

One module per command family; each turns Grafana datasource-proxy JSON into a `CommandOutput`. Own file because it scores 12 and is a distinct domain: per-datasource wire formats and the stable JSON output contracts.

## WHERE TO LOOK

| Task                                         | File                          | Notes                                               |
| -------------------------------------------- | ----------------------------- | --------------------------------------------------- |
| Output enum, shared window args, stub guards | `mod.rs`                      | `CommandOutput`, `QueryWindow`, `*_are_implemented` |
| Loki: `search`, `labels`, `label_values`     | `logs.rs`                     | `loki_context` resolves window + datasource once    |
| Prometheus: `instant`, `range`               | `metrics.rs`                  | `prom_result` guards the envelope, `flatten_matrix` |
| Tempo: `get`, `search`                       | `traces.rs`                   | `get` returns `Raw`; table falls back to raw JSON   |
| `health`, `datasources`                      | `health.rs`, `datasources.rs` | no datasource resolution                            |
| Profile add/list/use/remove/set-token        | `profile.rs`                  | sync, works on `Config`, never fans out             |

## CONTRACT

- Query command: `pub async fn name(client: &GrafanaClient, profile: &str, mode: OutputMode, ...) -> crate::Result<CommandOutput>`. The bin crate's `src/app.rs` runs it once per profile through `fanout::run`; `profile` is only used to tag `LogRecord`s.
- Pipeline: `client.datasources()` -> `datasource::resolve(&all, "loki" | "prometheus" | "tempo", flag)` -> `client.proxy_request(&ds.uid, "<api path>", &pairs)` -> reshape.
- Proxy paths (relative, no leading slash): Loki `loki/api/v1/query_range`, `loki/api/v1/labels`, `loki/api/v1/label/{label}/values`; Prometheus `api/v1/query`, `api/v1/query_range`; Tempo `api/traces/{id}`, `api/search`. `proxy_request` is GET-only with query pairs.
- Wire time units: Loki takes ns strings (`to_ns_string`), Prometheus fractional epoch seconds (`to_epoch_secs`), Tempo search whole epoch seconds (start floored, end ceiled; Tempo answers a fraction with 400). Windows always come from `timeutil::resolve_window`.
- The command shapes tables itself: return `CommandOutput::Table { headers, rows }` when `mode == OutputMode::Table`, else `Json` / `Logs` / `Raw`.
- Variants: `Logs` = `LogRecord`s rendered as JSONL; `Raw` = lossless upstream passthrough, printed exactly like `Json`; `Json` = reshaped value with stable field names.
- `labels` and `label_values` ignore `mode` and always return a JSON array.

## CONVENTIONS

- Deterministic output: logs sort by (ns timestamp, line), never by the ms-truncated `ts` string (`logs.rs:71`); range series sort by `label_string` (sorted keys), samples by ts.
- Field names are README contracts: metrics `{metric, value, ts}` with values kept as upstream strings; traces search `{traceID, rootServiceName, rootTraceName, durationMs, start}`; datasources `{name, type, uid, url, isDefault}`.
- Errors: Loki shape errors are `Error::Http { status: 200, message: "loki response parse: ..." }` (`logs.rs:13`); a Prometheus `status != "success"` is `Error::Http` carrying the upstream `error`; missing `data`, wrong `resultType` and bad `--at`/`--step` input are `Error::Config` (exit 2).
- Recoverable data oddities (non-numeric sample ts, unknown trace shape) are skipped with `eprintln!("note: ...")`.
- Profile commands return JSON action objects (`{"action": "add", "profile", "auth"}`) and call `config.save()` themselves; only `list` honours `--output table`.

## ADDING A COMMAND

1. Function here per the contract (a new module also needs `pub mod` in `mod.rs`); reuse `QueryWindow` for time-windowed queries.
2. Variant in `src/cli.rs` (`TimeArgs` via `#[command(flatten)]`, converted with `query_window`).
3. Arm in `src/app.rs::run` that clones its args into the `dispatch` closure.
4. Add the call to `query_commands_are_implemented` (or the profile twin) in `mod.rs`.
5. Unit tests for pure reshaping in the module; a wiremock scenario in `tests/`.

## ANTI-PATTERNS

- Never ship a command returning `Error::Unimplemented`: the `mod.rs` guard tests fail on it, and exit code 3 is reserved for stubs.
- An explicit `--datasource` matches uid or name with NO type check (`src/datasource.rs:9`); don't count on it to reject a wrong-type datasource.
- No per-command HTTP or auth handling: `GrafanaClient::get_json` owns bearer/basic/OAuth headers, the single OAuth 401 retry and Grafana `message` extraction.
