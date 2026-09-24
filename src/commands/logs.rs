use crate::client::{Datasource, GrafanaClient, Reply};
use crate::commands::prom;
use crate::commands::{Bounds, CommandOutput, QueryWindow, Windowed, resolve_ds, success_data};
use crate::datasource::Kind;
use crate::output::{LogRecord, OutputMode};
use crate::timeutil::{resolve_window, to_ns_string};
use crate::{Result, error::Error};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(clap::Subcommand, Clone, Debug)]
pub enum LogsCmd {
    Search(SearchArgs),
    Instant(InstantArgs),
    Labels {
        #[command(flatten)]
        window: QueryWindow,
        // The selector label discovery is scoped to, sent as Loki's query=.
        #[arg(long, value_name = "SELECTOR")]
        r#match: Option<String>,
    },
    LabelValues {
        label: String,
        #[command(flatten)]
        window: QueryWindow,
        #[arg(long, value_name = "SELECTOR")]
        r#match: Option<String>,
    },
}

#[derive(clap::Args, Clone, Debug)]
pub struct SearchArgs {
    pub query: String,
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long, default_value_t = 100)]
    pub limit: u64,
    #[arg(long, value_enum, default_value = "backward")]
    pub direction: Direction,
    // The sampling step of a LogQL metric query, in whole seconds; without
    // it Loki picks a dynamic default.
    #[arg(long, value_name = "DURATION")]
    pub step: Option<String>,
    // Keep printing new entries as they arrive; --output is ignored (always
    // JSONL) and exactly one profile may be selected.
    #[arg(short = 'f', long)]
    pub follow: bool,
    // How often a --follow run polls for new entries.
    #[arg(long, value_name = "DURATION", default_value = "2s")]
    pub interval: String,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Direction {
    Forward,
    Backward,
}

// A LogQL metric query evaluated once: no window, only an optional instant.
#[derive(clap::Args, Clone, Debug)]
pub struct InstantArgs {
    pub query: String,
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    #[arg(long)]
    pub datasource: Option<String>,
}

impl Direction {
    /// Loki's `direction` query parameter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
        }
    }
}

impl LogsCmd {
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
        bounds: Bounds,
    ) -> Result<CommandOutput> {
        match self {
            Self::Search(args) => search(client, profile, mode, Windowed { args, bounds }).await,
            Self::Instant(args) => instant(client, profile, mode, args).await,
            Self::Labels { window, r#match } => {
                labels(
                    client,
                    profile,
                    Windowed {
                        args: window,
                        bounds,
                    },
                    r#match.as_deref(),
                )
                .await
            }
            Self::LabelValues {
                label,
                window,
                r#match,
            } => {
                label_values(
                    client,
                    profile,
                    label,
                    Windowed {
                        args: window,
                        bounds,
                    },
                    r#match.as_deref(),
                )
                .await
            }
        }
    }
}

/// Window bounds (ns strings) + the resolved Loki datasource for a query.
/// The bounds are encoded before any datasource is fetched, so a window
/// outside the representable range never reaches the instance.
async fn loki_context(
    client: &GrafanaClient,
    window: Windowed<'_, QueryWindow>,
) -> Result<(String, String, Datasource)> {
    let start = to_ns_string(window.bounds.start)?;
    let end = to_ns_string(window.bounds.end)?;
    let ds = resolve_ds(client, Kind::Loki, window.args.datasource.as_deref()).await?;
    Ok((start, end, ds))
}

/// The names in a `labels` or `label/{label}/values` reply.
fn string_array(reply: &Reply) -> Result<Vec<String>> {
    // Loki leaves `data` out when no label or value matches.
    let Some(data) = success_data(reply)? else {
        return Ok(Vec::new());
    };
    Vec::<String>::deserialize(data).map_err(|e| reply.malformed(&format!("data: {e}")))
}

/// The step a LogQL metric query samples at, in whole seconds; `None` when
/// no `--step` was given, so Loki picks its own default. The same contract
/// as `metrics range`'s step: unparsable or sub-second is a config error.
fn step_secs(step: Option<&str>) -> Result<Option<u64>> {
    let Some(s) = step else {
        return Ok(None);
    };
    let secs = humantime::parse_duration(s)
        .map_err(|_| Error::Config {
            message: format!("invalid step duration: '{s}'"),
        })?
        .as_secs();
    if secs == 0 {
        return Err(Error::Config {
            message: format!("step '{s}' must be at least 1s"),
        });
    }
    Ok(Some(secs))
}

