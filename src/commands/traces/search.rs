use crate::Result;
use crate::client::GrafanaClient;
use crate::commands::{CommandOutput, Windowed, cell, resolve_ds};
use crate::datasource::Kind;
use crate::output::OutputMode;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use super::SearchArgs;
use super::tempo_secs;

fn nanos_to_rfc3339(s: &str) -> Option<String> {
    let ns: i64 = s.parse().ok()?;
    // rem_euclid(1_000_000_000) is in 0..1_000_000_000, always inside u32, so
    // the narrowing conversion cannot fail; try_from states that instead of a
    // truncating `as`.
    let subsec_nanos = u32::try_from(ns.rem_euclid(1_000_000_000)).ok()?;
    DateTime::<Utc>::from_timestamp(ns.div_euclid(1_000_000_000), subsec_nanos)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub async fn search(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    search: Windowed<'_, SearchArgs>,
) -> Result<CommandOutput> {
    let Windowed { args, bounds } = search;
    let ds = resolve_ds(client, Kind::Tempo, args.window.datasource.as_deref()).await?;
    let (start, end) = tempo_secs(bounds);
    let pairs = [
        ("q", args.query.clone()),
        ("start", start),
        ("end", end),
        ("limit", args.limit.to_string()),
    ];
    let mut reply = client.proxy_request(&ds.uid, "api/search", &pairs).await?;
    // `traces` is taken out of the body instead of cloned: it is the whole
    // result, and nothing else reads the reply.
    let traces = match reply.body.pointer_mut("/traces").map(Value::take) {
        Some(Value::Array(traces)) => traces,
        _ => Vec::new(),
    };

    let mut records = Vec::new();
    for t in &traces {
        let start = t
            .get("startTimeUnixNano")
            .and_then(Value::as_str)
            .and_then(nanos_to_rfc3339);
        if t.get("startTimeUnixNano").is_some() && start.is_none() {
            eprintln!(
                "note: trace {} has unparseable startTimeUnixNano",
                t.get("traceID").and_then(Value::as_str).unwrap_or("?")
            );
        }
        records.push(json!({
            "traceID": t.get("traceID").cloned().unwrap_or(Value::Null),
            "rootServiceName": t.get("rootServiceName").cloned().unwrap_or(Value::Null),
            "rootTraceName": t.get("rootTraceName").cloned().unwrap_or(Value::Null),
            "durationMs": t.get("durationMs").cloned().unwrap_or(Value::Null),
            "start": start,
        }));
    }
    if mode == OutputMode::Table {
        let rows = records
            .iter()
            .map(|r| {
                vec![
                    cell(&r["traceID"]),
                    cell(&r["rootServiceName"]),
                    cell(&r["rootTraceName"]),
                    cell(&r["durationMs"]),
                    cell(&r["start"]),
                ]
            })
            .collect();
        return Ok(CommandOutput::Table {
            headers: [
                "traceID",
                "rootServiceName",
                "rootTraceName",
                "durationMs",
                "start",
            ]
            .map(String::from)
            .to_vec(),
            rows,
        });
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanos_string_to_rfc3339_millis() {
        assert_eq!(
            nanos_to_rfc3339("1759372800000000000").as_deref(),
            Some("2025-10-02T02:40:00.000Z")
        );
        assert_eq!(
            nanos_to_rfc3339("1759372800500000000").as_deref(),
            Some("2025-10-02T02:40:00.500Z")
        );
        assert_eq!(nanos_to_rfc3339("junk"), None);
        assert_eq!(nanos_to_rfc3339(""), None);
    }
}
