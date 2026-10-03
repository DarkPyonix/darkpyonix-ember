//! Several accounts per agent, usage and routing (SPEC FR-U1, FR-U2, FR-U3, FR-U5).
//!
//! ChatGPT accounts (Sign in with ChatGPT, FR-U4) are a separate kind in [`crate::chatgpt`]: they
//! hold OAuth tokens rather than an agent CLI's config directory, are never routed to agent
//! sessions, and their per-day usage is merged into [`Accounts::usage_since`].
//!
//! # Isolation without patching the agent (E2, FR-U1)
//!
//! Each account is a directory under `<data dir>/accounts/<id>` (mode `0700`) that the agent CLI
//! is pointed at through its own, documented configuration-root variable. Everything the CLI keeps
//! per user — credentials, settings, native session files — then lives there, so two accounts of
//! one agent never share credentials or settings, and the vendor binary is unchanged.
//!
//! | Agent | Variable | Sources | Verified (2026-10-03) |
//! | ----- | -------- | ------- | --------------------- |
//! | Claude Code | `CLAUDE_CONFIG_DIR` | Claude Code docs, "Environment variables" (code.claude.com/docs/en/env-vars): where Claude Code keeps its configuration and data files; the 2.1.288 binary also validates it (it must be an absolute path). | `CLAUDE_CONFIG_DIR=<empty tmp> claude auth status` (2.1.288) printed `"loggedIn": false`, `"authMethod": "none"`, `"configDirectory": <tmp>`, `"projectsDirectory": <tmp>/projects`, and wrote only `<tmp>/.claude.json` and `<tmp>/backups/`; the same command without the variable reports the real login as `"loggedIn": true`. So the macOS keychain login of the default config is not picked up by another config dir. |
//! | Codex | `CODEX_HOME` | Codex docs (github.com/openai/codex `docs/config.md`, developers.openai.com/codex/config): defaults to `~/.codex`, holds `config.toml`, `auth.json` and `sessions/`; `codex login --help` (0.155.1) refers to `~/.codex/config.toml`; `cli_auth_credentials_store` (file/keyring) is read from that config. | `CODEX_HOME=<empty tmp> codex login status` (0.155.1) printed `Not logged in` (exit 1) and created only `<tmp>/tmp/`; without the variable it prints `Logged in using ChatGPT`. |
//!
//! Neither check logged anything in or out. Login is never automated here: the API returns the
//! exact command (the agent's own login, run with the account's variable) for the user to run on
//! the server, and [`Accounts::check_login`] runs the agent's read-only status command.
//!
//! Consequences: the account's native session files live in its directory, so resuming an Ember
//! session from the agent's own CLI needs the same variable set (FR-A4). Variables in the server's
//! own environment that override login (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `CODEX_API_KEY`)
//! still reach every agent process and would defeat the separation; [`Accounts::install`] warns.
//!
//! # Routing (FR-U2, FR-U3)
//!
//! A session's account is chosen once, at creation, and stored on the session with the reason.
//! An explicitly requested account is used as is. Otherwise the router picks, among the agent's
//! accounts that are neither rate-limited nor known to be logged out, the one with the fewest
//! tokens used today (UTC), preferring the default account on ties, then the oldest. With no
//! accounts for an agent the session uses the server's own login (`account_id` = `None`); when
//! accounts exist but none is available, creation fails with the reason instead of falling back.
//!
//! # Rate limits
//!
//! Adapters emit [`AgentEvent::RateLimited`] from the agents' structured signals (Claude Code's
//! `rate_limit_event` with status `rejected`; Codex's `account/rateLimits/updated` and `error`
//! with `codexErrorInfo` `usageLimitExceeded` / `rateLimitExceeded`). As a fallback, an `Error`
//! whose text looks like a limit ([`limit_in_message`]) also marks the account. Without a reset
//! time the account is held back for [`DEFAULT_COOLDOWN_MS`]. A turn that completes on the
//! account clears the mark.