pub async fn search(
    client: &GrafanaClient,
    profile: &str,
    mode: OutputMode,
    search: Windowed<'_, SearchArgs>,
) -> Result<CommandOutput> {
    let Windowed { args, bounds } = search;
    let window = Windowed {
        args: &args.window,
        bounds,
    };
    let (start, end, ds) = loki_context(client, window).await?;
    let mut pairs = vec![
        ("query", args.query.clone()),
        ("start", start),
        ("end", end),
        ("limit", args.limit.to_string()),
        ("direction", args.direction.as_str().to_string()),
    ];
    if let Some(secs) = step_secs(args.step.as_deref())? {
        pairs.push(("step", secs.to_string()));
    }
    let mut reply = client
        .proxy_request(&ds.uid, "loki/api/v1/query_range", &pairs)
        .await?;
    // A LogQL metric query answers resultType "matrix" with Prometheus-
    // shaped series; flatten them exactly like `metrics range` does.
    if reply
        .body
        .pointer("/data/resultType")
        .and_then(Value::as_str)
        == Some("matrix")
    {
        let records = prom::flatten_matrix(prom::prom_result(&mut reply, "matrix")?);
        if mode == OutputMode::Table {
            return Ok(CommandOutput::Table {
                headers: ["metric", "timestamp", "value"].map(String::from).to_vec(),
                rows: prom::matrix_rows(&records),
            });
        }
        return Ok(CommandOutput::Json(Value::Array(records)));
    }
    let records = log_records(&reply, profile, &ds.name)?;
    if mode == OutputMode::Table {
        let rows = records
            .iter()
            .map(|r| vec![r.ts.clone(), r.line.clone(), labels_kv(&r.labels)])
            .collect();
        return Ok(CommandOutput::Table {
            headers: ["ts", "line", "labels"].map(String::from).to_vec(),
            rows,
        });
    }
    Ok(CommandOutput::Logs(records))
}

/// The records of a `query_range` streams reply for `profile` and `datasource`.
fn log_records(reply: &Reply, profile: &str, datasource: &str) -> Result<Vec<LogRecord>> {
    Ok(log_entries(reply, profile, datasource)?
        .into_iter()
        .map(|entry| entry.record)
        .collect())
}

// A parsed log entry with its true nanosecond timestamp alongside the
// rendered record: the sort key must be true (nanosecond) chronology, not
// the millisecond-truncated ts string, and --follow needs the ns as its
// polling cursor.
struct Entry {
    ns: i64,
    record: LogRecord,
}

/// The entries of a `query_range` streams reply, sorted by (nanosecond
/// timestamp, line).
fn log_entries(reply: &Reply, profile: &str, datasource: &str) -> Result<Vec<Entry>> {
    let streams = success_data(reply)?
        .and_then(|data| data.get("result"))
        .and_then(Value::as_array)
        .ok_or_else(|| reply.malformed("no data.result array"))?;

    let mut entries: Vec<Entry> = Vec::new();
    for stream in streams {
        let labels: BTreeMap<String, String> = serde_json::from_value(
            stream
                .get("stream")
                .cloned()
                .ok_or_else(|| reply.malformed("stream without a stream label map"))?,
        )
        .map_err(|e| reply.malformed(&format!("stream labels: {e}")))?;
        let values = stream
            .get("values")
            .and_then(Value::as_array)
            .ok_or_else(|| reply.malformed("stream without a values array"))?;
        for entry in values {
            let pair = entry
                .as_array()
                .filter(|p| p.len() >= 2)
                .ok_or_else(|| reply.malformed("entry is not a [ts, line] pair"))?;
            let ns = pair[0]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| reply.malformed("entry timestamp is not integer nanoseconds"))?;
            entries.push(Entry {
                ns,
                record: LogRecord {
                    profile: profile.to_string(),
                    datasource: datasource.to_string(),
                    ts: DateTime::from_timestamp_nanos(ns)
                        .to_rfc3339_opts(SecondsFormat::Millis, true),
                    labels: labels.clone(),
                    line: pair[1].as_str().unwrap_or_default().to_string(),
                },
            });
        }
    }
    entries.sort_by(|a, b| (a.ns, a.record.line.as_str()).cmp(&(b.ns, b.record.line.as_str())));
    Ok(entries)
}

/// The `--follow` cursor: the newest nanosecond printed and the
/// (labels, line) pairs already printed at exactly that nanosecond. A
/// forward poll starts at `last_ns`, so Loki re-sends the boundary entries;
/// only a pair already printed at that nanosecond is a duplicate.
#[derive(Debug, Clone)]
pub(crate) struct FollowCursor {
    last_ns: i64,
    seen_at_last: Vec<(BTreeMap<String, String>, String)>,
}

