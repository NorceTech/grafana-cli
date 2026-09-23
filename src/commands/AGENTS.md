# src/commands/

## OVERVIEW

One module per command family; each owns its clap types and turns Grafana datasource-proxy JSON into a `CommandOutput`. Own file because it scores 12 and is a distinct domain: per-datasource wire formats and the stable JSON output contracts.

## WHERE TO LOOK

| Task                                                | File                          | Notes                                                                                   |
| --------------------------------------------------- | ----------------------------- | --------------------------------------------------------------------------------------- |
| Output enum, fanout subcommands, shared window args | `mod.rs`                      | `CommandOutput`, `Query` + `Query::run`, `QueryWindow`                                  |
| Loki: `search`, `labels`, `label_values`            | `logs.rs`                     | `loki_context` resolves window + datasource once                                        |
| Prometheus: `instant`, `range`                      | `metrics.rs`                  | `prom_result` guards the envelope, `flatten_matrix`                                     |
| Tempo: `get`, `search`                              | `traces.rs`                   | `get` returns `Raw` OTLP; table = row per span                                          |
| `health`, `datasources`                             | `health.rs`, `datasources.rs` | no datasource resolution                                                                |
| Profile add/list/use/remove/set-token               | `profile.rs`                  | sync, works on `Config`, never fans out; `app.rs::profile_command` matches `ProfileCmd` |

## CONTRACT

- Query command: `pub async fn name(client: &GrafanaClient, profile: &str, mode: OutputMode, args: &NameArgs) -> crate::Result<CommandOutput>`, where `NameArgs` is the command's `#[derive(clap::Args)]` struct (`labels` / `label_values` take their one or two inputs directly). `profile` is only used to tag `LogRecord`s.
- Dispatch: each family's `#[derive(clap::Subcommand)]` enum (`LogsCmd`, `MetricsCmd`, `TracesCmd`) has `run(&self, client, profile, mode)` matching its variants; `Query::run` in `mod.rs` delegates per family; the bin flattens `Query` into its `Commands` and `app::dispatch` calls `Query::run` once per profile through `fanout::run`.
- Pipeline: `client.datasources()` -> `datasource::resolve(&all, Kind::Loki | Kind::Prometheus | Kind::Tempo, flag)` -> `client.proxy_request(&ds.uid, "<api path>", &pairs)` -> reshape.
- Proxy paths (relative, no leading slash): Loki `loki/api/v1/query_range`, `loki/api/v1/labels`, `loki/api/v1/label/{label}/values`; Prometheus `api/v1/query`, `api/v1/query_range`; Tempo `api/traces/{id}`, `api/search`. `proxy_request` is GET-only with query pairs.
- Wire time units: Loki takes ns strings (`to_ns_string`), Prometheus fractional epoch seconds (`to_epoch_secs`), Tempo search whole epoch seconds (start floored, end ceiled; Tempo answers a fraction with 400). Windows always come from `timeutil::resolve_window`.
- The command shapes tables itself: return `CommandOutput::Table { headers, rows }` when `mode == OutputMode::Table`, else `Json` / `Logs` / `Raw`.
- Variants: `Logs` = `LogRecord`s rendered as JSONL; `Raw` = lossless upstream passthrough, printed exactly like `Json`; `Json` = reshaped value with stable field names.
- `labels` and `label_values` ignore `mode` and always return a JSON array.

## CONVENTIONS

- Deterministic output: logs sort by (ns timestamp, line), never by the ms-truncated `ts` string (`logs.rs:124`); range series sort by `label_string` (sorted keys), samples by ts.
- Field names are README contracts: metrics `{metric, value, ts}` with values kept as upstream strings; traces search `{traceID, rootServiceName, rootTraceName, durationMs, start}`; datasources `{name, type, uid, url, isDefault}`.
- Errors: Loki shape errors are `Error::Http { status: 200, message: "loki response parse: ..." }` (`logs.rs:69`); a Prometheus `status != "success"` is `Error::Http` carrying the upstream `error`; missing `data`, wrong `resultType` and bad `--at`/`--step` input are `Error::Config` (exit 2).
- Typed until the wire: datasource types are `datasource::Kind` and Loki's direction is `logs::Direction`; each becomes a string only via its `as_str()` where the request or the type filter is built.
- Recoverable data oddities (non-numeric sample ts, unknown trace shape) are skipped with `eprintln!("note: ...")`.
- Profile commands return JSON action objects (`{"action": "add", "profile", "auth"}`) and call `config.save()` themselves; only `list` honours `--output table`.

## ADDING A COMMAND

1. `#[derive(clap::Args)]` struct and function here per the contract; `#[command(flatten)]` a `QueryWindow` for time-windowed queries. `//` comments only on clap-derived items: `///` becomes help text.
2. Variant in the family's `*Cmd` enum plus its arm in that enum's `run`. A new family also needs `pub mod`, a `Query` variant and a `Query::run` arm in `mod.rs`.
3. No bin change: `Commands` flattens `Query`. Only commands that must not fan out (like `profile`, `completions`) get a variant in `src/cli.rs` and an arm in `src/app.rs::run`.
4. Unit tests for pure reshaping in the module; a wiremock scenario in `tests/`.

## ANTI-PATTERNS

- An explicit `--datasource` matches uid or name with NO type check (`src/datasource.rs:28`); don't count on it to reject a wrong-type datasource.
- No per-command HTTP or auth handling: `GrafanaClient::get_json` owns bearer/basic/OAuth headers, the single OAuth 401 retry and Grafana `message` extraction.
