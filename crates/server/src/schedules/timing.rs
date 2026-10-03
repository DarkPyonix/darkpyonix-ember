//! When schedules fire (FR-A8): cron expressions in a time zone, fixed intervals, one-off times.
//!
//! Cron expressions are parsed by `croner` (five fields: minute, hour, day of month, month, day
//! of week; `L`, `W`, `#` and `@daily`-style nicknames) and evaluated in an IANA time zone from
//! `chrono-tz`, so "09:00 Europe/Berlin" stays 09:00 local across daylight-saving changes.
//! Around a change:
//! - a wall-clock time that occurs twice (autumn, clocks go back) fires once — [`next_after`]
//!   skips an occurrence with the same local time as the one it continues from;
//! - a wall-clock time that does not exist (spring, clocks go forward) follows `croner`'s
//!   handling; the result is always a real instant after the previous one.
//!
//! Intervals count whole periods from the schedule's anchor (its creation, or resume), in real
//! elapsed time, independent of time zones. All instants are Unix milliseconds.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;

use super::ScheduleKind;

/// Shortest interval a schedule may use.
pub const MIN_INTERVAL_SECS: u64 = 60;

/// Upper bound on how many past triggers [`occurrences_between`] walks (a one-minute cron over a
/// week down is ~10k).
pub const MAX_WALK: usize = 200_000;

fn parse_tz(tz: &str) -> Result<Tz, String> {
    tz.parse::<Tz>().map_err(|_| format!("unknown time zone {tz:?} (use an IANA name like \"Asia/Seoul\" or \"UTC\")"))
}

fn parse_cron(expr: &str) -> Result<Cron, String> {
    let expr = expr.trim();
    if !expr.starts_with('@') && expr.split_whitespace().count() != 5 {
        return Err(format!(
            "cron expression {expr:?} must have five fields: minute hour day-of-month month day-of-week"
        ));
    }
    Cron::new(expr).parse().map_err(|e| format!("invalid cron expression {expr:?}: {e}"))
}

/// Check a schedule kind: the expression parses, the zone exists, the interval is long enough.
/// One-off times are checked against the clock by the caller.
pub fn validate(kind: &ScheduleKind) -> Result<(), String> {
    match kind {
        ScheduleKind::Cron { expr, tz } => {
            parse_tz(tz)?;
            parse_cron(expr)?;
            Ok(())
        }
        ScheduleKind::Interval { seconds } => {
            if *seconds < MIN_INTERVAL_SECS {
                return Err(format!("interval must be at least {MIN_INTERVAL_SECS} seconds"));
            }
            Ok(())
        }
        ScheduleKind::Once { .. } => Ok(()),
    }
}

/// The first trigger strictly after `after_ms`, or `None` when there is none (a one-off already
/// past, a cron expression that never matches again). `anchor_ms` is where intervals count from.
pub fn next_after(kind: &ScheduleKind, anchor_ms: i64, after_ms: i64) -> Result<Option<i64>, String> {
    match kind {
        ScheduleKind::Once { at } => Ok((*at > after_ms).then_some(*at)),
        ScheduleKind::Interval { seconds } => {
            let period = (*seconds as i64).saturating_mul(1000).max(1);
            let first = anchor_ms.saturating_add(period);
            if after_ms < first {
                return Ok(Some(first));
            }
            let k = (after_ms - anchor_ms) / period + 1;
            Ok(Some(anchor_ms.saturating_add(k.saturating_mul(period))))
        }
        ScheduleKind::Cron { expr, tz } => {
            let tz = parse_tz(tz)?;
            let cron = parse_cron(expr)?;
            let after = DateTime::<Utc>::from_timestamp_millis(after_ms)
                .ok_or_else(|| format!("time {after_ms} is out of range"))?
                .with_timezone(&tz);
            let previous_local = after.naive_local();
            let mut from = after;
            // A repeated wall-clock time (clocks went back) is skipped; at most once per change.
            for _ in 0..4 {
                let next = match cron.find_next_occurrence(&from, false) {
                    Ok(t) => t,
                    // croner gives up when no match exists within its search horizon.
                    Err(_) => return Ok(None),
                };
                if next.naive_local() == previous_local {
                    from = next;
                    continue;
                }
                return Ok(Some(next.timestamp_millis()));
            }
            Ok(None)
        }
    }
}

/// Triggers in `[first, until]`, where `first` is a planned trigger time: how many there are
/// (walking at most [`MAX_WALK`]), the earliest, and the latest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub count: usize,
    pub first: i64,
    pub latest: i64,
    /// The latest triggers, oldest first, at most `keep` of them (for per-trigger records).
    pub recent: Vec<i64>,
}

pub fn occurrences_between(
    kind: &ScheduleKind,
    anchor_ms: i64,
    first: i64,
    until: i64,
    keep: usize,
) -> Result<Due, String> {
    let mut due = Due { count: 1, first, latest: first, recent: vec![first] };
    let mut t = first;
    while due.count < MAX_WALK {
        match next_after(kind, anchor_ms, t)? {
            Some(n) if n <= until => {
                due.count += 1;
                due.latest = n;
                due.recent.push(n);
                if due.recent.len() > keep.max(1) {
                    due.recent.remove(0);
                }
                t = n;
            }
            _ => break,
        }
    }
    Ok(due)
}

/// `2026-10-03T09:00:00Z`, for notices.
pub fn rfc3339(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| ms.to_string())
}

