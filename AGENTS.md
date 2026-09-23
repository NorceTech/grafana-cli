# PROJECT KNOWLEDGE BASE

**Generated:** 2026-09-23
**Commit:** 1fad86e
**Branch:** main

## OVERVIEW

`gcli` (crate `grafana-cli`): Rust 2024 CLI that queries Loki logs, Prometheus metrics and Tempo traces through Grafana's datasource proxy API, with named profiles, multi-profile fanout and a stable JSON/exit-code contract for scripts and agents. Stack: clap 4, tokio, reqwest 0.13 (rustls), serde_json, thiserror, comfy-table; wiremock in tests.

## STRUCTURE

```text
grafana-cli/
├── src/
│   ├── main.rs        # bin `gcli`; declares the three bin-only modules below
│   ├── app.rs         # [bin] Commands match; dispatch() runs a Query per profile
│   ├── cli.rs         # [bin] clap root: global flags, Commands (flattens Query)
│   ├── render.rs      # [bin] every stdout/stderr write, BrokenPipe, exit code
│   ├── lib.rs         # lib `gcli_lib`: modules, flat re-exports
│   ├── commands/      # one module per command family: clap types + run (own AGENTS.md)
│   ├── client.rs      # GrafanaClient: auth headers, OAuth 401 retry, proxy GET
│   ├── config.rs      # config.toml model, path, load/save, profile resolution
│   ├── environment.rs # Environment (the only env reader), ProfileOverrides
│   ├── auth.rs        # AuthProvider, *_command via sh -c, OAuth token fetch
│   ├── token_cache.rs # OAuth tokens.json: path, lookup, store, remove
│   ├── fanout.rs      # --profile a,b / --all-profiles, overrides, JoinSet, exit aggregation
│   ├── datasource.rs  # Kind enum; pick: flag > sole of type > unique default
│   ├── error.rs       # Error -> JSON error kind + exit code
│   ├── output.rs      # OutputMode (TTY auto), LogRecord, JSONL, comfy-table
│   └── timeutil.rs    # window parsing; ns vs epoch-second encoders
├── tests/             # black-box suite against wiremock (own AGENTS.md)
├── .cargo/config.toml # -Zthreads=14: nightly rustc only
└── flake.nix          # dev shell: fenix nightly, rust-analyzer, treefmt hook
```

## WHERE TO LOOK

| Task                                   | Location                                    | Notes                                                                                                   |
| -------------------------------------- | ------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Add or change a command                | `src/commands/`                             | checklist in `src/commands/AGENTS.md`; `src/cli.rs` + `src/app.rs` only for commands that don't fan out |
| Global flag or per-invocation override | `Cli` in `src/cli.rs`, `src/environment.rs` | flags are `global = true`; overrides are `ProfileOverrides`                                             |
| Rendering, multi-profile wrapping      | `src/render.rs`                             | the only stdout writer                                                                                  |
| New error kind or exit code            | `src/error.rs`                              | `emit_json_error` + `exit_code` + their tests + README table                                            |
| Profile selection, env vars            | `src/{environment,fanout,config}.rs`        | see NOTES                                                                                               |
| Auth modes, OAuth token cache          | `src/{auth,token_cache,client}.rs`          |                                                                                                         |
| Time windows                           | `src/timeutil.rs`                           |                                                                                                         |
| Integration tests                      | `tests/`                                    | own AGENTS.md                                                                                           |
| User-facing contract                   | `README.md`                                 | sections Output modes, Errors and exit codes, For AI agents                                             |

## CODE MAP

Refs = ast-grep AST-node references across `src/` and `tests/`, definition excluded (rust-analyzer returned no references).

| Symbol                         | Type   | Location                 | Refs | Role                                                                                 |
| ------------------------------ | ------ | ------------------------ | ---- | ------------------------------------------------------------------------------------ |
| `CommandOutput`                | enum   | `src/commands/mod.rs:13` | 64   | every command's result; `render.rs` matches on it                                    |
| `OutputMode`                   | enum   | `src/output.rs:6`        | 53   | json/table, resolved once in `app::run`                                              |
| `Config`                       | struct | `src/config.rs:38`       | ~31  | config.toml model; profile CRUD and resolution                                       |
| `GrafanaClient`                | struct | `src/client.rs:17`       | 31   | HTTP facade: `health`, `datasources`, `proxy_request`                                |
| `AuthProvider`                 | enum   | `src/auth.rs:7`          | 30   | Token / Basic / OAuth, built by `auth::from_profile`                                 |
| `resolve_window`               | fn     | `src/timeutil.rs:4`      | 20   | `--since` / `--from` / `--to` -> UTC bounds                                          |
| `Datasource`                   | struct | `src/client.rs:7`        | 20   | `/api/datasources` row fed to `datasource::resolve`                                  |
| `Query`                        | enum   | `src/commands/mod.rs:40` | 18   | clap subcommands that fan out; `Query::run` delegates to each family's `run`         |
| `QueryWindow`                  | struct | `src/commands/mod.rs:26` | 15   | clap `Args`: window + `--datasource`, flattened into query subcommands               |
| `resolve_profiles`             | fn     | `src/fanout.rs:8`        | 9    | decides which profiles run                                                           |
| `emit_json_error`, `exit_code` | fn     | `src/error.rs:33`, `:57` | 9, 9 | the error contract                                                                   |
| `dispatch`                     | fn     | `src/app.rs:38`          | 1    | config -> profiles -> overrides -> auth -> client -> `Query::run` (fanout) -> render |

