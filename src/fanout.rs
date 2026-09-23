use crate::config::{Config, Profile};
use crate::environment::ProfileOverrides;
use crate::{Error, Result};

/// The profiles to run, as stored: `--profile a,b` (order kept), else
/// `--all-profiles` (sorted), else the active profile (`env_profile`, then
/// `current_profile`). Overrides are applied afterwards by `apply_overrides`.
pub fn resolve_profiles(
    profiles_flag: Option<&str>,
    all: bool,
    config: &Config,
    env_profile: Option<&str>,
) -> Result<Vec<(String, Profile)>> {
    if let Some(csv) = profiles_flag {
        let names: Vec<&str> = csv
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        if names.is_empty() {
            return Err(Error::Config {
                message: format!("--profile is empty; available: {}", config.available()),
            });
        }
        let mut out = Vec::new();
        for name in names {
            let profile = config.profiles.get(name).ok_or_else(|| Error::Config {
                message: format!(
                    "profile '{name}' not found; available: {}",
                    config.available()
                ),
            })?;
            out.push((name.to_string(), profile.clone()));
        }
        return Ok(out);
    }
    if all {
        if config.profiles.is_empty() {
            return Err(Error::Config {
                message: "no profiles configured".into(),
            });
        }
        return Ok(config
            .profiles
            .iter()
            .map(|(name, profile)| (name.clone(), profile.clone()))
            .collect());
    }
    Ok(vec![config.active_profile(env_profile)?])
}

/// Applies the `GRAFANA_CLI_*` overrides, then the flag overrides on top, to
/// the selected profile: config file < environment < flags. Overrides name one
/// instance and one identity, so with several profiles selected any set
/// override is a config error naming it, instead of one host receiving every
/// profile's credentials or one token going to every instance.
pub fn apply_overrides(
    mut profiles: Vec<(String, Profile)>,
    env: ProfileOverrides,
    flags: ProfileOverrides,
) -> Result<Vec<(String, Profile)>> {
    if let [(_, profile)] = profiles.as_mut_slice() {
        *profile = flags.apply(env.apply(std::mem::take(profile)));
        return Ok(profiles);
    }
    let set: Vec<String> = flags
        .set_fields()
        .into_iter()
        .map(|field| format!("--{field}"))
        .chain(
            env.set_fields()
                .into_iter()
                .map(|field| format!("GRAFANA_CLI_{}", field.to_ascii_uppercase())),
        )
        .collect();
    if set.is_empty() {
        return Ok(profiles);
    }
    let names: Vec<&str> = profiles.iter().map(|(name, _)| name.as_str()).collect();
    Err(Error::Config {
        message: format!(
            "url and credential overrides apply to exactly one profile, but {} are \
             selected ({}); drop {} or select a single profile",
            names.len(),
            names.join(", "),
            set.join(", ")
        ),
    })
}

pub async fn run<T, F, Fut>(profiles: Vec<(String, Profile)>, f: F) -> Vec<(String, Result<T>)>
where
    T: Send + 'static,
    F: Fn(String, Profile) -> Fut,
    Fut: std::future::Future<Output = Result<T>> + Send + 'static,
{
    let total = profiles.len();
    let mut slots: Vec<Option<(String, Result<T>)>> = (0..total).map(|_| None).collect();
    let mut set = tokio::task::JoinSet::new();
    for (index, (name, profile)) in profiles.into_iter().enumerate() {
        let fut = f(name.clone(), profile);
        set.spawn(async move { (index, name, fut.await) });
    }
    let mut panics: Vec<String> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((index, name, result)) => slots[index] = Some((name, result)),
            Err(e) => panics.push(e.to_string()),
        }
    }
    slots
        .into_iter()
        .map(|slot| {
            slot.unwrap_or_else(|| {
                let note = panics.first().cloned().unwrap_or_else(|| "unknown".into());
                (
                    String::new(),
                    Err(Error::Io(std::io::Error::other(format!(
                        "profile task failed: {note}"
                    )))),
                )
            })
        })
        .collect()
}

