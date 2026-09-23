# gcli

A command line client for querying Grafana, built for humans and AI agents. It queries logs (Loki), metrics (Prometheus) and traces (Tempo) through Grafana's datasource proxy API, so one URL and one set of credentials covers every datasource on the instance. Named profiles let you work with several Grafana instances (prod, staging, cloud) from one config file, and every result is available as a table for people or as stable, line-oriented JSON for scripts and agents.

## Install

From a checkout of this repository:

```console
$ cargo install --path .
$ gcli --version
gcli 0.1.0
```

To run from source without installing:

```console
$ cargo run -q -- --help
```

The repo ships a `flake.nix` with a development shell (`nix develop`) containing the Rust toolchain, rust-analyzer and formatting hooks. It is for working on the crate itself; the CLI builds with plain cargo.

## Quickstart

```console
$ export GCLI_CONFIG_DIR=$(mktemp -d)
$ gcli profile add prod --url https://grafana.example.com --token "$GRAFANA_TOKEN"
{"action":"add","auth":"token","profile":"prod"}
$ gcli profile use prod
{"action":"use","profile":"prod"}
$ gcli health
{"commit":"a1b2c3d","database":"ok","version":"11.4.0"}
$ gcli logs search '{job="api"}' --since 1h
{"profile":"prod","datasource":"api-logs","ts":"2026-08-28T14:05:00.000Z","labels":{"job":"api"},"line":"api two"}
```

## Configuration

The config file lives at `$GCLI_CONFIG_DIR/config.toml` if `GCLI_CONFIG_DIR` is set, otherwise in the platform config directory: `~/.config/grafana-cli/config.toml` on Linux, `~/Library/Application Support/grafana-cli/config.toml` on macOS. A missing file is fine; the CLI then has no profiles until you add one.

Full example showing all three auth modes:

```toml
current_profile = "prod"

# Token auth: a Grafana service account token, sent as a Bearer header.
[profiles.prod]
url = "https://grafana.example.com"
# token = "glc_eyJ..."                              # plaintext, simplest
token_command = ["op", "read", "op://vault/grafana-prod/token"]

# Basic auth: username plus a password (or a command that prints one).
[profiles.staging]
url = "https://staging.grafana.example.com"
username = "viewer"
# password = "plaintext-here"                       # avoid
password_command = ["op", "read", "op://vault/grafana-staging/password"]

# OAuth2 client credentials. Tokens are fetched from token_url, cached in
# the cache dir until they expire, and refreshed automatically. A 401 from
# Grafana drops the cached token, fetches a fresh one and retries once.
[profiles.cloud]
url = "https://cloud.grafana.example.com"

[profiles.cloud.oauth]
token_url = "https://sso.example.com/oauth2/token"
client_id = "gcli"
# client_secret = "plaintext-here"                  # or:
client_secret_command = ["cat", "/run/secrets/grafana-cloud-client-secret"]
scopes = ["metrics:read", "logs:read"]
audience = "grafana-cloud"
```

A profile must use exactly one auth mode: `token`/`token_command`, `username` plus `password`/`password_command`, or an `oauth` table. Mixing them, or providing none, is rejected.

**Resolution order.** Which profile to use and its settings are resolved as:

1. CLI flags: `--profile`, `--url`, `--token`, `--username`, `--password`
2. Environment: `GRAFANA_CLI_PROFILE`, `GRAFANA_CLI_URL`, `GRAFANA_CLI_TOKEN`, `GRAFANA_CLI_USERNAME`, `GRAFANA_CLI_PASSWORD`
3. `current_profile` in `config.toml`

The `GRAFANA_CLI_URL`, `GRAFANA_CLI_TOKEN`, `GRAFANA_CLI_USERNAME` and `GRAFANA_CLI_PASSWORD` variables override the matching fields of whichever profile was selected, which is handy for one-shot runs against a different instance.

