use crate::Result;
use crate::client::{Datasource, GrafanaClient};
use crate::commands::CommandOutput;
use crate::output::OutputMode;
use serde_json::{Value, json};

pub async fn run(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
    ds_type: Option<&str>,
) -> Result<CommandOutput> {
    let all = client.datasources().await?;
    let list: Vec<&Datasource> = all
        .iter()
        .filter(|d| ds_type.is_none_or(|t| d.r#type == t))
        .collect();
    if mode == OutputMode::Table {
        let rows = list
            .iter()
            .map(|d| {
                vec![
                    d.name.clone(),
                    d.r#type.clone(),
                    d.uid.clone(),
                    d.url.clone(),
                    if d.is_default {
                        "*".into()
                    } else {
                        String::new()
                    },
                ]
            })
            .collect();
        Ok(CommandOutput::Table {
            headers: ["name", "type", "uid", "url", "default"]
                .map(String::from)
                .to_vec(),
            rows,
        })
    } else {
        let records = list
            .iter()
            .map(|d| {
                json!({
                    "name": d.name,
                    "type": d.r#type,
                    "uid": d.uid,
                    "url": d.url,
                    "isDefault": d.is_default,
                })
            })
            .collect();
        Ok(CommandOutput::Json(Value::Array(records)))
    }
}
