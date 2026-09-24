//! Prometheus wire-format helpers: the shared query envelope, label sets and
//! the flattening of vector and matrix results into gcli's
//! `{metric, value, ts}` records. Kept apart from `metrics.rs` (request
//! building and dispatch) so a Loki metric query can reuse the same parsing.

use crate::client::Reply;
use crate::commands::{Result, cell, success_data};
use serde_json::{Value, json};

/// Prometheus label string: `{k="v", ...}` with keys sorted (serde_json BTreeMap).
pub(crate) fn label_string(metric: &Value) -> String {
    let Some(map) = metric.as_object() else {
        return "{}".into();
    };
    let parts: Vec<String> = map
        .iter()
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}=\"{s}\""),
            other => format!("{k}={other}"),
        })
        .collect();
    format!("{{{}}}", parts.join(", "))
}

/// `data.result` of a Prometheus reply whose `resultType` is `expected_type`,
/// taken out of the reply instead of cloned: a large range result is the
/// single biggest allocation a query makes, and nothing else reads the body.
pub(crate) fn prom_result(reply: &mut Reply, expected_type: &str) -> Result<Vec<Value>> {
    let result_type = success_data(reply)?
        .ok_or_else(|| reply.malformed("no data"))?
        .get("resultType")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if result_type != expected_type {
        return Err(reply.malformed(&format!(
            "unexpected resultType '{result_type}' (expected '{expected_type}')"
        )));
    }
    Ok(
        match reply.body.pointer_mut("/data/result").map(Value::take) {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        },
    )
}

/// Flatten a vector `result`: one `{metric, value, ts}` record per item.
/// Items without a value are skipped with a stderr note; values stay strings.
pub(crate) fn flatten_vector(result: Vec<Value>) -> Vec<Value> {
    let mut records = Vec::new();
    for item in result {
        let Some(pair) = item.get("value").and_then(Value::as_array) else {
            eprintln!("note: skipping result without value");
            continue;
        };
        records.push(json!({
            "metric": item.get("metric").cloned().unwrap_or(json!({})),
            "value": pair.get(1).cloned().unwrap_or(Value::Null),
            "ts": pair.first().cloned().unwrap_or(Value::Null),
        }));
    }
    records
}

/// (label sort key, ts Value, value Value) per sample.
type Sample = (f64, Value, Value);
type Series = (String, Value, Vec<Sample>);

/// Flatten a matrix `result`: series ordered by label string, samples by ts.
/// Non-numeric timestamps are skipped with a stderr note; values stay strings.
pub(crate) fn flatten_matrix(result: Vec<Value>) -> Vec<Value> {
    let mut series: Vec<Series> = Vec::new();
    for mut s in result {
        let metric = s
            .pointer_mut("/metric")
            .map(Value::take)
            .unwrap_or_else(|| json!({}));
        let values = match s.pointer_mut("/values").map(Value::take) {
            Some(Value::Array(values)) => values,
            _ => Vec::new(),
        };
        let mut samples = Vec::new();
        for pair in values {
            let Value::Array(arr) = pair else {
                continue;
            };
            let ts_value = arr.first().cloned().unwrap_or(Value::Null);
            let value = arr.get(1).cloned().unwrap_or(Value::Null);
            match ts_value.as_f64() {
                Some(ts) => samples.push((ts, ts_value, value)),
                None => eprintln!("note: skipping sample with non-numeric timestamp"),
            }
        }
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        series.push((label_string(&metric), metric, samples));
    }
    series.sort_by(|a, b| a.0.cmp(&b.0));
    let mut records = Vec::new();
    for (_, metric, samples) in &series {
        for (_, ts, value) in samples {
            records.push(json!({"metric": metric, "value": value, "ts": ts}));
        }
    }
    records
}

/// Table rows for a flattened vector: metric, value, ts.
pub(crate) fn vector_rows(records: &[Value]) -> Vec<Vec<String>> {
    records
        .iter()
        .map(|r| {
            vec![
                label_string(&r["metric"]),
                cell(&r["value"]),
                cell(&r["ts"]),
            ]
        })
        .collect()
}