pub mod api;
pub mod schema;
pub mod secrets;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::agents::AgentKind;
use crate::events::{AgentEvent, TurnOutcome};
use crate::session::{AccountChoice, Sessions};
use crate::store::{now_ms, Store, StoredEvent};
use secrets::SecretBox;

/// How long an account is held back after a limit with no known reset time.
pub const DEFAULT_COOLDOWN_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The variable that points `agent` at an account's directory.
pub fn config_env_var(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::ClaudeCode => "CLAUDE_CONFIG_DIR",
        AgentKind::Codex => "CODEX_HOME",
        AgentKind::Scripted => "EMBER_SCRIPTED_HOME",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub id: String,
    pub agent: AgentKind,
    pub label: String,
    pub config_dir: String,
    /// `unknown` | `logged_in` | `logged_out`, as last checked.
    pub status: String,
    pub is_default: bool,
    /// Unix ms; the account is limited while this is in the future.
    pub limited_until: Option<i64>,
    pub limit_reason: Option<String>,
    pub created_at: i64,
}

impl Account {
    pub fn limited_at(&self, now: i64) -> bool {
        self.limited_until.is_some_and(|t| t > now)
    }
}

/// Tokens reported by agents for one account (or the server's own login) on one UTC day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailyUsage {
    pub account_id: Option<String>,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Number of `usage` events (≈ turns).
    pub reports: u64,
}

/// How the user logs an account in: the agent's own commands, with the account's variable.
#[derive(Debug, Clone, Serialize)]
pub struct LoginInstructions {
    pub env: HashMap<&'static str, String>,
    pub login: String,
    pub status: String,
    pub logout: String,
    pub note: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("account {0} not found")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct Accounts {
    store: Arc<Store>,
    root: PathBuf,
    secrets: SecretBox,
    bins: HashMap<AgentKind, PathBuf>,
}

const ACCOUNT_COLUMNS: &str =
    "id, agent, label, config_dir, status, is_default, limited_until, limit_reason, created_at";

fn row_to_account(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let agent: String = r.get(1)?;
    Ok(Account {
        id: r.get(0)?,
        agent: AgentKind::parse(&agent).unwrap_or(AgentKind::Scripted),
        label: r.get(2)?,
        config_dir: r.get(3)?,
        status: r.get(4)?,
        is_default: r.get(5)?,
        limited_until: r.get(6)?,
        limit_reason: r.get(7)?,
        created_at: r.get(8)?,
    })
}

impl Accounts {
    /// `data_dir` holds `accounts/` and `secret.key`.
    pub fn open(store: Arc<Store>, data_dir: &Path) -> anyhow::Result<Arc<Accounts>> {
        let secrets = SecretBox::open_or_create(&data_dir.join("secret.key"))?;
        Self::with_secrets(store, &data_dir.join("accounts"), secrets)
    }

    pub fn with_secrets(
        store: Arc<Store>,
        root: &Path,
        secrets: SecretBox,
    ) -> anyhow::Result<Arc<Accounts>> {
        make_private_dir(root)?;
        let bins = HashMap::from([
            (
                AgentKind::ClaudeCode,
                bin_from_env("EMBER_CLAUDE_BIN", "claude"),
            ),
            (AgentKind::Codex, bin_from_env("EMBER_CODEX_BIN", "codex")),
        ]);
        Ok(Arc::new(Accounts {
            store,
            root: root.to_path_buf(),
            secrets,
            bins,
        }))
    }

