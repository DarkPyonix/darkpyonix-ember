//! Agent-to-agent messaging (SPEC §T, INTENT D5).
//!
//! Every agent process gets runtime credentials in its environment (`EMBER_URL`,
//! `EMBER_SESSION_ID`, `EMBER_RUNTIME_TOKEN`) and instructions for the `ember-a2a` command, which
//! calls the API in [`api`] (FR-T2). A message is stored first, then delivered through
//! [`Sessions::send`], so it enters the target exactly like a user message, behind a header
//! naming the sender (FR-T3). A target that is mid-turn gets its queued messages at the next turn
//! boundary; an idle or released target is woken by the delivery; undelivered messages survive a
//! restart (FR-T4). Sends are limited per session and per pair within a sliding window (FR-T5),
//! and can be switched off server-wide or per session (FR-T6).
//!
//! Users mention other sessions from the composer ([`mention`], FR-T6), and a leader session
//! runs a team of teammate sessions with a shared task list and mailbox ([`team`], FR-T7).

pub mod api;
pub mod client;
pub mod mention;
pub mod store;
pub mod team;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;

use crate::events::{AgentEvent, SessionStatus};
use crate::session::{Push, Sessions};
use crate::store::SessionRecord;
pub use store::{A2aMessage, A2aStore};

/// Name of the agent-side command (a bin target of this crate).
pub const CLI_NAME: &str = "ember-a2a";
/// Marks the first line of every delivered message.
pub const HEADER_TAG: &str = "[ember a2a]";
/// Marks the first line of every scheduled prompt (FR-A8).
const SCHEDULE_TAG: &str = crate::schedules::HEADER_TAG;

/// Loop protection limits (FR-T5).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub window: Duration,
    /// Messages one session may send within `window`.
    pub per_session: u32,
    /// Messages between one pair of sessions, either direction, within `window`.
    pub per_pair: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            window: Duration::from_secs(600),
            per_session: 20,
            per_pair: 10,
        }
    }
}

#[derive(Debug, Clone)]
pub struct A2aConfig {
    /// Base URL agents use to reach this server, e.g. `http://127.0.0.1:8740`.
    pub base_url: String,
    /// Absolute path of the `ember-a2a` binary, if it is installed. Its directory is put first
    /// on the agents' `PATH`.
    pub cli_path: Option<PathBuf>,
    pub limits: Limits,
    /// Server-wide switch used until a user sets one through the API.
    pub enabled_by_default: bool,
}

impl A2aConfig {
    pub fn new(base_url: impl Into<String>) -> A2aConfig {
        A2aConfig {
            base_url: base_url.into(),
            cli_path: None,
            limits: Limits::default(),
            enabled_by_default: true,
        }
    }