**Directories.** `GCLI_CONFIG_DIR` overrides where `config.toml` is read and written. `GCLI_CACHE_DIR` overrides where the OAuth token cache (`tokens.json`) lives; the default is the platform cache directory (`~/.cache/grafana-cli` on Linux, `~/Library/Caches/grafana-cli` on macOS).

**Security.** Prefer the `*_command` variants (`token_command`, `password_command`, `client_secret_command`) over plaintext secrets. The command runs through `sh -c` at request time and its trimmed stdout becomes the secret, so credentials can come from a password manager, a file or an agent instead of the config file. Note that this also means anyone who can edit your config file can run arbitrary shell commands as you; that is by design and scoped to your own machine.

## Profiles

```text
$ gcli profile add --help
Usage: gcli profile add [OPTIONS] --url <URL> <NAME>

Arguments:
  <NAME>

Options:
      --profile <NAMES>                                Comma-separated profile names
      --url <URL>
      --all-profiles
      --token <TOKEN>
      --output <OUTPUT>                                [possible values: json, table]
      --token-command <TOKEN_COMMAND>
      --username <USERNAME>
      --password <PASSWORD>
      --password-command <PASSWORD_COMMAND>
      --token-url <TOKEN_URL>
      --client-id <CLIENT_ID>
      --client-secret <CLIENT_SECRET>
      --client-secret-command <CLIENT_SECRET_COMMAND>
      --scope <SCOPE>
      --audience <AUDIENCE>
  -h, --help                                           Print help
```

`--token-command`, `--password-command` and `--client-secret-command` take a comma-separated command (`--token-command "cat,~/.tokens/grafana"`). `--scope` is repeatable and also accepts commas. Adding a profile with OAuth flags (`--token-url`, `--client-id`, ...) writes the `oauth` table for you.

Lifecycle, with real output:

```console
$ gcli profile add prod --url https://grafana.example.com --token "$GRAFANA_TOKEN"
{"action":"add","auth":"token","profile":"prod"}
$ gcli profile add staging --url https://staging.grafana.example.com --username viewer --password-command "op read op://vault/grafana-staging/password"
{"action":"add","auth":"basic","profile":"staging"}
$ gcli profile list
[{"auth":"token","current":false,"name":"prod","url":"https://grafana.example.com"},{"auth":"basic","current":false,"name":"staging","url":"https://staging.grafana.example.com"}]
$ gcli profile use prod
{"action":"use","profile":"prod"}
$ gcli profile list
[{"auth":"token","current":true,"name":"prod","url":"https://grafana.example.com"},{"auth":"basic","current":false,"name":"staging","url":"https://staging.grafana.example.com"}]
```

`profile list` respects `--output table`:

```console
$ gcli --output table profile list
+---------+---------+-------------------------------------+-------+
| current | name    | url                                 | auth  |
+=================================================================+
| *       | prod    | https://grafana.example.com         | token |
|---------+---------+-------------------------------------+-------|
|         | staging | https://staging.grafana.example.com | basic |
+---------+---------+-------------------------------------+-------+
```

Rotating and cleaning up:

```console
$ gcli profile set-token prod --token glc_rotated
{"action":"set-token","profile":"prod"}
$ gcli profile remove staging
{"action":"remove","profile":"staging"}
$ gcli profile list
[{"auth":"token","current":true,"name":"prod","url":"https://grafana.example.com"}]
```

Usage lines for the rest: `gcli profile list [OPTIONS]`, `gcli profile use [OPTIONS] <NAME>`, `gcli profile remove [OPTIONS] <NAME>`, `gcli profile set-token [OPTIONS] --token <TOKEN> <NAME>`.

## Querying

Top-level commands:

```text
$ gcli --help
Query Grafana logs, metrics and traces

Usage: gcli [OPTIONS] <COMMAND>

Commands:
  profile
  health
  datasources
  logs
  metrics
  traces
  completions
  help         Print this message or the help of the given subcommand(s)

Options:
      --profile <NAMES>      Comma-separated profile names
      --all-profiles
      --output <OUTPUT>      [possible values: json, table]
      --url <URL>
      --token <TOKEN>
      --username <USERNAME>
      --password <PASSWORD>
  -h, --help                 Print help
  -V, --version              Print version
```

