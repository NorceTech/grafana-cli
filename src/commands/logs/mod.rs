use crate::Result;
use crate::client::{Datasource, GrafanaClient};
use crate::commands::{Bounds, CommandOutput, QueryWindow, Windowed, resolve_ds};
use crate::datasource::Kind;
use crate::output::OutputMode;
use crate::timeutil::to_ns_string;

mod follow;
mod instant;
mod labels;
mod search;

pub use self::follow::{FollowStream, follow_start};
pub use self::instant::instant;
pub(crate) use self::labels::validate_label_name;
pub use self::labels::{label_values, labels};
pub use self::search::search;

#[derive(clap::Subcommand, Clone, Debug)]
pub enum LogsCmd {
    Search(SearchArgs),
    Instant(InstantArgs),
    Labels {
        #[command(flatten)]
        window: QueryWindow,
        // The selector label discovery is scoped to, sent as Loki's query=.
        #[arg(long, value_name = "SELECTOR")]
        r#match: Option<String>,
    },
    LabelValues {
        label: String,
        #[command(flatten)]
        window: QueryWindow,
        #[arg(long, value_name = "SELECTOR")]
        r#match: Option<String>,
    },
}

#[derive(clap::Args, Clone, Debug)]
pub struct SearchArgs {
    pub query: String,
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long, default_value_t = 100)]
    pub limit: u64,
    #[arg(long, value_enum, default_value = "backward")]
    pub direction: Direction,
    // The sampling step of a LogQL metric query, in whole seconds; without
    // it Loki picks a dynamic default.
    #[arg(long, value_name = "DURATION")]
    pub step: Option<String>,
    // Keep printing new entries as they arrive; --output is ignored (always
    // JSONL) and exactly one profile may be selected.
    #[arg(short = 'f', long)]
    pub follow: bool,
    // How often a --follow run polls for new entries.
    #[arg(long, value_name = "DURATION", default_value = "2s")]
    pub interval: String,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Direction {
    Forward,
    Backward,
}

// A LogQL metric query evaluated once: no window, only an optional instant.
#[derive(clap::Args, Clone, Debug)]
pub struct InstantArgs {
    pub query: String,
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    #[arg(long)]
    pub datasource: Option<String>,
}

impl Direction {
    /// Loki's `direction` query parameter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
        }
    }
}

impl LogsCmd {
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
        bounds: Bounds,
    ) -> Result<CommandOutput> {
        match self {
            Self::Search(args) => search(client, profile, mode, Windowed { args, bounds }).await,
            Self::Instant(args) => instant(client, profile, mode, args).await,
            Self::Labels { window, r#match } => {
                labels(
                    client,
                    profile,
                    Windowed {
                        args: window,
                        bounds,
                    },
                    r#match.as_deref(),
                )
                .await
            }
            Self::LabelValues {
                label,
                window,
                r#match,
            } => {
                label_values(
                    client,
                    profile,
                    label,
                    Windowed {
                        args: window,
                        bounds,
                    },
                    r#match.as_deref(),
                )
                .await
            }
        }
    }
}

/// Window bounds (ns strings) + the resolved Loki datasource for a query.
/// The bounds are encoded before any datasource is fetched, so a window
/// outside the representable range never reaches the instance.
async fn loki_context(
    client: &GrafanaClient,
    window: Windowed<'_, QueryWindow>,
) -> Result<(String, String, Datasource)> {
    let start = to_ns_string(window.bounds.start)?;
    let end = to_ns_string(window.bounds.end)?;
    let ds = resolve_ds(client, Kind::Loki, window.args.datasource.as_deref()).await?;
    Ok((start, end, ds))
}
