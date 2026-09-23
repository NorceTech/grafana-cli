use crate::config::Profile;
use std::path::PathBuf;

/// Every environment variable gcli reads. `Environment::from_process` is the
/// only code that touches the process environment; the binary calls it once
/// and library code receives these values as arguments.
pub struct Environment {
    /// `GRAFANA_CLI_PROFILE`: the profile used when neither `--profile` nor
    /// `--all-profiles` is given.
    pub profile: Option<String>,
    /// `GRAFANA_CLI_URL`, `GRAFANA_CLI_TOKEN`, `GRAFANA_CLI_USERNAME`,
    /// `GRAFANA_CLI_PASSWORD`.
    pub overrides: ProfileOverrides,
    /// `GCLI_CONFIG_DIR`: holds `config.toml` instead of the platform config dir.
    pub config_dir: Option<PathBuf>,
    /// `GCLI_CACHE_DIR`: holds `tokens.json` instead of the platform cache dir.
    pub cache_dir: Option<PathBuf>,
}

impl Environment {
    /// Reads the variables above; one that is unset or not UTF-8 is `None`.
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self {
            profile: var("GRAFANA_CLI_PROFILE"),
            overrides: ProfileOverrides {
                url: var("GRAFANA_CLI_URL"),
                token: var("GRAFANA_CLI_TOKEN"),
                username: var("GRAFANA_CLI_USERNAME"),
                password: var("GRAFANA_CLI_PASSWORD"),
            },
            config_dir: var("GCLI_CONFIG_DIR").map(PathBuf::from),
            cache_dir: var("GCLI_CACHE_DIR").map(PathBuf::from),
        }
    }
}

/// One-shot replacements for a profile's url and credentials, from the
/// `GRAFANA_CLI_*` variables or the `--url`/`--token`/`--username`/`--password`
/// flags.
#[derive(Debug, Clone, Default)]
pub struct ProfileOverrides {
    pub url: Option<String>,
    pub token: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ProfileOverrides {
    /// Each set field replaces the profile's; unset fields keep the profile's.
    pub fn apply(self, profile: Profile) -> Profile {
        Profile {
            url: self.url.unwrap_or(profile.url),
            token: self.token.or(profile.token),
            username: self.username.or(profile.username),
            password: self.password.or(profile.password),
            ..profile
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_profile() -> Profile {
        Profile {
            url: "https://file".into(),
            token: Some("filetok".into()),
            ..Default::default()
        }
    }

    #[test]
    fn apply_replaces_only_the_set_fields() {
        let overrides = ProfileOverrides {
            url: Some("https://override".into()),
            token: Some("envtok".into()),
            ..Default::default()
        };

        let got = overrides.apply(token_profile());

        assert_eq!(
            got,
            Profile {
                url: "https://override".into(),
                token: Some("envtok".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn apply_sets_basic_credentials() {
        let basic = Profile {
            url: "https://file".into(),
            username: Some("fileuser".into()),
            password_command: Some(vec!["op".into(), "read".into()]),
            ..Default::default()
        };
        let overrides = ProfileOverrides {
            username: Some("envu".into()),
            password: Some("envp".into()),
            ..Default::default()
        };

        let got = overrides.apply(basic.clone());

        assert_eq!(
            got,
            Profile {
                username: Some("envu".into()),
                password: Some("envp".into()),
                ..basic
            }
        );
    }

    #[test]
    fn empty_overrides_keep_the_profile() {
        assert_eq!(
            ProfileOverrides::default().apply(token_profile()),
            token_profile()
        );
    }
}
