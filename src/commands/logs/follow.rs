use crate::Result;
use crate::client::{Datasource, GrafanaClient};
use crate::commands::resolve_ds;
use crate::datasource::Kind;
use crate::error::Error;
use crate::output::LogRecord;
use crate::timeutil::{resolve_window, to_ns_string};
use chrono::Utc;
use std::collections::BTreeMap;

use super::Direction;
use super::SearchArgs;
use super::search::{Entry, log_entries, step_secs};

/// The `--follow` cursor: the newest nanosecond printed and the
/// (labels, line) pairs already printed at exactly that nanosecond. A
/// forward poll starts at `last_ns`, so Loki re-sends the boundary entries;
/// only a pair already printed at that nanosecond is a duplicate.
#[derive(Debug, Clone)]
pub(crate) struct FollowCursor {
    last_ns: i64,
    seen_at_last: Vec<(BTreeMap<String, String>, String)>,
}

impl FollowCursor {
    /// The cursor after the ascending `printed` batch; an empty initial
    /// batch starts from `fallback_ns`, the end of the queried window.
    fn new(printed: &[Entry], fallback_ns: i64) -> Self {
        let Some(newest) = printed.last() else {
            return Self {
                last_ns: fallback_ns,
                seen_at_last: Vec::new(),
            };
        };
        let last_ns = newest.ns;
        Self {
            last_ns,
            seen_at_last: printed
                .iter()
                .filter(|entry| entry.ns == last_ns)
                .map(|entry| (entry.record.labels.clone(), entry.record.line.clone()))
                .collect(),
        }
    }

    /// Keeps only the entries not printed yet (given ascending, like every
    /// `log_entries` reply) and advances the cursor past them: an entry at
    /// the boundary nanosecond is old news when its labels and line were
    /// printed there, and the first step past the boundary starts a fresh
    /// duplicate set for the new newest nanosecond.
    fn take_new(&mut self, entries: Vec<Entry>) -> Vec<LogRecord> {
        let boundary = self.last_ns;
        let mut last_ns = boundary;
        let mut seen_at_last = self.seen_at_last.clone();
        let mut fresh = Vec::new();
        for Entry { ns, record } in entries {
            if ns == boundary
                && seen_at_last
                    .iter()
                    .any(|(labels, line)| *labels == record.labels && *line == record.line)
            {
                continue;
            }
            if ns > last_ns {
                last_ns = ns;
                seen_at_last.clear();
            }
            if ns == last_ns {
                seen_at_last.push((record.labels.clone(), record.line.clone()));
            }
            fresh.push(record);
        }
        self.last_ns = last_ns;
        self.seen_at_last = seen_at_last;
        fresh
    }
}

/// The polling half of `logs search --follow`: the datasource resolved once
/// and the cursor. `poll` runs one forward `query_range` from the last seen
/// nanosecond to now and returns only the entries not printed yet.
pub struct FollowStream {
    ds: Datasource,
    profile: String,
    query: String,
    limit: u64,
    cursor: FollowCursor,
}

impl FollowStream {
    /// One polling step; every entry it returns is ascending and unprinted.
    pub async fn poll(&mut self, client: &GrafanaClient) -> Result<Vec<LogRecord>> {
        let end = to_ns_string(Utc::now())?;
        let reply = client
            .proxy_request(
                &self.ds.uid,
                "loki/api/v1/query_range",
                &[
                    ("query", self.query.clone()),
                    ("start", self.cursor.last_ns.to_string()),
                    ("end", end),
                    ("limit", self.limit.to_string()),
                    ("direction", Direction::Forward.as_str().to_string()),
                ],
            )
            .await?;
        let entries = log_entries(&reply, &self.profile, &self.ds.name)?;
        Ok(self.cursor.take_new(entries))
    }
}