Every command accepts the same global flags, before or after the subcommand. `--url`, `--token`, `--username`, `--password` override the active profile for that one invocation.

### Time windows

Commands that take a time window accept either a relative `--since` or an absolute `--from`/`--to` pair, never both:

- default: the last hour
- `--since 30s|5m|2h|1d|1w`: human durations, counted back from now
- `--from 2026-08-28T14:00:00Z`: RFC3339 start; `--to` defaults to now
- `--to` requires `--from`; combining `--since` with `--from`/`--to` is a config error

### Datasource selection

Queries go through the datasource matching the command: Loki for logs, Prometheus for metrics, Tempo for traces. `--datasource` picks one explicitly, by name or UID. Without it, the rule is: if the instance has exactly one datasource of the needed type, use it; if it has several, use the one marked default, and if there is no unique default, fail with an `ambiguous_datasource` error. Zero datasources of the type is a `no_datasource` error. Both exit with code 2 and list the candidates. Run `gcli datasources` to see what is available.

### health

`Usage: gcli health [OPTIONS]`

```console
$ gcli health
{"commit":"a1b2c3d","database":"ok","version":"11.4.0"}
```

### datasources

`Usage: gcli datasources [OPTIONS]`

Flags: `--type loki|prometheus|tempo` to filter.

```console
$ gcli datasources
[{"isDefault":true,"name":"Loki main","type":"loki","uid":"u-loki","url":"http://loki"},{"isDefault":false,"name":"Prom","type":"prometheus","uid":"u-prom","url":"http://prom"},{"isDefault":false,"name":"Tempo","type":"tempo","uid":"u-tempo","url":"http://tempo"}]
$ gcli datasources --type loki
[{"isDefault":true,"name":"Loki main","type":"loki","uid":"u-loki","url":"http://loki"}]
```

### logs search

```text
$ gcli logs search --help
Usage: gcli logs search [OPTIONS] <QUERY>

Arguments:
  <QUERY>

Options:
      --profile <NAMES>          Comma-separated profile names
      --since <SINCE>
      --all-profiles
      --from <FROM>
      --output <OUTPUT>          [possible values: json, table]
      --to <TO>
      --datasource <DATASOURCE>
      --url <URL>
      --limit <LIMIT>            [default: 100]
      --token <TOKEN>
      --direction <DIRECTION>    [default: backward] [possible values: forward, backward]
      --username <USERNAME>
      --password <PASSWORD>
  -h, --help                     Print help
```

The query is LogQL. `--limit` defaults to 100 entries; `--direction` defaults to `backward` (newest first).

```console
$ gcli logs search '{job=~"api|web"}' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z
{"profile":"prod","datasource":"Loki","ts":"2026-08-28T14:00:10.000Z","labels":{"job":"web"},"line":"web one"}
{"profile":"prod","datasource":"Loki","ts":"2026-08-28T14:05:00.000Z","labels":{"job":"api"},"line":"api two"}
{"profile":"prod","datasource":"Loki","ts":"2026-08-28T14:20:30.250Z","labels":{"job":"web"},"line":"web three"}
{"profile":"prod","datasource":"Loki","ts":"2026-08-28T14:59:00.000Z","labels":{"job":"api"},"line":"api boom"}
```

The same query in table mode:

```console
$ gcli logs search '{job=~"api|web"}' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --output table
+--------------------------+-----------+---------+
| ts                       | line      | labels  |
+================================================+
| 2026-08-28T14:00:10.000Z | web one   | job=web |
|--------------------------+-----------+---------|
| 2026-08-28T14:05:00.000Z | api two   | job=api |
|--------------------------+-----------+---------|
| 2026-08-28T14:20:30.250Z | web three | job=web |
|--------------------------+-----------+---------|
| 2026-08-28T14:59:00.000Z | api boom  | job=api |
+--------------------------+-----------+---------+
```

