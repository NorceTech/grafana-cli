use crate::Result;
use crate::client::{Datasource, GrafanaClient, Reply};
use crate::commands::{Bounds, CommandOutput, QueryWindow, Windowed, success_data};
use crate::datasource::Kind;
use crate::output::OutputMode;
use crate::timeutil::{default_step_secs, to_epoch_secs};
use crate::{Error, datasource};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

#[derive(clap::Subcommand, Clone, Debug)]
pub enum MetricsCmd {
    Instant(InstantArgs),
    Range(RangeArgs),
}

#[derive(clap::Args, Clone, Debug)]
pub struct InstantArgs {
    pub query: String,
    #[arg(long)]
    pub at: Option<String>,
    #[arg(long)]
    pub datasource: Option<String>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct RangeArgs {
    pub query: String,
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long)]
    pub step: Option<String>,
}

impl MetricsCmd {
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
        bounds: Bounds,
    ) -> Result<CommandOutput> {
        match self {
            Self::Instant(args) => instant(client, profile, mode, args).await,
            Self::Range(args) => range(client, profile, mode, Windowed { args, bounds }).await,
        }
    }
}

async fn resolve_ds(client: &GrafanaClient, flag: Option<&str>) -> Result<Datasource> {
    let all = client.datasources().await?;
    Ok(datasource::resolve(&all, Kind::Prometheus, flag)?.clone())
}

