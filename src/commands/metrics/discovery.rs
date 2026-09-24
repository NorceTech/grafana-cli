use crate::Result;
use crate::client::{GrafanaClient, Reply};
use crate::commands::prom;
use crate::commands::{Bounds, CommandOutput, Windowed, cell, resolve_ds, success_data};
use crate::datasource::Kind;
use crate::output::OutputMode;
use crate::timeutil::to_epoch_secs;
use serde::Deserialize;
use serde_json::Value;

use super::{ExemplarsArgs, LabelValuesArgs, LabelsArgs, SeriesArgs, table};

/// The shared `match[]` + window pairs of the series-metadata endpoints,
/// Prometheus fractional epoch seconds.
fn windowed_pairs(
    matches: &[String],
    Bounds { start, end }: Bounds,
) -> Vec<(&'static str, String)> {
    let mut pairs: Vec<(&str, String)> = matches.iter().map(|m| ("match[]", m.clone())).collect();
    pairs.push(("start", format!("{}", to_epoch_secs(start))));
    pairs.push(("end", format!("{}", to_epoch_secs(end))));
    pairs
}

/// The strings in a `labels` or `label/{name}/values` reply. Mirrors
/// `logs::string_array`: a success without `data` reads as empty rather than
/// an error.
fn string_array(reply: &Reply) -> Result<Vec<String>> {
    let Some(data) = success_data(reply)? else {
        return Ok(Vec::new());
    };
    Vec::<String>::deserialize(data).map_err(|e| reply.malformed(&format!("data: {e}")))
}

fn json_string_array(names: Vec<String>) -> Value {
    Value::Array(names.into_iter().map(Value::String).collect())
}

/// `data` of a `series` or `query_exemplars` reply: an array, taken out of
/// the body (nothing else reads it). A success without `data`, or with a
/// non-array, is malformed.
fn data_array(reply: &mut Reply) -> Result<Vec<Value>> {
    match success_data(reply)? {
        Some(data) if data.is_array() => {}
        Some(_) => return Err(reply.malformed("data is not an array")),
        None => return Err(reply.malformed("no data")),
    }
    Ok(match reply.body.pointer_mut("/data").map(Value::take) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    })
}

/// Series label sets ordered by their label string, the family's
/// deterministic order.
fn sorted_series(data: Vec<Value>) -> Vec<(String, Value)> {
    let mut series: Vec<(String, Value)> = data
        .into_iter()
        .map(|labels| (prom::label_string(&labels), labels))
        .collect();
    series.sort_by(|a, b| a.0.cmp(&b.0));
    series
}

/// Flatten a `query_exemplars` result: one `{metric, labels, value, ts}`
/// record per exemplar, ordered by (series label string, ts). Values stay
/// strings, ts stays the upstream fractional-seconds number; an exemplar
/// with a non-numeric timestamp is skipped with a stderr note.
fn exemplar_records(data: Vec<Value>) -> Vec<Value> {
    let mut keyed: Vec<((String, f64), Value)> = Vec::new();
    for mut group in data {
        let metric = group
            .pointer_mut("/seriesLabels")
            .map(Value::take)
            .unwrap_or_else(|| Value::Object(Default::default()));
        let exemplars = match group.pointer_mut("/exemplars").map(Value::take) {
            Some(Value::Array(exemplars)) => exemplars,
            _ => Vec::new(),
        };
        let series = prom::label_string(&metric);
        for mut exemplar in exemplars {
            let labels = exemplar
                .pointer_mut("/labels")
                .map(Value::take)
                .unwrap_or_else(|| Value::Object(Default::default()));
            let ts = exemplar.get("timestamp").cloned().unwrap_or(Value::Null);
            let value = exemplar.get("value").cloned().unwrap_or(Value::Null);
            let Some(ts_secs) = ts.as_f64() else {
                eprintln!("note: skipping exemplar with non-numeric timestamp");
                continue;
            };
            keyed.push((
                (series.clone(), ts_secs),
                serde_json::json!({"metric": metric, "labels": labels, "value": value, "ts": ts}),
            ));
        }
    }
    keyed.sort_by(|a, b| a.0.0.cmp(&b.0.0).then_with(|| a.0.1.total_cmp(&b.0.1)));
    keyed.into_iter().map(|(_, record)| record).collect()
}

