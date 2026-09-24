# PROJECT KNOWLEDGE BASE

**Generated:** 2026-09-24
**Commit:** ca2c463
**Branch:** main

## OVERVIEW

`gcli` (crate `grafana-cli`): Rust 2024 CLI that queries Loki logs, Prometheus metrics and Tempo traces through Grafana's datasource proxy API, with named profiles, multi-profile fanout and a stable JSON/exit-code contract for scripts and agents. Stack: clap 4, tokio, reqwest 0.13 (rustls, no default features), serde_json, thiserror, comfy-table; wiremock in tests.

## STRUCTURE

```text
grafana-cli/
├── src/
│   ├── main.rs        # bin `gcli`; declares the three bin-only modules below
│   ├── app.rs         # [bin] Commands match; dispatch() runs a Query per profile; the --follow poll loop
│   ├── cli.rs         # [bin] clap root: global flags, Commands (flattens Query)
│   ├── render.rs      # [bin] every stdout/stderr write, BrokenPipe, exit code
│   ├── lib.rs         # lib `gcli_lib`: modules, Result alias, `Config` re-export
│   ├── commands/      # one family per submodule, clap types + run (own AGENTS.md); prom.rs = shared Prometheus/Loki wire format
│   ├── client.rs      # GrafanaClient: auth headers, OAuth 401 retry, proxy GET
│   ├── config.rs      # config.toml model, path, load/save, profile resolution
│   ├── environment.rs # Environment (the only env reader), ProfileOverrides
│   ├── auth/          # mod.rs AuthProvider + from_profile, oauth.rs token fetch, token_cache.rs tokens.json (TokenKey, locked store/remove/forget), secret.rs *_command helpers (single element sh -c, multi-element argv)
│   ├── secret_file.rs # write_secret: atomic 0600 writes of config.toml and tokens.json
│   ├── fanout.rs      # --profile a,b / --all-profiles, overrides, JoinSet, exit aggregation
│   ├── datasource.rs  # Kind enum; pick: flag > sole of type > unique default; core-type check
│   ├── error.rs       # Error -> JSON error kind + exit code
│   ├── output.rs      # OutputMode (TTY auto), LogRecord, JSONL, comfy-table
│   └── timeutil.rs    # window parsing; ns vs epoch-second encoders; default step
├── tests/             # black-box suite against wiremock (own AGENTS.md)
├── skills/gcli/       # distributable agent skill (mirrors the README contract)
├── .cargo/config.toml # -Zthreads=14: nightly rustc only
├── .github/workflows/ # ci.yml: nix flake check per platform + Windows cross build, cached /nix/store; release.yml: v* tag -> verify, CI, publish
└── flake.nix          # crane: packages.default (static musl on Linux) and gcli-windows (mingw cross), checks (build, clippy, tests, treefmt), dev shell: fenix nightly, rust-analyzer, treefmt hook
```

## WHERE TO LOOK

| Task                                              | Location                                    | Notes                                                                                                                          |
| ------------------------------------------------- | ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| Add or change a command                           | `src/commands/`                             | checklist in `src/commands/AGENTS.md`; `src/cli.rs` + `src/app.rs` only for commands that don't fan out (profile, completions) |
| Global flag or per-invocation override            | `Cli` in `src/cli.rs`, `src/environment.rs` | flags are `global = true`; overrides are `ProfileOverrides`                                                                    |
| Rendering, multi-profile wrapping, follow batches | `src/render.rs`                             | the only stdout writer; the follow poll loop is `app::follow`                                                                  |
| New error kind or exit code                       | `src/error.rs`                              | `emit_json_error` + `exit_code` + their tests + README table                                                                   |
| Profile selection, env vars                       | `src/{environment,fanout,config}.rs`        | see NOTES                                                                                                                      |
| Auth modes, OAuth token cache, secret helpers     | `src/{auth/,client.rs}`                     | `*_command` semantics live in `auth/secret.rs`                                                                                 |
| Time windows, default `--step`                    | `src/timeutil.rs`                           | resolved once per invocation, before fanout, by `Query::resolve` (`src/commands/mod.rs`)                                       |
| Prometheus/Loki shared result shapes              | `src/commands/prom.rs`                      | vector/matrix flattening used by both families                                                                                 |
| Integration tests                                 | `tests/`                                    | own AGENTS.md                                                                                                                  |
| User-facing contract                              | `README.md`, `skills/gcli/SKILL.md`         | sections Output modes, Errors and exit codes, For AI agents                                                                    |

