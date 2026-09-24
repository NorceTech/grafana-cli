use crate::Result;
use crate::client::GrafanaClient;
use crate::commands::{Bounds, CommandOutput, QueryWindow, Windowed};
use crate::output::OutputMode;

mod discovery;
mod query;

pub use self::discovery::{exemplars, label_values, labels, series};
pub use self::query::{instant, range};

#[derive(clap::Subcommand, Clone, Debug)]
pub enum MetricsCmd {
    Instant(InstantArgs),
    Range(RangeArgs),
    Labels(LabelsArgs),
    LabelValues(LabelValuesArgs),
    Series(SeriesArgs),
    Exemplars(ExemplarsArgs),
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

#[derive(clap::Args, Clone, Debug)]
pub struct LabelsArgs {
    #[arg(long = "match")]
    pub matches: Vec<String>,
    #[command(flatten)]
    pub window: QueryWindow,
}

#[derive(clap::Args, Clone, Debug)]
pub struct LabelValuesArgs {
    pub label: String,
    #[arg(long = "match")]
    pub matches: Vec<String>,
    #[command(flatten)]
    pub window: QueryWindow,
}

#[derive(clap::Args, Clone, Debug)]
pub struct SeriesArgs {
    #[arg(long = "match", required = true)]
    pub matches: Vec<String>,
    #[command(flatten)]
    pub window: QueryWindow,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ExemplarsArgs {
    pub query: String,
    #[command(flatten)]
    pub window: QueryWindow,
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
            Self::Labels(args) => labels(client, profile, Windowed { args, bounds }).await,
            Self::LabelValues(args) => {
                label_values(client, profile, Windowed { args, bounds }).await
            }
            Self::Series(args) => series(client, profile, mode, Windowed { args, bounds }).await,
            Self::Exemplars(args) => {
                exemplars(client, profile, mode, Windowed { args, bounds }).await
            }
        }
    }
}

fn table(headers: &[&str], rows: Vec<Vec<String>>) -> CommandOutput {
    CommandOutput::Table {
        headers: headers.iter().map(|h| h.to_string()).collect(),
        rows,
    }
}
