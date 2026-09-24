use clap::{Parser, Subcommand};
use gcli_lib::commands::Query;
use gcli_lib::commands::profile::ProfileCmd;
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

    #[arg(
        long,
        global = true,
        value_name = "DURATION",
        default_value = "60s",
        help = "Request timeout, e.g. 30s, 2m or 500ms"
    )]
    pub(crate) timeout: String,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    #[command(flatten)]
    Query(Query),
    Completions {
        shell: Shell,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(crate) enum Shell {
    Bash,
    Zsh,
    Fish,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use gcli_lib::commands::QueryWindow;
    use gcli_lib::commands::datasources::DatasourcesArgs;
    use gcli_lib::commands::logs::{self, Direction, LogsCmd};
    use gcli_lib::commands::metrics::{InstantArgs, MetricsCmd, RangeArgs};
    use gcli_lib::commands::profile::AddArgs;
    use gcli_lib::commands::traces::{self, GetArgs, TracesCmd};
    use gcli_lib::datasource::Kind;
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
        assert!(matches!(c.command, Commands::Query(Query::Health)));
    }

    #[test]
    fn global_flags_accepted_after_subcommand() {
        let c = parse(&["health", "--output", "table", "--profile", "x"]);
        assert_eq!(c.output, Some(OutputMode::Table));
    }

    #[test]
    fn global_timeout_flag() {
        let c = parse(&["--timeout", "30s", "health"]);
        assert_eq!(c.timeout, "30s");
        let c = parse(&["health", "--timeout", "2m"]);
        assert_eq!(c.timeout, "2m");
        let c = parse(&["health"]);
        assert_eq!(c.timeout, "60s");
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
                    args:
                        AddArgs {
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
            Commands::Query(Query::Datasources(DatasourcesArgs { r#type: None }))
        ));
        let Commands::Query(Query::Datasources(DatasourcesArgs { r#type })) =
            parse(&["datasources", "--type", "loki"]).command
        else {
            panic!()
        };
        assert!(matches!(r#type, Some(Kind::Loki)));
        parse_err(&["datasources", "--type", "influxdb"]);
    }

    #[test]
    fn logs_search_defaults_and_flags() {
        let Commands::Query(Query::Logs {
            cmd:
                LogsCmd::Search(logs::SearchArgs {
                    query,
                    window,
                    limit,
                    direction,
                    ..
                }),
        }) = parse(&["logs", "search", "{job=\"api\"}"]).command
        else {
            panic!()
        };
        assert_eq!(query, "{job=\"api\"}");
        assert_eq!(window.since, None);
        assert_eq!(limit, 100);
        assert!(matches!(direction, Direction::Backward));

        let Commands::Query(Query::Logs {
            cmd: LogsCmd::Search(logs::SearchArgs { window, .. }),
        }) = parse(&[
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
        assert_eq!(window.since.as_deref(), Some("1h"));
        assert_eq!(window.datasource.as_deref(), Some("loki-x"));
    }

    #[test]
    fn since_and_from_both_parse() {
        // mutual exclusion is enforced in timeutil::resolve_window, not clap
        assert!(matches!(
            parse(&[
                "logs",
                "search",
                "q",
                "--since",
                "1h",
                "--from",
                "2026-01-01T00:00:00Z"
            ])
            .command,
            Commands::Query(Query::Logs {
                cmd: LogsCmd::Search(logs::SearchArgs {
                    window: QueryWindow { since: Some(_), .. },
                    ..
                })
            })
        ));
    }

    #[test]
    fn logs_labels_and_label_values() {
        assert!(matches!(
            parse(&["logs", "labels"]).command,
            Commands::Query(Query::Logs {
                cmd: LogsCmd::Labels { .. }
            })
        ));
        let Commands::Query(Query::Logs {
            cmd: LogsCmd::LabelValues { label, window },
        }) = parse(&[
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
        assert_eq!(window.from.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(window.to.as_deref(), Some("2026-01-02T00:00:00Z"));
    }

    #[test]
    fn metrics_instant_and_range() {
        let Commands::Query(Query::Metrics {
            cmd: MetricsCmd::Instant(InstantArgs { query, at, .. }),
        }) = parse(&["metrics", "instant", "up", "--at", "2026-01-01T00:00:00Z"]).command
        else {
            panic!()
        };
        assert_eq!(query, "up");
        assert_eq!(at.as_deref(), Some("2026-01-01T00:00:00Z"));

        let Commands::Query(Query::Metrics {
            cmd:
                MetricsCmd::Range(RangeArgs {
                    query,
                    window,
                    step,
                    ..
                }),
        }) = parse(&[
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
        assert_eq!(window.since.as_deref(), Some("1h"));
        assert_eq!(step.as_deref(), Some("15s"));
    }

    #[test]
    fn traces_get_and_search() {
        let Commands::Query(Query::Traces {
            cmd: TracesCmd::Get(GetArgs { trace_id, .. }),
        }) = parse(&["traces", "get", "abc123", "--datasource", "tempo-x"]).command
        else {
            panic!()
        };
        assert_eq!(trace_id, "abc123");

        let Commands::Query(Query::Traces {
            cmd: TracesCmd::Search(traces::SearchArgs { query, limit, .. }),
        }) = parse(&["traces", "search", "{ duration > 1s }", "--limit", "50"]).command
        else {
            panic!()
        };
        assert_eq!(query, "{ duration > 1s }");
        assert_eq!(limit, 50);
        let Commands::Query(Query::Traces {
            cmd: TracesCmd::Search(traces::SearchArgs { limit, .. }),
        }) = parse(&["traces", "search", "q"]).command
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