## CODE MAP

Refs are `rg -w` word matches across `src/` and `tests/` (definitions, `use` lines and comments included), so treat them as approximate.

| Symbol                         | Type   | Location                  | Refs     | Role                                                                                                                            |
| ------------------------------ | ------ | ------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `CommandOutput`                | enum   | `src/commands/mod.rs:17`  | ~88      | every command's result; `render.rs` matches on it                                                                               |
| `OutputMode`                   | enum   | `src/output.rs:6`         | ~66      | json/table, resolved once in `app::run`                                                                                         |
| `Config`                       | struct | `src/config.rs:39`        | ~38      | config.toml model; profile CRUD and resolution                                                                                  |
| `GrafanaClient`                | struct | `src/client.rs:102`       | ~49      | HTTP facade: `health`, `datasources`, `datasource_health`, `proxy_request`                                                      |
| `AuthProvider`                 | enum   | `src/auth/mod.rs:34`      | ~37      | Token / Basic / OAuth, built by `auth::from_profile`                                                                            |
| `resolve_window`               | fn     | `src/timeutil.rs:4`       | ~22      | `--since` / `--from` / `--to` -> UTC bounds                                                                                     |
| `Datasource`                   | struct | `src/client.rs:9`         | ~25      | `/api/datasources` row fed to `datasource::resolve`                                                                             |
| `Query`                        | enum   | `src/commands/mod.rs:88`  | ~34      | clap subcommands that fan out; `Query::resolve` fixes the window once, `Resolved::run` delegates to each family's `run`         |
| `QueryWindow`                  | struct | `src/commands/mod.rs:47`  | ~26      | clap `Args`: window + `--datasource`, flattened into query subcommands                                                          |
| `resolve_profiles`             | fn     | `src/fanout.rs:10`        | ~15      | decides which profiles run                                                                                                      |
| `emit_json_error`, `exit_code` | fn     | `src/error.rs:86`, `:110` | ~11, ~10 | the error contract                                                                                                              |
| `dispatch`                     | fn     | `src/app.rs:106`          | ~4       | config -> profiles -> overrides -> `Query::resolve` -> `BaseUrl::parse` -> auth -> client -> `Resolved::run` (fanout) -> render |

## CONVENTIONS