    /// `ember-a2a` next to the running executable, if present.
    pub fn cli_next_to_current_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let cli = exe.parent()?.join(CLI_NAME);
        cli.is_file().then_some(cli)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum A2aError {
    #[error("missing or invalid runtime token")]
    Unauthorized,
    #[error("{0}")]
    Disabled(String),
    #[error("{0}")]
    RateLimited(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    /// Not allowed for this caller (team roles and scope, FR-T7).
    #[error("{0}")]
    Forbidden(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Moves a session to a computer: `(session id, computer id)`. Installed by `main.rs` from
/// `crate::computers::Computers::switch`, so a teammate can be spawned on a chosen computer.
pub type ComputerSetter =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// A session the caller may message.
#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub id: String,
    pub title: String,
    pub project: String,
    pub agent: String,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Entered the target session now.
    Delivered,
    /// The target is mid-turn (or could not be started); it gets the message at its next turn
    /// boundary.
    Queued,
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub id: String,
    pub to: String,
    pub status: Delivery,
}

pub struct A2a {
    sessions: Arc<Sessions>,
    store: A2aStore,
    config: A2aConfig,
    /// Serialises the limit check and insert of sends.
    send_lock: tokio::sync::Mutex<()>,
    /// One delivery at a time per target.
    flush_locks: StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Targets we delivered to whose turn has not ended yet, with their last event seq before
    /// the delivery. Guards against delivering twice before the store shows the turn running.
    awaiting: StdMutex<HashMap<String, i64>>,
    /// Last loop-protection notice per (from, to), so a retrying agent does not flood both
    /// transcripts.
    last_notice: StdMutex<HashMap<(String, String), Instant>>,
    computer_setter: StdMutex<Option<ComputerSetter>>,
}

const NOTICE_EVERY: Duration = Duration::from_secs(60);

fn is_busy(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Running | SessionStatus::WaitingForApproval
    )
}

impl A2a {
    pub fn new(sessions: Arc<Sessions>, store: A2aStore, config: A2aConfig) -> Arc<A2a> {
        Arc::new(A2a {
            sessions,
            store,
            config,
            send_lock: tokio::sync::Mutex::new(()),
            flush_locks: StdMutex::new(HashMap::new()),
            awaiting: StdMutex::new(HashMap::new()),
            last_notice: StdMutex::new(HashMap::new()),
            computer_setter: StdMutex::new(None),
        })
    }

    /// Let `team spawn` put teammates on a chosen computer.
    pub fn set_computer_setter(&self, setter: ComputerSetter) {
        *self.computer_setter.lock().unwrap() = Some(setter);
    }

    /// Hook into the session manager: give every agent process its credentials and
    /// instructions, deliver queued messages at turn boundaries, and deliver whatever was still
    /// queued when the server last stopped. Call once, inside the runtime.
    pub fn install(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.sessions.add_start_hook(Arc::new(move |rec| {
            weak.upgrade()
                .map(|a| a.runtime_env(rec))
                .unwrap_or_default()
        }));
        let weak = Arc::downgrade(self);
        self.sessions.add_instructions_hook(Arc::new(move |rec: &SessionRecord| {
            weak.upgrade().map(|a| a.instructions_for(rec))
        }));
        // Mentions in user messages (FR-T6).
        let weak = Arc::downgrade(self);
        self.sessions.add_user_message_hook(Arc::new(move |session: String, text: String| {
            let weak = weak.clone();
            Box::pin(async move {
                if let Some(a) = weak.upgrade() {
                    a.deliver_mentions(&session, &text).await;
                }
            }) as BoxFuture<'static, ()>
        }));

        // Subscribe before the initial flush so no turn end is missed in between.
        let rx = self.sessions.subscribe();
        tokio::spawn(watch_turns(Arc::downgrade(self), rx));
        let this = self.clone();
        tokio::spawn(async move { this.flush_all().await });
    }

    pub fn store(&self) -> &A2aStore {
        &self.store
    }

    /// Environment for an agent process: a fresh runtime token per process start.
    fn runtime_env(&self, rec: &SessionRecord) -> Vec<(String, String)> {
        let token = match self.store.issue_token(&rec.id) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(session = %rec.id, "issuing A2A token failed: {e:#}");
                return Vec::new();
            }
        };
        let mut env = vec![
            ("EMBER_URL".to_string(), self.config.base_url.clone()),
            ("EMBER_SESSION_ID".to_string(), rec.id.clone()),
            ("EMBER_RUNTIME_TOKEN".to_string(), token),
        ];
        if let Some(cli) = &self.config.cli_path {
            env.push(("EMBER_A2A_BIN".into(), cli.to_string_lossy().into_owned()));
            if let Some(dir) = cli.parent() {
                let mut paths = vec![dir.to_path_buf()];
                if let Some(existing) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&existing));
                }
                if let Ok(joined) = std::env::join_paths(paths) {
                    env.push(("PATH".into(), joined.to_string_lossy().into_owned()));
                }
            }
        }
        env
    }

    /// How the agent uses A2A. Stable across restarts (no per-process values), so it does not
    /// disturb prompt caching on resume.
    pub fn instructions(&self) -> String {
        let path_note = match &self.config.cli_path {
            Some(p) => format!(" (also at `{}`, or `$EMBER_A2A_BIN`)", p.display()),
            None => String::new(),
        };
        format!(
            "# Messaging other agent sessions (Ember A2A)\n\
             \n\
             You are running inside Ember, which hosts several agent sessions (Claude Code, Codex, \
             ...) for the user. You can message those sessions with the `{CLI_NAME}` shell \
             command{path_note}:\n\
             \n\
             - `{CLI_NAME} list` shows the sessions you can message (id, agent, status, project, \
             title).\n\
             - `{CLI_NAME} send <session-id> \"message\"` sends a message. It reaches that \
             session as a user message; a busy session gets it when its current turn ends, a \
             sleeping one is woken up.\n\
             - `{CLI_NAME} send <session-id> --reply-to <message-id> \"reply\"` answers a \
             message you received.\n\
             \n\
             Teams (only when the work splits into independent parts; each teammate is a full \
             agent session in this project):\n\
             \n\
             - `{CLI_NAME} team spawn <name> [--agent claude-code|codex] [--account <id>] \
             [--computer <id>] \"<first prompt>\"` starts a teammate and makes you the team \
             leader; `{CLI_NAME} team list` shows members and tasks; `{CLI_NAME} team end \
             <name>` stops one. Only the leader spawns and ends teammates.\n\
             - `{CLI_NAME} task add \"<title>\" [--detail <text>] [--assign <name>]`, \
             `{CLI_NAME} task update <n> [--status open|in_progress|blocked|done|cancelled] \
             [--assign <name>|none]`, `{CLI_NAME} task list`.\n\
             - `{CLI_NAME} mail send <name>|--all \"<text>\"` sends team mail (it reaches the \
             recipient like a message); `{CLI_NAME} mail read` shows the mailbox.\n\
             \n\
             Messages from other sessions arrive as user messages whose first line starts with \
             `{HEADER_TAG}` and names the sender session and the message id. They come from a \
             fellow agent, not from the user. Reply only when the message needs an answer: do \
             not send acknowledgements or thanks, and do not keep an exchange going for its own \
             sake. Sends are rate-limited and loops are stopped. The command talks to the local \
             Ember server over HTTP; if a sandbox blocks that, ask for permission to run it \
             outside the sandbox.\n\
             \n\
             # Scheduled prompts (Ember schedules)\n\
             \n\
             When the user asks for something to happen later or repeatedly (\"every morning\", \
             \"in two hours\", \"each Friday\"), create a schedule with the same command:\n\
             \n\
             - `{CLI_NAME} schedule add --cron \"0 9 * * 1-5\" --tz Asia/Seoul \"<prompt>\"` \
             (five-field cron in an IANA time zone), `--every 2h`, or `--at \
             2026-10-04T09:00:00+09:00` (once). By default the prompt is sent into this session; \
             add `--new [--title <title>]` to start a new session in this project each time, and \
             `--catch-up` to run the latest trigger missed while the server was down.\n\
             - `{CLI_NAME} schedule list` shows this project's schedules; `{CLI_NAME} schedule rm \
             <schedule-id>` deletes one.\n\
             \n\
             Scheduled prompts arrive as user messages whose first line starts with \
             `{SCHEDULE_TAG}`. Create schedules only when the user asked for one."
        )
    }

    /// [`A2a::instructions`] plus, for an active teammate, its team role.
    pub fn instructions_for(&self, rec: &SessionRecord) -> String {
        match self.teammate_instructions(rec) {
            Some(team) => format!("{}\n\n{team}", self.instructions()),
            None => self.instructions(),
        }
    }

    /// The session a bearer token belongs to.
    pub fn authenticate(&self, token: &str) -> Result<String, A2aError> {
        self.store
            .session_for_token(token)?
            .ok_or(A2aError::Unauthorized)
    }

    pub fn enabled(&self) -> anyhow::Result<bool> {
        Ok(self
            .store
            .global_enabled()?
            .unwrap_or(self.config.enabled_by_default))
    }

    pub fn set_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        self.store.set_global_enabled(enabled)
    }

    pub fn session_enabled(&self, id: &str) -> Result<bool, A2aError> {
        self.session(id)?;
        Ok(self.store.session_enabled(id)?)
    }

    pub fn set_session_enabled(&self, id: &str, enabled: bool) -> Result<(), A2aError> {
        self.session(id)?;
        Ok(self.store.set_session_enabled(id, enabled)?)
    }

    fn session(&self, id: &str) -> Result<SessionRecord, A2aError> {
        self.sessions
            .store()
            .session(id)?
            .ok_or_else(|| A2aError::NotFound(format!("session {id} not found")))
    }

    /// Rejects when A2A is off server-wide or for the calling session.
    fn check_caller(&self, caller: &str) -> Result<(), A2aError> {
        if !self.enabled()? {
            return Err(A2aError::Disabled(
                "agent-to-agent messaging is turned off on this server".into(),
            ));
        }
        if !self.store.session_enabled(caller)? {
            return Err(A2aError::Disabled(format!(
                "agent-to-agent messaging is turned off for this session ({caller})"
            )));
        }
        Ok(())
    }

    /// Why `caller` may not message `target` under the team rule (FR-T7), if it may not: an
    /// active teammate is reachable only from its own team, and reaches only its own team.
    fn team_scope_denial(
        caller: Option<&team::MemberRow>,
        target: Option<&team::MemberRow>,
        target_id: &str,
    ) -> Option<String> {
        let caller_team = caller.map(|m| m.team_id.as_str());
        let target_team = target.map(|m| m.team_id.as_str());
        if let Some(c) = caller.filter(|m| m.is_active_teammate()) {
            if target_team != Some(c.team_id.as_str()) {
                return Some(format!(
                    "you are teammate {} in team {}; you can message only your team (`{CLI_NAME} \
                     team list`)",
                    c.name, c.team_id
                ));
            }
        }
        if let Some(t) = target.filter(|m| m.is_active_teammate()) {
            if caller_team != Some(t.team_id.as_str()) {
                return Some(format!(
                    "session {target_id} is a teammate in team {}; only its team can message it",
                    t.team_id
                ));
            }
        }
        None
    }

    /// Sessions `caller` may message: every other session with A2A on, within the team rule.
    pub fn targets(&self, caller: &str) -> Result<Vec<Target>, A2aError> {
        self.check_caller(caller)?;
        let members = self.sessions.store().active_team_members()?;
        let me = members.get(caller);
        let mut out = Vec::new();
        for s in self.sessions.store().sessions(None)? {
            if s.id == caller || !self.store.session_enabled(&s.id)? {
                continue;
            }
            if Self::team_scope_denial(me, members.get(&s.id), &s.id).is_some() {
                continue;
            }
            out.push(Target {
                id: s.id,
                title: s.title,
                project: s.project,
                agent: s.agent.as_str().to_string(),
                status: s.status,
            });
        }
        Ok(out)
    }

    /// Send `text` from `from` to `to`. The message is stored before delivery is attempted.
    pub async fn send(
        self: &Arc<Self>,
        from: &str,
        to: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<Receipt, A2aError> {
        if text.trim().is_empty() {
            return Err(A2aError::BadRequest("message text is empty".into()));
        }
        if from == to {
            return Err(A2aError::BadRequest(
                "a session cannot message itself".into(),
            ));
        }
        self.check_caller(from)?;
        self.session(to)?;
        if !self.store.session_enabled(to)? {
            return Err(A2aError::Disabled(format!(
                "agent-to-agent messaging is turned off for the target session ({to})"
            )));
        }
        {
            let store = self.sessions.store();
            let (me, them) = (store.team_member(from)?, store.team_member(to)?);
            let active = |m: Option<team::MemberRow>| m.filter(team::MemberRow::active);
            if let Some(why) =
                Self::team_scope_denial(active(me).as_ref(), active(them).as_ref(), to)
            {
                return Err(A2aError::Forbidden(why));
            }
        }
        if let Some(r) = reply_to {
            let orig = self
                .store
                .message(r)?
                .ok_or_else(|| A2aError::BadRequest(format!("unknown message {r} in reply_to")))?;
            if orig.from_session != from && orig.to_session != from {
                return Err(A2aError::BadRequest(format!(
                    "message {r} was not sent to or by this session"
                )));
            }
        }

        let msg = {
            let _guard = self.send_lock.lock().await;
            self.check_limits(from, &[to.to_string()])?;
            self.store.insert_message(from, to, reply_to, text)?
        };
        self.deliver(&msg).await
    }

    /// Try to deliver a stored message now and say whether it went in or stays queued.
    pub(crate) async fn deliver(self: &Arc<Self>, msg: &A2aMessage) -> Result<Receipt, A2aError> {
        self.flush_logged(&msg.to_session).await;
        let delivered = self
            .store
            .message(&msg.id)?
            .is_some_and(|m| m.delivered_at.is_some());
        let status = if delivered {
            Delivery::Delivered
        } else {
            Delivery::Queued
        };
        Ok(Receipt {
            id: msg.id.clone(),
            to: msg.to_session.clone(),
            status,
        })
    }

    /// Loop protection for one message from `from` to each of `tos` (a team broadcast counts
    /// once per recipient). All or nothing.
    fn check_limits(&self, from: &str, tos: &[String]) -> Result<(), A2aError> {
        let l = self.config.limits;
        let since = store::now_ms() - l.window.as_millis() as i64;
        let mins = l.window.as_secs().div_ceil(60);
        let mut refused: Option<(String, String)> = None;
        let sent = self.store.sent_since(from, since)?;
        if sent + tos.len() as u32 > l.per_session {
            refused = tos.first().map(|to| {
                (
                    to.clone(),
                    format!(
                        "session {from} has sent {} agent-to-agent messages in the last {mins} min \
                         (limit)",
                        l.per_session
                    ),
                )
            });
        } else {
            for to in tos {
                if self.store.pair_since(from, to, since)? >= l.per_pair {
                    refused = Some((
                        to.clone(),
                        format!(
                            "sessions {from} and {to} have exchanged {} agent-to-agent messages in \
                             the last {mins} min (limit)",
                            l.per_pair
                        ),
                    ));
                    break;
                }
            }
        }
        let Some((to, reason)) = refused else { return Ok(()) };
        let to = to.as_str();
        let message = format!(
            "A2A message from {from} to {to} was refused by loop protection: {reason}. Wait \
             before sending more, or ask the user."
        );
        self.notify_rejection(from, to, &message);
        Err(A2aError::RateLimited(message))
    }

    /// A visible notice in both sessions (FR-T5), at most once per pair per minute.
    fn notify_rejection(&self, from: &str, to: &str, message: &str) {
        let now = Instant::now();
        {
            let mut last = self.last_notice.lock().unwrap();
            let key = (from.to_string(), to.to_string());
            if last
                .get(&key)
                .is_some_and(|at| now.duration_since(*at) < NOTICE_EVERY)
            {
                return;
            }
            last.insert(key, now);
        }
        for id in [from, to] {
            let event = AgentEvent::Notice {
                message: message.to_string(),
            };
            if let Err(e) = self.sessions.record_event(id, &event) {
                tracing::warn!(session = %id, "recording A2A notice failed: {e:#}");
            }
        }
    }

    fn flush_lock(&self, target: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.flush_locks
            .lock()
            .unwrap()
            .entry(target.to_string())
            .or_default()
            .clone()
    }

    /// Deliver everything queued for `target` if it is between turns. Returns whether a
    /// delivery happened.
    pub async fn flush(self: &Arc<Self>, target: &str) -> anyhow::Result<bool> {
        let lock = self.flush_lock(target);
        let _guard = lock.lock().await;
        if !self.settle_awaiting(target) {
            return Ok(false);
        }
        let Some(rec) = self.sessions.store().session(target)? else {
            return Ok(false);
        };
        if is_busy(rec.status) {
            return Ok(false);
        }
        let pending = self.store.pending_for(target)?;
        if pending.is_empty() {
            return Ok(false);
        }
        let text = self.render(&pending)?;
        // Before sending: the turn could end before `send` returns.
        self.awaiting
            .lock()
            .unwrap()
            .insert(target.to_string(), rec.last_seq);
        if let Err(e) = self.sessions.send(target, &text).await {
            self.awaiting.lock().unwrap().remove(target);
            return Err(e.into());
        }
        let ids: Vec<String> = pending.into_iter().map(|m| m.id).collect();
        self.store.mark_delivered(&ids)?;
        Ok(true)
    }

    /// Whether `target`'s last delivered turn is over (or there is none). Checks the store, so
    /// it does not depend on the turn watcher having seen the turn end yet.
    fn settle_awaiting(&self, target: &str) -> bool {
        let Some(before) = self.awaiting.lock().unwrap().get(target).copied() else {
            return true;
        };
        let ended = self
            .sessions
            .store()
            .events_after(target, before)
            .is_ok_and(|evs| {
                evs.iter()
                    .any(|e| matches!(e.event, AgentEvent::TurnEnded { .. }))
            });
        if ended {
            let mut awaiting = self.awaiting.lock().unwrap();
            if awaiting.get(target) == Some(&before) {
                awaiting.remove(target);
            }
        }
        ended
    }

    async fn flush_logged(self: &Arc<Self>, target: &str) {
        if let Err(e) = self.flush(target).await {
            tracing::warn!(target_session = %target, "A2A delivery failed, kept queued: {e:#}");
        }
    }

    async fn flush_all(self: &Arc<Self>) {
        match self.store.sessions_with_pending() {
            Ok(ids) => {
                for id in ids {
                    self.flush_logged(&id).await;
                }
            }
            Err(e) => tracing::error!("listing queued A2A messages failed: {e:#}"),
        }
    }

    /// The user-message text for a batch of messages to one target (FR-T3).
    fn render(&self, msgs: &[A2aMessage]) -> anyhow::Result<String> {
        let mut parts = Vec::new();
        for m in msgs {
            let sender = match self.sessions.store().session(&m.from_session)? {
                Some(s) => format!(
                    "\"{}\" (project {}, agent {})",
                    s.title,
                    s.project,
                    s.agent.as_str()
                ),
                None => "(deleted session)".into(),
            };
            let mut header = format!(
                "{HEADER_TAG} Message {} from session {} {sender}",
                m.id, m.from_session
            );
            if let Some(r) = &m.reply_to {
                header.push_str(&format!("\nIn reply to message {r}."));
            }
            header.push_str(&format!(
                "\nTo reply: {CLI_NAME} send {} --reply-to {} \"<your reply>\"",
                m.from_session, m.id
            ));
            parts.push(format!("{header}\n\n{}", m.text));
        }
        Ok(parts.join("\n\n---\n\n"))
    }
}

/// Delivers queued messages whenever a turn ends.
async fn watch_turns(a2a: Weak<A2a>, mut rx: tokio::sync::broadcast::Receiver<Push>) {
    loop {
        let push = rx.recv().await;
        let Some(this) = a2a.upgrade() else { return };
        match push {
            Ok(Push::Event { event, .. })
                if matches!(event.event, AgentEvent::TurnEnded { .. }) =>
            {
                let sid = event.session_id;
                {
                    let mut awaiting = this.awaiting.lock().unwrap();
                    if awaiting.get(&sid).is_some_and(|before| event.seq > *before) {
                        awaiting.remove(&sid);
                    }
                }
                tokio::spawn(async move { this.flush_logged(&sid).await });
            }
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => {
                // Turn ends may have been missed; `flush` settles them from the store.
                tokio::spawn(async move { this.flush_all().await });
            }
            Err(RecvError::Closed) => return,
        }
    }
}