### logs labels and logs label-values

`Usage: gcli logs labels [OPTIONS]` and `Usage: gcli logs label-values [OPTIONS] <LABEL>`

Both take the shared time window and `--datasource` flags. They always print a JSON array; `--output table` is ignored for these two.

```console
$ gcli logs labels --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z
["app","level"]
$ gcli logs label-values app --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z
["web"]
```

### metrics instant and metrics range

`Usage: gcli metrics instant [OPTIONS] <QUERY>` and `Usage: gcli metrics range [OPTIONS] <QUERY>`

Queries are PromQL. `metrics instant` evaluates now, or at an RFC3339 timestamp given with `--at`. `metrics range` evaluates over the time window; `--step` takes a duration like `15s` and defaults to `max(1, ceil(window_seconds / 1000))` seconds, i.e. roughly 1000 points across the window.

```console
$ gcli metrics instant up
[{"metric":{"__name__":"up","job":"node"},"ts":1759372800,"value":"1"}]
$ gcli metrics instant up --at 2026-08-28T15:00:00Z
[{"metric":{"__name__":"up","job":"node"},"ts":1759372800,"value":"1"}]
$ gcli metrics range up --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z
[{"metric":{"job":"api"},"ts":1759372700,"value":"1"},{"metric":{"job":"api"},"ts":1759372800,"value":"2"},{"metric":{"job":"zeta"},"ts":1759372700,"value":"4"},{"metric":{"job":"zeta"},"ts":1759372800,"value":"5"}]
```

### traces get and traces search

`Usage: gcli traces get [OPTIONS] <TRACE_ID>` and `Usage: gcli traces search [OPTIONS] <QUERY>`

Search queries are TraceQL; `--limit` defaults to 20. `traces get` passes Tempo's trace JSON (OpenTelemetry `batches`) through unchanged; `--output table` lists one row per span, ordered by start time.

```console
$ gcli traces get abc123
{"batches":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"cart"}}]},"scopeSpans":[{"scope":{"name":"db"},"spans":[{"endTimeUnixNano":"1759372800003000000","name":"SELECT items","spanId":"s2","startTimeUnixNano":"1759372800000500000","traceId":"abc123"}]}]},{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"checkout"}}]},"scopeSpans":[{"scope":{"name":"http"},"spans":[{"endTimeUnixNano":"1759372800001500000","name":"GET /cart","spanId":"s1","startTimeUnixNano":"1759372800000000000","traceId":"abc123"}]}]}]}
$ gcli traces get abc123 --output table
+----------+--------------+----------------+
| service  | name         | durationMillis |
+==========================================+
| checkout | GET /cart    | 1.5            |
|----------+--------------+----------------|
| cart     | SELECT items | 2.5            |
+----------+--------------+----------------+
$ gcli traces search '{ resource.service.name = "checkout" }' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --limit 5
[{"durationMs":42,"rootServiceName":"checkout","rootTraceName":"GET /cart","start":"2025-10-02T02:40:00.000Z","traceID":"t1"},{"durationMs":"7","rootServiceName":"api","rootTraceName":"POST /order","start":"2025-10-02T02:40:00.500Z","traceID":"t2"}]
```

## Output modes

`--output` accepts `json` or `table`. If the flag is omitted, the mode is automatic: `table` when stdout is a terminal, `json` when stdout is piped or redirected. Pass `--output json` explicitly when a script depends on the format.

JSON contracts, stable for machine consumption:

- `logs search` prints JSONL: one object per line with the fields `profile`, `datasource`, `ts`, `labels`, `line`.
- `logs labels` and `logs label-values` print a JSON array.
- Everything else prints a single JSON value per line.
- With more than one profile (see below), JSON output is wrapped one line per profile: `{"data":...,"profile":"prod"}`. Log records are already tagged with `profile`, so they stay as plain JSONL.
- Table mode renders comfy-table output; with more than one profile, each block is preceded by a `profile: <name>` line.
- Profile management commands (`add`, `use`, `remove`, `set-token`) always print JSON, regardless of `--output`.

