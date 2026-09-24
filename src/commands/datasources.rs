use crate::Result;
use crate::client::{Datasource, DatasourceHealth, GrafanaClient};
use crate::commands::CommandOutput;
use crate::datasource::Kind;
use crate::error::Error;
use crate::output::OutputMode;
use serde_json::{Value, json};

#[derive(clap::Args, Clone, Debug)]
pub struct DatasourcesArgs {
    #[arg(long, value_enum)]
    pub r#type: Option<Kind>,
    /// Check each listed datasource's health
    #[arg(long)]
    pub check: bool,
}

pub async fn run(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    args: &DatasourcesArgs,
) -> Result<CommandOutput> {
    let all = client.datasources().await?;
    let list: Vec<&Datasource> = all
        .iter()
        .filter(|d| args.r#type.is_none_or(|kind| d.r#type == kind.as_str()))
        .collect();
    if mode == OutputMode::Table {
        let mut headers = ["name", "type", "uid", "url", "default"]
            .map(String::from)
            .to_vec();
        if args.check {
            headers.extend(["health", "message"].map(String::from));
        }
        let mut rows = Vec::with_capacity(list.len());
        for d in &list {
            let mut row = vec![
                d.name.clone(),
                d.r#type.clone(),
                d.uid.clone(),
                d.url.clone(),
                if d.is_default {
                    "*".into()
                } else {
                    String::new()
                },
            ];
            if args.check {
                let health = health_of(client, &d.uid).await;
                row.push(health.status);
                row.push(health.message);
            }
            rows.push(row);
        }
        Ok(CommandOutput::Table { headers, rows })
    } else {
        let mut records = Vec::with_capacity(list.len());
        for d in &list {
            let mut record = json!({
                "name": d.name,
                "type": d.r#type,
                "uid": d.uid,
                "url": d.url,
                "isDefault": d.is_default,
            });
            if args.check {
                let health = health_of(client, &d.uid).await;
                record["health"] = json!(health.status);
                record["healthMessage"] = json!(health.message);
            }
            records.push(record);
        }
        Ok(CommandOutput::Json(Value::Array(records)))
    }
}

/// One datasource's health for `--check`: the endpoint's status and message,
/// or status "ERROR" and the error text when the call failed. Datasource
/// types without health support answer 4xx/5xx; one failing health call
/// never fails the command.
async fn health_of(client: &GrafanaClient, uid: &str) -> DatasourceHealth {
    match client.datasource_health(uid).await {
        Ok(health) => health,
        Err(err) => DatasourceHealth {
            status: "ERROR".into(),
            message: health_error_text(&err),
        },
    }
}

/// The text reported for a failed health call: the upstream's own message
/// when the reply carried one, else the error's text.
fn health_error_text(err: &Error) -> String {
    match err {
        Error::Http { message, body, .. } => {
            message.clone().unwrap_or_else(|| body.trim().to_string())
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_error_text_prefers_the_upstream_message() {
        let err = Error::Http {
            status: 404,
            body: r#"{"message":"Data source does not support health checks"}"#.into(),
            message: Some("Data source does not support health checks".into()),
        };

        assert_eq!(
            health_error_text(&err),
            "Data source does not support health checks"
        );
    }

    #[test]
    fn health_error_text_falls_back_to_the_plain_body() {
        let err = Error::Http {
            status: 502,
            body: "bad gateway\n".into(),
            message: None,
        };

        assert_eq!(health_error_text(&err), "bad gateway");
    }

    #[test]
    fn health_error_text_uses_the_error_text_otherwise() {
        let err = Error::Config {
            message: "bad".into(),
        };

        assert_eq!(health_error_text(&err), "config error: bad");
    }
}
