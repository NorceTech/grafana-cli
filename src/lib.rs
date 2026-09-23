pub mod auth;
pub mod client;
pub mod commands;
pub mod config;
pub mod datasource;
pub mod environment;
pub mod error;
pub mod fanout;
pub mod output;
pub mod timeutil;
pub mod token_cache;

pub type Result<T> = std::result::Result<T, Error>;

pub use auth::AuthProvider;
pub use client::{Datasource, GrafanaClient};
pub use config::{Config, OAuthConfig, Profile, config_path};
pub use environment::{Environment, ProfileOverrides};
pub use error::{Error, emit_json_error, exit_code};
pub use fanout::{aggregate_exit_code, apply_overrides, resolve_profiles, run as fanout_run};
pub use output::{LogRecord, OutputMode, render_table, resolve_output, write_jsonl};
pub use timeutil::{default_step_secs, resolve_window, to_epoch_secs, to_ns_string};