pub fn aggregate_exit_code<T>(results: &[(&String, &Result<T>)]) -> i32 {
    if results.is_empty() {
        return 0;
    }
    if results.iter().all(|(_, r)| r.is_err()) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(url: &str) -> Profile {
        Profile {
            url: url.into(),
            token: Some("t".into()),
            ..Default::default()
        }
    }

    fn sample() -> Config {
        let mut c = Config::default();
        c.add_profile("b", profile("https://b")).unwrap();
        c.add_profile("a", profile("https://a")).unwrap();
        c.add_profile("c", profile("https://c")).unwrap();
        c.current_profile = Some("b".into());
        c
    }

    #[test]
    fn csv_flag_wins_and_keeps_order() {
        let c = sample();
        let got = resolve_profiles(Some("c,a"), false, &c, None).unwrap();
        assert_eq!(
            got.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["c", "a"]
        );
    }

    #[test]
    fn csv_flag_beats_env_profile() {
        let got = resolve_profiles(Some("a"), false, &sample(), Some("c")).unwrap();
        assert_eq!(got[0].0, "a");
    }

    #[test]
    fn csv_unknown_name_lists_available() {
        let c = sample();
        let err = resolve_profiles(Some("a,zzz"), false, &c, None).unwrap_err();
        match err {
            Error::Config { message } => {
                assert!(message.contains("zzz") && message.contains("a") && message.contains("b"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn all_flag_returns_sorted_everything() {
        let c = sample();
        let got = resolve_profiles(None, true, &c, None).unwrap();
        assert_eq!(
            got.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn all_flag_with_no_profiles_is_error() {
        assert!(matches!(
            resolve_profiles(None, true, &Config::default(), None),
            Err(Error::Config { .. })
        ));
    }

    #[test]
    fn csv_flag_beats_all_flag() {
        let c = sample();
        let got = resolve_profiles(Some("a"), true, &c, None).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn single_active_profile_when_no_flags() {
        let c = sample();
        let got = resolve_profiles(None, false, &c, None).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "b");
    }

    #[test]
    fn single_profile_uses_env_when_set() {
        let got = resolve_profiles(None, false, &sample(), Some("c")).unwrap();
        assert_eq!(got[0].0, "c");
    }

    fn env_overrides() -> ProfileOverrides {
        ProfileOverrides {
            url: Some("https://env".into()),
            token: Some("envtok".into()),
            ..Default::default()
        }
    }

    #[test]
    fn env_overrides_with_csv_profiles_are_a_config_error_naming_them() {
        let selected = resolve_profiles(Some("c,a"), false, &sample(), None).unwrap();

        let err = apply_overrides(selected, env_overrides(), Default::default()).unwrap_err();

        assert!(
            matches!(&err, Error::Config { message }
                if message.contains("GRAFANA_CLI_URL, GRAFANA_CLI_TOKEN")
                    && message.contains("(c, a)")),
            "{err:?}"
        );
    }

    #[test]
    fn flag_override_with_all_profiles_is_a_config_error_naming_it() {
        let selected = resolve_profiles(None, true, &sample(), None).unwrap();
        let flags = ProfileOverrides {
            token: Some("flagtok".into()),
            ..Default::default()
        };

        let err = apply_overrides(selected, Default::default(), flags).unwrap_err();

        assert!(
            matches!(&err, Error::Config { message } if message.contains("--token")),
            "{err:?}"
        );
    }

    #[test]
    fn several_profiles_without_overrides_are_returned_as_stored() {
        let selected = resolve_profiles(Some("c,a"), false, &sample(), None).unwrap();

        let got = apply_overrides(selected.clone(), Default::default(), Default::default());

        assert_eq!(got.unwrap(), selected);
    }

    #[test]
    fn all_profiles_selecting_a_single_profile_takes_overrides() {
        let mut config = Config::default();
        config.add_profile("only", profile("https://only")).unwrap();
        let selected = resolve_profiles(None, true, &config, None).unwrap();

        let got = apply_overrides(selected, env_overrides(), Default::default()).unwrap();

        assert_eq!(got[0].1.url, "https://env");
    }

    #[test]
    fn flag_overrides_beat_env_overrides() {
        let selected = resolve_profiles(Some("a"), false, &sample(), None).unwrap();
        let flags = ProfileOverrides {
            token: Some("flagtok".into()),
            ..Default::default()
        };

        let got = apply_overrides(selected, env_overrides(), flags).unwrap();

        assert_eq!(got[0].1.url, "https://env");
        assert_eq!(got[0].1.token.as_deref(), Some("flagtok"));
    }

    #[tokio::test]
    async fn run_preserves_input_order_despite_completion_order() {
        let profiles = vec![
            ("slow".to_string(), profile("slow")),
            ("fast".to_string(), profile("fast")),
        ];
        let results = run(profiles, |name, _| async move {
            if name == "slow" {
                for _ in 0..100 {
                    tokio::task::yield_now().await;
                }
            }
            Ok(format!("out:{name}"))
        })
        .await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "slow");
        assert_eq!(results[0].1.as_ref().unwrap(), "out:slow");
        assert_eq!(results[1].0, "fast");
        assert_eq!(results[1].1.as_ref().unwrap(), "out:fast");
    }

    #[tokio::test]
    async fn run_carries_errors_per_profile() {
        let profiles = vec![
            ("ok".to_string(), profile("ok")),
            ("bad".to_string(), profile("bad")),
        ];
        let results = run(profiles, |name, _| async move {
            if name == "bad" {
                Err(Error::Config {
                    message: "nope".into(),
                })
            } else {
                Ok(1u8)
            }
        })
        .await;
        assert!(results[0].1.is_ok());
        assert!(matches!(&results[1].1, Err(Error::Config { message }) if message == "nope"));
    }

    #[test]
    fn aggregate_exit_codes() {
        let ok: Result<u8> = Ok(1);
        let err = Err(Error::Config {
            message: "x".into(),
        });
        let a = "a".to_string();
        let b = "b".to_string();
        assert_eq!(aggregate_exit_code(&[(&a, &ok), (&b, &ok)]), 0);
        assert_eq!(aggregate_exit_code(&[(&a, &ok), (&b, &err)]), 0);
        assert_eq!(aggregate_exit_code(&[(&a, &err), (&b, &err)]), 1);
        assert_eq!(aggregate_exit_code::<u8>(&[]), 0);
    }
}
