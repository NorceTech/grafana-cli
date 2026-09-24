use crate::client::Datasource;
use crate::{Error, Result};
use clap::ValueEnum;

// The datasource types gcli queries; also the values of `datasources --type`.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
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

    /// The kind whose Grafana `type` string is `ds_type`; `None` for plugins.
    fn of_type(ds_type: &str) -> Option<Self> {
        Self::value_variants()
            .iter()
            .copied()
            .find(|kind| kind.as_str() == ds_type)
    }
}

/// Whether an explicit `--datasource` of `ds.r#type` may serve a `wanted`
/// query. Its own type passes silently; another core type (a Loki datasource
/// for a metrics query) is a config error. Any other type is a plugin, which
/// gcli queries anyway with the returned stderr note: Prometheus-compatible
/// plugins (VictoriaMetrics, Amazon Managed Prometheus) have their own type
/// but answer the same API.
pub fn check_type(ds: &Datasource, wanted: Kind) -> Result<Option<String>> {
    let expected = wanted.as_str();
    match Kind::of_type(&ds.r#type) {
        Some(kind) if kind == wanted => Ok(None),
        Some(_) => Err(Error::Config {
            message: format!(
                "datasource '{}' is type {}, expected {expected}; see gcli datasources --type {expected}",
                ds.name, ds.r#type
            ),
        }),
        None => Ok(Some(format!(
            "note: datasource '{}' is type {}, expected {expected}; querying it anyway",
            ds.name, ds.r#type
        ))),
    }
}

pub fn resolve<'a>(
    all: &'a [Datasource],
    kind: Kind,
    flag: Option<&str>,
) -> Result<&'a Datasource> {
    if let Some(flag) = flag {
        let ds = all
            .iter()
            .find(|d| d.uid == flag || d.name == flag)
            .ok_or_else(|| Error::Config {
                message: format!("datasource '{flag}' not found"),
            })?;
        if let Some(note) = check_type(ds, kind)? {
            eprintln!("{note}");
        }
        return Ok(ds);
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
            resolve(&all, Kind::Prometheus, Some("Prom")).unwrap().uid,
            "u-prom"
        );
    }

    #[test]
    fn check_type_passes_the_wanted_type_silently() {
        let prom = ds(3, "u-prom", "Prom", "prometheus", false);

        let note = check_type(&prom, Kind::Prometheus).unwrap();

        assert_eq!(note, None);
    }

    #[test]
    fn check_type_rejects_another_core_type_naming_both() {
        let loki = ds(1, "u-loki", "Loki main", "loki", true);

        let err = check_type(&loki, Kind::Prometheus).unwrap_err();

        assert!(
            matches!(&err, Error::Config { message }
                if message.contains("loki") && message.contains("prometheus")),
            "{err:?}"
        );
    }

    #[test]
    fn check_type_notes_a_plugin_type() {
        // Prometheus-compatible, but its own plugin type.
        let vm = ds(5, "u-vm", "VM", "victoriametrics-metrics-datasource", false);

        let note = check_type(&vm, Kind::Prometheus).unwrap().expect("a note");

        assert!(note.starts_with("note: "), "{note}");
        for fact in ["'VM'", "victoriametrics-metrics-datasource", "prometheus"] {
            assert!(note.contains(fact), "{fact} missing: {note}");
        }
    }

    #[test]
    fn flag_unknown_is_config_error() {
        let all = fixtures();
        let err = resolve(&all, Kind::Loki, Some("nope")).unwrap_err();
        assert!(matches!(err, Error::Config { message } if message.contains("nope")));
    }
}
