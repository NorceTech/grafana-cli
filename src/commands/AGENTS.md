# src/commands/

## OVERVIEW

One module per command family; each owns its clap types and turns Grafana datasource-proxy JSON into a `CommandOutput`. Own file because it scores 12 and is a distinct domain: per-datasource wire formats and the stable JSON output contracts.

## WHERE TO LOOK

| Task                                                                                                        | File                          | Notes                                                                                                             |
| ----------------------------------------------------------------------------------------------------------- | ----------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| Output enum, fanout subcommands, shared window args and their one-time resolution, Prometheus/Loki envelope | `mod.rs`                      | `CommandOutput`, `Query` + `Query::resolve`, `Resolved::run`, `QueryWindow`, `Bounds`, `Windowed`, `success_data` |
| Loki: `search`, `labels`, `label_values`                                                                    | `logs.rs`                     | `loki_context` resolves the datasource once, bounds as ns strings; `log_records` parses `query_range`             |
| Prometheus: `instant`, `range`                                                                              | `metrics.rs`                  | `prom_result` guards the envelope, `flatten_matrix`                                                               |
| Tempo: `get`, `search`                                                                                      | `traces.rs`                   | `get` returns `Raw` OTLP; table = row per span                                                                    |
| `health`, `datasources`                                                                                     | `health.rs`, `datasources.rs` | no datasource resolution                                                                                          |
| Profile add/list/use/remove/set-token                                                                       | `profile.rs`                  | sync, works on `Config`, never fans out; `app.rs::profile_command` matches `ProfileCmd`                           |

## CONTRACT

- Query command: `pub async fn name(client: &GrafanaClient, profile: &str, mode: OutputMode, args: &NameArgs) -> crate::Result<CommandOutput>`, where `NameArgs` is the command's `#[derive(clap::Args)]` struct; a windowed command takes `Windowed<'_, NameArgs>` (the args plus the resolved `Bounds`) instead of `&NameArgs` (`labels` / `label_values` take `Windowed<'_, QueryWindow>` and the label directly). `profile` is only used to tag `LogRecord`s.
- Dispatch: `app::dispatch` calls `Query::resolve` once, before fanout, then `Resolved::run` once per profile through `fanout::run`; `Resolved::run` in `mod.rs` delegates to each family's `#[derive(clap::Subcommand)]` enum (`LogsCmd`, `MetricsCmd`, `TracesCmd`), whose `run(&self, client, profile, mode, bounds)` matches its variants. The bin flattens `Query` into its `Commands`.
- Pipeline: `client.datasources()` -> `datasource::resolve(&all, Kind::Loki | Kind::Prometheus | Kind::Tempo, flag)` -> `client.proxy_request(&ds.uid, "<api path>", &pairs)`, a `Reply` (JSON `body` plus the path, status and content type in `meta`) -> reshape `reply.body`.
- Proxy paths (relative, no leading slash): Loki `loki/api/v1/query_range`, `loki/api/v1/labels`, `loki/api/v1/label/{label}/values`; Prometheus `api/v1/query`, `api/v1/query_range`; Tempo `api/traces/{id}`, `api/search`. `proxy_request` is GET-only with query pairs.
- Wire time units: Loki takes ns strings (`to_ns_string`), Prometheus fractional epoch seconds (`to_epoch_secs`), Tempo search whole epoch seconds (start floored, end ceiled; Tempo answers a fraction with 400). Windows are resolved once per invocation: `Query::resolve` feeds the variant's `QueryWindow` (found by the exhaustive `Query::window` match) to `timeutil::resolve_window`, so a bad window is one config error before fanout and every profile queries the same `Bounds`. Commands never read `--since` / `--from` / `--to` themselves.
- The command shapes tables itself: return `CommandOutput::Table { headers, rows }` when `mode == OutputMode::Table`, else `Json` / `Logs` / `Raw`.
- Variants: `Logs` = `LogRecord`s rendered as JSONL; `Raw` = lossless upstream passthrough, printed exactly like `Json`; `Json` = reshaped value with stable field names.
- `labels` and `label_values` ignore `mode` and always return a JSON array; `[]` when Loki's success reply leaves `data` out, which it does for an empty result.

## CONVENTIONS

- Deterministic output: logs sort by (ns timestamp, line), never by the ms-truncated `ts` string (`logs.rs:159`); range series sort by `label_string` (sorted keys), samples by ts.
- Field names are README contracts: metrics `{metric, value, ts}` with values kept as upstream strings; traces search `{traceID, rootServiceName, rootTraceName, durationMs, start}`; datasources `{name, type, uid, url, isDefault}`.
- Errors: a 2xx reply of the wrong shape (no `data`, wrong `resultType`, a malformed stream or entry) is `reply.malformed(what)`, an `Error::Response` carrying the reply's status and naming its path, content type and first 200 chars. Prometheus and Loki replies pass `success_data` (`mod.rs`) first: `"status": "error"` is `Error::Http` with the upstream `error`, any other status is malformed. Bad `--at`/`--step` input is `Error::Config` (exit 2).
- Typed until the wire: datasource types are `datasource::Kind` and Loki's direction is `logs::Direction`; each becomes a string only via its `as_str()` where the request or the type filter is built.
- Recoverable data oddities (non-numeric sample ts, unknown trace shape) are skipped with `eprintln!("note: ...")`.
- Profile commands return JSON action objects (`{"action": "add", "profile", "auth"}`) and call `config.save()` themselves; only `list` honours `--output table`.

## ADDING A COMMAND

1. `#[derive(clap::Args)]` struct and function here per the contract; `#[command(flatten)]` a `QueryWindow` for time-windowed queries, whose function takes `Windowed<'_, NameArgs>`. `//` comments only on clap-derived items: `///` becomes help text.
2. Variant in the family's `*Cmd` enum plus its arm in that enum's `run` and in `Query::window` (`mod.rs`; the compiler flags a missing arm). A new family also needs `pub mod`, a `Query` variant and a `Resolved::run` arm in `mod.rs`.
3. No bin change: `Commands` flattens `Query`. Only commands that must not fan out (like `profile`, `completions`) get a variant in `src/cli.rs` and an arm in `src/app.rs::run`.
4. Unit tests for pure reshaping in the module; a wiremock scenario in `tests/`.

## ANTI-PATTERNS

- Never query an explicit `--datasource` of another core type, and never reject one of a plugin type. `datasource::resolve` matches uid or name, then `check_type` (`src/datasource.rs:38`) sorts the match by type. The command's own type is used silently. Another core type (`loki`, `prometheus`, `tempo`; a Loki datasource for `metrics range`, say) is `Error::Config` (exit 2) naming both types, raised before anything is proxied. Any other type is a plugin, queried anyway with one `note: datasource '<name>' is type <type>, expected <wanted>; querying it anyway` stderr line, because Prometheus-compatible plugins (VictoriaMetrics, Amazon Managed Prometheus) have their own type.
- No per-command HTTP or auth handling: `GrafanaClient::get_json` owns bearer/basic/OAuth headers, the single OAuth 401 retry and, through `Error::http`, the non-2xx error message (JSON `message`, else JSON `error`, else the plain-text body).
