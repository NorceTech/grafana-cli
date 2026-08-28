pub mod auth;
pub mod client;
pub mod commands;
pub mod config;
pub mod datasource;
pub mod error;
pub mod fanout;
pub mod output;
pub mod timeutil;

// One process-wide lock: unit tests mutate process env vars (GCLI_*_DIR,
// GRAFANA_CLI_PROFILE) and cargo test runs them on parallel threads.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub type Result<T> = std::result::Result<T, Error>;

pub use auth::AuthProvider;
pub use client::{Datasource, GrafanaClient};
pub use config::{Config, OAuthConfig, Profile, apply_env_overrides, config_path};
pub use error::{Error, emit_json_error, exit_code};
pub use fanout::{aggregate_exit_code, resolve_profiles, run as fanout_run};
pub use output::{LogRecord, OutputMode, render_table, resolve_output, write_jsonl};
pub use timeutil::{default_step_secs, resolve_window, to_epoch_secs, to_ns_string};
