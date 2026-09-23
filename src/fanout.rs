use crate::config::{Config, Profile, apply_env_overrides};
use crate::{Error, Result};

pub fn resolve_profiles(
    profiles_flag: Option<&str>,
    all: bool,
    config: &Config,
) -> Result<Vec<(String, Profile)>> {
    if let Some(csv) = profiles_flag {
        let names: Vec<&str> = csv
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        let available = || -> String {
            if config.profiles.is_empty() {
                "(none)".into()
            } else {
                config
                    .profiles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };
        if names.is_empty() {
            return Err(Error::Config {
                message: format!("--profile is empty; available: {}", available()),
            });
        }
        let mut out = Vec::new();
        for name in names {
            let profile = config.profiles.get(name).ok_or_else(|| Error::Config {
                message: format!("profile '{name}' not found; available: {}", available()),
            })?;
            // GRAFANA_CLI_{URL,TOKEN,USERNAME,PASSWORD} override every selected
            // profile, like Config::active_profile does for the single case.
            out.push((name.to_string(), apply_env_overrides(profile.clone())));
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
            .map(|(name, profile)| (name.clone(), apply_env_overrides(profile.clone())))
            .collect());
    }
    let (name, profile) = config.active_profile(None)?;
    Ok(vec![(name, profile)])
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
        let got = resolve_profiles(Some("c,a"), false, &c).unwrap();
        assert_eq!(
            got.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["c", "a"]
        );
    }

    #[test]
    fn csv_unknown_name_lists_available() {
        let c = sample();
        let err = resolve_profiles(Some("a,zzz"), false, &c).unwrap_err();
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
        let got = resolve_profiles(None, true, &c).unwrap();
        assert_eq!(
            got.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn all_flag_with_no_profiles_is_error() {
        assert!(matches!(
            resolve_profiles(None, true, &Config::default()),
            Err(Error::Config { .. })
        ));
    }

    #[test]
    fn csv_flag_beats_all_flag() {
        let c = sample();
        let got = resolve_profiles(Some("a"), true, &c).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn single_active_profile_when_no_flags() {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap();
        let c = sample();
        let got = resolve_profiles(None, false, &c).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "b");
    }

    #[test]
    fn single_profile_uses_env_when_set() {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("GRAFANA_CLI_PROFILE", "c") };
        let got = resolve_profiles(None, false, &sample()).unwrap();
        assert_eq!(got[0].0, "c");
        unsafe { std::env::remove_var("GRAFANA_CLI_PROFILE") };
    }

    #[test]
    fn csv_and_all_flags_apply_env_overrides() {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap();
        let unset = resolve_profiles(Some("c,a"), false, &sample()).unwrap();
        unsafe {
            std::env::set_var("GRAFANA_CLI_URL", "https://env");
            std::env::set_var("GRAFANA_CLI_TOKEN", "envtok");
        }
        let csv = resolve_profiles(Some("c,a"), false, &sample()).unwrap();
        let all = resolve_profiles(None, true, &sample()).unwrap();
        unsafe {
            std::env::remove_var("GRAFANA_CLI_URL");
            std::env::remove_var("GRAFANA_CLI_TOKEN");
        }
        // without the env vars every profile keeps its own settings
        assert_eq!(unset[0].1.url, "https://c");
        assert_eq!(unset[1].1.token.as_deref(), Some("t"));
        assert_eq!(all.len(), 3);
        for (name, p) in csv.iter().chain(&all) {
            assert_eq!(p.url, "https://env", "{name}");
            assert_eq!(p.token.as_deref(), Some("envtok"), "{name}");
        }
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