/// The start of a `--follow` run: the window query `search` would run, its
/// records (ascending, exactly `search`'s output) and the poller positioned
/// after them. The window is resolved here, once, because a follow run
/// never reaches the fanout that resolves it for every other command.
pub async fn follow_start(
    client: &GrafanaClient,
    profile: &str,
    args: &SearchArgs,
) -> Result<(Vec<LogRecord>, FollowStream)> {
    let (start_dt, end_dt) = resolve_window(
        args.window.since.as_deref(),
        args.window.from.as_deref(),
        args.window.to.as_deref(),
    )?;
    let start = to_ns_string(start_dt)?;
    let end = to_ns_string(end_dt)?;
    let ds = resolve_ds(client, Kind::Loki, args.window.datasource.as_deref()).await?;
    let mut pairs = vec![
        ("query", args.query.clone()),
        ("start", start),
        ("end", end.clone()),
        ("limit", args.limit.to_string()),
        ("direction", args.direction.as_str().to_string()),
    ];
    if let Some(secs) = step_secs(args.step.as_deref())? {
        pairs.push(("step", secs.to_string()));
    }
    let reply = client
        .proxy_request(&ds.uid, "loki/api/v1/query_range", &pairs)
        .await?;
    let entries = log_entries(&reply, profile, &ds.name)?;
    // Nothing matched: the cursor starts at the window end, the newest
    // moment known to hold no unprinted entry. `to_ns_string` rendered the
    // window end from an i64, so this parse cannot fail.
    let fallback_ns: i64 = end.parse().map_err(|_| Error::Config {
        message: format!("window end '{end}' is not integer nanoseconds"),
    })?;
    let cursor = FollowCursor::new(&entries, fallback_ns);
    let records = entries.into_iter().map(|entry| entry.record).collect();
    Ok((
        records,
        FollowStream {
            ds,
            profile: profile.to_string(),
            query: args.query.clone(),
            limit: args.limit,
            cursor,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, SecondsFormat};

    // --follow cursor: newest printed nanosecond, plus the (labels, line)
    // pairs printed at exactly that nanosecond, is what separates a boundary
    // re-send from a new entry.
    fn follow_entry(ns: i64, job: &str, line: &str) -> Entry {
        Entry {
            ns,
            record: LogRecord {
                profile: "p".into(),
                datasource: "Loki".into(),
                ts: DateTime::from_timestamp_nanos(ns).to_rfc3339_opts(SecondsFormat::Millis, true),
                labels: BTreeMap::from([("job".into(), job.into())]),
                line: line.into(),
            },
        }
    }

    #[test]
    fn cursor_from_the_initial_batch_sits_at_the_newest_nanosecond() {
        let cursor = FollowCursor::new(
            &[follow_entry(100, "api", "a"), follow_entry(200, "api", "b")],
            500,
        );

        assert_eq!(cursor.last_ns, 200);
        assert_eq!(cursor.seen_at_last.len(), 1);
    }

    #[test]
    fn cursor_from_an_empty_initial_batch_falls_back_to_the_window_end() {
        let cursor = FollowCursor::new(&[], 500);

        assert_eq!(cursor.last_ns, 500);
        assert!(cursor.seen_at_last.is_empty());
    }

    #[test]
    fn take_new_dedups_boundary_entries_by_labels_and_line() {
        let mut cursor = FollowCursor::new(&[follow_entry(200, "api", "b")], 500);

        let fresh = cursor.take_new(vec![
            follow_entry(200, "api", "b"), // re-sent at the boundary: duplicate
            follow_entry(200, "web", "c"), // new labels at the boundary: printed
            follow_entry(300, "api", "d"),
        ]);

        let lines: Vec<&str> = fresh.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["c", "d"]);
        assert_eq!(cursor.last_ns, 300);
    }

    #[test]
    fn take_new_remembers_every_entry_at_the_new_boundary() {
        let mut cursor = FollowCursor::new(&[follow_entry(200, "api", "b")], 500);

        let fresh = cursor.take_new(vec![
            follow_entry(300, "api", "d"),
            follow_entry(300, "web", "e"),
        ]);

        assert_eq!(fresh.len(), 2);
        let again = cursor.take_new(vec![
            follow_entry(300, "api", "d"),
            follow_entry(300, "web", "e"),
        ]);
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn take_new_with_only_duplicates_keeps_the_cursor() {
        let mut cursor = FollowCursor::new(&[follow_entry(200, "api", "b")], 500);

        let fresh = cursor.take_new(vec![follow_entry(200, "api", "b")]);

        assert!(fresh.is_empty(), "{fresh:?}");
        assert_eq!(cursor.last_ns, 200);
        assert_eq!(cursor.seen_at_last.len(), 1);
    }
}
