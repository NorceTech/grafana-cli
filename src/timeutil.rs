use crate::{Result, error::Error};
use chrono::{DateTime, Duration, SecondsFormat, Utc};

pub fn resolve_window(
    since: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let now = Utc::now();
    let invalid = |what: &str, bad: &str| Error::Config {
        message: format!("invalid {what}: '{bad}'"),
    };
    // Chrono's subtraction panics beyond its representable range and Loki's
    // nanosecond wire format cannot express the i64-ns range's outside, so a
    // window that far out is an error naming the flag that caused it.
    let out_of_range = |flag: &str, bad: &str| Error::Config {
        message: format!(
            "{flag} '{bad}' reaches outside the queryable time range \
             (1677-09-21 to 2262-04-11)"
        ),
    };
    let in_ns_range = |flag: &str, bad: &str, dt: DateTime<Utc>| -> Result<DateTime<Utc>> {
        if dt.timestamp_nanos_opt().is_some() {
            return Ok(dt);
        }
        Err(out_of_range(flag, bad))
    };
    match (since, from, to) {
        (None, None, None) => Ok((now - Duration::hours(1), now)),
        (Some(spec), None, None) => {
            let dur =
                humantime::parse_duration(spec).map_err(|_| invalid("duration (--since)", spec))?;
            let dur = Duration::from_std(dur).map_err(|_| out_of_range("--since", spec))?;
            let start = now
                .checked_sub_signed(dur)
                .ok_or_else(|| out_of_range("--since", spec))?;
            Ok((in_ns_range("--since", spec, start)?, now))
        }
        (None, Some(from_s), to_s) => {
            let start = DateTime::parse_from_rfc3339(from_s)
                .map_err(|_| invalid("RFC3339 timestamp (--from)", from_s))?
                .with_timezone(&Utc);
            let start = in_ns_range("--from", from_s, start)?;
            let end = match to_s {
                Some(to_s) => {
                    let end = DateTime::parse_from_rfc3339(to_s)
                        .map_err(|_| invalid("RFC3339 timestamp (--to)", to_s))?
                        .with_timezone(&Utc);
                    in_ns_range("--to", to_s, end)?
                }
                None => now,
            };
            if start > end {
                return Err(Error::Config {
                    message: format!("--from ({from_s}) is after --to ({to_s:?})"),
                });
            }
            Ok((start, end))
        }
        (None, None, Some(to_s)) => Err(Error::Config {
            message: format!("--to ({to_s}) requires --from"),
        }),
        _ => Err(Error::Config {
            message: "--since cannot be combined with --from/--to".into(),
        }),
    }
}

/// Loki's wire format: integer nanoseconds since the epoch, as a string.
/// Dates outside 1677-09-21 to 2262-04-11 have no i64-nanosecond
/// representation; `resolve_window` keeps every window inside that range.
pub fn to_ns_string(dt: DateTime<Utc>) -> Result<String> {
    dt.timestamp_nanos_opt()
        .map(|ns| ns.to_string())
        .ok_or_else(|| Error::Config {
            message: format!(
                "timestamp {} is outside the queryable time range \
                 (1677-09-21 to 2262-04-11)",
                dt.to_rfc3339_opts(SecondsFormat::Secs, true)
            ),
        })
}

/// Fractional epoch seconds (f64, not i64): Grafana/Prometheus APIs accept
/// float epochs and sub-second precision must survive.
pub fn to_epoch_secs(dt: DateTime<Utc>) -> f64 {
    dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_nanos()) / 1e9
}

