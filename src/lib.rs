pub mod auth;
pub mod client;
pub mod commands;
pub mod config;
pub mod datasource;
pub mod environment;
pub mod error;
pub mod fanout;
pub mod output;
mod secret_file;
pub mod timeutil;

use error::Error;

pub type Result<T> = std::result::Result<T, Error>;

// The only flat re-export the binary or the tests use (tests/config_cli.rs
// reads a config.toml back as gcli_lib::Config); everything else is reached
// through its module path.
pub use config::Config;
