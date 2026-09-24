use crate::Result;
use crate::client::GrafanaClient;
use crate::commands::{CommandOutput, cell};
use crate::output::OutputMode;

pub async fn run(
    client: &GrafanaClient,
    _profile: &str,
    mode: OutputMode,
) -> Result<CommandOutput> {
    let value = client.health().await?;
    if mode == OutputMode::Table {
        let rows = value
            .as_object()
            .map(|obj| obj.iter().map(|(k, v)| vec![k.clone(), cell(v)]).collect())
            .unwrap_or_default();
        return Ok(CommandOutput::Table {
            headers: ["key", "value"].map(String::from).to_vec(),
            rows,
        });
    }
    Ok(CommandOutput::Json(value))
}