    /// Override the CLI used for login checks and instructions (tests, custom installs).
    pub fn set_bin(self: &mut Arc<Self>, agent: AgentKind, bin: PathBuf) {
        Arc::get_mut(self)
            .expect("set_bin before sharing")
            .bins
            .insert(agent, bin);
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn secrets(&self) -> &SecretBox {
        &self.secrets
    }

    /// Apply accounts to `sessions`: the env of each agent start, the router, limit detection.
    pub fn install(self: &Arc<Self>, sessions: &Sessions) {
        for var in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "CODEX_API_KEY"] {
            if std::env::var_os(var).is_some() {
                tracing::warn!(
                    "{var} is set in the server's environment; it reaches every agent process and \
                     may override per-account logins"
                );
            }
        }
        let this = self.clone();
        sessions.add_start_hook(Arc::new(move |rec| match &rec.account_id {
            Some(id) => vec![(config_env_var(rec.agent).to_string(), this.dir_of(id))],
            None => Vec::new(),
        }));
        let this = self.clone();
        sessions.add_event_hook(Arc::new(move |ev| {
            if let Err(e) = this.on_event(ev) {
                tracing::warn!(session = %ev.session_id, "account limit tracking failed: {e:#}");
            }
        }));
        let this = self.clone();
        sessions.set_account_router(Arc::new(move |agent, requested| {
            this.route(agent, requested)
        }));
    }

    /// The account's directory. Derived from the id when the row is gone, so a session never
    /// silently runs under another login (FR-U2): the agent then reports not-logged-in.
    fn dir_of(&self, id: &str) -> String {
        match self.get(id) {
            Ok(Some(a)) => a.config_dir,
            _ => self.root.join(id).to_string_lossy().into_owned(),
        }
    }

