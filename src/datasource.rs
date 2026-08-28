use crate::client::Datasource;
use crate::{Error, Result};

pub fn resolve<'a>(
    all: &'a [Datasource],
    wanted_type: &str,
    flag: Option<&str>,
) -> Result<&'a Datasource> {
    if let Some(flag) = flag {
        return all
            .iter()
            .find(|d| d.uid == flag || d.name == flag)
            .ok_or_else(|| Error::Config {
                message: format!("datasource '{flag}' not found"),
            });
    }
    let candidates = |list: &[Datasource]| -> Vec<String> {
        list.iter()
            .map(|d| format!("{}({})", d.name, d.r#type))
            .collect()
    };
    let matching: Vec<&Datasource> = all.iter().filter(|d| d.r#type == wanted_type).collect();
    match matching.as_slice() {
        [] => Err(Error::NoDatasource {
            wanted_type: wanted_type.to_string(),
            candidates: candidates(all),
        }),
        [only] => Ok(only),
        many => {
            let defaults: Vec<&&Datasource> = many.iter().filter(|d| d.is_default).collect();
            match defaults.as_slice() {
                [one] => Ok(one),
                _ => Err(Error::AmbiguousDatasource {
                    wanted_type: wanted_type.to_string(),
                    candidates: candidates(all),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds(id: i64, uid: &str, name: &str, ds_type: &str, is_default: bool) -> Datasource {
        Datasource {
            id,
            uid: uid.into(),
            name: name.into(),
            r#type: ds_type.into(),
            url: String::new(),
            is_default,
        }
    }

    fn fixtures() -> Vec<Datasource> {
        vec![
            ds(1, "u-loki", "Loki main", "loki", true),
            ds(2, "u-loki2", "Loki spare", "loki", false),
            ds(3, "u-prom", "Prom", "prometheus", false),
            ds(4, "u-tempo", "Tempo", "tempo", false),
        ]
    }

    #[test]
    fn single_of_type_wins() {
        let all = fixtures();
        assert_eq!(resolve(&all, "prometheus", None).unwrap().uid, "u-prom");
        assert_eq!(resolve(&all, "tempo", None).unwrap().uid, "u-tempo");
    }

    #[test]
    fn default_of_type_wins_over_multiple() {
        let all = fixtures();
        assert_eq!(resolve(&all, "loki", None).unwrap().uid, "u-loki");
    }

    #[test]
    fn multiple_without_single_default_is_ambiguous() {
        let all = vec![
            ds(1, "u-loki", "Loki main", "loki", false),
            ds(2, "u-loki2", "Loki spare", "loki", false),
            ds(3, "u-prom", "Prom", "prometheus", true),
        ];
        let err = resolve(&all, "loki", None).unwrap_err();
        match err {
            Error::AmbiguousDatasource {
                wanted_type,
                candidates,
            } => {
                assert_eq!(wanted_type, "loki");
                assert!(
                    candidates.contains(&"Loki main(loki)".to_string()),
                    "{candidates:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn two_defaults_is_ambiguous() {
        let all = vec![ds(1, "a", "A", "loki", true), ds(2, "b", "B", "loki", true)];
        assert!(matches!(
            resolve(&all, "loki", None),
            Err(Error::AmbiguousDatasource { .. })
        ));
    }

    #[test]
    fn none_of_type_lists_candidates() {
        let all = fixtures();
        let err = resolve(&all, "influxdb", None).unwrap_err();
        match err {
            Error::NoDatasource {
                wanted_type,
                candidates,
            } => {
                assert_eq!(wanted_type, "influxdb");
                assert_eq!(candidates.len(), 4);
                assert!(candidates.iter().all(|c| c.contains('(')));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn flag_matches_by_uid_or_name() {
        let all = fixtures();
        assert_eq!(
            resolve(&all, "loki", Some("u-loki2")).unwrap().uid,
            "u-loki2"
        );
        assert_eq!(resolve(&all, "loki", Some("Prom")).unwrap().uid, "u-prom");
    }

    #[test]
    fn flag_unknown_is_config_error() {
        let all = fixtures();
        let err = resolve(&all, "loki", Some("nope")).unwrap_err();
        assert!(matches!(err, Error::Config { message } if message.contains("nope")));
    }
}
