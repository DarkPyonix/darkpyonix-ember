//! Tokens per session and per day (SPEC FR-U7).
//!
//! Every `usage` event an agent adapter emits is already stored in `events`, so the totals are
//! queries over that table and nothing is recorded twice. A turn's counts are what the harness
//! itself reported (`AgentEvent::Usage`); a session whose harness reported none has
//! `reported: false` and its zeros mean "unknown", not "none used".
//!
//! Cost is always `null`. No price table is specified in SPEC FR-U7, and Ember does not guess
//! prices: a cost shown as a number would be an estimate presented as a fact. [`COST_NOTE`] says
//! why, and the client shows it where it shows the cost tile.
//!
//! Not recorded yet: cache read and write tokens (the `Usage` event carries input and output
//! only), and the account and model per turn beyond what the session record holds.

use std::collections::BTreeMap;

use rusqlite::params;
use serde::Serialize;

use crate::store::Store;

/// Why `cost_usd` is `null`.
pub const COST_NOTE: &str = "cost is not computed: FR-U7 specifies no price table and Ember does not estimate prices";

/// Token counts for one scope (a session, a day, or one harness on a day).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Tokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `input_tokens + output_tokens`.
    pub total_tokens: u64,
    /// Number of `usage` reports (about one per turn).
    pub turns: u64,
    /// Always `None` (serialised `null`), see [`COST_NOTE`].
    pub cost_usd: Option<f64>,
}

impl Tokens {
    fn add(&mut self, input: u64, output: u64, turns: u64) {
        self.input_tokens += input;
        self.output_tokens += output;
        self.total_tokens += input + output;
        self.turns += turns;
    }
}

/// One session's totals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionUsage {
    pub session_id: String,
    #[serde(flatten)]
    pub tokens: Tokens,
    /// `false` when no usage event was stored: the counts are unknown, not zero.
    pub reported: bool,
    pub cost_note: &'static str,
}

/// One harness's totals on one day.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentUsage {
    pub agent: String,
    #[serde(flatten)]
    pub tokens: Tokens,
}

/// One UTC day's totals, with the split by harness.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayUsage {
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    #[serde(flatten)]
    pub tokens: Tokens,
    /// Sessions that reported usage on this day.
    pub sessions: u64,
    pub by_agent: Vec<AgentUsage>,
}

/// Totals over a run of days.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageSummary {
    pub from: String,
    pub to: String,
    /// Oldest first, every day in the range present (zeros for days with no report).
    pub days: Vec<DayUsage>,
    #[serde(flatten)]
    pub totals: Tokens,
    pub by_agent: Vec<AgentUsage>,
    pub cost_note: &'static str,
}

const DAY_MS: i64 = 86_400_000;

