use clap::{Parser, Subcommand};
use gcli_lib::output::OutputMode;

#[derive(Parser, Debug)]
#[command(
    name = "gcli",
    version,
    about = "Query Grafana logs, metrics and traces"
)]
pub(crate) struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "NAMES",
        help = "Comma-separated profile names"
    )]
    pub(crate) profile: Option<String>,

    #[arg(long, global = true, conflicts_with = "profile")]
    pub(crate) all_profiles: bool,

    #[arg(long, global = true, value_enum)]
    pub(crate) output: Option<OutputMode>,

    #[arg(long, global = true)]
    pub(crate) url: Option<String>,

    #[arg(long, global = true)]
    pub(crate) token: Option<String>,

    #[arg(long, global = true)]
    pub(crate) username: Option<String>,

    #[arg(long, global = true)]
    pub(crate) password: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    Health,
    Datasources {
        #[arg(long, value_enum)]
        r#type: Option<DsType>,
    },
    Logs {
        #[command(subcommand)]
        cmd: LogsCmd,
    },
    Metrics {
        #[command(subcommand)]
        cmd: MetricsCmd,
    },
    Traces {
        #[command(subcommand)]
        cmd: TracesCmd,
    },
    Completions {
        shell: Shell,
    },
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum ProfileCmd {
    Add {
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long, value_delimiter = ',')]
        token_command: Option<Vec<String>>,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, value_delimiter = ',')]
        password_command: Option<Vec<String>>,
        #[arg(long)]
        token_url: Option<String>,
        #[arg(long)]
        client_id: Option<String>,
        #[arg(long)]
        client_secret: Option<String>,
        #[arg(long, value_delimiter = ',')]
        client_secret_command: Option<Vec<String>>,
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        #[arg(long)]
        audience: Option<String>,
    },
    List,
    Use {
        name: String,
    },
    Remove {
        name: String,
    },
    SetToken {
        name: String,
        #[arg(long)]
        token: String,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(crate) enum DsType {
    Loki,
    Prometheus,
    Tempo,
}

#[derive(clap::Args, Debug)]
pub(crate) struct TimeArgs {
    #[arg(long)]
    since: Option<String>,
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    datasource: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum LogsCmd {
    Search {
        query: String,
        #[command(flatten)]
        time: TimeArgs,
        #[arg(long, default_value_t = 100)]
        limit: u64,
        #[arg(long, value_enum, default_value = "backward")]
        direction: Direction,
    },
    Labels {
        #[command(flatten)]
        time: TimeArgs,
    },
    LabelValues {
        label: String,
        #[command(flatten)]
        time: TimeArgs,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(crate) enum Direction {
    Forward,
    Backward,
}

#[derive(Subcommand, Debug)]
pub(crate) enum MetricsCmd {
    Instant {
        query: String,
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        datasource: Option<String>,
    },
    Range {
        query: String,
        #[command(flatten)]
        time: TimeArgs,
        #[arg(long)]
        step: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum TracesCmd {
    Get {
        trace_id: String,
        #[arg(long)]
        datasource: Option<String>,
    },
    Search {
        query: String,
        #[command(flatten)]
        time: TimeArgs,
        #[arg(long, default_value_t = 20)]
        limit: u64,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(crate) enum Shell {
    Bash,
    Zsh,
    Fish,
}

pub(crate) fn ds_type_str(t: DsType) -> &'static str {
    match t {
        DsType::Loki => "loki",
        DsType::Prometheus => "prometheus",
        DsType::Tempo => "tempo",
    }
}

pub(crate) fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::Forward => "forward",
        Direction::Backward => "backward",
    }
}

pub(crate) fn query_window(time: &TimeArgs) -> gcli_lib::commands::QueryWindow {
    gcli_lib::commands::QueryWindow {
        since: time.since.clone(),
        from: time.from.clone(),
        to: time.to.clone(),
        datasource: time.datasource.clone(),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use gcli_lib::output::OutputMode;

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("gcli").chain(args.iter().copied())).unwrap()
    }

    fn parse_err(args: &[&str]) -> clap::Error {
        Cli::try_parse_from(std::iter::once("gcli").chain(args.iter().copied())).unwrap_err()
    }

    #[test]
    fn clap_contract_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags() {
        let c = parse(&[
            "--profile",
            "a,b",
            "--output",
            "json",
            "--url",
            "https://g.example.com",
            "--token",
            "t",
            "--username",
            "u",
            "--password",
            "p",
            "health",
        ]);
        assert_eq!(c.profile.as_deref(), Some("a,b"));
        assert_eq!(c.output, Some(OutputMode::Json));
        assert_eq!(c.url.as_deref(), Some("https://g.example.com"));
        assert_eq!(c.token.as_deref(), Some("t"));
        assert_eq!(c.username.as_deref(), Some("u"));
        assert_eq!(c.password.as_deref(), Some("p"));
        assert!(matches!(c.command, Commands::Health));
    }

    #[test]
    fn global_flags_accepted_after_subcommand() {
        let c = parse(&["health", "--output", "table", "--profile", "x"]);
        assert_eq!(c.output, Some(OutputMode::Table));
    }

    #[test]
    fn profile_conflicts_with_all_profiles() {
        parse_err(&["--profile", "a", "--all-profiles", "health"]);
        let c = parse(&["--all-profiles", "health"]);
        assert!(c.all_profiles);
    }

    #[test]
    fn profile_add_full_flag_surface() {
        let c = parse(&[
            "profile",
            "add",
            "prod",
            "--url",
            "https://g.example.com",
            "--token-url",
            "https://sso/token",
            "--client-id",
            "cid",
            "--client-secret",
            "sec",
            "--client-secret-command",
            "cat,secret-file",
            "--scope",
            "a",
            "--scope",
            "b,c",
            "--audience",
            "aud",
        ]);
        let Commands::Profile {
            cmd:
                ProfileCmd::Add {
                    name,
                    url,
                    token_url,
                    client_id,
                    client_secret,
                    client_secret_command,
                    scope,
                    audience,
                    ..
                },
        } = c.command
        else {
            panic!("expected profile add");
        };
        assert_eq!(name, "prod");
        assert_eq!(url, "https://g.example.com");
        assert_eq!(token_url.as_deref(), Some("https://sso/token"));
        assert_eq!(client_id.as_deref(), Some("cid"));
        assert_eq!(client_secret, Some("sec".to_string()));
        assert_eq!(
            client_secret_command,
            Some(vec!["cat".to_string(), "secret-file".to_string()])
        );
        assert_eq!(scope, vec!["a", "b", "c"]);
        assert_eq!(audience.as_deref(), Some("aud"));
    }

    #[test]
    fn profile_add_auth_modes() {
        parse(&["profile", "add", "t", "--url", "u", "--token", "tok"]);
        parse(&[
            "profile",
            "add",
            "tc",
            "--url",
            "u",
            "--token-command",
            "cat,f",
        ]);
        parse(&[
            "profile",
            "add",
            "b",
            "--url",
            "u",
            "--username",
            "us",
            "--password",
            "pw",
        ]);
        parse(&[
            "profile",
            "add",
            "pc",
            "--url",
            "u",
            "--username",
            "us",
            "--password-command",
            "op,read",
        ]);
    }

    #[test]
    fn profile_subcommands() {
        assert!(matches!(
            parse(&["profile", "list"]).command,
            Commands::Profile {
                cmd: ProfileCmd::List
            }
        ));
        assert!(matches!(
            parse(&["profile", "use", "p"]).command,
            Commands::Profile {
                cmd: ProfileCmd::Use { .. }
            }
        ));
        assert!(matches!(
            parse(&["profile", "remove", "p"]).command,
            Commands::Profile {
                cmd: ProfileCmd::Remove { .. }
            }
        ));
        let Commands::Profile {
            cmd: ProfileCmd::SetToken { name, token },
        } = parse(&["profile", "set-token", "p", "--token", "t"]).command
        else {
            panic!()
        };
        assert_eq!(name, "p");
        assert_eq!(token, "t");
    }

    #[test]
    fn datasources_type_filter() {
        assert!(matches!(
            parse(&["datasources"]).command,
            Commands::Datasources { r#type: None }
        ));
        let Commands::Datasources { r#type } = parse(&["datasources", "--type", "loki"]).command
        else {
            panic!()
        };
        assert!(matches!(r#type, Some(DsType::Loki)));
        parse_err(&["datasources", "--type", "influxdb"]);
    }

    #[test]
    fn logs_search_defaults_and_flags() {
        let Commands::Logs {
            cmd:
                LogsCmd::Search {
                    query,
                    time,
                    limit,
                    direction,
                    ..
                },
        } = parse(&["logs", "search", "{job=\"api\"}"]).command
        else {
            panic!()
        };
        assert_eq!(query, "{job=\"api\"}");
        assert_eq!(time.since, None);
        assert_eq!(limit, 100);
        assert!(matches!(direction, Direction::Backward));

        let Commands::Logs {
            cmd: LogsCmd::Search { time, .. },
        } = parse(&[
            "logs",
            "search",
            "q",
            "--since",
            "1h",
            "--limit",
            "5",
            "--direction",
            "forward",
            "--datasource",
            "loki-x",
        ])
        .command
        else {
            panic!()
        };
        assert_eq!(time.since.as_deref(), Some("1h"));
        assert_eq!(time.datasource.as_deref(), Some("loki-x"));
    }

    #[test]
    fn since_and_from_both_parse() {
        // mutual exclusion is enforced in timeutil::resolve_window, not clap
        assert!(
            parse(&[
                "logs",
                "search",
                "q",
                "--since",
                "1h",
                "--from",
                "2026-01-01T00:00:00Z"
            ])
            .command
            .matches_logs_search_with_since()
        );
    }

    trait LogsSearchProbe {
        fn matches_logs_search_with_since(&self) -> bool;
    }
    impl LogsSearchProbe for Commands {
        fn matches_logs_search_with_since(&self) -> bool {
            matches!(
                self,
                Commands::Logs {
                    cmd: LogsCmd::Search {
                        time: TimeArgs { since: Some(_), .. },
                        ..
                    }
                }
            )
        }
    }

    #[test]
    fn logs_labels_and_label_values() {
        assert!(matches!(
            parse(&["logs", "labels"]).command,
            Commands::Logs {
                cmd: LogsCmd::Labels { .. }
            }
        ));
        let Commands::Logs {
            cmd: LogsCmd::LabelValues { label, time },
        } = parse(&[
            "logs",
            "label-values",
            "job",
            "--from",
            "2026-01-01T00:00:00Z",
            "--to",
            "2026-01-02T00:00:00Z",
            "--datasource",
            "ds",
        ])
        .command
        else {
            panic!()
        };
        assert_eq!(label, "job");
        assert_eq!(time.from.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(time.to.as_deref(), Some("2026-01-02T00:00:00Z"));
    }

    #[test]
    fn metrics_instant_and_range() {
        let Commands::Metrics {
            cmd: MetricsCmd::Instant { query, at, .. },
        } = parse(&["metrics", "instant", "up", "--at", "2026-01-01T00:00:00Z"]).command
        else {
            panic!()
        };
        assert_eq!(query, "up");
        assert_eq!(at.as_deref(), Some("2026-01-01T00:00:00Z"));

        let Commands::Metrics {
            cmd: MetricsCmd::Range {
                query, time, step, ..
            },
        } = parse(&[
            "metrics",
            "range",
            "up",
            "--since",
            "1h",
            "--step",
            "15s",
            "--datasource",
            "prom-x",
        ])
        .command
        else {
            panic!()
        };
        assert_eq!(query, "up");
        assert_eq!(time.since.as_deref(), Some("1h"));
        assert_eq!(step.as_deref(), Some("15s"));
    }

    #[test]
    fn traces_get_and_search() {
        let Commands::Traces {
            cmd: TracesCmd::Get { trace_id, .. },
        } = parse(&["traces", "get", "abc123", "--datasource", "tempo-x"]).command
        else {
            panic!()
        };
        assert_eq!(trace_id, "abc123");

        let Commands::Traces {
            cmd: TracesCmd::Search { query, limit, .. },
        } = parse(&["traces", "search", "{ duration > 1s }", "--limit", "50"]).command
        else {
            panic!()
        };
        assert_eq!(query, "{ duration > 1s }");
        assert_eq!(limit, 50);
        let Commands::Traces {
            cmd: TracesCmd::Search { limit, .. },
        } = parse(&["traces", "search", "q"]).command
        else {
            panic!()
        };
        assert_eq!(limit, 20);
    }

    #[test]
    fn completions_generated_for_all_shells() {
        for (shell_spec, shell) in [
            ("bash", Shell::Bash),
            ("zsh", Shell::Zsh),
            ("fish", Shell::Fish),
        ] {
            let c = parse(&["completions", shell_spec]);
            assert!(matches!(
                c.command,
                Commands::Completions { shell: s } if std::mem::discriminant(&s) == std::mem::discriminant(&shell)
            ));
            let mut buf: Vec<u8> = Vec::new();
            clap_complete::generate(
                match shell {
                    Shell::Bash => clap_complete::Shell::Bash,
                    Shell::Zsh => clap_complete::Shell::Zsh,
                    Shell::Fish => clap_complete::Shell::Fish,
                },
                &mut Cli::command(),
                "gcli",
                &mut buf,
            );
            assert!(buf.len() > 100, "{shell_spec} completions too small");
        }
    }
}
