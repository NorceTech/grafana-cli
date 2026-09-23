use crate::Result;
use crate::commands::CommandOutput;
use crate::config::{Config, OAuthConfig, Profile};
use crate::error::Error;
use crate::output::OutputMode;
use crate::token_cache;
use serde_json::{Value, json};
use std::path::Path;

#[derive(clap::Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ProfileCmd {
    Add {
        #[command(flatten)]
        args: AddArgs,
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

#[derive(clap::Args, Debug)]
pub struct AddArgs {
    pub name: String,
    #[arg(long)]
    pub url: String,
    #[arg(long)]
    pub token: Option<String>,
    #[arg(long, value_delimiter = ',')]
    pub token_command: Option<Vec<String>>,
    #[arg(long)]
    pub username: Option<String>,
    #[arg(long)]
    pub password: Option<String>,
    #[arg(long, value_delimiter = ',')]
    pub password_command: Option<Vec<String>>,
    #[arg(long)]
    pub token_url: Option<String>,
    #[arg(long)]
    pub client_id: Option<String>,
    #[arg(long)]
    pub client_secret: Option<String>,
    #[arg(long, value_delimiter = ',')]
    pub client_secret_command: Option<Vec<String>>,
    #[arg(long, value_delimiter = ',')]
    pub scope: Vec<String>,
    #[arg(long)]
    pub audience: Option<String>,
}

pub fn add(config: &mut Config, config_path: &Path, args: &AddArgs) -> Result<CommandOutput> {
    let profile = Profile {
        url: args.url.clone(),
        token: args.token.clone(),
        token_command: args.token_command.clone(),
        username: args.username.clone(),
        password: args.password.clone(),
        password_command: args.password_command.clone(),
        oauth: build_oauth(args)?,
    };
    let auth = auth_mode(&profile);
    config.add_profile(&args.name, profile)?;
    config.save_to(config_path)?;
    Ok(CommandOutput::Json(json!({
        "action": "add",
        "profile": args.name,
        "auth": auth,
    })))
}

pub fn list(config: &Config, mode: OutputMode) -> Result<CommandOutput> {
    let is_current = |name: &str| config.current_profile.as_deref() == Some(name);
    if mode == OutputMode::Table {
        let rows = config
            .profiles
            .iter()
            .map(|(name, p)| {
                vec![
                    marker(is_current(name)),
                    name.clone(),
                    p.url.clone(),
                    auth_mode(p).into(),
                ]
            })
            .collect();
        Ok(CommandOutput::Table {
            headers: ["current", "name", "url", "auth"]
                .map(String::from)
                .to_vec(),
            rows,
        })
    } else {
        let records = config
            .profiles
            .iter()
            .map(|(name, p)| {
                json!({
                    "current": is_current(name),
                    "name": name,
                    "url": p.url,
                    "auth": auth_mode(p),
                })
            })
            .collect();
        Ok(CommandOutput::Json(Value::Array(records)))
    }
}

pub fn use_profile(config: &mut Config, config_path: &Path, name: &str) -> Result<CommandOutput> {
    config.use_profile(name)?;
    config.save_to(config_path)?;
    Ok(CommandOutput::Json(
        json!({"action": "use", "profile": name}),
    ))
}

/// Also forgets the profile's cached OAuth tokens (before saving, so a failed
/// save leaves a profile that merely refetches its token).
pub fn remove(
    config: &mut Config,
    config_path: &Path,
    cache_path: &Path,
    name: &str,
) -> Result<CommandOutput> {
    config.remove_profile(name)?;
    token_cache::forget_profile(cache_path, name)?;
    config.save_to(config_path)?;
    Ok(CommandOutput::Json(
        json!({"action": "remove", "profile": name}),
    ))
}

pub fn set_token(
    config: &mut Config,
    config_path: &Path,
    name: &str,
    token: &str,
) -> Result<CommandOutput> {
    config.set_token(name, Some(token))?;
    config.save_to(config_path)?;
    Ok(CommandOutput::Json(
        json!({"action": "set-token", "profile": name}),
    ))
}

fn marker(current: bool) -> String {
    if current { "*".into() } else { String::new() }
}

fn auth_mode(profile: &Profile) -> &'static str {
    if profile.oauth.is_some() {
        "oauth"
    } else if profile.username.is_some()
        || profile.password.is_some()
        || profile.password_command.is_some()
    {
        "basic"
    } else {
        "token"
    }
}

