use crate::{Error, Result};
use chrono::{DateTime, Duration, Utc};

pub fn resolve_window(
    since: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let now = Utc::now();
    let invalid = |what: &str, bad: &str| Error::Config {
        message: format!("invalid {what}: '{bad}'"),
    };
    match (since, from, to) {
        (None, None, None) => Ok((now - Duration::hours(1), now)),
        (Some(spec), None, None) => {
            let dur =
                humantime::parse_duration(spec).map_err(|_| invalid("duration (--since)", spec))?;
            let dur = Duration::from_std(dur).map_err(|_| invalid("duration (--since)", spec))?;
            Ok((now - dur, now))
        }
        (None, Some(from_s), to_s) => {
            let start = DateTime::parse_from_rfc3339(from_s)
                .map_err(|_| invalid("RFC3339 timestamp (--from)", from_s))?
                .with_timezone(&Utc);
            let end = match to_s {
                Some(to_s) => DateTime::parse_from_rfc3339(to_s)
                    .map_err(|_| invalid("RFC3339 timestamp (--to)", to_s))?
                    .with_timezone(&Utc),
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

pub fn to_ns_string(dt: DateTime<Utc>) -> String {
    // ponytail: saturates at i64 nanosecond range (year ~2262); fine for logs.
    dt.timestamp_nanos_opt().unwrap_or(i64::MAX).to_string()
}

/// Fractional epoch seconds (f64, not i64): Grafana/Prometheus APIs accept
/// float epochs and sub-second precision must survive.
pub fn to_epoch_secs(dt: DateTime<Utc>) -> f64 {
    dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_nanos()) / 1e9
}

pub fn default_step_secs(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    let window_secs = (end - start).num_seconds().max(0) as f64;
    ((window_secs / 1000.0).ceil() as u64).max(1)
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
        assert!((end - Utc::now()).num_seconds().abs() <= 1);
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
        assert!((end - Utc::now()).num_seconds().abs() <= 1);
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

    #[test]
    fn ns_string() {
        let dt: DateTime<Utc> = DateTime::from_timestamp(1_000_000_007, 500_000_000).unwrap();
        assert_eq!(to_ns_string(dt), "1000000007500000000");
        assert_eq!(to_ns_string(DateTime::UNIX_EPOCH), "0");
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
        assert_eq!(default_step_secs(t0, t0 + Duration::seconds(1000)), 1);
        assert_eq!(default_step_secs(t0, t0 + Duration::seconds(1500)), 2);
        assert_eq!(default_step_secs(t0, t0), 1);
        assert_eq!(default_step_secs(t0, t0 + Duration::seconds(1)), 1);
        assert_eq!(
            default_step_secs(t0, t0 + Duration::seconds(9_999_999)),
            10000
        );
    }
}