/// Parse an RFC 3339 time (`2026-10-04T09:00:00+09:00`) to Unix ms.
pub fn parse_rfc3339(s: &str) -> Result<i64, String> {
    DateTime::parse_from_rfc3339(s.trim())
        .map(|t| t.timestamp_millis())
        .map_err(|e| format!("invalid time {s:?} (use RFC 3339, e.g. 2026-10-04T09:00:00+09:00): {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        parse_rfc3339(s).unwrap()
    }

    fn cron(expr: &str, tz: &str) -> ScheduleKind {
        ScheduleKind::Cron { expr: expr.into(), tz: tz.into() }
    }

    #[test]
    fn cron_in_a_time_zone_keeps_local_time_across_dst() {
        // Europe/Berlin goes to summer time on 2026-03-29: 09:00 local is 08:00Z before, 07:00Z after.
        let k = cron("0 9 * * *", "Europe/Berlin");
        let a = next_after(&k, 0, ms("2026-03-27T12:00:00Z")).unwrap().unwrap();
        assert_eq!(rfc3339(a), "2026-03-28T08:00:00Z");
        let b = next_after(&k, 0, a).unwrap().unwrap();
        assert_eq!(rfc3339(b), "2026-03-29T07:00:00Z");
        // And back on 2026-10-25.
        let c = next_after(&k, 0, ms("2026-10-24T12:00:00Z")).unwrap().unwrap();
        assert_eq!(rfc3339(c), "2026-10-25T08:00:00Z");

        // Seoul has no DST: 09:00 KST is always 00:00Z.
        let k = cron("0 9 * * 1-5", "Asia/Seoul");
        // 2026-10-03 is a Saturday; next weekday 09:00 KST is Monday 2026-10-05.
        let n = next_after(&k, 0, ms("2026-10-03T00:00:00Z")).unwrap().unwrap();
        assert_eq!(rfc3339(n), "2026-10-05T00:00:00Z");
    }

    #[test]
    fn repeated_wall_clock_time_fires_once() {
        // America/New_York falls back on 2026-11-01: 01:30 local happens at 05:30Z and 06:30Z.
        let k = cron("30 1 * * *", "America/New_York");
        let first = next_after(&k, 0, ms("2026-10-31T12:00:00Z")).unwrap().unwrap();
        assert!(first == ms("2026-11-01T05:30:00Z") || first == ms("2026-11-01T06:30:00Z"), "{}", rfc3339(first));
        let second = next_after(&k, 0, first).unwrap().unwrap();
        assert_eq!(rfc3339(second), "2026-11-02T06:30:00Z", "not twice on the same day");
    }

    #[test]
    fn nonexistent_wall_clock_time_still_moves_forward() {
        // America/New_York springs forward on 2026-03-08: 02:30 local does not exist that day.
        let k = cron("30 2 * * *", "America/New_York");
        let start = ms("2026-03-07T12:00:00Z");
        let n = next_after(&k, 0, start).unwrap().unwrap();
        assert!(n > start);
        // Either adjusted on the 8th or the next real 02:30 (2026-03-09 06:30Z); never later.
        assert!(n <= ms("2026-03-09T06:30:00Z"), "{}", rfc3339(n));
        let after = next_after(&k, 0, n).unwrap().unwrap();
        assert!(after > n);
    }

    #[test]
    fn cron_validation() {
        assert!(validate(&cron("0 9 * * *", "Asia/Seoul")).is_ok());
        assert!(validate(&cron("0 9 * * *", "Mars/Olympus")).is_err());
        assert!(validate(&cron("* * * * * *", "UTC")).is_err(), "no seconds field");
        assert!(validate(&cron("61 * * * *", "UTC")).is_err());
        assert!(validate(&ScheduleKind::Interval { seconds: 59 }).is_err());
        assert!(validate(&ScheduleKind::Interval { seconds: 60 }).is_ok());
    }

    #[test]
    fn interval_counts_whole_periods_from_the_anchor() {
        let k = ScheduleKind::Interval { seconds: 3600 };
        let anchor = ms("2026-10-03T00:00:00Z");
        assert_eq!(next_after(&k, anchor, anchor).unwrap(), Some(anchor + 3_600_000));
        assert_eq!(next_after(&k, anchor, anchor + 3_600_000).unwrap(), Some(anchor + 7_200_000));
        assert_eq!(next_after(&k, anchor, anchor + 3_600_001).unwrap(), Some(anchor + 7_200_000));
        // DST does not change real intervals.
        let a = ms("2026-03-29T00:30:00Z");
        assert_eq!(next_after(&k, a, a + 1).unwrap(), Some(ms("2026-03-29T01:30:00Z")));
    }

    #[test]
    fn once_fires_only_while_ahead() {
        let at = ms("2026-10-04T09:00:00+09:00");
        let k = ScheduleKind::Once { at };
        assert_eq!(next_after(&k, 0, at - 1).unwrap(), Some(at));
        assert_eq!(next_after(&k, 0, at).unwrap(), None);
    }

    #[test]
    fn occurrences_between_counts_and_keeps_the_latest() {
        let k = ScheduleKind::Interval { seconds: 60 };
        let due = occurrences_between(&k, 0, 60_000, 600_000, 3).unwrap();
        assert_eq!(due.count, 10);
        assert_eq!((due.first, due.latest), (60_000, 600_000));
        assert_eq!(due.recent, [480_000, 540_000, 600_000]);
        let once = ScheduleKind::Once { at: 5 };
        let due = occurrences_between(&once, 0, 5, 1_000, 10).unwrap();
        assert_eq!((due.count, due.latest), (1, 5));
    }
}
