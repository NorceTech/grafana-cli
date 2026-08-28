use clap::CommandFactory;
use clap_complete::generate;
use gcli_lib::client::GrafanaClient;
use gcli_lib::commands::{self, CommandOutput};
use gcli_lib::config::Profile;
use gcli_lib::output::{OutputMode, resolve_output};
use std::io::IsTerminal;

use crate::cli::{
    Cli, Commands, LogsCmd, MetricsCmd, ProfileCmd, Shell, TracesCmd, direction_str, ds_type_str,
    query_window,
};
use crate::render::{fail, print_output, print_results, write_stdout};

pub(crate) async fn run(cli: Cli) -> i32 {
    if let Commands::Completions { shell } = cli.command {
        let shell = match shell {
            Shell::Bash => clap_complete::Shell::Bash,
            Shell::Zsh => clap_complete::Shell::Zsh,
            Shell::Fish => clap_complete::Shell::Fish,
        };
        let mut cmd = Cli::command();
        let mut buf: Vec<u8> = Vec::new();
        generate(shell, &mut cmd, "gcli", &mut buf);
        write_stdout(&buf);
        return 0;
    }
    let mode = resolve_output(cli.output, std::io::stdout().is_terminal());
    match &cli.command {
        Commands::Completions { .. } => 0,
        Commands::Profile { cmd } => profile_command(cmd, mode),
        Commands::Health => {
            dispatch(&cli, mode, move |client, name, mode| async move {
                commands::health::run(&client, &name, mode).await
            })
            .await
        }
        Commands::Datasources { r#type } => {
            let ds_type = r#type.map(ds_type_str);
            dispatch(&cli, mode, move |client, name, mode| async move {
                commands::datasources::run(&client, &name, mode, ds_type).await
            })
            .await
        }
        Commands::Logs { cmd } => match cmd {
            LogsCmd::Search {
                query,
                time,
                limit,
                direction,
            } => {
                let window = query_window(time);
                let query = query.clone();
                let limit = *limit;
                let direction = direction_str(*direction);
                dispatch(&cli, mode, move |client, name, mode| {
                    let query = query.clone();
                    let window = window.clone();
                    async move {
                        commands::logs::search(
                            &client, &name, mode, &query, &window, limit, direction,
                        )
                        .await
                    }
                })
                .await
            }
            LogsCmd::Labels { time } => {
                let window = query_window(time);
                dispatch(&cli, mode, move |client, name, _mode| {
                    let window = window.clone();
                    async move { commands::logs::labels(&client, &name, &window).await }
                })
                .await
            }
            LogsCmd::LabelValues { label, time } => {
                let window = query_window(time);
                let label = label.clone();
                dispatch(&cli, mode, move |client, name, _mode| {
                    let label = label.clone();
                    let window = window.clone();
                    async move {
                        commands::logs::label_values(&client, &name, &label, &window).await
                    }
                })
                .await
            }
        },
        Commands::Metrics { cmd } => match cmd {
            MetricsCmd::Instant {
                query,
                at,
                datasource,
            } => {
                let query = query.clone();
                let at = at.clone();
                let datasource = datasource.clone();
                dispatch(&cli, mode, move |client, name, mode| {
                    let query = query.clone();
                    let at = at.clone();
                    let datasource = datasource.clone();
                    async move {
                        commands::metrics::instant(
                            &client,
                            &name,
                            mode,
                            &query,
                            at.as_deref(),
                            datasource.as_deref(),
                        )
                        .await
                    }
                })
                .await
            }
            MetricsCmd::Range { query, time, step } => {
                let window = query_window(time);
                let query = query.clone();
                let step = step.clone();
                dispatch(&cli, mode, move |client, name, mode| {
                    let query = query.clone();
                    let window = window.clone();
                    let step = step.clone();
                    async move {
                        commands::metrics::range(
                            &client,
                            &name,
                            mode,
                            &query,
                            &window,
                            step.as_deref(),
                        )
                        .await
                    }
                })
                .await
            }
        },
        Commands::Traces { cmd } => match cmd {
            TracesCmd::Get {
                trace_id,
                datasource,
            } => {
                let trace_id = trace_id.clone();
                let datasource = datasource.clone();
                dispatch(&cli, mode, move |client, name, mode| {
                    let trace_id = trace_id.clone();
                    let datasource = datasource.clone();
                    async move {
                        commands::traces::get(
                            &client,
                            &name,
                            mode,
                            &trace_id,
                            datasource.as_deref(),
                        )
                        .await
                    }
                })
                .await
            }
            TracesCmd::Search { query, time, limit } => {
                let window = query_window(time);
                let query = query.clone();
                let limit = *limit;
                dispatch(&cli, mode, move |client, name, mode| {
                    let query = query.clone();
                    let window = window.clone();
                    async move {
                        commands::traces::search(&client, &name, mode, &query, &window, limit).await
                    }
                })
                .await
            }
        },
    }
}