## CONVENTIONS

- One package, two crates: lib `gcli_lib` holds everything reusable; bin `gcli` is `main.rs` plus `app.rs`, `cli.rs`, `render.rs`, declared in `main.rs`, not `lib.rs`. In those three files `crate::` is the bin and library items are `gcli_lib::...`; integration tests can only reach `gcli_lib` or spawn the binary.
- Clap types live in the lib beside the code that consumes them (`*Cmd` / `*Args` in `commands/`, `QueryWindow`, `datasource::Kind`, `OutputMode`); the bin keeps only `Cli`, `Commands` and `Shell`. Clap-derived items take `//` comments, never `///`: clap turns doc comments into help text.
- Lib code never prints data: commands return `CommandOutput` and the bin renders it. Diagnostics go to stderr.
- Fallible lib code returns `crate::Result<T>` with the thiserror `Error`; the variant decides the JSON `kind` and the exit code. `anyhow` is in Cargo.toml but unused; don't introduce it.
- Bad user input is `Error::Config` (exit 2); transport, HTTP and upstream-parse failures are `Http` / `Io` / `Url` (exit 1).
- Deterministic output: `BTreeMap` for profiles and labels, explicit sorts before rendering.
- `config.toml` and `tokens.json` get a best-effort chmod 0600 after every write.
- Unit tests sit in `#[cfg(test)]` modules and never touch process env: `Environment::from_process`, called once in `app::run`, is the only env read, and lib functions take the profile name, overrides and directories as arguments, so tests pass explicit values and tempdir paths.

## ANTI-PATTERNS (THIS PROJECT)

- Never write to stdout outside `render.rs::write_stdout`; errors never touch stdout.
- Never change the error line shape: exactly `{"error":{"profile","kind","status","message"}}` (pinned by `json_error_shape_is_exact`).
- Never make a partial multi-profile failure exit non-zero: 0 if any profile succeeded, 1 only when all failed (`aggregate_exit_code`).
- Never "fix" BrokenPipe -> silent exit 0 in `write_stdout`; `tests/pipe.rs` pins it.
- Never add prompts, pagers or spinners: the README promises a non-interactive CLI.
- Never change README-documented JSON fields, error kinds, exit codes or defaults as a side effect; the README is the public contract.

## UNIQUE STYLES

- Output mode: `--output` wins, else table on a TTY and JSON when piped.
- Multi-profile runs: JSON wraps each result as `{"data":...,"profile":...}` except log records, which already carry `profile` and stay plain JSONL; table mode prefixes each block with `profile: <name>`; per-profile errors go to stderr.
- Profile management (`profile_command` in `app.rs`) bypasses `dispatch` and fanout entirely.

## COMMANDS

```bash
nix develop                  # fenix nightly toolchain; stable rustc rejects -Zthreads
cargo build
cargo run -q -- --help
cargo test                   # unit + integration; rewrites qa/transcripts/ (gitignored)
cargo test --lib             # unit tests only
cargo test --test logs s4a   # one integration file, filtered by scenario id
cargo clippy --all-targets
nix fmt                      # treefmt: rustfmt + nixfmt + prettier (also the pre-commit hook)
```

## NOTES

- Profile selection: `--profile a,b` (order kept) or `--all-profiles` (sorted; clap makes them conflict), else `GRAFANA_CLI_PROFILE` (one name, no CSV), else `current_profile`.
- `GRAFANA_CLI_{URL,TOKEN,USERNAME,PASSWORD}` override every selected profile and the `--url/--token/--username/--password` flags win over them: `app::dispatch` calls `fanout::apply_overrides` after `resolve_profiles`, which (like `Config::active_profile`) returns profiles as stored.
- clap's `env` feature is enabled but unused: env vars are read by hand, only in `Environment::from_process` (`src/environment.rs`), and do not appear in `--help`.
- `auth::from_profile` order is oauth > token/token_command > basic, so `--token` cannot turn an OAuth profile into token auth.
- `*_command` secrets run as `sh -c "<args joined by spaces>"` at request time; a non-zero exit or empty output is an auth error.
- No HTTP timeouts and no retries except the single OAuth 401 refetch; fanout spawns one task per profile with no concurrency cap.