impl FollowCursor {
    /// The cursor after the ascending `printed` batch; an empty initial
    /// batch starts from `fallback_ns`, the end of the queried window.
    fn new(printed: &[Entry], fallback_ns: i64) -> Self {
        let Some(newest) = printed.last() else {
            return Self {
                last_ns: fallback_ns,
                seen_at_last: Vec::new(),
            };
        };
        let last_ns = newest.ns;
        Self {
            last_ns,
            seen_at_last: printed
                .iter()
                .filter(|entry| entry.ns == last_ns)
                .map(|entry| (entry.record.labels.clone(), entry.record.line.clone()))
                .collect(),
        }
    }

    /// Keeps only the entries not printed yet (given ascending, like every
    /// `log_entries` reply) and advances the cursor past them: an entry at
    /// the boundary nanosecond is old news when its labels and line were
    /// printed there, and the first step past the boundary starts a fresh
    /// duplicate set for the new newest nanosecond.
    fn take_new(&mut self, entries: Vec<Entry>) -> Vec<LogRecord> {
        let boundary = self.last_ns;
        let mut last_ns = boundary;
        let mut seen_at_last = self.seen_at_last.clone();
        let mut fresh = Vec::new();
        for Entry { ns, record } in entries {
            if ns == boundary
                && seen_at_last
                    .iter()
                    .any(|(labels, line)| *labels == record.labels && *line == record.line)
            {
                continue;
            }
            if ns > last_ns {
                last_ns = ns;
                seen_at_last.clear();
            }
            if ns == last_ns {
                seen_at_last.push((record.labels.clone(), record.line.clone()));
            }
            fresh.push(record);
        }
        self.last_ns = last_ns;
        self.seen_at_last = seen_at_last;
        fresh
    }
}

/// The polling half of `logs search --follow`: the datasource resolved once
/// and the cursor. `poll` runs one forward `query_range` from the last seen
/// nanosecond to now and returns only the entries not printed yet.
pub struct FollowStream {
    ds: Datasource,
    profile: String,
    query: String,
    limit: u64,
    cursor: FollowCursor,
}

impl FollowStream {
    /// One polling step; every entry it returns is ascending and unprinted.
    pub async fn poll(&mut self, client: &GrafanaClient) -> Result<Vec<LogRecord>> {
        let end = to_ns_string(Utc::now())?;
        let reply = client
            .proxy_request(
                &self.ds.uid,
                "loki/api/v1/query_range",
                &[
                    ("query", self.query.clone()),
                    ("start", self.cursor.last_ns.to_string()),
                    ("end", end),
                    ("limit", self.limit.to_string()),
                    ("direction", Direction::Forward.as_str().to_string()),
                ],
            )
            .await?;
        let entries = log_entries(&reply, &self.profile, &self.ds.name)?;
        Ok(self.cursor.take_new(entries))
    }
}

/// The start of a `--follow` run: the window query `search` would run, its
/// records (ascending, exactly `search`'s output) and the poller positioned
/// after them. The window is resolved here, once, because a follow run
/// never reaches the fanout that resolves it for every other command.
pub async fn follow_start(
    client: &GrafanaClient,
    profile: &str,
    args: &SearchArgs,
) -> Result<(Vec<LogRecord>, FollowStream)> {
    let (start_dt, end_dt) = resolve_window(
        args.window.since.as_deref(),
        args.window.from.as_deref(),
        args.window.to.as_deref(),
    )?;
    let start = to_ns_string(start_dt)?;
    let end = to_ns_string(end_dt)?;
    let ds = resolve_ds(client, Kind::Loki, args.window.datasource.as_deref()).await?;
    let mut pairs = vec![
        ("query", args.query.clone()),
        ("start", start),
        ("end", end.clone()),
        ("limit", args.limit.to_string()),
        ("direction", args.direction.as_str().to_string()),
    ];
    if let Some(secs) = step_secs(args.step.as_deref())? {
        pairs.push(("step", secs.to_string()));
    }
    let reply = client
        .proxy_request(&ds.uid, "loki/api/v1/query_range", &pairs)
        .await?;
    let entries = log_entries(&reply, profile, &ds.name)?;
    // Nothing matched: the cursor starts at the window end, the newest
    // moment known to hold no unprinted entry. `to_ns_string` rendered the
    // window end from an i64, so this parse cannot fail.
    let fallback_ns: i64 = end.parse().map_err(|_| Error::Config {
        message: format!("window end '{end}' is not integer nanoseconds"),
    })?;
    let cursor = FollowCursor::new(&entries, fallback_ns);
    let records = entries.into_iter().map(|entry| entry.record).collect();
    Ok((
        records,
        FollowStream {
            ds,
            profile: profile.to_string(),
            query: args.query.clone(),
            limit: args.limit,
            cursor,
        },
    ))
}

