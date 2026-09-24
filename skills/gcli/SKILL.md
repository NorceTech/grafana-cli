---
name: gcli
description: Debug production issues through Grafana from the terminal. Queries Loki logs, Prometheus/Mimir metrics, and Tempo traces via one CLI with stable JSON output and non-interactive error handling. Trigger when the task involves logs, metrics, traces, Grafana, Loki, Prometheus, Tempo, an incident, error rate, latency, or a trace id.
---

# gcli

Query Grafana for logs (Loki), metrics (Prometheus/Mimir) and traces (Tempo) through its datasource proxy. One URL and credential set per instance; profiles switch between instances.

## Install and one-time auth

Install a release binary (`gcli-macos-aarch64`, `gcli-linux-x86_64`, `gcli-linux-aarch64`, `gcli-windows-x86_64.exe`) from https://github.com/NorceTech/grafana-cli/releases/latest, `cargo install --path .` from a checkout, or `nix profile install .#gcli`.

```bash
gcli profile add prod --url https://grafana.example.com --token "$GRAFANA_TOKEN"   # token auth
gcli profile add staging --url https://grafana.example.com --username user --password -   # basic
gcli profile add cloud --url https://grafana.example.com --token-url https://sso.example.com/token --client-id id --client-secret -   # oauth
gcli profile use prod && gcli profile list
```

- Secret flags (`--token`, `--password`, `--client-secret`) accept `-` to read the secret from stdin to EOF, so it skips shell history and `ps`. Or fetch at call time with `--token-command`/`--password-command`/`--client-secret-command`: a comma-separated command, one element runs as a shell line, several run as argv.
- Exactly one auth mode per profile. Rotate a token with `gcli profile set-token prod --token -`.
- Profile selection: `profile use` sets the default (env `GRAFANA_CLI_PROFILE` wins for one run), `--profile a,b` fans out over several, `--all-profiles` queries every one.
- `GRAFANA_CLI_URL`, `GRAFANA_CLI_TOKEN`, `GRAFANA_CLI_USERNAME`, `GRAFANA_CLI_PASSWORD` (and the matching flags) override the selected profile's fields, one field each. They apply only when exactly one profile is selected; with `--profile a,b` or `--all-profiles`, any override is a config error (exit 2).

## Contract

- Data on stdout only, JSON or JSONL. `logs search` prints JSONL records `{profile,datasource,ts,labels,line}`; `logs labels` and `logs label-values` print a JSON array; other commands print one JSON value.
- Output is a table on a TTY, json when piped. Pass `--output json` whenever you parse.
- Errors are one JSON line on stderr, never stdout: `{"error":{"profile","kind","status","message"}}`. Kinds: http, response, url, io, auth, config, no_datasource, ambiguous_datasource. For `http`, `message` is the upstream's own text (a LogQL, PromQL or TraceQL parse error, say) and `status` is its HTTP code.
- Exit codes: 0 ok (with several profiles, any success counts), 1 http/response/url/io (with several profiles, only if all failed), 2 bad invocation (config, auth, datasource resolution).
- `--timeout 30s` (humantime syntax, default 60s) bounds every request.

## Time windows

The default window is the last hour. Use `--since 30m` (duration), or `--from` and `--to` with RFC3339 timestamps; `--to` defaults to now and the two forms are exclusive.

## Datasource selection

A query picks its datasource automatically: the only one of the needed type, else the instance default. Several of one type without a default fails with `ambiguous_datasource`; run `gcli datasources` to list them and pass `--datasource <uid-or-name>`.

## Discovery

- Logs: `gcli logs labels [--match '{job=~"api.*"}']`, then `gcli logs label-values job`, then `gcli logs search '<LogQL>'`.
- Metrics: `gcli metrics labels`, then `gcli metrics label-values __name__` (which metric names exist), `gcli metrics series --match '{job="api"}'` (full label sets), then `gcli metrics instant '<PromQL>'` or `gcli metrics range '<PromQL>' --step 1m`.
- Traces: `gcli traces tags`, then `gcli traces tag-values resource.service.name`, then `gcli traces search '<TraceQL>'`, then `gcli traces get <TRACE_ID>`.

## Cheat sheet

```bash
gcli health                                  # instance up, which version
gcli datasources --check                     # list datasources, probe health
gcli logs labels --since 24h                 # label names in the window
gcli logs label-values job --since 24h       # values of one label
gcli logs search '{job="api"} |= "ERROR"' --since 1h --limit 100 --output json
gcli logs instant 'sum(count_over_time({job="api"}[5m]))'
gcli metrics label-values __name__           # which metric names exist
gcli metrics series --match '{job="api"}'
gcli metrics instant up
gcli metrics range 'sum(rate(http_requests_total[5m]))' --since 3h --step 1m --output json
gcli traces tag-values resource.service.name --since 24h
gcli traces search '{ resource.service.name = "checkout" }' --since 6h --output json
gcli traces get <TRACE_ID> --output json
gcli --all-profiles health --output json     # compare every instance at once
```

## Gotchas

- Big `metrics range` windows: pass `--step`. The default lands near 1000 points, and anything finer than 15s just repeats samples (Grafana's coarsest scrape interval).
- `logs search --follow` streams JSONL until interrupted, ignores `--output`, and requires exactly one profile.
- `logs instant` on a plain log query is rejected by Loki with a 400; use `logs search` for lines. LogQL metric queries come back flattened as `{metric,value,ts}` records.
- With several profiles, JSON output is wrapped one line per profile: `{"data":...,"profile":"prod"}`. Log records already carry `profile`, so they stay plain JSONL.
- A trace too big for Tempo's response limit fails upstream as `http` with status 500. Narrow the search window; don't retry as-is.

## Recipes

```bash
# Error spike: which job, when did it start
gcli logs search 'sum by (job) (count_over_time({job="api"} |= "ERROR"[5m]))' --since 6h --step 5m --output json

# p95 latency from a request-duration histogram
gcli metrics range 'histogram_quantile(0.95, sum by (le) (rate(http_request_duration_seconds_bucket[5m])))' --since 3h --step 1m --output json

# A log line carries a trace id, pull the full trace
gcli logs search '{job="api"} |= "4bf92f3577b34da6a3ce929d0e0e4736"' --since 1h --output json
gcli traces get 4bf92f3577b34da6a3ce929d0e0e4736 --output json

# Which services report traces at all
gcli traces tag-values resource.service.name --since 24h --output json

# Compare environments in one shot
gcli --all-profiles health --output json
```
