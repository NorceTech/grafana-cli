use clap::CommandFactory;
use clap_complete::generate;
use gcli_lib::auth::token_cache;
use gcli_lib::client::GrafanaClient;
use gcli_lib::commands::profile::ProfileCmd;
use gcli_lib::commands::{self, Query};
use gcli_lib::config::{Config, config_path};
use gcli_lib::environment::{Environment, ProfileOverrides};
use gcli_lib::output::{OutputMode, resolve_output};
use std::io::IsTerminal;
use std::sync::Arc;

use crate::cli::{Cli, Commands, Shell};
use crate::render::{fail, print_output, print_results, write_stdout};
use gcli_lib::error::Error;
use std::time::Duration;

pub(crate) async fn run(cli: Cli) -> i32 {
    // The one place --timeout is parsed: every request and every secret
    // helper command below gets this budget.
    let timeout = match humantime::parse_duration(&cli.timeout) {
        Ok(timeout) => timeout,
        Err(e) => {
            return fail(
                None,
                &Error::Config {
                    message: format!("--timeout '{}': {e}", cli.timeout),
                },
            );
        }
    };
    // Lazy: completions output ignores --output.
    let mode = || resolve_output(cli.output, std::io::stdout().is_terminal());
    // The only read of the process environment: everything below gets values.
    let env = Environment::from_process();
    match &cli.command {
        Commands::Completions { shell } => {
            let shell = match shell {
                Shell::Bash => clap_complete::Shell::Bash,
                Shell::Zsh => clap_complete::Shell::Zsh,
                Shell::Fish => clap_complete::Shell::Fish,
            };
            let mut cmd = Cli::command();
            let mut buf: Vec<u8> = Vec::new();
            generate(shell, &mut cmd, "gcli", &mut buf);
            write_stdout(&buf);
            0
        }
        Commands::Profile { cmd } => profile_command(cmd, mode(), &env),
        Commands::Query(query) => dispatch(&cli, mode(), query, env, timeout).await,
    }
}

async fn dispatch(
    cli: &Cli,
    mode: OutputMode,
    query: &Query,
    env: Environment,
    timeout: Duration,
) -> i32 {
    let flags = ProfileOverrides {
        url: cli.url.clone(),
        token: cli.token.clone(),
        username: cli.username.clone(),
        password: cli.password.clone(),
    };
    let selected = config_path(env.config_dir.as_deref())
        .and_then(|path| Config::load_from(&path))
        .and_then(|config| {
            gcli_lib::fanout::resolve_profiles(
                cli.profile.as_deref(),
                cli.all_profiles,
                &config,
                env.profile.as_deref(),
            )
        })
        .and_then(|profiles| gcli_lib::fanout::apply_overrides(profiles, env.overrides, flags));
    let profiles = match selected {
        Ok(p) => p,
        Err(e) => return fail(cli.profile.as_deref(), &e),
    };
    // One time window for every profile: a bad one is a single error.
    let query = match query.resolve() {
        Ok(resolved) => Arc::new(resolved),
        Err(e) => {
            let label = match profiles.as_slice() {
                [(name, _)] => Some(name.as_str()),
                _ => cli.profile.as_deref(),
            };
            return fail(label, &e);
        }
    };
    let http = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(timeout)
        .build()
    {
        Ok(http) => http,
        Err(e) => return fail(cli.profile.as_deref(), &Error::Io(std::io::Error::other(e))),
    };
    let cache_dir = env.cache_dir;
    let results = gcli_lib::fanout::run(profiles, move |name, profile| {
        let query = Arc::clone(&query);
        let http = http.clone();
        let cache_dir = cache_dir.clone();
        async move {
            // The url is checked before any auth runs, so a secret-fetching
            // command never executes for a profile that cannot be queried.
            let base = gcli_lib::client::BaseUrl::parse(&profile.url)?;
            let auth = gcli_lib::auth::from_profile(&name, &profile, cache_dir.as_deref(), timeout)
                .await?;
            let client = GrafanaClient::new(base.as_str(), auth, http)?;
            query.run(&client, &name, mode).await
        }
    })
    .await;
    print_results(&results, mode)
}

fn profile_command(cmd: &ProfileCmd, mode: OutputMode, env: &Environment) -> i32 {
    let loaded = config_path(env.config_dir.as_deref())
        .and_then(|path| Config::load_from(&path).map(|config| (config, path)));
    let (mut config, path) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => return fail(None, &e),
    };
    let result = match cmd {
        ProfileCmd::Add { args } => commands::profile::add(&mut config, &path, args),
        ProfileCmd::List => commands::profile::list(&config, mode),
        ProfileCmd::Use { name } => commands::profile::use_profile(&mut config, &path, name),
        ProfileCmd::Remove { name } => {
            token_cache::path(env.cache_dir.as_deref()).and_then(|cache_path| {
                commands::profile::remove(&mut config, &path, &cache_path, name)
            })
        }
        ProfileCmd::SetToken { name, token } => {
            commands::profile::set_token(&mut config, &path, name, token)
        }
    };
    match result {
        Ok(out) => {
            print_output(&out);
            0
        }
        Err(e) => fail(None, &e),
    }
}