/// Table rows for a flattened matrix: metric, timestamp, value.
pub(crate) fn matrix_rows(records: &[Value]) -> Vec<Vec<String>> {
    records
        .iter()
        .map(|r| {
            vec![
                label_string(&r["metric"]),
                cell(&r["ts"]),
                cell(&r["value"]),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn label_string_sorted_quoted() {
        assert_eq!(
            label_string(&json!({"job": "node", "__name__": "up"})),
            "{__name__=\"up\", job=\"node\"}"
        );
        assert_eq!(label_string(&json!({})), "{}");
        assert_eq!(label_string(&json!(null)), "{}");
    }

    #[test]
    fn vector_records_keep_upstream_value_strings() {
        let result = vec![json!({
            "metric": {"__name__": "up", "job": "node"},
            "value": [1759372800, "1"]
        })];

        let records = flatten_vector(result);

        assert_eq!(
            Value::Array(records),
            json!([
                {"metric": {"__name__": "up", "job": "node"}, "value": "1", "ts": 1759372800}
            ])
        );
    }

    #[test]
    fn vector_result_without_value_is_skipped() {
        let result = vec![json!({"metric": {"job": "a"}})];

        let records = flatten_vector(result);

        assert!(records.is_empty(), "{records:?}");
    }

    #[test]
    fn range_rows_sorted_series_then_ts() {
        let body = json!({
            "status": "success",
            "data": {"resultType": "matrix", "result": [
                {"metric": {"job": "zeta"}, "values": [[1759372800, "5"], [1759372700, "4"]]},
                {"metric": {"job": "api"}, "values": [[1759372700, "1"], [1759372800, "2"]]}
            ]}
        });
        let records = flatten_matrix(prom_result(&mut Reply::ok(body), "matrix").unwrap());
        assert_eq!(
            Value::Array(records.clone()),
            json!([
                {"metric": {"job": "api"}, "value": "1", "ts": 1759372700},
                {"metric": {"job": "api"}, "value": "2", "ts": 1759372800},
                {"metric": {"job": "zeta"}, "value": "4", "ts": 1759372700},
                {"metric": {"job": "zeta"}, "value": "5", "ts": 1759372800}
            ])
        );
        let rows = matrix_rows(&records);
        let flat: Vec<String> = rows.iter().flatten().cloned().collect();
        assert_eq!(
            flat,
            vec![
                "{job=\"api\"}",
                "1759372700",
                "1",
                "{job=\"api\"}",
                "1759372800",
                "2",
                "{job=\"zeta\"}",
                "1759372700",
                "4",
                "{job=\"zeta\"}",
                "1759372800",
                "5",
            ]
        );
        let rendered =
            crate::output::render_table(&["metric", "timestamp", "value"].map(String::from), &rows);
        assert!(rendered.contains("{job=\"api\"}"), "{rendered}");
        assert!(rendered.contains("{job=\"zeta\"}"), "{rendered}");
    }

    #[test]
    fn non_numeric_ts_skipped_not_panicked() {
        let result = json!([
            {"metric": {"job": "a"}, "values": [["not-a-number", "1"], [1759372700, "2"]]}
        ]);

        let records = flatten_matrix(result.as_array().unwrap().clone());

        assert_eq!(records.len(), 1);
    }

    fn prom_err(body: Value) -> Error {
        prom_result(&mut Reply::ok(body), "vector").unwrap_err()
    }

    #[test]
    fn error_status_is_http_with_the_upstream_error() {
        let err =
            prom_err(json!({"status": "error", "errorType": "bad_data", "error": "parse error"}));

        assert!(
            matches!(&err, Error::Http { status: 200, message: Some(m), .. } if m == "parse error"),
            "{err:?}"
        );
    }

    #[test]
    fn wrong_result_type_is_a_response_error() {
        let err =
            prom_err(json!({"status": "success", "data": {"resultType": "scalar", "result": []}}));

        assert!(
            matches!(&err, Error::Response { status: 200, .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("scalar"), "{err}");
    }

    #[test]
    fn missing_data_is_a_response_error() {
        let err = prom_err(json!({"status": "success"}));

        assert!(
            matches!(&err, Error::Response { status: 200, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn missing_status_is_a_response_error() {
        let err = prom_err(json!({"data": {"resultType": "vector", "result": []}}));

        assert!(
            matches!(&err, Error::Response { status: 200, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn result_of_the_wrong_shape_is_empty_not_an_error() {
        let body = json!({"status": "success", "data": {"resultType": "matrix", "result": 7}});

        let result = prom_result(&mut Reply::ok(body), "matrix").unwrap();

        assert!(result.is_empty(), "{result:?}");
    }
}
