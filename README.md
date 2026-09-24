# gcli

A command line client for querying Grafana, built for humans and AI agents. It queries logs (Loki), metrics (Prometheus) and traces (Tempo) through Grafana's datasource proxy API, so one URL and one set of credentials covers every datasource on the instance. Named profiles let you work with several Grafana instances (prod, staging, cloud) from one config file, and every result is available as a table for people or as stable, line-oriented JSON for scripts and agents.

## Install

Each [release](https://github.com/NorceTech/grafana-cli/releases/latest) ships a binary for macOS (Apple silicon), Linux (x86_64 and aarch64) and Windows (x86_64), plus `checksums.txt` with their SHA-256 sums. The Linux binaries are statically linked, so they run on any distribution.

```console
$ curl -fsSL https://github.com/NorceTech/grafana-cli/releases/latest/download/gcli-macos-aarch64 -o /usr/local/bin/gcli
$ chmod +x /usr/local/bin/gcli
```

On Linux, download `gcli-linux-$(uname -m)` instead. On Windows, in PowerShell, then add the directory to your `PATH`:

```powershell
$dir = Join-Path $env:LOCALAPPDATA 'Programs\gcli'
New-Item -ItemType Directory -Path $dir -Force | Out-Null
Invoke-WebRequest 'https://github.com/NorceTech/grafana-cli/releases/latest/download/gcli-windows-x86_64.exe' -OutFile (Join-Path $dir 'gcli.exe')
```

From a checkout of this repository:

```console
$ cargo install --path .
$ gcli --version
gcli 0.0.1-alpha
```

To run from source without installing:

```console
$ cargo run -q -- --help
```

Nix users, also from a checkout of this repository:

```console
$ nix run .                  # build and run gcli
$ nix build .#default        # build the package; the binary lands in result/bin
$ nix profile install .      # install gcli into your Nix profile
```

The flake's development shell (`nix develop`) adds the Rust toolchain, rust-analyzer and formatting hooks, for working on the crate itself.

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

The config file lives at `$GCLI_CONFIG_DIR/config.toml` if `GCLI_CONFIG_DIR` is set, otherwise in the platform config directory: `~/.config/grafana-cli/config.toml` on Linux, `~/Library/Application Support/grafana-cli/config.toml` on macOS, `%APPDATA%\grafana-cli\config\config.toml` on Windows. A missing file is fine; the CLI then has no profiles until you add one.

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

The `GRAFANA_CLI_URL`, `GRAFANA_CLI_TOKEN`, `GRAFANA_CLI_USERNAME` and `GRAFANA_CLI_PASSWORD` variables override the matching fields of the selected profile, which is handy for one-shot runs against a different instance; the `--url`, `--token`, `--username` and `--password` flags override both. These overrides describe one instance and one identity, so they apply only when exactly one profile is selected: when `--profile a,b` or `--all-profiles` selects several, setting any of them fails with a `config` error (exit 2) that names it, instead of sending one profile's credentials to every instance.

**Directories.** `GCLI_CONFIG_DIR` overrides where `config.toml` is read and written. `GCLI_CACHE_DIR` overrides where the OAuth token cache (`tokens.json`) lives; the default is the platform cache directory (`~/.cache/grafana-cli` on Linux, `~/Library/Caches/grafana-cli` on macOS, `%LOCALAPPDATA%\grafana-cli\cache` on Windows).

**Security.** Prefer the `*_command` variants (`token_command`, `password_command`, `client_secret_command`) over plaintext secrets. The command runs at request time and its trimmed stdout becomes the secret, so credentials can come from a password manager, a file or an agent instead of the config file. A single-element command is a shell line run through `sh -c`, so `~` expansion and pipes work (on Windows this needs `sh` on `PATH`, which Git for Windows provides); an array of two or more elements is executed directly as argv, without a shell, so elements containing spaces (like `op://vault/Grafana Prod/token`) survive intact. Anywhere a secret is passed as a flag value (the global `--token` and `--password`, `profile add --token`/`--password`/`--client-secret`, `profile set-token --token`), a value of `-` instead reads the secret from stdin (trimmed, non-empty, never a prompt), keeping it out of `ps` output and shell history. Note that this also means anyone who can edit your config file can run arbitrary commands as you; that is by design and scoped to your own machine.

**Secret files.** `config.toml` and `tokens.json` are written through one helper: a fresh 0600 temp file next to the target, fsynced, then renamed over it. A write is never partial and the file is never wider than 0600; missing config and cache directories are created 0700. The modes apply on Unix; on Windows the files keep the default permissions of your user profile directory.

**Proxies.** Outbound requests honor the standard proxy environment variables (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`). Operating-system proxy settings are not read.

## Profiles

```text
$ gcli profile add --help
Usage: gcli profile add [OPTIONS] --url <URL> <NAME>

Arguments:
  <NAME>

Options:
      --profile <NAMES>
          Comma-separated profile names
      --url <URL>

      --all-profiles

      --token <TOKEN>

      --output <OUTPUT>
          [possible values: json, table]
      --token-command <TOKEN_COMMAND>

      --username <USERNAME>

      --password <PASSWORD>

      --password-command <PASSWORD_COMMAND>

      --token-url <TOKEN_URL>

      --client-id <CLIENT_ID>

      --timeout <DURATION>
          Request timeout, e.g. 30s, 2m or 500ms [default: 60s]
      --client-secret <CLIENT_SECRET>

      --client-secret-command <CLIENT_SECRET_COMMAND>

      --scope <SCOPE>

      --audience <AUDIENCE>

  -h, --help
          Print help
```

`--token-command`, `--password-command` and `--client-secret-command` take a comma-separated command (`--token-command "cat ~/.tokens/grafana"`); the comma-separated pieces become the command's elements, and a single element is one shell line run through `sh -c` (`~`, pipes) while two or more elements are executed directly as argv, spaces kept intact. `--scope` is repeatable and also accepts commas. Adding a profile with OAuth flags (`--token-url`, `--client-id`, ...) writes the `oauth` table for you.

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
      --timeout <DURATION>   Request timeout, e.g. 30s, 2m or 500ms [default: 60s]
  -h, --help                 Print help
  -V, --version              Print version
```

Every command accepts the same global flags, before or after the subcommand. `--url`, `--token`, `--username`, `--password` override the active profile for that one invocation.

`--timeout <DURATION>` bounds every request (default `60s`; humantime syntax like `30s`, `2m` or `500ms`), including every poll of a `--follow` run and every `*_command` secret helper. Connecting must start within 10 seconds, and the server may not fall silent for longer than the timeout, so a slow but steadily streaming response still completes. A request that gives up is an `io` error (exit 1); an unparsable duration is a `config` error (exit 2).

### Time windows

Commands that take a time window accept either a relative `--since` or an absolute `--from`/`--to` pair, never both:

- default: the last hour
- `--since 30s|5m|2h|1d|1w`: human durations, counted back from now
- `--from 2026-08-28T14:00:00Z`: RFC3339 start; `--to` defaults to now
- `--to` requires `--from`; combining `--since` with `--from`/`--to` is a config error

The window is resolved once per invocation, before any profile runs: a bad window is one `config` error, and every selected profile queries the same bounds.

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

Flags: `--type loki|prometheus|tempo` to filter, `--check` to health-check each listed datasource.

```console
$ gcli datasources
[{"isDefault":true,"name":"Loki main","type":"loki","uid":"u-loki","url":"http://loki"},{"isDefault":false,"name":"Prom","type":"prometheus","uid":"u-prom","url":"http://prom"},{"isDefault":false,"name":"Tempo","type":"tempo","uid":"u-tempo","url":"http://tempo"}]
$ gcli datasources --type loki
[{"isDefault":true,"name":"Loki main","type":"loki","uid":"u-loki","url":"http://loki"}]
```

`--check` asks Grafana's per-datasource health endpoint about each listed datasource and adds `health` and `healthMessage` to every JSON record (table mode: `health` and `message` columns). A datasource type without health support answers 4xx/5xx, which shows as `"health":"ERROR"` with the error text as the message; one failing check never fails the command.

```console
$ gcli datasources --type prometheus --check
[{"health":"OK","healthMessage":"Successfully queried the Prometheus data source.","isDefault":false,"name":"Prom","type":"prometheus","uid":"u-prom","url":"http://prom"}]
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
      --step <DURATION>
  -f, --follow
      --timeout <DURATION>       Request timeout, e.g. 30s, 2m or 500ms [default: 60s]
      --interval <DURATION>      [default: 2s]
  -h, --help                     Print help
```

The query is LogQL. `--limit` defaults to 100 entries; `--direction` defaults to `backward` (newest first).

A log query prints JSONL, one object per line:

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

A LogQL metric query (`count_over_time`, `rate`, ...) answers with Prometheus-shaped series, flattened to `{metric, value, ts}` records sorted by label string then timestamp. `--step` samples the query at whole seconds (`15s`, `1m`; at least `1s`, sub-second values are a config error); without it Loki picks its own default.

```console
$ gcli logs search 'sum by (job) (count_over_time({job="api"}[5m]))' --since 1h --step 1m
[{"metric":{"job":"api"},"ts":1759372500,"value":"42"},{"metric":{"job":"api"},"ts":1759372800,"value":"45"}]
```

`--follow` (`-f`) keeps printing new entries as they arrive. The first batch is exactly what `logs search` prints for the window; then, every `--interval` (default `2s`), it polls from the newest printed nanosecond to now and prints only entries not printed yet. Output is always JSONL; `--output` is ignored. Follow streams from exactly one profile: selecting several (`--profile a,b`, `--all-profiles`) is a config error. SIGINT, SIGTERM or a closed stdout end the run with exit 0; a failed poll ends it with that error's JSON and its exit code.

### logs instant

`Usage: gcli logs instant [OPTIONS] <QUERY>`

Evaluates a LogQL metric query at one instant: now by default, or at an RFC3339 timestamp with `--at`. The vector result is flattened like `metrics instant`, except timestamps are Loki's nanoseconds:

```console
$ gcli logs instant 'sum by (job) (count_over_time({job="api"}[5m]))'
[{"metric":{"job":"api"},"ts":"1759372800000000000","value":"42"}]
```

A plain log query here is rejected by Loki with a plain-text 400 whose text becomes the error message.

### logs labels and logs label-values

`Usage: gcli logs labels [OPTIONS]` and `Usage: gcli logs label-values [OPTIONS] <LABEL>`

Both take the shared time window and `--datasource` flags. They always print a JSON array; `--output table` is ignored for these two. `--match <SELECTOR>` scopes the request to streams matching a LogQL selector:

```console
$ gcli logs labels --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z
["app","level"]
$ gcli logs label-values job --match '{job=~"api|web"}' --since 24h
["api","web"]
```

### metrics instant and metrics range

`Usage: gcli metrics instant [OPTIONS] <QUERY>` and `Usage: gcli metrics range [OPTIONS] <QUERY>`

Queries are PromQL. `metrics instant` evaluates now, or at an RFC3339 timestamp given with `--at`. `metrics range` evaluates over the time window; `--step` takes a duration like `15s` and defaults to `max(15, ceil(window_seconds / 1000))` seconds: the 15 s floor matches Grafana's coarsest scrape interval (a finer step only repeats the last stored sample), and the window-scaled part keeps roughly 1000 points across large windows.

```console
$ gcli metrics instant up
[{"metric":{"__name__":"up","job":"node"},"ts":1759372800,"value":"1"}]
$ gcli metrics instant up --at 2026-08-28T15:00:00Z
[{"metric":{"__name__":"up","job":"node"},"ts":1759372800,"value":"1"}]
$ gcli metrics range up --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z
[{"metric":{"job":"api"},"ts":1759372700,"value":"1"},{"metric":{"job":"api"},"ts":1759372800,"value":"2"},{"metric":{"job":"zeta"},"ts":1759372700,"value":"4"},{"metric":{"job":"zeta"},"ts":1759372800,"value":"5"}]
```

### metrics labels, label-values, series and exemplars

`Usage: gcli metrics labels [OPTIONS]`, `Usage: gcli metrics label-values [OPTIONS] <LABEL>`, `Usage: gcli metrics series [OPTIONS] --match <MATCHES>`, `Usage: gcli metrics exemplars [OPTIONS] <QUERY>`

The series-metadata commands behind metric discovery. All four take the shared time window and `--datasource` flags. `--match` takes a PromQL selector, is repeatable, and `series` requires at least one. `labels` and `label-values` always print a JSON array; `--output table` is ignored for them.

```console
$ gcli metrics labels --since 24h
["__name__","instance","job"]
$ gcli metrics label-values job --match 'up' --since 24h
["node"]
$ gcli metrics series --match 'up' --since 24h
[{"__name__":"up","instance":"node:9100","job":"node"}]
```

`series` prints label-set objects sorted by their label string. `exemplars` prints one `{metric, labels, value, ts}` record per exemplar, sorted by series then timestamp; `ts` keeps the upstream fractional-seconds number and `value` the upstream string. In table mode `series` renders one `series` column, and `exemplars` renders `metric`, `traceID`, `value`, `ts`, taking the trace id from the exemplar's `trace_id` (or `traceID`) label.

```console
$ gcli metrics exemplars 'http_request_duration_seconds_bucket' --since 6h
[{"labels":{"trace_id":"5b8efff69803be10153f4ea8a3b5186c"},"metric":{"__name__":"http_request_duration_seconds_bucket","service":"api"},"ts":1759372800.25,"value":"0.2"}]
```

### traces get and traces search

`Usage: gcli traces get [OPTIONS] <TRACE_ID>` and `Usage: gcli traces search [OPTIONS] <QUERY>`

Search queries are TraceQL; `--limit` defaults to 20. `traces get` passes Tempo's trace JSON (OpenTelemetry `batches`) through unchanged; `--output table` lists one row per span, ordered by start time, with `status` the span's OTLP status code (`ok` or `error`, empty when unset).

```console
$ gcli traces get abc123
{"batches":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"cart"}}]},"scopeSpans":[{"scope":{"name":"db"},"spans":[{"endTimeUnixNano":"1759372800003000000","name":"SELECT items","spanId":"s2","startTimeUnixNano":"1759372800000500000","status":{"code":"STATUS_CODE_ERROR"},"traceId":"abc123"}]}]},{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"checkout"}}]},"scopeSpans":[{"scope":{"name":"http"},"spans":[{"endTimeUnixNano":"1759372800001500000","name":"GET /cart","spanId":"s1","startTimeUnixNano":"1759372800000000000","traceId":"abc123"}]}]}]}
$ gcli traces get abc123 --output table
+----------+--------------+--------+----------------+
| service  | name         | status | durationMillis |
+===================================================+
| checkout | GET /cart    |        | 1.5            |
|----------+--------------+--------+----------------|
| cart     | SELECT items | error  | 2.5            |
+----------+--------------+--------+----------------+
$ gcli traces search '{ resource.service.name = "checkout" }' --from 2026-08-28T14:00:00Z --to 2026-08-28T15:00:00Z --limit 5
[{"durationMs":42,"rootServiceName":"checkout","rootTraceName":"GET /cart","start":"2025-10-02T02:40:00.000Z","traceID":"t1"},{"durationMs":"7","rootServiceName":"api","rootTraceName":"POST /order","start":"2025-10-02T02:40:00.500Z","traceID":"t2"}]
```

### traces tags and traces tag-values

`Usage: gcli traces tags [OPTIONS]` and `Usage: gcli traces tag-values [OPTIONS] <TAG>`

The discovery half of TraceQL. Both take the shared time window and `--datasource` flags; `tags` also takes `--scope resource|span|intrinsic|event|link|instrumentation` to filter.

`traces tags` lists every tag under its TraceQL-ready name, qualified by its scope (`resource.service.name`, `span.http.method`) except intrinsics, which TraceQL addresses bare (`status`, `duration`), sorted and de-duplicated. Like `logs labels`, the output is a JSON array in every mode.

```console
$ gcli traces tags --since 6h
["duration","name","resource.service.name","span.http.method","status"]
```

`traces tag-values` lists a tag's values as typed `{type, value}` objects, sorted by type then value; the type says how to quote the value in TraceQL (strings take quotes, ints and durations don't). Table mode renders `type` and `value` columns.

```console
$ gcli traces tag-values resource.service.name --since 6h
[{"type":"string","value":"api"},{"type":"string","value":"checkout"}]
```

## Output modes

`--output` accepts `json` or `table`. If the flag is omitted, the mode is automatic: `table` when stdout is a terminal, `json` when stdout is piped or redirected. Pass `--output json` explicitly when a script depends on the format.

JSON contracts, stable for machine consumption:

- `logs search` with a log query prints JSONL: one object per line with the fields `profile`, `datasource`, `ts`, `labels`, `line`. With a LogQL metric query it prints one JSON array of `{metric, value, ts}` records.
- `logs labels`, `logs label-values` and `traces tags` print a JSON array; `--output table` is ignored for them.
- Everything else prints a single JSON value per line (most query results are one JSON array).
- With more than one profile (see below), JSON output is wrapped one line per profile: `{"data":...,"profile":"prod"}`. Log records are already tagged with `profile`, so they stay as plain JSONL.
- Table mode renders comfy-table output; with more than one profile, each block is preceded by a `profile: <name>` line.
- Profile management commands (`add`, `use`, `remove`, `set-token`) always print JSON, regardless of `--output`.
- `logs search --follow` always prints JSONL, whatever `--output` says.

Errors never touch stdout. See the next section.

## Errors and exit codes

Every error is a single JSON line on stderr:

```text
{"error":{"kind":"config","message":"config error: profile 'missing' not found; available: (none)","profile":"missing","status":null}}
```

The `error` object always has the four keys `profile`, `kind`, `status`, `message`. `profile` names the failing profile (null when none was resolved), `status` is the HTTP status code for `http` and `response` errors and null otherwise. Error kinds: `http`, `response`, `url`, `io`, `auth`, `config`, `no_datasource`, `ambiguous_datasource`.

Where `message` comes from for the kinds that talk to the instance:

- `http`: a non-2xx reply. `message` is the upstream's own error text: the JSON `message` field (Grafana), else the JSON `error` field (Prometheus, Loki), else the response body as plain text (Loki and Tempo answer many errors in plain text), trimmed and cut to 500 characters. It is null only when the error response has an empty body.
- An error of kind response: a success status whose body gcli cannot use, either not JSON (an SSO proxy's HTML login page, say) or JSON of an unexpected shape. `status` is that reply's status; `message` says what was wrong, names the request path and content type, and quotes the start of the body.
- `url`: the profile's url (set by `profile add`, `--url` or `GRAFANA_CLI_URL`) is not an absolute `http`/`https` address. `profile add` refuses to save it, and a query rejects it before contacting anything, so no credentials are spent.
- `io`: a request that got no complete reply. `message` names the request path followed by the chain of causes, so a refused connection, a DNS failure and an untrusted TLS certificate read differently.

Exit codes:

| Code | Meaning                                                                                                       |
| ---- | ------------------------------------------------------------------------------------------------------------- |
| 0    | Success. Also used when a multi-profile run had at least one success.                                         |
| 1    | HTTP error, malformed response, invalid URL, I/O error. Also: a multi-profile run where all failed.           |
| 2    | Config, auth or datasource resolution error (unknown profile, ambiguous datasource, bad time arguments, ...). |

A bad time window (`--since`, `--from`, `--to`) fails before any profile runs: one `config` error and exit code 2, even when several profiles are selected. The same is true for arguments that become part of a request path, like a label name or a trace id.

An explicit `--datasource` of another core type (a Loki datasource for a metrics query, say) is a `config` error naming both types, and nothing is queried. One of a plugin type, such as a Prometheus-compatible VictoriaMetrics or Amazon Managed Prometheus datasource, is queried anyway with a `note:` line on stderr.

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

The exit code is 0 if any profile succeeded, 1 only when every profile failed. An unknown `--profile` name never fans out; it is a single config error with exit code 2. The one exception to fanout is `logs search --follow`, which refuses to run over several profiles.

## Agent skill

The distributable skill for AI agents lives at `skills/gcli/SKILL.md`. Install it by copying or symlinking the `skills/gcli` directory into your agent's skills directory, for example `~/.agents/skills/gcli` or `~/.claude/skills/gcli`. It teaches the same contracts as this README (discovery workflow, output shapes, exit codes) in the form an agent loads on demand.

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
- **Stable error contract.** All errors are one JSON line on stderr with the fixed keys `profile`, `kind`, `status`, `message`. For `http` errors, `message` carries the upstream's own error text (a LogQL, PromQL or TraceQL parse error, say); it is null only when the response body was empty. `kind: "response"` means the instance answered with a success status but a body that is not the expected JSON (an SSO login page, for example); its `message` quotes the start of that body.
- **Notes on stderr.** Besides the JSON error lines, stderr can carry plain-text `note: ...` lines that do not change the result, for example when `--datasource` names a plugin datasource instead of a core `loki`, `prometheus` or `tempo` one (gcli still queries it: Prometheus-compatible plugins such as VictoriaMetrics have their own type). Treat only the lines that parse as JSON as errors.
- **Meaningful exit codes.** 0 ok (including partial multi-profile success), 1 upstream or transport failure (`http`, `response`, `io`, `url`), 2 bad invocation (config/auth/datasource).
- **Auto JSON.** Piping the output selects JSON mode without extra flags, but always pass `--output json` in scripts; it is the only way to be sure.
- **Bounded and secret-safe.** Every request and `*_command` helper is bounded by `--timeout` (default 60s), and any secret flag accepts `-` to read the secret from stdin, so nothing hangs and no secret needs to appear in a process listing.

Suggested discovery workflow, from unknown instance to first result, across all three datasources:

```console
$ gcli datasources --check
$ gcli logs labels --since 24h
$ gcli logs label-values job --since 24h
$ gcli logs search '{job="api"}' --since 1h --limit 50 --output json | jq -c .
$ gcli metrics labels --since 24h
$ gcli metrics label-values __name__ --since 24h
$ gcli metrics series --match '{job="api"}' --since 1h
$ gcli metrics instant up
$ gcli traces tags --since 6h
$ gcli traces tag-values resource.service.name --since 6h
$ gcli traces search '{ resource.service.name = "checkout" }' --since 6h --output json
```

For unattended or long-running use, pass an explicit `--timeout`; it bounds every request, every `--follow` poll and every secret helper command.

To survey several Grafana instances in one shot: `gcli --all-profiles --output json health`, then parse the `{"data":...,"profile":...}` lines; stderr tells you which profiles are unhealthy.