Errors never touch stdout. See the next section.

## Errors and exit codes

Every error is a single JSON line on stderr:

```text
{"error":{"kind":"config","message":"config error: profile 'missing' not found; available: (none)","profile":"missing","status":null}}
```

The `error` object always has the four keys `profile`, `kind`, `status`, `message`. `profile` names the failing profile (null when none was resolved), `status` is the HTTP status code for `http` errors and null otherwise. Error kinds: `http`, `url`, `io`, `auth`, `config`, `no_datasource`, `ambiguous_datasource`, `unimplemented`.

Exit codes:

| Code | Meaning                                                                                                       |
| ---- | ------------------------------------------------------------------------------------------------------------- |
| 0    | Success. Also used when a multi-profile run had at least one success.                                         |
| 1    | HTTP error, invalid URL, I/O error. Also: a multi-profile run where all failed.                               |
| 2    | Config, auth or datasource resolution error (unknown profile, ambiguous datasource, bad time arguments, ...). |
| 3    | Unimplemented.                                                                                                |

A 401 while using token or basic auth fails fast with `kind: "http"`, `status: 401`. OAuth profiles retry once with a fresh token first.

## Multiple instances at once

Every query command fans out over several profiles. Pass `--profile a,b` or `--all-profiles`; the queries run concurrently and results are tagged per profile:

```console
$ gcli --profile prod,staging health
{"data":{"commit":"a1b2c3d","database":"ok","version":"11.4.0"},"profile":"prod"}
{"data":{"commit":"a1b2c3d","database":"ok","version":"11.4.0"},"profile":"staging"}
```

Failures do not abort the run. Successful profiles print to stdout as usual; failed ones print their error JSON to stderr:

```console
$ gcli --profile prod,staging health
{"data":{"commit":"a1b2c3d","database":"ok","version":"11.4.0"},"profile":"prod"}
{"error":{"kind":"http","message":"boom","profile":"staging","status":500}}
```

The exit code is 0 if any profile succeeded, 1 only when every profile failed. An unknown `--profile` name never fans out; it is a single config error with exit code 2.

## Shell completions

```console
$ gcli completions bash
$ gcli completions zsh
$ gcli completions fish
```

Install hints:

- bash: `gcli completions bash > ~/.local/share/bash-completion/completions/gcli`, or add `source <(gcli completions bash)` to `~/.bashrc`
- zsh: `mkdir -p ~/.zfunc && gcli completions zsh > ~/.zfunc/_gcli`, then make sure `~/.zfunc` is in `fpath` before `compinit` in `~/.zshrc`
- fish: `gcli completions fish > ~/.config/fish/completions/gcli.fish`

## For AI agents

This CLI is designed to be driven by tools like you. The guarantees:

- **Non-interactive.** It never prompts, never pages, never animates. If information is missing it fails immediately with a structured error.
- **Stable stdout.** Data goes to stdout only. JSON mode is line-oriented: one JSON value or one JSONL record per line, safe for `jq` and streaming parsers.
- **Stable error contract.** All errors are one JSON line on stderr with the fixed keys `profile`, `kind`, `status`, `message`.
- **Meaningful exit codes.** 0 ok (including partial multi-profile success), 1 transport failure, 2 bad invocation (config/auth/datasource), 3 unimplemented.
- **Auto JSON.** Piping the output selects JSON mode without extra flags, but prefer explicit `--output json` for determinism.

Suggested discovery workflow, from unknown instance to first result:

```console
$ gcli datasources
$ gcli logs labels --since 24h
$ gcli logs label-values job --since 24h
$ gcli logs search '{job="api"}' --since 1h --limit 50 --output json | jq -c .
$ gcli metrics instant up
$ gcli traces search '{ resource.service.name = "checkout" }' --since 6h --output json
```

To survey several Grafana instances in one shot: `gcli --all-profiles --output json health`, then parse the `{"data":...,"profile":...}` lines; stderr tells you which profiles are unhealthy.
