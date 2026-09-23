use clap::CommandFactory;
use clap_complete::generate;
use gcli_lib::client::GrafanaClient;
use gcli_lib::commands::profile::ProfileCmd;
use gcli_lib::commands::{self, Query};
use gcli_lib::config::Profile;
use gcli_lib::output::{OutputMode, resolve_output};
use std::io::IsTerminal;
use std::sync::Arc;

use crate::cli::{Cli, Commands, Shell};
use crate::render::{fail, print_output, print_results, write_stdout};

pub(crate) async fn run(cli: Cli) -> i32 {
    // Lazy: completions output ignores --output.
    let mode = || resolve_output(cli.output, std::io::stdout().is_terminal());
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
        Commands::Profile { cmd } => profile_command(cmd, mode()),
        Commands::Query(query) => dispatch(&cli, mode(), query).await,
    }
}

async fn dispatch(cli: &Cli, mode: OutputMode, query: &Query) -> i32 {
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
    let query = Arc::new(query.clone());
    let results = gcli_lib::fanout::run(profiles, move |name, profile| {
        let query = Arc::clone(&query);
        let overrides = overrides.clone();
        let http = http.clone();
        async move {
            let profile = overrides.apply(profile);
            let auth = gcli_lib::auth::from_profile(&name, &profile)?;
            let client = GrafanaClient::new(profile.url, auth, http);
            query.run(&client, &name, mode).await
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
    let mut config = match gcli_lib::Config::load() {
        Ok(c) => c,
        Err(e) => return fail(None, &e),
    };
    let result = match cmd {
        ProfileCmd::Add { args } => commands::profile::add(&mut config, args),
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
        Err(e) => fail(None, &e),
    }
}