fn labels_kv(labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// A LogQL metric query evaluated at one instant: Loki's `query` endpoint,
/// with `time` in nanoseconds (Loki's wire unit, unlike Prometheus's epoch
/// seconds) and omitted without `--at` so Loki evaluates at now. The vector
/// result is flattened like `metrics instant`; a log query here is rejected
/// by Loki with a plain-text 400 that surfaces as the error message.
pub async fn instant(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    args: &InstantArgs,
) -> Result<CommandOutput> {
    let ds = resolve_ds(client, Kind::Loki, args.datasource.as_deref()).await?;
    let mut pairs = vec![("query", args.query.clone())];
    if let Some(at) = args.at.as_deref() {
        let dt = DateTime::parse_from_rfc3339(at)
            .map_err(|_| Error::Config {
                message: format!("invalid RFC3339 timestamp (--at): '{at}'"),
            })?
            .with_timezone(&chrono::Utc);
        pairs.push(("time", to_ns_string(dt)?));
    }
    let mut reply = client
        .proxy_request(&ds.uid, "loki/api/v1/query", &pairs)
        .await?;
    let records = prom::flatten_vector(prom::prom_result(&mut reply, "vector")?);
    if mode == OutputMode::Table {
        return Ok(CommandOutput::Table {
            headers: ["metric", "value", "ts"].map(String::from).to_vec(),
            rows: prom::vector_rows(&records),
        });
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}

fn json_string_array(names: Vec<String>) -> Value {
    Value::Array(names.into_iter().map(Value::String).collect())
}

pub async fn labels(
    client: &GrafanaClient,
    _profile: &str,
    window: Windowed<'_, QueryWindow>,
    selector: Option<&str>,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, window).await?;
    let mut pairs = vec![("start", start), ("end", end)];
    if let Some(selector) = selector {
        pairs.push(("query", selector.to_string()));
    }
    let reply = client
        .proxy_request(&ds.uid, "loki/api/v1/labels", &pairs)
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &reply,
    )?)))
}

/// Rejects a label name Loki would not accept, `[A-Za-z_][A-Za-z0-9_]*`,
/// before it becomes a segment of `label/{label}/values`; see
/// `traces::validate_trace_id` for why.
pub(crate) fn validate_label_name(label: &str) -> Result<()> {
    let mut bytes = label.bytes();
    let valid = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if valid {
        return Ok(());
    }
    Err(Error::Config {
        message: format!("invalid label name: '{label}' (expected [A-Za-z_][A-Za-z0-9_]*)"),
    })
}

