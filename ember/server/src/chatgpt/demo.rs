//! Demo data for screenshots (#193): one ChatGPT plan account and 14 days of its usage.
//!
//! Demo only, replaced when real accounts sign in. It is off unless `EMBER_DEMO_DATA=1`, and the
//! account has no token material, so it can never reach OpenAI. `main` calls [`seed`] once at
//! start; running it again changes nothing.

use rusqlite::params;

use crate::store::{now_ms, Store};

/// The environment switch the demo launch sets.
pub const ENV: &str = "EMBER_DEMO_DATA";
/// The fixed id of the demo account.
pub const ACCOUNT_ID: &str = "demo-chatgpt-plan";
/// How many days of usage the demo carries (Home shows 7 and 14).
pub const DAYS: i64 = 14;

const DAY_MS: i64 = 86_400_000;

/// Whether the demo switch value turns demo data on. Only `1` does.
pub fn enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

/// `(days_ago, input_tokens, output_tokens, reports)` for each of the [`DAYS`] days, today first.
/// Deterministic, uneven and never zero, so the bars differ in height.
pub fn usage_rows() -> Vec<(i64, u64, u64, u64)> {
    (0..DAYS)
        .map(|ago| {
            let wave = ((ago * 5 + 3) % 7) as u64 + 1;
            let input = 40_000 * wave + 1_500 * ago as u64;
            let output = input / 4;
            (ago, input, output, 3 + wave)
        })
        .collect()
}

/// Write the demo account and its usage when `value` (the switch) is `1`. Returns whether it did.
pub fn seed(store: &Store, value: Option<&str>) -> anyhow::Result<bool> {
    if !enabled(value) {
        return Ok(false);
    }
    let now = now_ms();
    let conn = store.conn();
    conn.execute(
        "INSERT OR IGNORE INTO chatgpt_accounts (id, label, client_id, subject, email, scopes,
             plan_usage, status, created_at, updated_at)
         VALUES (?1, 'ChatGPT Plus (demo)', 'demo-client', 'demo-subject', 'demo@example.com',
             'chatgpt.tokens.use.direct', 1, 'signed_in', ?2, ?2)",
        params![ACCOUNT_ID, now],
    )?;
    for (ago, input, output, reports) in usage_rows() {
        conn.execute(
            "INSERT OR REPLACE INTO chatgpt_usage (account_id, day, input_tokens, output_tokens, reports)
             VALUES (?1, date(?2 / 1000, 'unixepoch'), ?3, ?4, ?5)",
            params![ACCOUNT_ID, now - ago * DAY_MS, input as i64, output as i64, reports as i64],
        )?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatgpt::store as gstore;

    #[test]
    fn demo_data_is_off_unless_the_switch_is_one() {
        assert!(!enabled(None));
        assert!(!enabled(Some("")));
        assert!(!enabled(Some("0")));
        assert!(!enabled(Some("true")));
        assert!(enabled(Some("1")));
        let store = Store::open_in_memory().unwrap();
        assert!(!seed(&store, None).unwrap());
        assert!(gstore::list(&store).unwrap().is_empty());
        assert!(gstore::usage_since(&store, 0).unwrap().is_empty());
    }

    #[test]
    fn generated_usage_covers_14_distinct_days_with_uneven_values() {
        let rows = usage_rows();
        assert_eq!(rows.len(), 14);
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), (0..14).collect::<Vec<_>>());
        assert!(rows.iter().all(|r| r.1 > 0 && r.2 > 0 && r.3 > 0));
        let mut inputs: Vec<u64> = rows.iter().map(|r| r.1).collect();
        inputs.sort_unstable();
        inputs.dedup();
        assert!(inputs.len() > 3);
    }

    #[test]
    fn seed_writes_one_plan_account_and_14_days_once() {
        let store = Store::open_in_memory().unwrap();
        assert!(seed(&store, Some("1")).unwrap());
        assert!(seed(&store, Some("1")).unwrap());
        let accounts = gstore::list(&store).unwrap();
        assert_eq!(accounts.len(), 1);
        assert!(accounts[0].plan_usage);
        let usage = gstore::usage_since(&store, 0).unwrap();
        assert_eq!(usage.len(), 14);
        assert!(usage.iter().all(|u| u.account_id.as_deref() == Some(ACCOUNT_ID)));
    }
}