/// Table rows for flattened exemplars: metric, traceID, value, ts. The trace
/// id is `labels.trace_id`, else `labels.traceID`, else empty.
fn exemplar_rows(records: &[Value]) -> Vec<Vec<String>> {
    records
        .iter()
        .map(|r| {
            let labels = &r["labels"];
            let trace_id = labels
                .get("trace_id")
                .or_else(|| labels.get("traceID"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![
                prom::label_string(&r["metric"]),
                trace_id.to_string(),
                cell(&r["value"]),
                cell(&r["ts"]),
            ]
        })
        .collect()
}

// `labels` and `label-values` ignore `mode` and always answer a JSON array,
// like their logs siblings.
pub async fn labels(
    client: &GrafanaClient,
    _profile: &str,
    args: Windowed<'_, LabelsArgs>,
) -> Result<CommandOutput> {
    let Windowed { args, bounds } = args;
    let ds = resolve_ds(client, Kind::Prometheus, args.window.datasource.as_deref()).await?;
    let reply = client
        .proxy_request(
            &ds.uid,
            "api/v1/labels",
            &windowed_pairs(&args.matches, bounds),
        )
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &reply,
    )?)))
}

// The label is validated (`logs::validate_label_name`) before the proxy path
// is built; see `Query::resolve`.
pub async fn label_values(
    client: &GrafanaClient,
    _profile: &str,
    args: Windowed<'_, LabelValuesArgs>,
) -> Result<CommandOutput> {
    let Windowed { args, bounds } = args;
    let ds = resolve_ds(client, Kind::Prometheus, args.window.datasource.as_deref()).await?;
    let reply = client
        .proxy_request(
            &ds.uid,
            &format!("api/v1/label/{}/values", args.label),
            &windowed_pairs(&args.matches, bounds),
        )
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &reply,
    )?)))
}

pub async fn series(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    args: Windowed<'_, SeriesArgs>,
) -> Result<CommandOutput> {
    let Windowed { args, bounds } = args;
    let ds = resolve_ds(client, Kind::Prometheus, args.window.datasource.as_deref()).await?;
    let mut reply = client
        .proxy_request(
            &ds.uid,
            "api/v1/series",
            &windowed_pairs(&args.matches, bounds),
        )
        .await?;
    let sorted = sorted_series(data_array(&mut reply)?);
    if mode == OutputMode::Table {
        let rows = sorted
            .iter()
            .map(|(label, _)| vec![label.clone()])
            .collect();
        return Ok(table(&["series"], rows));
    }
    Ok(CommandOutput::Json(Value::Array(
        sorted.into_iter().map(|(_, labels)| labels).collect(),
    )))
}

