use crate::client::Datasource;
use crate::{Error, Result};

// The datasource types gcli queries; also the values of `datasources --type`.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Kind {
    Loki,
    Prometheus,
    Tempo,
}

impl Kind {
    /// Grafana's datasource `type` string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loki => "loki",
            Self::Prometheus => "prometheus",
            Self::Tempo => "tempo",
        }
    }
}

pub fn resolve<'a>(
    all: &'a [Datasource],
    kind: Kind,
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
    let wanted_type = kind.as_str();
    let label = |d: &Datasource| format!("{}({})", d.name, d.r#type);
    let matching: Vec<&Datasource> = all.iter().filter(|d| d.r#type == wanted_type).collect();
    match matching.as_slice() {
        // Nothing of the wanted type: list everything the instance has.
        [] => Err(Error::NoDatasource {
            wanted_type: wanted_type.to_string(),
            candidates: all.iter().map(label).collect(),
        }),
        [only] => Ok(only),
        many => {
            let defaults: Vec<&&Datasource> = many.iter().filter(|d| d.is_default).collect();
            match defaults.as_slice() {
                [one] => Ok(one),
                // Only the same-type datasources are real choices for --datasource.
                _ => Err(Error::AmbiguousDatasource {
                    wanted_type: wanted_type.to_string(),
                    candidates: many.iter().copied().map(label).collect(),
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
        assert_eq!(resolve(&all, Kind::Prometheus, None).unwrap().uid, "u-prom");
        assert_eq!(resolve(&all, Kind::Tempo, None).unwrap().uid, "u-tempo");
    }

    #[test]
    fn default_of_type_wins_over_multiple() {
        let all = fixtures();
        assert_eq!(resolve(&all, Kind::Loki, None).unwrap().uid, "u-loki");
    }

    #[test]
    fn multiple_without_single_default_is_ambiguous() {
        let all = vec![
            ds(1, "u-loki", "Loki main", "loki", false),
            ds(2, "u-loki2", "Loki spare", "loki", false),
            ds(3, "u-prom", "Prom", "prometheus", true),
        ];
        let err = resolve(&all, Kind::Loki, None).unwrap_err();
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
            resolve(&all, Kind::Loki, None),
            Err(Error::AmbiguousDatasource { .. })
        ));
    }

    #[test]
    fn ambiguous_lists_only_same_type_candidates() {
        let all = vec![
            ds(1, "u-loki", "Loki main", "loki", false),
            ds(2, "u-loki2", "Loki spare", "loki", false),
            ds(3, "u-prom", "Prom", "prometheus", true),
        ];
        match resolve(&all, Kind::Loki, None).unwrap_err() {
            Error::AmbiguousDatasource { candidates, .. } => {
                assert_eq!(candidates, ["Loki main(loki)", "Loki spare(loki)"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn none_of_type_lists_candidates() {
        let all: Vec<Datasource> = fixtures()
            .into_iter()
            .filter(|d| d.r#type != "tempo")
            .collect();
        let err = resolve(&all, Kind::Tempo, None).unwrap_err();
        match err {
            Error::NoDatasource {
                wanted_type,
                candidates,
            } => {
                assert_eq!(wanted_type, "tempo");
                assert_eq!(candidates.len(), 3);
                assert!(candidates.iter().all(|c| c.contains('(')));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn flag_matches_by_uid_or_name() {
        let all = fixtures();
        assert_eq!(
            resolve(&all, Kind::Loki, Some("u-loki2")).unwrap().uid,
            "u-loki2"
        );
        assert_eq!(
            resolve(&all, Kind::Loki, Some("Prom")).unwrap().uid,
            "u-prom"
        );
    }

    #[test]
    fn flag_unknown_is_config_error() {
        let all = fixtures();
        let err = resolve(&all, Kind::Loki, Some("nope")).unwrap_err();
        assert!(matches!(err, Error::Config { message } if message.contains("nope")));
    }
}