async fn dispatch<F, Fut>(cli: &Cli, mode: OutputMode, run_one: F) -> i32
where
    F: Fn(GrafanaClient, String, OutputMode) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = gcli_lib::Result<CommandOutput>> + Send + 'static,
{
    let config = match gcli_lib::Config::load() {
        Ok(c) => c,
        Err(e) => return fail(cli.profile.as_deref(), &e),
    };
    let profiles =
        match gcli_lib::fanout::resolve_profiles(cli.profile.as_deref(), cli.all_profiles, &config)
        {
            Ok(p) => p,
            Err(e) => return fail(cli.profile.as_deref(), &e),
        };
    let overrides = Overrides::from_cli(cli);
    let http = reqwest::Client::new();
    let results = gcli_lib::fanout::run(profiles, move |name, profile| {
        let run_one = run_one.clone();
        let overrides = overrides.clone();
        let http = http.clone();
        async move {
            let profile = overrides.apply(profile);
            let auth = gcli_lib::auth::from_profile(&name, &profile)?;
            let client = GrafanaClient::new(profile.url, auth, http);
            run_one(client, name, mode).await
        }
    })
    .await;
    print_results(&results, mode)
}

#[derive(Clone, Default)]
struct Overrides {
    url: Option<String>,
    token: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

impl Overrides {
    fn from_cli(cli: &Cli) -> Self {
        Self {
            url: cli.url.clone(),
            token: cli.token.clone(),
            username: cli.username.clone(),
            password: cli.password.clone(),
        }
    }

    fn apply(&self, mut profile: Profile) -> Profile {
        if let Some(url) = &self.url {
            profile.url = url.clone();
        }
        if self.token.is_some() {
            profile.token = self.token.clone();
        }
        if let Some(username) = &self.username {
            profile.username = Some(username.clone());
        }
        if let Some(password) = &self.password {
            profile.password = Some(password.clone());
        }
        profile
    }
}

fn profile_command(cmd: &ProfileCmd, mode: OutputMode) -> i32 {
    let flag_profile: Option<&str> = None;
    let mut config = match gcli_lib::Config::load() {
        Ok(c) => c,
        Err(e) => return fail(flag_profile, &e),
    };
    let result = match cmd {
        ProfileCmd::Add {
            name,
            url,
            token,
            token_command,
            username,
            password,
            password_command,
            token_url,
            client_id,
            client_secret,
            client_secret_command,
            scope,
            audience,
        } => commands::profile::add(
            &mut config,
            &commands::profile::AddArgs {
                name: name.clone(),
                url: url.clone(),
                token: token.clone(),
                token_command: token_command.clone(),
                username: username.clone(),
                password: password.clone(),
                password_command: password_command.clone(),
                token_url: token_url.clone(),
                client_id: client_id.clone(),
                client_secret: client_secret.clone(),
                client_secret_command: client_secret_command.clone(),
                scope: scope.clone(),
                audience: audience.clone(),
            },
        ),
        ProfileCmd::List => commands::profile::list(&config, mode),
        ProfileCmd::Use { name } => commands::profile::use_profile(&mut config, name),
        ProfileCmd::Remove { name } => commands::profile::remove(&mut config, name),
        ProfileCmd::SetToken { name, token } => {
            commands::profile::set_token(&mut config, name, token)
        }
    };
    match result {
        Ok(out) => {
            print_output(&out);
            0
        }
        Err(e) => fail(flag_profile, &e),
    }
}