/// The default `metrics range` step: `max(15, ceil(window/1000))` seconds.
/// Grafana stores at its scrape interval (15 s at the coarsest), so a finer
/// step only repeats the last sample (a 15-minute `up` query at 1 s asked for
/// 2.28 M samples); the window-scaled half keeps big windows near 1000 points.
pub fn default_step_secs(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    let window_secs = (end - start).num_seconds().max(0);
    // ceil(window/1000) on the stable unsigned `div_ceil`; the `max(0)` keeps
    // the conversion lossless, a negative window is just a zero one.
    u64::try_from(window_secs)
        .unwrap_or(0)
        .div_ceil(1000)
        .max(15)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn all_none_defaults_to_1h() {
        let (start, end) = resolve_window(None, None, None).unwrap();
        let window = end - start;
        assert_eq!(window.num_seconds(), 3600);
        // 5 s, not 1 s: a loaded CI runner can sit between the two now() reads
        assert!((end - Utc::now()).num_seconds().abs() <= 5);
    }

    #[test]
    fn since_durations() {
        for (spec, secs) in [
            ("30s", 30),
            ("5m", 300),
            ("2h", 7200),
            ("1d", 86400),
            ("1w", 604800),
        ] {
            let (start, end) = resolve_window(Some(spec), None, None).unwrap();
            assert_eq!(end - start, Duration::seconds(secs), "{spec}");
        }
    }

    #[test]
    fn since_rejects_overlap_with_from_or_to() {
        assert!(resolve_window(Some("1h"), Some("2026-01-01T00:00:00Z"), None).is_err());
        assert!(resolve_window(Some("1h"), None, Some("2026-01-01T00:00:00Z")).is_err());
        assert!(
            resolve_window(
                Some("1h"),
                Some("2026-01-01T00:00:00Z"),
                Some("2026-01-02T00:00:00Z")
            )
            .is_err()
        );
    }

    #[test]
    fn from_to_rfc3339() {
        let (start, end) = resolve_window(
            None,
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-01T01:30:00Z"),
        )
        .unwrap();
        assert_eq!(start.to_rfc3339(), "2026-01-01T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-01-01T01:30:00+00:00");
    }

    #[test]
    fn from_only_defaults_to_now() {
        let (_, end) = resolve_window(None, Some("2026-01-01T00:00:00Z"), None).unwrap();
        assert!((end - Utc::now()).num_seconds().abs() <= 5);
    }

    #[test]
    fn from_after_to_rejected() {
        assert!(
            resolve_window(
                None,
                Some("2026-01-02T00:00:00Z"),
                Some("2026-01-01T00:00:00Z")
            )
            .is_err()
        );
    }

    #[test]
    fn to_without_from_rejected() {
        assert!(resolve_window(None, None, Some("2026-01-01T00:00:00Z")).is_err());
    }

    #[test]
    fn bad_inputs_rejected() {
        assert!(resolve_window(Some("notaduration"), None, None).is_err());
        assert!(resolve_window(None, Some("not-a-date"), None).is_err());
        assert!(resolve_window(None, None, Some("not-a-date")).is_err());
    }

    // chrono's date subtraction panics past its representable range; a huge
    // --since must be a config error naming the duration instead.
    #[test]
    fn since_beyond_representable_time_is_a_config_error_not_a_panic() {
        let result = resolve_window(Some("300000y"), None, None);

        assert!(
            matches!(&result, Err(Error::Config { message }) if message.contains("300000y")),
            "{result:?}"
        );
    }

    // Bounds beyond the i64 nanosecond range (1677-09-21 to 2262-04-11) have
    // no Loki wire representation; they are config errors naming the flag.
    #[test]
    fn windows_outside_the_ns_range_are_config_errors() {
        let cases = [
            ("1600-01-01T00:00:00Z", None),
            ("2270-01-01T00:00:00Z", None),
            ("2026-01-01T00:00:00Z", Some("2270-01-01T00:00:00Z")),
        ];
        for (from, to) in cases {
            let result = resolve_window(None, Some(from), to);

            let named = to.unwrap_or(from);
            assert!(
                matches!(&result, Err(Error::Config { message }) if message.contains(named)),
                "{from}/{to:?}: {result:?}"
            );
        }
    }

    #[test]
    fn ns_string() {
        let dt: DateTime<Utc> = DateTime::from_timestamp(1_000_000_007, 500_000_000).unwrap();
        assert_eq!(to_ns_string(dt).unwrap(), "1000000007500000000");
        assert_eq!(to_ns_string(DateTime::UNIX_EPOCH).unwrap(), "0");
    }

    // Beyond the i64 nanosecond range (1677-09-21 to 2262-04-11) there is no
    // representation to saturate to; the encoder says so instead.
    #[test]
    fn ns_string_outside_the_ns_range_is_an_error() {
        let after = DateTime::from_timestamp(10_000_000_000, 0).unwrap(); // year 2286
        let before = DateTime::from_timestamp(-10_000_000_000, 0).unwrap(); // year 1653

        assert!(to_ns_string(after).is_err());
        assert!(to_ns_string(before).is_err());
    }

    #[test]
    fn epoch_secs() {
        let dt: DateTime<Utc> = DateTime::from_timestamp(1_000_000_000, 500_000_000).unwrap();
        assert_eq!(to_epoch_secs(dt), 1_000_000_000.5);
        assert_eq!(to_epoch_secs(DateTime::UNIX_EPOCH), 0.0);
    }

    #[test]
    fn step_secs() {
        let t0 = DateTime::UNIX_EPOCH;
        // Grafana floors at the 15 s scrape interval: a finer step only asks
        // for repeated last samples.
        assert_eq!(default_step_secs(t0, t0 + Duration::seconds(1000)), 15);
        assert_eq!(default_step_secs(t0, t0 + Duration::seconds(1500)), 15);
        assert_eq!(default_step_secs(t0, t0), 15);
        assert_eq!(default_step_secs(t0, t0 + Duration::seconds(1)), 15);
        assert_eq!(
            default_step_secs(t0, t0 + Duration::seconds(9_999_999)),
            10000
        );
        assert_eq!(default_step_secs(t0, t0 + Duration::seconds(20_000)), 20);
    }
}