/// `true` for the shape `YYYY-MM-DD`; [`Store::day_start_ms`] then rejects impossible dates.
pub fn valid_day(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

impl Store {
    /// Unix milliseconds at the start of the UTC day `day` (`YYYY-MM-DD`); `None` when it is not
    /// a real date.
    pub fn day_start_ms(&self, day: &str) -> anyhow::Result<Option<i64>> {
        if !valid_day(day) {
            return Ok(None);
        }
        // SQLite rolls 2026-02-31 over to March, so a date that does not survive `date()` unchanged
        // is refused.
        let secs: Option<String> = self.conn().query_row(
            "SELECT CASE WHEN date(?1) = ?1 THEN strftime('%s', ?1) END",
            params![day],
            |r| r.get(0),
        )?;
        Ok(secs.and_then(|s| s.parse::<i64>().ok()).map(|s| s * 1000))
    }

    /// Tokens the harness reported for `session_id`. `None` when the session does not exist.
    pub fn session_usage(&self, session_id: &str) -> anyhow::Result<Option<SessionUsage>> {
        if self.session(session_id)?.is_none() {
            return Ok(None);
        }
        let conn = self.conn();
        let (input, output, turns): (Option<i64>, Option<i64>, i64) = conn.query_row(
            "SELECT SUM(json_extract(event, '$.input_tokens')),
                    SUM(json_extract(event, '$.output_tokens')),
                    COUNT(*)
             FROM events WHERE session_id = ?1 AND json_extract(event, '$.kind') = 'usage'",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut tokens = Tokens::default();
        tokens.add(input.unwrap_or(0) as u64, output.unwrap_or(0) as u64, turns as u64);
        Ok(Some(SessionUsage {
            session_id: session_id.to_string(),
            tokens,
            reported: turns > 0,
            cost_note: COST_NOTE,
        }))
    }

    /// Per-day totals for the `days` UTC days ending on the day of `now`, oldest first. With
    /// `project`, only that project's sessions count.
    pub fn usage_summary(&self, now: i64, days: i64, project: Option<&str>) -> anyhow::Result<UsageSummary> {
        let days = days.clamp(1, 366);
        let today = now - now.rem_euclid(DAY_MS);
        let first = today - (days - 1) * DAY_MS;
        let conn = self.conn();
        let day_of = |ms: i64| -> rusqlite::Result<String> {
            conn.query_row("SELECT date(?1 / 1000, 'unixepoch')", params![ms], |r| r.get(0))
        };
        let mut by_day: BTreeMap<String, DayUsage> = BTreeMap::new();
        for i in 0..days {
            let day = day_of(first + i * DAY_MS)?;
            by_day.insert(
                day.clone(),
                DayUsage { day, tokens: Tokens::default(), sessions: 0, by_agent: Vec::new() },
            );
        }
        let mut stmt = conn.prepare(
            "SELECT date(e.at / 1000, 'unixepoch') AS day, s.agent,
                    SUM(json_extract(e.event, '$.input_tokens')),
                    SUM(json_extract(e.event, '$.output_tokens')),
                    COUNT(*), COUNT(DISTINCT e.session_id)
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE json_extract(e.event, '$.kind') = 'usage'
               AND e.at >= ?1 AND e.at < ?2 AND (?3 IS NULL OR s.project = ?3)
             GROUP BY day, s.agent ORDER BY day, s.agent",
        )?;
        let rows = stmt.query_map(params![first, today + DAY_MS, project], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                r.get::<_, i64>(4)? as u64,
                r.get::<_, i64>(5)? as u64,
            ))
        })?;
        let mut totals = Tokens::default();
        let mut agents: BTreeMap<String, Tokens> = BTreeMap::new();
        for row in rows {
            let (day, agent, input, output, turns, sessions) = row?;
            let Some(d) = by_day.get_mut(&day) else { continue };
            d.tokens.add(input, output, turns);
            // Sessions are counted per (day, agent); a session has one agent, so the sum is exact.
            d.sessions += sessions;
            let mut a = Tokens::default();
            a.add(input, output, turns);
            d.by_agent.push(AgentUsage { agent: agent.clone(), tokens: a });
            totals.add(input, output, turns);
            agents.entry(agent).or_default().add(input, output, turns);
        }
        let days: Vec<DayUsage> = by_day.into_values().collect();
        Ok(UsageSummary {
            from: days.first().map(|d| d.day.clone()).unwrap_or_default(),
            to: days.last().map(|d| d.day.clone()).unwrap_or_default(),
            days,
            totals,
            by_agent: agents.into_iter().map(|(agent, tokens)| AgentUsage { agent, tokens }).collect(),
            cost_note: COST_NOTE,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(i: u64, o: u64) -> crate::events::AgentEvent {
        crate::events::AgentEvent::Usage { input_tokens: i, output_tokens: o }
    }

    /// Move every event of `session` to `at` (Unix ms), to place reports on past days.
    fn set_at(store: &Store, session: &str, seq: i64, at: i64) {
        store
            .conn()
            .execute("UPDATE events SET at = ?3 WHERE session_id = ?1 AND seq = ?2", params![session, seq, at])
            .unwrap();
    }

    #[test]
    fn fr_u7_days_split_at_utc_midnight_and_empty_days_are_zero() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_session("p", crate::agents::AgentKind::Scripted, "/", None, "a").unwrap();
        // 2026-11-04 23:59:59.999 UTC and 2026-11-05 00:00:00.000 UTC.
        let d4_end = 1_793_836_799_999;
        let d5_start = 1_793_836_800_000;
        store.append(&a.id, &usage(100, 10)).unwrap();
        store.append(&a.id, &usage(200, 20)).unwrap();
        set_at(&store, &a.id, 1, d4_end);
        set_at(&store, &a.id, 2, d5_start);
        let s = store.usage_summary(d5_start + 5_000, 3, None).unwrap();
        assert_eq!(s.days.iter().map(|d| d.day.as_str()).collect::<Vec<_>>(), ["2026-11-03", "2026-11-04", "2026-11-05"]);
        assert_eq!(s.days[0].tokens.total_tokens, 0);
        assert_eq!(s.days[1].tokens.input_tokens, 100);
        assert_eq!(s.days[2].tokens.input_tokens, 200);
        assert_eq!(s.totals.total_tokens, 330);
        assert_eq!(s.totals.turns, 2);
        assert_eq!(s.totals.cost_usd, None);
        assert_eq!(store.day_start_ms("2026-11-05").unwrap(), Some(d5_start));
        assert_eq!(store.day_start_ms("2026-02-31").unwrap(), None);
    }

    #[test]
    fn day_format_is_checked() {
        assert!(valid_day("2026-11-06"));
        assert!(!valid_day("2026-1-06"));
        assert!(!valid_day("2026/11/06"));
        assert!(!valid_day("abcd-ef-gh"));
    }
}