pub async fn label_values(
    client: &GrafanaClient,
    _profile: &str,
    label: &str,
    window: Windowed<'_, QueryWindow>,
    selector: Option<&str>,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, window).await?;
    let mut pairs = vec![("start", start), ("end", end)];
    if let Some(selector) = selector {
        pairs.push(("query", selector.to_string()));
    }
    let reply = client
        .proxy_request(
            &ds.uid,
            &format!("loki/api/v1/label/{label}/values"),
            &pairs,
        )
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &reply,
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn records(body: Value) -> Result<Vec<LogRecord>> {
        log_records(&Reply::ok(body), "p", "Loki")
    }

    #[test]
    fn loki_label_names_are_valid() {
        for label in ["job", "_", "__name__", "Job_9"] {
            let result = validate_label_name(label);

            assert!(result.is_ok(), "{label}: {result:?}");
        }
    }

    #[test]
    fn step_is_whole_seconds_or_none_and_validated() {
        assert_eq!(step_secs(None).unwrap(), None);
        assert_eq!(step_secs(Some("5m")).unwrap(), Some(300));
        assert_eq!(step_secs(Some("15s")).unwrap(), Some(15));
        for bad in ["bogus", "0s", "500ms"] {
            assert!(
                matches!(step_secs(Some(bad)), Err(Error::Config { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn other_label_names_are_config_errors_naming_the_label() {
        for label in [
            "",
            "9job",
            "service.name",
            "a-b",
            "job/../x",
            "job?",
            "j\u{f6}b",
        ] {
            let result = validate_label_name(label);

            assert!(
                matches!(&result, Err(Error::Config { message }) if message.contains(&format!("'{label}'"))),
                "{label}: {result:?}"
            );
        }
    }

    fn streams(result: Value) -> Value {
        json!({"status": "success", "data": {"resultType": "streams", "result": result}})
    }

    #[test]
    fn entries_sort_by_nanosecond_timestamp_then_line() {
        // One millisecond, entries 1 ns apart, newest listed first.
        let body = streams(json!([
            {"stream": {"job": "api"},
             "values": [["1787925600000000002", "b"], ["1787925600000000001", "c"]]},
            {"stream": {"job": "web"}, "values": [["1787925600000000001", "a"]]}
        ]));

        let got = records(body).unwrap();

        let lines: Vec<&str> = got.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["a", "c", "b"]);
    }

    #[test]
    fn reply_without_data_is_a_response_error() {
        let result = records(json!({"status": "success"}));

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }

    #[test]
    fn stream_with_non_array_values_is_a_response_error() {
        let body = streams(json!([{"stream": {"job": "api"}, "values": "api boom"}]));

        let result = records(body);

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }

    #[test]
    fn entry_with_non_integer_ns_is_a_response_error() {
        // Fractional seconds, not Loki's integer nanoseconds.
        let body = streams(json!([
            {"stream": {"job": "api"}, "values": [["1787925600.5", "api boom"]]}
        ]));

        let result = records(body);

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }

    #[test]
    fn error_status_is_http_with_the_upstream_error() {
        let body = json!({"status": "error", "errorType": "bad_data", "error": "parse error"});

        let result = records(body);

        assert!(
            matches!(&result, Err(Error::Http { status: 200, message: Some(m), .. }) if m == "parse error"),
            "{result:?}"
        );
    }

    #[test]
    fn label_reply_without_data_is_empty() {
        // Loki leaves `data` out when no label or value matches.
        let reply = Reply::ok(json!({"status": "success"}));

        let names = string_array(&reply).unwrap();

        assert!(names.is_empty(), "{names:?}");
    }

    #[test]
    fn label_reply_with_non_string_names_is_a_response_error() {
        let reply = Reply::ok(json!({"status": "success", "data": [1, 2]}));

        let result = string_array(&reply);

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }

    // --follow cursor: newest printed nanosecond, plus the (labels, line)
    // pairs printed at exactly that nanosecond, is what separates a boundary
    // re-send from a new entry.
    fn follow_entry(ns: i64, job: &str, line: &str) -> Entry {
        Entry {
            ns,
            record: LogRecord {
                profile: "p".into(),
                datasource: "Loki".into(),
                ts: DateTime::from_timestamp_nanos(ns).to_rfc3339_opts(SecondsFormat::Millis, true),
                labels: BTreeMap::from([("job".into(), job.into())]),
                line: line.into(),
            },
        }
    }

    #[test]
    fn cursor_from_the_initial_batch_sits_at_the_newest_nanosecond() {
        let cursor = FollowCursor::new(
            &[follow_entry(100, "api", "a"), follow_entry(200, "api", "b")],
            500,
        );

        assert_eq!(cursor.last_ns, 200);
        assert_eq!(cursor.seen_at_last.len(), 1);
    }

    #[test]
    fn cursor_from_an_empty_initial_batch_falls_back_to_the_window_end() {
        let cursor = FollowCursor::new(&[], 500);

        assert_eq!(cursor.last_ns, 500);
        assert!(cursor.seen_at_last.is_empty());
    }

    #[test]
    fn take_new_dedups_boundary_entries_by_labels_and_line() {
        let mut cursor = FollowCursor::new(&[follow_entry(200, "api", "b")], 500);

        let fresh = cursor.take_new(vec![
            follow_entry(200, "api", "b"), // re-sent at the boundary: duplicate
            follow_entry(200, "web", "c"), // new labels at the boundary: printed
            follow_entry(300, "api", "d"),
        ]);

        let lines: Vec<&str> = fresh.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["c", "d"]);
        assert_eq!(cursor.last_ns, 300);
    }

    #[test]
    fn take_new_remembers_every_entry_at_the_new_boundary() {
        let mut cursor = FollowCursor::new(&[follow_entry(200, "api", "b")], 500);

        let fresh = cursor.take_new(vec![
            follow_entry(300, "api", "d"),
            follow_entry(300, "web", "e"),
        ]);

        assert_eq!(fresh.len(), 2);
        let again = cursor.take_new(vec![
            follow_entry(300, "api", "d"),
            follow_entry(300, "web", "e"),
        ]);
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn take_new_with_only_duplicates_keeps_the_cursor() {
        let mut cursor = FollowCursor::new(&[follow_entry(200, "api", "b")], 500);

        let fresh = cursor.take_new(vec![follow_entry(200, "api", "b")]);

        assert!(fresh.is_empty(), "{fresh:?}");
        assert_eq!(cursor.last_ns, 200);
        assert_eq!(cursor.seen_at_last.len(), 1);
    }
}