fn build_oauth(args: &AddArgs) -> Result<Option<OAuthConfig>> {
    match (&args.token_url, &args.client_id) {
        (Some(token_url), Some(client_id)) => Ok(Some(OAuthConfig {
            token_url: token_url.clone(),
            client_id: client_id.clone(),
            client_secret: args.client_secret.clone(),
            client_secret_command: args.client_secret_command.clone(),
            scopes: (!args.scope.is_empty()).then(|| args.scope.clone()),
            audience: args.audience.clone(),
        })),
        (None, None) => Ok(None),
        _ => Err(Error::Config {
            message: "--token-url and --client-id must be used together".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_args(token_url: Option<&str>, client_id: Option<&str>) -> AddArgs {
        AddArgs {
            name: "n".into(),
            url: "http://x".into(),
            token: None,
            token_command: None,
            username: None,
            password: None,
            password_command: None,
            token_url: token_url.map(String::from),
            client_id: client_id.map(String::from),
            client_secret: None,
            client_secret_command: None,
            scope: vec![],
            audience: None,
        }
    }

    #[test]
    fn oauth_requires_token_url_and_client_id_together() {
        assert!(build_oauth(&add_args(Some("https://idp/token"), None)).is_err());
        assert!(build_oauth(&add_args(None, Some("cid"))).is_err());
        let oauth = build_oauth(&add_args(Some("https://idp/token"), Some("cid")))
            .unwrap()
            .unwrap();
        assert_eq!(oauth.token_url, "https://idp/token");
        assert_eq!(oauth.client_id, "cid");
        assert!(build_oauth(&add_args(None, None)).unwrap().is_none());
    }

    #[test]
    fn oauth_scopes_only_when_given() {
        let mut args = add_args(Some("https://idp/token"), Some("cid"));
        args.scope = vec!["a".into(), "b".into()];
        let oauth = build_oauth(&args).unwrap().unwrap();
        assert_eq!(
            oauth.scopes.as_deref(),
            Some(["a".to_string(), "b".to_string()].as_slice())
        );
    }

    #[test]
    fn auth_mode_derivation() {
        let token = Profile {
            url: "u".into(),
            token: Some("t".into()),
            ..Default::default()
        };
        let basic = Profile {
            url: "u".into(),
            username: Some("u".into()),
            password: Some("p".into()),
            ..Default::default()
        };
        let oauth = Profile {
            oauth: Some(OAuthConfig {
                token_url: "https://idp/token".into(),
                client_id: "cid".into(),
                client_secret: None,
                client_secret_command: None,
                scopes: None,
                audience: None,
            }),
            ..Default::default()
        };
        assert_eq!(auth_mode(&token), "token");
        assert_eq!(auth_mode(&basic), "basic");
        assert_eq!(auth_mode(&oauth), "oauth");
    }

    #[test]
    fn add_and_lifecycle_ops_mutate_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = Config::default();
        let out = add(
            &mut config,
            &path,
            &AddArgs {
                token: Some("t".into()),
                ..add_args(None, None)
            },
        )
        .unwrap();
        match out {
            CommandOutput::Json(v) => assert_eq!(v["action"], "add"),
            _ => panic!("expected json"),
        }
        assert!(config.profiles.contains_key("n"));

        use_profile(&mut config, &path, "n").unwrap();
        assert_eq!(config.current_profile.as_deref(), Some("n"));
        set_token(&mut config, &path, "n", "t2").unwrap();
        assert_eq!(config.profiles["n"].token.as_deref(), Some("t2"));
        remove(&mut config, &path, &dir.path().join("tokens.json"), "n").unwrap();
        assert!(config.profiles.is_empty());
    }
}