pub async fn exemplars(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    args: Windowed<'_, ExemplarsArgs>,
) -> Result<CommandOutput> {
    let Windowed {
        args,
        bounds: Bounds { start, end },
    } = args;
    let ds = resolve_ds(client, Kind::Prometheus, args.window.datasource.as_deref()).await?;
    let pairs = [
        ("query", args.query.clone()),
        ("start", format!("{}", to_epoch_secs(start))),
        ("end", format!("{}", to_epoch_secs(end))),
    ];
    let mut reply = client
        .proxy_request(&ds.uid, "api/v1/query_exemplars", &pairs)
        .await?;
    let records = exemplar_records(data_array(&mut reply)?);
    if mode == OutputMode::Table {
        return Ok(table(
            &["metric", "traceID", "value", "ts"],
            exemplar_rows(&records),
        ));
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Reply;
    use crate::error::Error;
    use serde_json::json;

    fn exemplar_metric(service: &str) -> Value {
        json!({"__name__": "http_request_duration_seconds_bucket", "service": service})
    }

    #[test]
    fn exemplar_records_sort_by_series_label_string_then_ts() {
        // cart listed before api, each group's exemplars unsorted.
        let data = vec![
            json!({
                "seriesLabels": exemplar_metric("cart"),
                "exemplars": [
                    {"labels": {"trace_id": "t-cart-2"}, "value": "0.42", "timestamp": 1759372860.5},
                    {"labels": {"trace_id": "t-cart-1"}, "value": "0.11", "timestamp": 1759372800.25}
                ]
            }),
            json!({
                "seriesLabels": exemplar_metric("api"),
                "exemplars": [
                    {"labels": {"trace_id": "t-api-1"}, "value": "0.05", "timestamp": 1759372900.0}
                ]
            }),
        ];

        let records = exemplar_records(data);

        assert_eq!(
            Value::Array(records),
            json!([
                {"metric": exemplar_metric("api"), "labels": {"trace_id": "t-api-1"},
                 "value": "0.05", "ts": 1759372900.0},
                {"metric": exemplar_metric("cart"), "labels": {"trace_id": "t-cart-1"},
                 "value": "0.11", "ts": 1759372800.25},
                {"metric": exemplar_metric("cart"), "labels": {"trace_id": "t-cart-2"},
                 "value": "0.42", "ts": 1759372860.5}
            ])
        );
    }

    #[test]
    fn exemplar_rows_trace_id_prefers_trace_id_then_trace_id() {
        let records = vec![
            json!({"metric": {"service": "a"}, "labels": {"trace_id": "tid-1"},
                   "value": "1", "ts": 1.0}),
            json!({"metric": {"service": "b"}, "labels": {"traceID": "tid-2"},
                   "value": "2", "ts": 2.0}),
            json!({"metric": {"service": "c"}, "labels": {"other": "x"},
                   "value": "3", "ts": 3.0}),
        ];

        let rows = exemplar_rows(&records);

        assert_eq!(
            rows,
            vec![
                vec!["{service=\"a\"}", "tid-1", "1", "1.0"],
                vec!["{service=\"b\"}", "tid-2", "2", "2.0"],
                vec!["{service=\"c\"}", "", "3", "3.0"],
            ]
        );
    }

    #[test]
    fn exemplar_without_a_numeric_ts_is_skipped() {
        let data = vec![json!({
            "seriesLabels": exemplar_metric("api"),
            "exemplars": [
                {"labels": {"trace_id": "t-1"}, "value": "1", "timestamp": "not-a-number"},
                {"labels": {"trace_id": "t-2"}, "value": "2", "timestamp": 1759372800.0}
            ]
        })];

        let records = exemplar_records(data);

        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0]["labels"]["trace_id"], "t-2");
    }

    #[test]
    fn series_ordered_by_label_string() {
        let data = vec![
            json!({"__name__": "up", "job": "node", "instance": "node-1:9100"}),
            json!({"__name__": "up", "job": "api", "instance": "api-1:8080"}),
        ];

        let sorted = sorted_series(data);

        let keys: Vec<&str> = sorted.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "{__name__=\"up\", instance=\"api-1:8080\", job=\"api\"}",
                "{__name__=\"up\", instance=\"node-1:9100\", job=\"node\"}"
            ]
        );
        assert_eq!(sorted[1].1["job"], "node");
    }

    #[test]
    fn series_data_without_an_array_is_a_response_error() {
        for body in [
            json!({"status": "success"}),
            json!({"status": "success", "data": 7}),
        ] {
            let result = data_array(&mut Reply::ok(body));

            assert!(
                matches!(result, Err(Error::Response { status: 200, .. })),
                "{result:?}"
            );
        }
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

    #[test]
    fn label_reply_without_data_is_empty() {
        let reply = Reply::ok(json!({"status": "success"}));

        let names = string_array(&reply).unwrap();

        assert!(names.is_empty(), "{names:?}");
    }

    #[test]
    fn error_status_is_http_with_the_upstream_error() {
        let body = json!({"status": "error", "errorType": "bad_data", "error": "parse error"});

        let err = data_array(&mut Reply::ok(body)).unwrap_err();

        assert!(
            matches!(&err, Error::Http { status: 200, message: Some(m), .. } if m == "parse error"),
            "{err:?}"
        );
    }
}
