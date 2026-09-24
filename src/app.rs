use clap::CommandFactory;
use clap_complete::generate;
use gcli_lib::auth::token_cache;
use gcli_lib::client::GrafanaClient;
use gcli_lib::commands::logs::{self, LogsCmd, SearchArgs};
use gcli_lib::commands::profile::ProfileCmd;
use gcli_lib::commands::{self, Query};
use gcli_lib::config::{Config, Profile, config_path};
use gcli_lib::environment::{Environment, ProfileOverrides};
use gcli_lib::output::{OutputMode, resolve_output};
use std::io::IsTerminal;
use std::sync::Arc;

use crate::cli::{Cli, Commands, Shell};
use crate::render::{fail, print_follow_batch, print_output, print_results, write_stdout};
use gcli_lib::error::Error;
use std::time::Duration;

pub(crate) async fn run(mut cli: Cli) -> i32 {
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
    // Secret flags given '-' read the secret from stdin here, once, before
    // any command runs; everything downstream sees the real value.
    if let Err(e) = resolve_stdin_secrets(&mut cli) {
        return fail(None, &e);
    }
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

/// Replaces every secret flag given as `-` with the secret read from stdin,
/// so a secret can be piped in instead of appearing in `ps` output or shell
/// history. Never prompts: stdin is read to EOF, whatever it holds.
/// `profile add` and `profile set-token` own their secret flags — clap
/// mirrors them onto the global fields, so they are resolved instead of (not
/// in addition to) the globals, and stdin is read once.
fn resolve_stdin_secrets(cli: &mut Cli) -> gcli_lib::Result<()> {
    let mut read_globals = false;
    match &mut cli.command {
        Commands::Profile { cmd } => match cmd {
            ProfileCmd::Add { args } => {
                read_secret(&mut args.token, "profile add --token")?;
                read_secret(&mut args.password, "profile add --password")?;
                read_secret(&mut args.client_secret, "profile add --client-secret")?;
            }
            ProfileCmd::SetToken { token, .. } => {
                if token == "-" {
                    *token = gcli_lib::auth::secret_from_stdin(
                        &mut std::io::stdin(),
                        "profile set-token --token",
                    )?;
                }
            }
            ProfileCmd::List | ProfileCmd::Use { .. } | ProfileCmd::Remove { .. } => {
                read_globals = true;
            }
        },
        Commands::Query(_) | Commands::Completions { .. } => read_globals = true,
    }
    if read_globals {
        read_secret(&mut cli.token, "--token")?;
        read_secret(&mut cli.password, "--password")?;
    }
    Ok(())
}

fn read_secret(value: &mut Option<String>, flag: &str) -> gcli_lib::Result<()> {
    if value.as_deref() == Some("-") {
        *value = Some(gcli_lib::auth::secret_from_stdin(
            &mut std::io::stdin(),
            flag,
        )?);
    }
    Ok(())
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
    let follow_args = match query {
        Query::Logs {
            cmd: LogsCmd::Search(args),
        } if args.follow => Some(args),
        _ => None,
    };
    // The follow path below needs the cache dir after `env.overrides` has
    // moved into the profile resolution, so it is taken out up front.
    let cache_dir = env.cache_dir.clone();
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
    // --follow streams from exactly one profile and never returns, so it
    // bypasses the fanout-and-render pipeline entirely.
    if let Some(args) = follow_args {
        return follow(cli, &profiles, args, cache_dir, timeout).await;
    }
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

/// `logs search --follow`: prints the newest `--limit` entries of the window
/// ascending (exactly `search`'s output), then polls every `--interval` and
/// prints each new batch as it arrives, always as JSONL. SIGINT/SIGTERM and a
/// closed stdout end the run with exit 0; a failed request ends it with that
/// error's JSON and exit code.
///
/// Five parameters is more than the usual three, but they are the dispatch
/// context (flags, selected profiles, environment, timeout) every command
/// path threads through plus the follow arguments; grouping them would be a
/// one-off struct with a single caller.
async fn follow(
    cli: &Cli,
    profiles: &[(String, Profile)],
    args: &SearchArgs,
    cache_dir: Option<std::path::PathBuf>,
    timeout: Duration,
) -> i32 {
    if cli.all_profiles {
        return fail(None, &follow_selection_error("--all-profiles"));
    }
    let [(name, profile)] = profiles else {
        return fail(None, &follow_selection_error("--profile"));
    };
    let interval = match humantime::parse_duration(&args.interval) {
        Ok(interval) => interval,
        Err(e) => {
            return fail(
                Some(name),
                &Error::Config {
                    message: format!("--interval '{}': {e}", args.interval),
                },
            );
        }
    };
    // The url is checked before any auth runs, so a secret-fetching command
    // never executes for a profile that cannot be queried.
    let base = match gcli_lib::client::BaseUrl::parse(&profile.url) {
        Ok(base) => base,
        Err(e) => return fail(Some(name), &e),
    };
    let auth =
        match gcli_lib::auth::from_profile(name, profile, cache_dir.as_deref(), timeout).await {
            Ok(auth) => auth,
            Err(e) => return fail(Some(name), &e),
        };
    let http = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(timeout)
        .build()
    {
        Ok(http) => http,
        Err(e) => return fail(Some(name), &Error::Io(std::io::Error::other(e))),
    };
    let client = match GrafanaClient::new(base.as_str(), auth, http) {
        Ok(client) => client,
        Err(e) => return fail(Some(name), &e),
    };
    let (initial, mut stream) = match logs::follow_start(&client, name, args).await {
        Ok(started) => started,
        Err(e) => return fail(Some(name), &e),
    };
    print_follow_batch(&initial);
    let mut shutdown = match Shutdown::new() {
        Ok(shutdown) => shutdown,
        Err(e) => return fail(Some(name), &Error::Io(e)),
    };
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                match stream.poll(&client).await {
                    Ok(batch) => print_follow_batch(&batch),
                    Err(e) => return fail(Some(name), &e),
                }
            }
            _ = shutdown.recv() => return 0,
        }
    }
}

fn follow_selection_error(selection: &str) -> Error {
    Error::Config {
        message: format!(
            "logs search --follow streams exactly one profile, but several are selected \
             with {selection}; drop --follow or select a single profile"
        ),
    }
}

// SIGINT and SIGTERM end a --follow run cleanly. Registered once, before the
// loop, so a signal landing between two polls is still received.
struct Shutdown {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl Shutdown {
    fn new() -> std::io::Result<Self> {
        #[cfg(unix)]
        return Ok(Self {
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        });
        #[cfg(not(unix))]
        Ok(Self {})
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = self.terminate.recv() => {}
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    }
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