- One package, two crates: lib `gcli_lib` holds everything reusable; bin `gcli` is `main.rs` plus `app.rs`, `cli.rs`, `render.rs`, declared in `main.rs`, not `lib.rs`. In those three files `crate::` is the bin and library items are `gcli_lib::...`; integration tests can only reach `gcli_lib` or spawn the binary.
- Clap types live in the lib beside the code that consumes them (`*Cmd` / `*Args` in `commands/`, `QueryWindow`, `datasource::Kind`, `OutputMode`); the bin keeps only `Cli`, `Commands` and `Shell`. Clap-derived items take `//` comments, never `///`: clap turns doc comments into help text.
- Lib code never prints data: commands return `CommandOutput` and the bin renders it. Diagnostics go to stderr.
- Fallible lib code returns `crate::Result<T>` with the thiserror `Error`; the variant decides the JSON `kind` and the exit code. `anyhow` is not a dependency; don't introduce it.
- Bad user input is `Error::Config` (exit 2); transport failures are `Transport` (kind `io`, reqwest's error kept as `source`), non-2xx replies `Http`, 2xx replies gcli cannot use (not JSON, wrong shape) `Response`, other I/O `Io`, bad profile urls `Url` (`client::BaseUrl::parse`: absolute http/https only, checked in `profile add` and before the client is built) (all exit 1).
- Deterministic output: `BTreeMap` for profiles and labels, explicit sorts before rendering.
- `config.toml` and `tokens.json` are written only through `secret_file::write_secret`: a fresh 0600 sibling temp file, fsynced, then renamed over the target, so the file is never partial and never wider than 0600; missing config and cache directories are created 0700.
- Unit tests sit in `#[cfg(test)]` modules and never touch process env: `Environment::from_process`, called once in `app::run`, is the only env read, and lib functions take the profile name, overrides and directories as arguments, so tests pass explicit values and tempdir paths.

## ANTI-PATTERNS (THIS PROJECT)

- Never write to stdout outside `render.rs::write_stdout`; errors never touch stdout.
- Never change the error line shape: exactly `{"error":{"profile","kind","status","message"}}` (pinned by `json_error_shape_is_exact`).
- Never make a partial multi-profile failure exit non-zero: 0 if any profile succeeded, 1 only when all failed (`aggregate_exit_code`).
- Never "fix" BrokenPipe -> silent exit 0 in `write_stdout`; `tests/pipe.rs` pins it.
- Never add prompts, pagers or spinners: the README promises a non-interactive CLI.
- Never change README-documented JSON fields, error kinds, exit codes or defaults as a side effect; the README and `skills/gcli/SKILL.md` are the public contract.

## UNIQUE STYLES

- Output mode: `--output` wins, else table on a TTY and JSON when piped.
- Multi-profile runs: JSON wraps each result as `{"data":...,"profile":...}` except log records, which already carry `profile` and stay plain JSONL; table mode prefixes each block with `profile: <name>`; per-profile errors go to stderr.
- Profile management (`profile_command` in `app.rs`) bypasses `dispatch` and fanout entirely; so does `logs search --follow` (see NOTES).

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
nix build                    # the release binary, in result/bin
nix flake check              # what CI runs: package, clippy -D warnings, tests, treefmt
```

## NOTES

- Profile selection: `--profile a,b` (order kept) or `--all-profiles` (sorted; clap makes them conflict), else `GRAFANA_CLI_PROFILE` (one name, no CSV), else `current_profile`.
- `GRAFANA_CLI_{URL,TOKEN,USERNAME,PASSWORD}` override the selected profile and the `--url/--token/--username/--password` flags win over them, but only when exactly one profile is selected: with several (`--profile a,b`, `--all-profiles`) any set override is `Error::Config` (exit 2) naming it, so one host never receives every profile's credentials. `app::dispatch` calls `fanout::apply_overrides` after `resolve_profiles`, which (like `Config::active_profile`) returns profiles as stored.
- Env vars are read by hand, only in `Environment::from_process` (`src/environment.rs`), and do not appear in `--help`.
- `auth::from_profile` order is oauth > token/token_command > basic, so `--token` cannot turn an OAuth profile into token auth.
- Secret flags given `-` (global `--token`/`--password`, `profile add --token/--password/--client-secret`, `profile set-token --token`) read the secret from stdin once, in `app::resolve_stdin_secrets` before any command runs; `auth::secret_from_stdin` trims and rejects empty as a config error. The credential flags are clap globals mirrored onto `profile add`'s own flags, so the profile commands resolve their fields and never the globals, and stdin is read exactly once.
- `*_command` secrets: a single-element array runs as `sh -c <line>` (tilde expansion and pipes), two or more elements execute directly as argv without a shell, so elements with spaces survive. Every run has a null stdin and the global `--timeout` budget (an elapsed budget kills the child); a non-zero exit, empty output or output with control characters after trimming is an `Error::Auth` naming the command, and a failure also quotes the last ~200 chars of its stderr.
- HTTP requests are bounded by the global `--timeout` (default `60s`, humantime, parsed once in `app::run`; an unparsable value is a config error): the shared reqwest client gets `connect_timeout(10s)` plus `read_timeout(timeout)`, an idle timer that resets on every read, so a slow but streaming response completes while a silent server fails as `Transport` (kind io, exit 1). No retries except the single OAuth 401 refetch; fanout spawns one task per profile with no concurrency cap.
- Proxies are env-only: reqwest is built without `macos-system-configuration`, so its default matcher reads `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY` and nothing else; operating-system proxy settings are never consulted.
- `logs search --follow` is intercepted in `app::dispatch` before fanout: exactly one profile or a config error naming `--profile`/`--all-profiles`. It prints the window batch (exactly `search`'s output), then polls every `--interval` from the newest printed nanosecond; `FollowCursor` (`commands/logs/follow.rs`) dedups the boundary nanosecond by (labels, line). SIGINT/SIGTERM or a closed stdout end the run with exit 0; a failed poll ends it with that error's JSON and code.
- Metric results share one shape: `logs search` matrix replies, `logs instant` and `metrics instant`/`range` flatten through `commands/prom.rs` into `{metric, value, ts}` records. `metrics range`'s default step is `max(15, ceil(window/1000))` seconds (`timeutil::default_step_secs`); `logs search --step` is forwarded to Loki in whole seconds or omitted.
- Arguments that become proxy path segments (label names, trace ids, tags) are validated in `Query::validate_path_args` before fanout: one config error however many profiles are selected, and nothing is requested.
- `datasources --check` calls Grafana's per-datasource health endpoint for each listed datasource; a failing health call becomes `"health":"ERROR"` plus the error text in that record, never a command failure.
- `tokens.json` read-modify-writes (store after a fetch, invalidate after a 401) go through `auth::token_cache::update`, which holds an in-process mutex plus an exclusive lock on the `tokens.json.lock` sidecar, so parallel profiles and parallel gcli processes keep each other's tokens. Plain lookups take no lock: every write is an atomic rename.
- Cached tokens sit under `<profile>:<client_id>` in `tokens.json` (tests pin that layout), but each entry stores its full `TokenKey` (profile, client_id, token_url, audience, scopes) and is reused only on an exact match; `profile remove` forgets the profile's tokens. A cache written before entries carried their key reads as empty.
- Nix builds go through crane: dependencies build once per `Cargo.toml`/`Cargo.lock`/`.cargo/config.toml` state, and each derivation's fileset holds only what it reads (the binary gets the manifests and `src/`; clippy and tests add `tests/*.rs`), so doc edits rebuild no Rust derivation and a new test file never rebuilds the dependencies. Linux packages build through `pkgsStatic` (static musl), and `allowedReferences = [ ]` fails any build whose binary still points into `/nix/store`; on macOS std's libiconv is rewritten to `/usr/lib`. `packages.gcli-windows` cross-compiles from any system through a native crane pointed at mingw (crane's cross mode would add wine), links libgcc and winpthreads statically, and fails if `gcli.exe` imports a mingw `lib*.dll`. `checks.gcli-test` runs the suite in the release profile inside the Nix build sandbox, which allows loopback networking only, so tests stay on wiremock at 127.0.0.1 and temp dirs.
- Releases: bump `version` in `Cargo.toml`, let `cargo check` update `Cargo.lock`, commit and push main, then push an annotated tag `v<version>` on that commit. `release.yml` rejects a tag that is off main or disagrees with `Cargo.toml`, reruns CI (with main's cached store it rebuilds nothing main already built) and publishes `gcli-linux-x86_64`, `gcli-linux-aarch64`, `gcli-macos-aarch64`, `gcli-windows-x86_64.exe` and `checksums.txt`. Only main saves CI caches.
