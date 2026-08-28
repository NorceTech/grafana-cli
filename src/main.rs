mod app;
mod cli;
mod render;

use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = cli::Cli::parse();
    std::process::exit(app::run(cli).await);
}
