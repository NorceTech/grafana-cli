use crate::Result;
use crate::client::GrafanaClient;
use crate::commands::{Bounds, CommandOutput, QueryWindow, Windowed};
use crate::output::OutputMode;

mod get;
mod search;
mod tags;

pub use self::get::get;
pub(crate) use self::get::validate_trace_id;
pub use self::search::search;
pub(crate) use self::tags::validate_tag;
pub use self::tags::{tag_values, tags};

#[derive(clap::Subcommand, Clone, Debug)]
pub enum TracesCmd {
    Get(GetArgs),
    Search(SearchArgs),
    Tags(TagsArgs),
    TagValues(TagValuesArgs),
}

#[derive(clap::Args, Clone, Debug)]
pub struct GetArgs {
    pub trace_id: String,
    #[arg(long)]
    pub datasource: Option<String>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct SearchArgs {
    pub query: String,
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long, default_value_t = 20)]
    pub limit: u64,
}

#[derive(clap::Args, Clone, Debug)]
pub struct TagsArgs {
    #[command(flatten)]
    pub window: QueryWindow,
    #[arg(long, value_enum)]
    pub scope: Option<TagScope>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct TagValuesArgs {
    pub tag: String,
    #[command(flatten)]
    pub window: QueryWindow,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum TagScope {
    Resource,
    Span,
    Intrinsic,
    Event,
    Link,
    Instrumentation,
}

impl TagScope {
    /// Tempo's `scope` query parameter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resource => "resource",
            Self::Span => "span",
            Self::Intrinsic => "intrinsic",
            Self::Event => "event",
            Self::Link => "link",
            Self::Instrumentation => "instrumentation",
        }
    }
}

impl TracesCmd {
    pub async fn run(
        &self,
        client: &GrafanaClient,
        profile: &str,
        mode: OutputMode,
        bounds: Bounds,
    ) -> Result<CommandOutput> {
        match self {
            Self::Get(args) => get(client, profile, mode, args).await,
            Self::Search(args) => search(client, profile, mode, Windowed { args, bounds }).await,
            Self::Tags(args) => tags(client, Windowed { args, bounds }).await,
            Self::TagValues(args) => tag_values(client, mode, Windowed { args, bounds }).await,
        }
    }
}

/// Window bounds as Tempo's whole epoch seconds: Tempo parses start/end with
/// strconv.ParseInt, so a fraction is a 400. The start is floored and the end
/// ceiled to keep the requested window covered.
fn tempo_secs(Bounds { start, end }: Bounds) -> (String, String) {
    let end_secs = end.timestamp() + i64::from(end.timestamp_subsec_nanos() > 0);
    (start.timestamp().to_string(), end_secs.to_string())
}
