use crate::Result;
use crate::client::{Datasource, GrafanaClient};
use crate::commands::CommandOutput;
use crate::datasource::Kind;
use crate::output::OutputMode;
use serde_json::{Value, json};

#[derive(clap::Args, Clone, Debug)]
pub struct DatasourcesArgs {
    #[arg(long, value_enum)]
    pub r#type: Option<Kind>,
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