    pub fn create(&self, agent: AgentKind, label: &str) -> anyhow::Result<Account> {
        let id = uuid::Uuid::new_v4().to_string();
        let dir = self.root.join(&id);
        make_private_dir(&dir)?;
        let dir_str = dir.to_string_lossy().into_owned();
        self.store.conn().execute(
            "INSERT INTO accounts (id, agent, label, config_dir, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, agent.as_str(), label, dir_str, now_ms()],
        )?;
        Ok(self.get(&id)?.expect("just inserted"))
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Option<Account>> {
        Ok(self
            .store
            .conn()
            .query_row(
                &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1"),
                params![id],
                row_to_account,
            )
            .optional()?)
    }

    pub fn list(&self, agent: Option<AgentKind>) -> anyhow::Result<Vec<Account>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE (?1 IS NULL OR agent = ?1) ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![agent.map(AgentKind::as_str)], row_to_account)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Delete an account and its directory (credentials included). Refused while sessions use
    /// it, since their account must not change (FR-U2).
    pub fn delete(&self, id: &str) -> Result<(), AccountError> {
        let acc = self
            .get(id)?
            .ok_or_else(|| AccountError::NotFound(id.into()))?;
        {
            let conn = self.store.conn();
            let used: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE account_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(anyhow::Error::from)?;
            if used > 0 {
                return Err(AccountError::Conflict(format!(
                    "account {id} is used by {used} session(s); its sessions cannot change account"
                )));
            }
            conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])
                .map_err(anyhow::Error::from)?;
        }
        let dir = PathBuf::from(&acc.config_dir);
        if dir.starts_with(&self.root) && dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
    }

    /// Make `id` its agent's default account (preferred by the router on ties).
    pub fn set_default(&self, id: &str) -> Result<Account, AccountError> {
        let acc = self
            .get(id)?
            .ok_or_else(|| AccountError::NotFound(id.into()))?;
        {
            let mut conn = self.store.conn();
            let tx = conn.transaction().map_err(anyhow::Error::from)?;
            tx.execute(
                "UPDATE accounts SET is_default = 0 WHERE agent = ?1",
                params![acc.agent.as_str()],
            )
            .map_err(anyhow::Error::from)?;
            tx.execute(
                "UPDATE accounts SET is_default = 1 WHERE id = ?1",
                params![id],
            )
            .map_err(anyhow::Error::from)?;
            tx.commit().map_err(anyhow::Error::from)?;
        }
        Ok(self.get(id)?.expect("exists"))
    }

    pub fn set_limit(&self, id: &str, until: i64, reason: &str) -> anyhow::Result<()> {
        self.store.conn().execute(
            "UPDATE accounts SET limited_until = ?2, limit_reason = ?3 WHERE id = ?1",
            params![id, until, reason],
        )?;
        Ok(())
    }

    pub fn clear_limit(&self, id: &str) -> anyhow::Result<()> {
        self.store.conn().execute(
            "UPDATE accounts SET limited_until = NULL, limit_reason = NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    fn set_status(&self, id: &str, status: &str) -> anyhow::Result<()> {
        self.store.conn().execute(
            "UPDATE accounts SET status = ?2 WHERE id = ?1",
            params![id, status],
        )?;
        Ok(())
    }

    /// The agent's own login/status/logout commands for this account.
    pub fn login_instructions(&self, acc: &Account) -> LoginInstructions {
        let var = config_env_var(acc.agent);
        let bin = self
            .bins
            .get(&acc.agent)
            .map(|b| b.to_string_lossy().into_owned())
            .unwrap_or_else(|| acc.agent.as_str().to_string());
        let prefix = format!(
            "{var}={} {}",
            shell_quote(&acc.config_dir),
            shell_quote(&bin)
        );
        let (login, status, logout, note) = match acc.agent {
            AgentKind::ClaudeCode => (
                format!("{prefix} auth login"),
                format!("{prefix} auth status"),
                format!("{prefix} auth logout"),
                "Run on the server as the user ember runs as. Log out before deleting the account: \
                 on macOS Claude Code keeps the credential in the login keychain.",
            ),
            AgentKind::Codex => (
                format!("{prefix} login"),
                format!("{prefix} login status"),
                format!("{prefix} logout"),
                "Run on the server as the user ember runs as. On a headless server add \
                 --device-auth to the login command.",
            ),
            AgentKind::Scripted => (String::new(), String::new(), String::new(), "test agent"),
        };
        LoginInstructions {
            env: HashMap::from([(var, acc.config_dir.clone())]),
            login,
            status,
            logout,
            note,
        }
    }

    /// Run the agent's read-only login status command for the account and record the result.
    pub async fn check_login(&self, id: &str) -> Result<Account, AccountError> {
        let acc = self
            .get(id)?
            .ok_or_else(|| AccountError::NotFound(id.into()))?;
        let status = match acc.agent {
            AgentKind::Scripted => "logged_in",
            agent => {
                let bin = self
                    .bins
                    .get(&agent)
                    .cloned()
                    .unwrap_or_else(|| agent.as_str().into());
                let args: &[&str] = match agent {
                    AgentKind::ClaudeCode => &["auth", "status", "--json"],
                    _ => &["login", "status"],
                };
                let out = tokio::process::Command::new(&bin)
                    .args(args)
                    .env(config_env_var(agent), &acc.config_dir)
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output();
                let out = tokio::time::timeout(Duration::from_secs(20), out)
                    .await
                    .context("login status check timed out")?
                    .with_context(|| format!("running {}", bin.display()))?;
                let logged_in = match agent {
                    AgentKind::ClaudeCode => {
                        serde_json::from_slice::<serde_json::Value>(&out.stdout)
                            .ok()
                            .and_then(|v| v["loggedIn"].as_bool())
                            .context("unexpected `claude auth status` output")?
                    }
                    _ => out.status.success(),
                };
                if logged_in {
                    "logged_in"
                } else {
                    "logged_out"
                }
            }
        };
        self.set_status(id, status)?;
        Ok(self.get(id)?.expect("exists"))
    }

    /// Per-day usage since `since_ms`, grouped by account (`None` = the server's own login).
    pub fn usage_since(&self, since_ms: i64) -> anyhow::Result<Vec<DailyUsage>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare(
            "SELECT s.account_id, date(e.at / 1000, 'unixepoch') AS day,
                    SUM(json_extract(e.event, '$.input_tokens')),
                    SUM(json_extract(e.event, '$.output_tokens')),
                    COUNT(*)
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE json_extract(e.event, '$.kind') = 'usage' AND e.at >= ?1
             GROUP BY s.account_id, day
             ORDER BY day, s.account_id",
        )?;
        let rows = stmt.query_map(params![since_ms], |r| {
            Ok(DailyUsage {
                account_id: r.get(0)?,
                day: r.get(1)?,
                input_tokens: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                output_tokens: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                reports: r.get::<_, i64>(4)? as u64,
            })
        })?;
        let mut out: Vec<DailyUsage> = rows.collect::<Result<_, _>>()?;
        drop(stmt);
        drop(conn);
        // ChatGPT plan usage (FR-U4) sits next to the agent accounts, keyed by its account id.
        out.extend(crate::chatgpt::store::usage_since(&self.store, since_ms)?);
        out.sort_by(|a, b| (&a.day, &a.account_id).cmp(&(&b.day, &b.account_id)));
        Ok(out)
    }

    /// Total tokens per account today (UTC).
    pub fn tokens_today(&self) -> anyhow::Result<HashMap<String, u64>> {
        let start = now_ms() - now_ms().rem_euclid(DAY_MS);
        Ok(self
            .usage_since(start)?
            .into_iter()
            .filter_map(|u| {
                u.account_id
                    .map(|id| (id, u.input_tokens + u.output_tokens))
            })
            .fold(HashMap::new(), |mut m, (id, t)| {
                *m.entry(id).or_default() += t;
                m
            }))
    }

    /// The routing decision for a new `agent` session (see the module docs).
    pub fn route(
        &self,
        agent: AgentKind,
        requested: Option<&str>,
    ) -> Result<Option<AccountChoice>, String> {
        let now = now_ms();
        if let Some(id) = requested {
            let acc = self
                .get(id)
                .map_err(|e| format!("{e:#}"))?
                .ok_or_else(|| format!("account {id} not found"))?;
            if acc.agent != agent {
                return Err(format!(
                    "account {id} belongs to {}, not {}",
                    acc.agent.as_str(),
                    agent.as_str()
                ));
            }
            let mut reason = format!("chosen by the user: {}", acc.label);
            if acc.limited_at(now) {
                reason += &format!(" (warning: {})", limited_text(&acc));
            }
            return Ok(Some((acc.id, reason)));
        }
        let accounts = self.list(Some(agent)).map_err(|e| format!("{e:#}"))?;
        if accounts.is_empty() {
            return Ok(None);
        }
        let used = self.tokens_today().map_err(|e| format!("{e:#}"))?;
        let mut skipped = Vec::new();
        let mut candidates = Vec::new();
        for acc in accounts {
            if acc.limited_at(now) {
                skipped.push(format!("{} ({})", acc.label, limited_text(&acc)));
            } else if acc.status == "logged_out" {
                skipped.push(format!("{} (not logged in)", acc.label));
            } else {
                candidates.push(acc);
            }
        }
        let tokens = |a: &Account| used.get(&a.id).copied().unwrap_or(0);
        candidates.sort_by_key(|a| (tokens(a), !a.is_default, a.created_at));
        let Some(best) = candidates.first() else {
            return Err(format!(
                "no {} account is available: {}",
                agent.as_str(),
                skipped.join("; ")
            ));
        };
        let mut reason = format!(
            "router: least used today ({} tokens) among {} available {} account(s){}: {}",
            tokens(best),
            candidates.len(),
            agent.as_str(),
            if best.is_default { ", default" } else { "" },
            best.label
        );
        if !skipped.is_empty() {
            reason += &format!("; skipped {}", skipped.join("; "));
        }
        Ok(Some((best.id.clone(), reason)))
    }

    /// Track limits from a stored event of an account's session.
    pub fn on_event(&self, ev: &StoredEvent) -> anyhow::Result<()> {
        let relevant = matches!(
            ev.event,
            AgentEvent::RateLimited { .. }
                | AgentEvent::Error { .. }
                | AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Completed
                }
        );
        if !relevant {
            return Ok(());
        }
        let account: Option<String> = self
            .store
            .conn()
            .query_row(
                "SELECT account_id FROM sessions WHERE id = ?1",
                params![ev.session_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(id) = account else {
            return Ok(());
        };
        let now = now_ms();
        match &ev.event {
            AgentEvent::RateLimited { resets_at, message } => {
                let until = resets_at.unwrap_or(now + DEFAULT_COOLDOWN_MS);
                self.set_limit(&id, until, message)?;
            }
            AgentEvent::Error { message } => {
                if let Some(reset) = limit_in_message(message) {
                    // A structured `RateLimited` (which may carry the reset) wins.
                    let already = self.get(&id)?.is_some_and(|a| a.limited_at(now));
                    if !already {
                        self.set_limit(&id, reset.unwrap_or(now + DEFAULT_COOLDOWN_MS), message)?;
                    }
                }
            }
            AgentEvent::TurnEnded { .. }
                if self.get(&id)?.is_some_and(|a| a.limited_until.is_some()) =>
            {
                self.clear_limit(&id)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Does an agent error message report a rate or usage limit? `Some(reset)` if so, with the reset
/// time (Unix ms) when the message carries one.
///
/// Shapes recognised: Claude Code's `Claude AI usage limit reached|<unix s>` (older CLIs) and
/// `You've hit your limit · resets …`, Claude API `rate_limit_error` / `429` errors, and Codex's
/// `You've hit your usage limit…` and `exceeded retry limit, last status: 429 Too Many Requests`.
pub fn limit_in_message(message: &str) -> Option<Option<i64>> {
    let lower = message.to_ascii_lowercase();
    let hit = [
        "usage limit",
        "rate limit",
        "rate_limit",
        "hit your limit",
        "limit reached",
        "quota",
        "too many requests",
        "api error: 429",
    ]
    .iter()
    .any(|p| lower.contains(p));
    if !hit {
        return None;
    }
    let reset = message.rsplit_once('|').and_then(|(_, t)| {
        let t = t.trim();
        let n: i64 = t.parse().ok()?;
        match t.len() {
            10 => Some(n * 1000),
            13 => Some(n),
            _ => None,
        }
    });
    Some(reset)
}

fn limited_text(acc: &Account) -> String {
    let until = acc.limited_until.map(format_utc).unwrap_or_default();
    match &acc.limit_reason {
        Some(r) => format!("rate-limited until {until}: {r}"),
        None => format!("rate-limited until {until}"),
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time in ms.
pub fn format_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn bin_from_env(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| default.into())
}

fn make_private_dir(dir: &Path) -> anyhow::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::TurnOutcome;

    fn setup() -> (tempfile::TempDir, Arc<Accounts>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let acc =
            Accounts::with_secrets(store, &dir.path().join("accounts"), SecretBox::ephemeral())
                .unwrap();
        (dir, acc)
    }

    fn session_on(a: &Accounts, account: &str) -> String {
        a.store()
            .create_session_with_account(
                "p",
                AgentKind::Codex,
                "/tmp",
                None,
                "t",
                Some((account, "test")),
            )
            .unwrap()
            .id
    }

    fn emit(a: &Accounts, session: &str, event: AgentEvent) {
        let (stored, _) = a.store().append(session, &event).unwrap();
        a.on_event(&stored).unwrap();
    }

    #[test]
    fn create_makes_a_private_isolated_dir_and_delete_removes_it() {
        let (_d, a) = setup();
        let x = a.create(AgentKind::Codex, "work").unwrap();
        let y = a.create(AgentKind::Codex, "home").unwrap();
        assert_ne!(x.config_dir, y.config_dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&x.config_dir)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let how = a.login_instructions(&x);
        assert_eq!(how.env["CODEX_HOME"], x.config_dir);
        assert!(how.login.starts_with("CODEX_HOME="), "{}", how.login);
        assert!(how.login.ends_with(" login"), "{}", how.login);

        // In use: refused, so the session's account cannot change.
        let s = session_on(&a, &x.id);
        assert!(matches!(a.delete(&x.id), Err(AccountError::Conflict(_))));
        let _ = s;
        a.delete(&y.id).unwrap();
        assert!(!Path::new(&y.config_dir).exists());
        assert!(a.get(&y.id).unwrap().is_none());
    }

    #[test]
    fn one_default_per_agent() {
        let (_d, a) = setup();
        let x = a.create(AgentKind::Codex, "x").unwrap();
        let y = a.create(AgentKind::Codex, "y").unwrap();
        let c = a.create(AgentKind::ClaudeCode, "c").unwrap();
        a.set_default(&x.id).unwrap();
        a.set_default(&c.id).unwrap();
        a.set_default(&y.id).unwrap();
        assert!(!a.get(&x.id).unwrap().unwrap().is_default);
        assert!(a.get(&y.id).unwrap().unwrap().is_default);
        assert!(a.get(&c.id).unwrap().unwrap().is_default);
    }

    #[test]
    fn router_prefers_least_used_then_default_and_skips_limited_with_reason() {
        let (_d, a) = setup();
        // No accounts: the server's own login.
        assert_eq!(a.route(AgentKind::Codex, None).unwrap(), None);

        let x = a.create(AgentKind::Codex, "x").unwrap();
        let y = a.create(AgentKind::Codex, "y").unwrap();
        // Tie at 0 tokens: oldest first, unless another is the default.
        assert_eq!(a.route(AgentKind::Codex, None).unwrap().unwrap().0, x.id);
        a.set_default(&y.id).unwrap();
        let (id, why) = a.route(AgentKind::Codex, None).unwrap().unwrap();
        assert_eq!(id, y.id);
        assert!(why.contains("default"), "{why}");

        // y used more today: x wins.
        let sy = session_on(&a, &y.id);
        emit(
            &a,
            &sy,
            AgentEvent::Usage {
                input_tokens: 100,
                output_tokens: 10,
            },
        );
        assert_eq!(a.route(AgentKind::Codex, None).unwrap().unwrap().0, x.id);

        // x hits its limit: skipped, and the reason says why.
        let sx = session_on(&a, &x.id);
        let reset = now_ms() + 3_600_000;
        emit(
            &a,
            &sx,
            AgentEvent::RateLimited {
                resets_at: Some(reset),
                message: "Codex rate_limit_reached".into(),
            },
        );
        let (id, why) = a.route(AgentKind::Codex, None).unwrap().unwrap();
        assert_eq!(id, y.id);
        assert!(why.contains("skipped x (rate-limited until"), "{why}");
        assert!(why.contains(&format_utc(reset)), "{why}");

        // Both limited: no silent fallback.
        emit(
            &a,
            &sy,
            AgentEvent::Error {
                message: "You've hit your usage limit.".into(),
            },
        );
        let err = a.route(AgentKind::Codex, None).unwrap_err();
        assert!(err.contains("no codex account is available"), "{err}");

        // A completed turn clears the mark; an explicit choice is always honoured.
        emit(
            &a,
            &sx,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed,
            },
        );
        assert_eq!(a.route(AgentKind::Codex, None).unwrap().unwrap().0, x.id);
        let (id, why) = a.route(AgentKind::Codex, Some(&y.id)).unwrap().unwrap();
        assert_eq!(id, y.id);
        assert!(
            why.starts_with("chosen by the user") && why.contains("warning"),
            "{why}"
        );
        // Wrong agent or unknown id: refused.
        assert!(a.route(AgentKind::ClaudeCode, Some(&y.id)).is_err());
        assert!(a.route(AgentKind::Codex, Some("nope")).is_err());
    }

    #[test]
    fn expired_limit_and_logged_out_accounts() {
        let (_d, a) = setup();
        let x = a.create(AgentKind::Codex, "x").unwrap();
        a.set_limit(&x.id, now_ms() - 1, "old").unwrap();
        assert_eq!(a.route(AgentKind::Codex, None).unwrap().unwrap().0, x.id);
        a.set_status(&x.id, "logged_out").unwrap();
        assert!(a
            .route(AgentKind::Codex, None)
            .unwrap_err()
            .contains("not logged in"));
    }

    #[test]
    fn usage_is_aggregated_per_account_and_day() {
        let (_d, a) = setup();
        let x = a.create(AgentKind::Codex, "x").unwrap();
        let s1 = session_on(&a, &x.id);
        let s2 = session_on(&a, &x.id);
        let own = a
            .store()
            .create_session("p", AgentKind::Codex, "/tmp", None, "t")
            .unwrap()
            .id;
        emit(
            &a,
            &s1,
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 1,
            },
        );
        emit(
            &a,
            &s2,
            AgentEvent::Usage {
                input_tokens: 20,
                output_tokens: 2,
            },
        );
        emit(
            &a,
            &s2,
            AgentEvent::AssistantMessage {
                text: "not usage".into(),
            },
        );
        emit(
            &a,
            &own,
            AgentEvent::Usage {
                input_tokens: 5,
                output_tokens: 5,
            },
        );
        // An old event, two days back, lands on its own day.
        a.store()
            .conn()
            .execute(
                "INSERT INTO events (session_id, seq, at, event) VALUES (?1, 99, ?2, ?3)",
                params![
                    s1,
                    now_ms() - 2 * DAY_MS,
                    r#"{"kind":"usage","input_tokens":7,"output_tokens":0}"#
                ],
            )
            .unwrap();
        let today = format_utc(now_ms())[..10].to_string();
        let rows = a.usage_since(now_ms() - 7 * DAY_MS).unwrap();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows.contains(&DailyUsage {
            account_id: Some(x.id.clone()),
            day: today.clone(),
            input_tokens: 30,
            output_tokens: 3,
            reports: 2
        }));
        assert!(rows.contains(&DailyUsage {
            account_id: None,
            day: today,
            input_tokens: 5,
            output_tokens: 5,
            reports: 1
        }));
        assert_eq!(a.tokens_today().unwrap()[&x.id], 33);
    }

    #[test]
    fn limit_messages() {
        assert_eq!(
            limit_in_message("Claude AI usage limit reached|1790982600"),
            Some(Some(1_790_982_600_000))
        );
        assert_eq!(
            limit_in_message("You've hit your limit · resets 3pm"),
            Some(None)
        );
        assert_eq!(
            limit_in_message("exceeded retry limit, last status: 429 Too Many Requests"),
            Some(None)
        );
        assert_eq!(limit_in_message("claude exited during a turn"), None);
        assert_eq!(limit_in_message("file not found: 429.txt"), None);
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_790_982_600_000), "2026-10-02T23:10:00Z");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn login_check_runs_the_agent_status_command_with_the_account_dir() {
        use std::os::unix::fs::PermissionsExt;
        let (d, mut a) = setup();
        // A stand-in `claude` that is logged in only in dirs containing a marker file.
        let bin = d.path().join("fake-claude");
        std::fs::write(
            &bin,
            "#!/bin/sh\nif [ -f \"$CLAUDE_CONFIG_DIR/marker\" ]; then echo '{\"loggedIn\": true}'; \
             else echo '{\"loggedIn\": false}'; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        a.set_bin(AgentKind::ClaudeCode, bin);
        let x = a.create(AgentKind::ClaudeCode, "x").unwrap();
        let y = a.create(AgentKind::ClaudeCode, "y").unwrap();
        std::fs::write(Path::new(&x.config_dir).join("marker"), "").unwrap();
        assert_eq!(a.check_login(&x.id).await.unwrap().status, "logged_in");
        assert_eq!(a.check_login(&y.id).await.unwrap().status, "logged_out");
    }
}
