use crate::Result;
use crate::client::{GrafanaClient, Reply};
use crate::commands::prom;
use crate::commands::{CommandOutput, Windowed, success_data};
use crate::error::Error;
use crate::output::{LogRecord, OutputMode};
use chrono::{DateTime, SecondsFormat};
use serde_json::Value;
use std::collections::BTreeMap;

use super::SearchArgs;
use super::loki_context;

/// The step a LogQL metric query samples at, in whole seconds; `None` when
/// no `--step` was given, so Loki picks its own default. The same contract
/// as `metrics range`'s step: unparsable or sub-second is a config error.
pub(super) fn step_secs(step: Option<&str>) -> Result<Option<u64>> {
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
pub(super) struct Entry {
    pub(super) ns: i64,
    pub(super) record: LogRecord,
}

/// The entries of a `query_range` streams reply, sorted by (nanosecond
/// timestamp, line).
pub(super) fn log_entries(reply: &Reply, profile: &str, datasource: &str) -> Result<Vec<Entry>> {
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

fn labels_kv(labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn records(body: Value) -> Result<Vec<LogRecord>> {
        log_records(&Reply::ok(body), "p", "Loki")
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
}