/// Prometheus label string: `{k="v", ...}` with keys sorted (serde_json BTreeMap).
fn label_string(metric: &Value) -> String {
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

fn cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `data.result` of a Prometheus reply whose `resultType` is `expected_type`.
fn prom_result(reply: &Reply, expected_type: &str) -> Result<Vec<Value>> {
    let data = success_data(reply)?.ok_or_else(|| reply.malformed("no data"))?;
    let rt = data.get("resultType").and_then(Value::as_str).unwrap_or("");
    if rt != expected_type {
        return Err(reply.malformed(&format!(
            "unexpected resultType '{rt}' (expected '{expected_type}')"
        )));
    }
    Ok(data
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn table(headers: [&str; 3], rows: Vec<Vec<String>>) -> CommandOutput {
    CommandOutput::Table {
        headers: headers.map(String::from).to_vec(),
        rows,
    }
}

pub async fn instant(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    args: &InstantArgs,
) -> Result<CommandOutput> {
    let ds = resolve_ds(client, args.datasource.as_deref()).await?;
    let mut pairs = vec![("query", args.query.clone())];
    if let Some(at) = args.at.as_deref() {
        let dt = DateTime::parse_from_rfc3339(at)
            .map_err(|_| Error::Config {
                message: format!("invalid RFC3339 timestamp (--at): '{at}'"),
            })?
            .with_timezone(&Utc);
        pairs.push(("time", format!("{}", to_epoch_secs(dt))));
    }
    let reply = client
        .proxy_request(&ds.uid, "api/v1/query", &pairs)
        .await?;
    let result = prom_result(&reply, "vector")?;
    let mut records = Vec::new();
    for item in &result {
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
    if mode == OutputMode::Table {
        let rows = records
            .iter()
            .map(|r| {
                vec![
                    label_string(&r["metric"]),
                    cell(&r["value"]),
                    cell(&r["ts"]),
                ]
            })
            .collect();
        return Ok(table(["metric", "value", "ts"], rows));
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}

/// (label sort key, ts Value, value Value) per sample.
type Sample = (f64, Value, Value);
type Series = (String, Value, Vec<Sample>);

/// Flatten matrix result: series ordered by label string, samples by ts.
/// Non-numeric timestamps are skipped with a stderr note; values stay strings.
fn flatten_matrix(result: &[Value]) -> (Vec<Value>, Vec<Vec<String>>) {
    let mut series: Vec<Series> = Vec::new();
    for s in result {
        let metric = s.get("metric").cloned().unwrap_or(json!({}));
        let mut samples = Vec::new();
        for pair in s
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(arr) = pair.as_array() else {
                continue;
            };
            match arr.first().and_then(Value::as_f64) {
                Some(ts) => samples.push((
                    ts,
                    arr.first().cloned().unwrap_or(Value::Null),
                    arr.get(1).cloned().unwrap_or(Value::Null),
                )),
                None => eprintln!("note: skipping sample with non-numeric timestamp"),
            }
        }
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        series.push((label_string(&metric), metric, samples));
    }
    series.sort_by(|a, b| a.0.cmp(&b.0));
    let mut records = Vec::new();
    let mut rows = Vec::new();
    for (_, metric, samples) in &series {
        for (_, ts, value) in samples {
            records.push(json!({"metric": metric, "value": value, "ts": ts}));
            rows.push(vec![label_string(metric), cell(ts), cell(value)]);
        }
    }
    (records, rows)
}

pub async fn range(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    range: Windowed<'_, RangeArgs>,
) -> Result<CommandOutput> {
    let Windowed {
        args,
        bounds: Bounds { start, end },
    } = range;
    let ds = resolve_ds(client, args.window.datasource.as_deref()).await?;
    let step_secs = match args.step.as_deref() {
        Some(s) => {
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
            secs
        }
        None => default_step_secs(start, end),
    };
    let pairs = [
        ("query", args.query.clone()),
        ("start", format!("{}", to_epoch_secs(start))),
        ("end", format!("{}", to_epoch_secs(end))),
        ("step", step_secs.to_string()),
    ];
    let reply = client
        .proxy_request(&ds.uid, "api/v1/query_range", &pairs)
        .await?;
    let result = prom_result(&reply, "matrix")?;
    let (records, rows) = flatten_matrix(&result);
    if mode == OutputMode::Table {
        return Ok(table(["metric", "timestamp", "value"], rows));
    }
    Ok(CommandOutput::Json(Value::Array(records)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn range_rows_sorted_series_then_ts() {
        let body = json!({
            "status": "success",
            "data": {"resultType": "matrix", "result": [
                {"metric": {"job": "zeta"}, "values": [[1759372800, "5"], [1759372700, "4"]]},
                {"metric": {"job": "api"}, "values": [[1759372700, "1"], [1759372800, "2"]]}
            ]}
        });
        let result = prom_result(&Reply::ok(body), "matrix").unwrap();
        let (records, rows) = flatten_matrix(&result);
        assert_eq!(
            Value::Array(records),
            json!([
                {"metric": {"job": "api"}, "value": "1", "ts": 1759372700},
                {"metric": {"job": "api"}, "value": "2", "ts": 1759372800},
                {"metric": {"job": "zeta"}, "value": "4", "ts": 1759372700},
                {"metric": {"job": "zeta"}, "value": "5", "ts": 1759372800}
            ])
        );
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
        let rendered = crate::output::render_table(&["metric", "timestamp", "value"], rows);
        assert!(rendered.contains("{job=\"api\"}"), "{rendered}");
        assert!(rendered.contains("{job=\"zeta\"}"), "{rendered}");
    }

    #[test]
    fn non_numeric_ts_skipped_not_panicked() {
        let result = json!([
            {"metric": {"job": "a"}, "values": [["not-a-number", "1"], [1759372700, "2"]]}
        ]);
        let (records, rows) = flatten_matrix(result.as_array().unwrap());
        assert_eq!(records.len(), 1);
        assert_eq!(rows.len(), 1);
    }

    fn prom_err(body: Value) -> Error {
        prom_result(&Reply::ok(body), "vector").unwrap_err()
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
            matches!(err, Error::Response { status: 200, .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("scalar"), "{err}");
    }

    #[test]
    fn missing_data_is_a_response_error() {
        let err = prom_err(json!({"status": "success"}));

        assert!(
            matches!(err, Error::Response { status: 200, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn missing_status_is_a_response_error() {
        let err = prom_err(json!({"data": {"resultType": "vector", "result": []}}));

        assert!(
            matches!(err, Error::Response { status: 200, .. }),
            "{err:?}"
        );
    }
}
