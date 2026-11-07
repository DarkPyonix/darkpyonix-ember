//! Session lifecycle on the main server (SPEC FR-S1–FR-S3, FR-S6).
//!
//! A session exists in the store whether or not its agent process is running. The process is
//! started (or natively resumed) on the first message after the server starts or the session went
//! idle, and every event it emits is stored before it is pushed, so clients can come and go
//! without affecting the turn.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::agents::{AgentAdapter, AgentKind, AgentRun, Detected, Probe, StartRequest};
use crate::events::{
    approval_policy, redact_secret_answers, validate_question_answers, AgentEvent, AnswerInvalid, ApprovalDecision,
    Question, QuestionAnswer, SessionStatus, QUESTION_WAITS_NOTICE,
};
use crate::projects::Project;
use crate::schedules::{Schedule, ScheduleRun};
use crate::store::{SessionPatch, SessionRecord, Store, StoredEvent};

/// What the server pushes to every attached client (PR-1).
///
/// Variants are only ever added; a client skips a `type` it does not know.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Push {
    SessionCreated { session: SessionRecord },
    Event { status: SessionStatus, event: StoredEvent },
    /// A session's metadata changed (title, pinned, archived: FR-L9). Carries the whole record;
    /// a client takes the metadata fields from it and keeps its own activity fields
    /// (`last_seq`, `status`, `updated_at`), which events update.
    SessionUpdated { session: SessionRecord },
    /// A project was created or its computer assignment changed (FR-L4).
    ProjectUpdated { project: Project },
    /// A team's members or tasks changed (FR-T7). Carries the whole team.
    TeamUpdated { team: crate::a2a::team::TeamView },
    /// A schedule was created (FR-A8), by a user or by an agent (`ember-a2a schedule add`).
    ScheduleCreated { schedule: Schedule },
    /// A schedule changed: edited, paused, resumed. Carries the whole record.
    ScheduleUpdated { schedule: Schedule },
    /// A schedule was deleted.
    ScheduleDeleted { id: String, project: String },
    /// A schedule run was recorded or changed status (`running`, `completed`, `failed`,
    /// `interrupted`, `skipped`, `missed`). A `missed` run's `error` is the notice text.
    ScheduleRun { run: ScheduleRun },
    /// A session's approve-everything mode was switched (FR-A5).
    ApproveAll { session_id: String, enabled: bool },
    /// A message was appended to a channel's log (FR-T10, FR-L12).
    ChannelMessage { channel_id: String, message: crate::channels::Message },
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session {0} not found")]
    NotFound(String),
    #[error("agent {0} is not available on this server")]
    AgentUnavailable(&'static str),
    /// The requested account cannot be used, or the router found none available (FR-U2/U3).
    #[error("{0}")]
    Account(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Why a question could not be answered or skipped (FR-A5).
#[derive(Debug, thiserror::Error)]
pub enum QuestionError {
    #[error("session {0} not found")]
    NoSession(String),
    #[error("question {0} is not pending")]
    NotPending(String),
    #[error("{0}")]
    Invalid(#[from] AnswerInvalid),
    #[error("session {0} has no running agent")]
    NoAgent(String),
    /// The body names both or neither of `answers` and `decline: true`.
    #[error("send either answers or decline: true, not both")]
    BadBody,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// One answer in the body of a question route.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswerItem {
    pub question_id: String,
    pub values: Vec<String>,
}

/// The body of both question routes (`POST /api/sessions/{id}/questions/{question_id}/answer`
/// and `POST /api/channels/{id}/questions/{question_id}/answer`, FR-A5, FR-T11):
/// `{"answers":[{"question_id":"q0","values":["blue"]}]}` or `{"decline":true}`, exactly one.
/// An unknown field is refused (422). One type and one checker, so the routes cannot drift.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswerBody {
    pub answers: Option<Vec<QuestionAnswerItem>>,
    pub decline: Option<bool>,
}

/// What a valid [`QuestionAnswerBody`] asks for.
pub enum QuestionChoice {
    Answer(Vec<QuestionAnswer>),
    Decline,
}

impl QuestionAnswerBody {
    /// The one choice the body makes, or [`QuestionError::BadBody`] (400) when it makes both or none.
    pub fn choice(self) -> Result<QuestionChoice, QuestionError> {
        match (self.answers, self.decline) {
            (Some(items), None) => Ok(QuestionChoice::Answer(
                items
                    .into_iter()
                    .map(|a| QuestionAnswer { question_id: a.question_id, values: a.values })
                    .collect(),
            )),
            (None, Some(true)) => Ok(QuestionChoice::Decline),
            _ => Err(QuestionError::BadBody),
        }
    }
}

pub struct NewSession {
    pub project: String,
    pub agent: AgentKind,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub title: String,
}

/// A running agent plus the sender into its event pump. Session-originated events (user
/// messages, approval answers) go through the same channel as the agent's own, so the stored order
/// is the order things happened.
#[derive(Clone)]
struct LiveRun {
    run: Arc<Mutex<Box<dyn AgentRun>>>,
    events: mpsc::Sender<AgentEvent>,
    /// Distinguishes successive processes of one session, so an exiting pump only removes its
    /// own entry and never a newer process started after a release.
    generation: u64,
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Contributes environment variables to an agent process when its session starts.
pub type StartHook = Arc<dyn Fn(&SessionRecord) -> Vec<(String, String)> + Send + Sync>;

/// Called with a session id when the session ends for good (its team member is ended). Releases
/// the session's per-session resources; a later start calls the start hooks again.
pub type EndHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Observes every event after it is stored (e.g. rate-limit detection, FR-U3). Runs inline on
/// the session's event pump, so it must be quick.
pub type EventHook = Arc<dyn Fn(&StoredEvent) + Send + Sync>;

/// An account chosen for a new session: `(account id, reason)`.
pub type AccountChoice = (String, String);

/// Picks the account for a new session (FR-U2, FR-U3). Given the agent and the account the
/// client asked for (`None` = let the router choose), returns the account to use, `Ok(None)` to
/// use the server's own agent login, or `Err(reason)` when no account may be used.
pub type AccountRouter =
    Arc<dyn Fn(AgentKind, Option<&str>) -> Result<Option<AccountChoice>, String> + Send + Sync>;

/// Contributes system-level instructions to an agent process when its session starts (e.g. how
/// to use the A2A tool). Contributions are joined with blank lines.
pub type InstructionsHook = Arc<dyn Fn(&SessionRecord) -> Option<String> + Send + Sync>;

/// Adjusts the whole start request of an agent process (computer switching: remote executor,
/// shell shim; see `crate::computers`). Runs after the start and instructions hooks; an error fails
/// the start rather than silently running the agent somewhere else.
pub type StartConfigHook =
    Arc<dyn Fn(&SessionRecord, &mut StartRequest) -> anyhow::Result<()> + Send + Sync>;

/// Prepares what an agent process needs before it starts and may take time doing it (the
/// project mount for a Claude Code session on another computer; see `crate::computers::mount`).
/// Awaited before the start-config hooks; an error fails the start.
pub type PrepareHook = Arc<
    dyn Fn(SessionRecord) -> futures::future::BoxFuture<'static, anyhow::Result<()>> + Send + Sync,
>;

/// Called with a session id before a message is delivered to its agent. A returned note is
/// recorded as [`AgentEvent::SystemNotice`] and put in front of that one message (FR-S7 v0).
/// The hook owns the note's lifetime: return it once.
pub type MessageHook = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Called with `(session id, text)` after a message **the user** typed was delivered (not
/// A2A deliveries): mentions of other sessions (FR-T6). Awaited by [`Sessions::send_from_user`].
pub type UserMessageHook =
    Arc<dyn Fn(String, String) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// The `harness_settings` key of the per-harness memory limit (FR-L20).
pub const MEM_LIMIT_KEY: &str = "mem_limit_mb";
/// Smallest accepted memory limit in MiB. An UNMEASURED assumption (FR-L20): nobody measured the
/// least memory a real agent needs.
pub const MEM_LIMIT_MIN_MB: u64 = 256;
/// Largest accepted memory limit in MiB (256 GiB): stops typos and keeps the shim's byte
/// arithmetic far from overflow.
pub const MEM_LIMIT_MAX_MB: u64 = 262_144;
/// The fixed refusal text for an out-of-range limit.
pub const MEM_LIMIT_RANGE_MESSAGE: &str = "mem_limit_mb must be 0 (clear) or between 256 and 262144 MiB";

/// The synthetic `harness_settings.harness` row used for global new-member defaults (FR-L20, #481, #670).
pub const DEFAULTS_HARNESS_ID: &str = "_defaults";
pub const DEFAULT_AGENT_KEY: &str = "agent";
pub const DEFAULT_MODEL_KEY: &str = "model";
pub const DEFAULT_EFFORT_KEY: &str = "effort";
pub const DEFAULT_MODE_KEY: &str = "mode";
pub const EFFORT_LEVELS: &[&str] = &["Low", "Medium", "High"];

/// Global new-member defaults (`새 팀원 기본값`, FR-L20, #481, #670).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefaults {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
}

pub struct Sessions {
    store: Arc<Store>,
    start_hooks: std::sync::Mutex<Vec<StartHook>>,
    end_hooks: std::sync::Mutex<Vec<EndHook>>,
    event_hooks: std::sync::Mutex<Vec<EventHook>>,
    account_router: std::sync::Mutex<Option<AccountRouter>>,
    instructions_hooks: std::sync::Mutex<Vec<InstructionsHook>>,
    start_config_hooks: std::sync::Mutex<Vec<StartConfigHook>>,
    prepare_hooks: std::sync::Mutex<Vec<PrepareHook>>,
    message_hooks: std::sync::Mutex<Vec<MessageHook>>,
    user_message_hooks: std::sync::Mutex<Vec<UserMessageHook>>,
    adapters: HashMap<AgentKind, Arc<dyn AgentAdapter>>,
    live: Mutex<HashMap<String, LiveRun>>,
    push: broadcast::Sender<Push>,
    /// Last time each session recorded an event or received a message (FR-S6).
    last_active: std::sync::Mutex<HashMap<String, Instant>>,
    /// Sessions a client is looking at, and until when (FR-S6). Renewed by the client.
    leases: std::sync::Mutex<HashMap<String, Instant>>,
    /// Sessions in approve-everything mode (FR-A5). Opt-in per session, kept in memory: a server
    /// restart returns every session to asking.
    approve_all: std::sync::Mutex<HashSet<String>>,
    /// Questions answered or skipped through this server: `(session, question)`. Stops a second
    /// answer to the same question while the first one's event is still on its way to the store.
    questions_taken: std::sync::Mutex<HashSet<(String, String)>>,
    /// Memory limit in MiB per agent kind, from the settings (FR-L20, FR-S8). Applies to agents
    /// started afterwards; a running shim keeps the limit it started with.
    mem_limits: std::sync::Mutex<HashMap<AgentKind, u64>>,
}

impl Sessions {
    pub fn new(store: Arc<Store>, adapters: Vec<Arc<dyn AgentAdapter>>) -> Arc<Sessions> {
        let (push, _) = broadcast::channel(1024);
        Arc::new(Sessions {
            store,
            start_hooks: std::sync::Mutex::new(Vec::new()),
            end_hooks: std::sync::Mutex::new(Vec::new()),
            event_hooks: std::sync::Mutex::new(Vec::new()),
            account_router: std::sync::Mutex::new(None),
            instructions_hooks: std::sync::Mutex::new(Vec::new()),
            start_config_hooks: std::sync::Mutex::new(Vec::new()),
            prepare_hooks: std::sync::Mutex::new(Vec::new()),
            message_hooks: std::sync::Mutex::new(Vec::new()),
            user_message_hooks: std::sync::Mutex::new(Vec::new()),
            adapters: adapters.into_iter().map(|a| (a.kind(), a)).collect(),
            live: Mutex::new(HashMap::new()),
            push,
            last_active: std::sync::Mutex::new(HashMap::new()),
            leases: std::sync::Mutex::new(HashMap::new()),
            approve_all: std::sync::Mutex::new(HashSet::new()),
            questions_taken: std::sync::Mutex::new(HashSet::new()),
            mem_limits: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Set (`Some(mb)`) or clear (`None`, `Some(0)`) the memory limit for agents of `agent`
    /// started from now on (FR-L20, FR-S8). Cleared falls back to `EMBER_SESSION_MEM_LIMIT_MB`.
    pub fn set_mem_limit_mb(&self, agent: AgentKind, limit: Option<u64>) {
        let mut m = self.mem_limits.lock().unwrap();
        match limit.filter(|mb| *mb > 0) {
            Some(mb) => {
                m.insert(agent, mb);
            }
            None => {
                m.remove(&agent);
            }
        }
    }

    /// Load every stored `mem_limit_mb` into memory (FR-L20, FR-S8). Call once at startup, after
    /// the store is open. A value that is not a `u64`, is outside
    /// [`MEM_LIMIT_MIN_MB`]..=[`MEM_LIMIT_MAX_MB`], or belongs to a harness that is no longer a
    /// known agent is ignored with a warning that names the harness only.
    pub fn load_settings(&self) {
        let rows = match self.store.harness_settings_with_key(MEM_LIMIT_KEY) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("agent settings could not be read: {e:#}");
                return;
            }
        };
        for (harness, value) in rows {
            let Some(agent) = AgentKind::parse(&harness) else {
                tracing::warn!(harness = %harness, "stored agent memory limit ignored: unknown harness");
                continue;
            };
            match value.parse::<u64>() {
                Ok(mb) if (MEM_LIMIT_MIN_MB..=MEM_LIMIT_MAX_MB).contains(&mb) => {
                    self.set_mem_limit_mb(agent, Some(mb));
                }
                _ => tracing::warn!(harness = %harness, "stored agent memory limit ignored: invalid value"),
            }
        }
    }

    /// Set (`Some(mb)`) or clear (`None`, `Some(0)`) the limit for `agent`, store it and apply it
    /// to agents started from now on (FR-L20, FR-S8). The range is checked here; an out-of-range
    /// value is an error and nothing changes. Store first, memory second, so a failed write never
    /// leaves a limit that a restart would lose.
    pub fn set_mem_limit_persisted(&self, agent: AgentKind, limit: Option<u64>) -> anyhow::Result<()> {
        match limit.filter(|mb| *mb > 0) {
            None => {
                self.store.clear_harness_setting(agent.as_str(), MEM_LIMIT_KEY)?;
                self.set_mem_limit_mb(agent, None);
            }
            Some(mb) => {
                anyhow::ensure!(
                    (MEM_LIMIT_MIN_MB..=MEM_LIMIT_MAX_MB).contains(&mb),
                    "{MEM_LIMIT_RANGE_MESSAGE}"
                );
                self.store.set_harness_setting(agent.as_str(), MEM_LIMIT_KEY, &mb.to_string())?;
                self.set_mem_limit_mb(agent, Some(mb));
            }
        }
        Ok(())
    }

    /// The memory limit in MiB for the next agent of `agent`, if one was set.
    pub fn mem_limit_mb(&self, agent: AgentKind) -> Option<u64> {
        self.mem_limits.lock().unwrap().get(&agent).copied()
    }

    /// Read the stored new-member defaults (`새 팀원 기본값`, FR-L20, #481, #670). Any invalid row in
    /// the database is ignored rather than returned.
    pub fn agent_defaults(&self) -> anyhow::Result<AgentDefaults> {
        let agent = self
            .store
            .harness_setting(DEFAULTS_HARNESS_ID, DEFAULT_AGENT_KEY)?
            .and_then(|a| match AgentKind::parse(&a) {
                Some(k) if k != AgentKind::Scripted => Some(k.as_str().to_string()),
                _ => None,
            });
        let model = self
            .store
            .harness_setting(DEFAULTS_HARNESS_ID, DEFAULT_MODEL_KEY)?
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty() && m.chars().count() <= 128 && !m.chars().any(char::is_control));
        let effort = self
            .store
            .harness_setting(DEFAULTS_HARNESS_ID, DEFAULT_EFFORT_KEY)?
            .filter(|e| EFFORT_LEVELS.contains(&e.as_str()));
        let mode = self
            .store
            .harness_setting(DEFAULTS_HARNESS_ID, DEFAULT_MODE_KEY)?
            .filter(|m| crate::projects::PERMISSION_MODES.contains(&m.as_str()));
        Ok(AgentDefaults { agent, model, effort, mode })
    }

    /// Store or clear a single new-member default key (`agent`, `model`, `effort`, `mode`).
    pub fn set_agent_default_key(&self, key: &str, value: Option<&str>) -> anyhow::Result<()> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) => self.store.set_harness_setting(DEFAULTS_HARNESS_ID, key, v)?,
            None => self.store.clear_harness_setting(DEFAULTS_HARNESS_ID, key)?,
        }
        Ok(())
    }

    /// Register a hook that adds environment to every agent process this server starts.
    pub fn add_start_hook(&self, hook: StartHook) {
        self.start_hooks.lock().unwrap().push(hook);
    }

    /// Register a hook that runs when a session ends (see [`EndHook`]).
    pub fn add_end_hook(&self, hook: EndHook) {
        self.end_hooks.lock().unwrap().push(hook);
    }

    /// Run the end hooks for `id`. Call once the session has ended.
    pub fn session_ended(&self, id: &str) {
        self.release_questions(id, None);
        let hooks: Vec<EndHook> = self.end_hooks.lock().unwrap().clone();
        for hook in hooks {
            hook(id);
        }
    }

    /// Register a hook that sees every stored event.
    pub fn add_event_hook(&self, hook: EventHook) {
        self.event_hooks.lock().unwrap().push(hook);
    }

    /// Install the account router used by [`Sessions::create_with_account`].
    pub fn set_account_router(&self, router: AccountRouter) {
        *self.account_router.lock().unwrap() = Some(router);
    }

    /// Register a hook that adds instructions to every agent process this server starts.
    pub fn add_instructions_hook(&self, hook: InstructionsHook) {
        self.instructions_hooks.lock().unwrap().push(hook);
    }

    /// Register a hook that may change any part of the start request.
    pub fn add_start_config_hook(&self, hook: StartConfigHook) {
        self.start_config_hooks.lock().unwrap().push(hook);
    }

    /// Register an async hook awaited before every agent process start.
    pub fn add_prepare_hook(&self, hook: PrepareHook) {
        self.prepare_hooks.lock().unwrap().push(hook);
    }

    /// Register a hook that may put a one-time note in front of the next message.
    pub fn add_message_hook(&self, hook: MessageHook) {
        self.message_hooks.lock().unwrap().push(hook);
    }

    /// Register a hook that sees every message the user sends (after delivery).
    pub fn add_user_message_hook(&self, hook: UserMessageHook) {
        self.user_message_hooks.lock().unwrap().push(hook);
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Push> {
        self.push.subscribe()
    }

    /// Push something that did not come from a session's event pump (project changes, …).
    pub fn publish(&self, push: Push) {
        let _ = self.push.send(push);
    }

    /// Change a session's title, pin or archive mark and push the result (FR-L9).
    pub fn update_meta(&self, id: &str, patch: &SessionPatch) -> Result<SessionRecord, SessionError> {
        let rec = self.store.update_session_meta(id, patch)?.ok_or_else(|| SessionError::NotFound(id.into()))?;
        self.publish(Push::SessionUpdated { session: rec.clone() });
        Ok(rec)
    }

    pub async fn detect_agents(&self) -> Vec<Detected> {
        let mut out = Vec::new();
        for adapter in self.adapters.values() {
            if adapter.kind() != AgentKind::Scripted {
                out.push(adapter.detect().await);
            }
        }
        out.sort_by_key(|d| d.kind.as_str());
        out
    }

    /// [`Sessions::detect_agents`] with where each CLI was found and when (FR-A6.1). Runs every
    /// probe now; nothing is cached.
    pub async fn probe_agents(&self) -> Vec<Probe> {
        let mut out = Vec::new();
        for adapter in self.adapters.values() {
            if adapter.kind() == AgentKind::Scripted {
                continue;
            }
            let d = adapter.detect().await;
            let path = adapter
                .binary()
                .and_then(|b| crate::agents::resolve_executable(&b))
                .map(|p| p.to_string_lossy().into_owned());
            let detected_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|t| t.as_millis() as i64)
                .unwrap_or(0);
            out.push(Probe { kind: d.kind, installed: d.installed, version: d.version, path, detected_at });
        }
        out.sort_by_key(|d| d.kind.as_str());
        out
    }

    pub fn create(&self, new: NewSession) -> Result<SessionRecord, SessionError> {
        self.create_with_account(new, None)
    }

    /// Create a session under `account`, or under the router's choice when `None` (FR-U2). The
    /// account is fixed for the session's lifetime.
    pub fn create_with_account(
        &self,
        new: NewSession,
        account: Option<&str>,
    ) -> Result<SessionRecord, SessionError> {
        if !self.adapters.contains_key(&new.agent) {
            return Err(SessionError::AgentUnavailable(new.agent.as_str()));
        }
        let router = self.account_router.lock().unwrap().clone();
        let choice = match (router, account) {
            (Some(route), requested) => route(new.agent, requested).map_err(SessionError::Account)?,
            (None, Some(id)) => {
                return Err(SessionError::Account(format!("account {id}: accounts are not enabled")))
            }
            (None, None) => None,
        };
        let rec = self.store.create_session_with_account(
            &new.project,
            new.agent,
            &new.cwd.to_string_lossy(),
            new.model.as_deref(),
            &new.title,
            choice.as_ref().map(|(id, why)| (id.as_str(), why.as_str())),
        )?;
        let _ = self.push.send(Push::SessionCreated { session: rec.clone() });
        // The session may have created its project (FR-L1); clients listing projects learn it.
        if let Ok(Some(project)) = self.store.project(&rec.project) {
            let _ = self.push.send(Push::ProjectUpdated { project });
        }
        Ok(rec)
    }

    /// Record and push an event that did not come from the agent (e.g. an A2A notice).
    pub fn record_event(&self, session_id: &str, event: &AgentEvent) -> anyhow::Result<()> {
        self.record(session_id, event)
    }

    /// Record and push one event.
    fn record(&self, session_id: &str, event: &AgentEvent) -> anyhow::Result<()> {
        let (stored, status) = self.store.append(session_id, event)?;
        // The question is answered or withdrawn: the double-answer guard has no use for its key.
        match event {
            AgentEvent::QuestionAnswered { question_id, .. } => self.release_questions(session_id, Some(question_id.as_str())),
            AgentEvent::PromptWithdrawn { prompt_id } => self.release_questions(session_id, Some(prompt_id.as_str())),
            AgentEvent::TurnEnded { .. } => self.release_questions(session_id, None),
            _ => {}
        }
        self.touch(session_id);
        let hooks = self.event_hooks.lock().unwrap().clone();
        for hook in hooks {
            hook(&stored);
        }
        let _ = self.push.send(Push::Event { status, event: stored });
        Ok(())
    }

    /// The running agent for `id`, starting or natively resuming it if needed.
    async fn ensure_live(self: &Arc<Self>, id: &str) -> Result<LiveRun, SessionError> {
        let mut live = self.live.lock().await;
        // A dead shim cannot accept another message. Discard its adapter so the next start
        // removes stale shim files and resumes from the stored native session id.
        if crate::agents::proc::shim_died(id) {
            live.remove(id);
        }
        if let Some(run) = live.get(id) {
            return Ok(run.clone());
        }
        let rec = self.store.session(id)?.ok_or_else(|| SessionError::NotFound(id.into()))?;
        let adapter = self
            .adapters
            .get(&rec.agent)
            .ok_or(SessionError::AgentUnavailable(rec.agent.as_str()))?
            .clone();
        let env: Vec<(String, String)> =
            self.start_hooks.lock().unwrap().iter().flat_map(|h| h(&rec)).collect();
        let instructions: Vec<String> =
            self.instructions_hooks.lock().unwrap().iter().filter_map(|h| h(&rec)).collect();
        let mut env = env;
        // Lets the shared spawn helper run the agent under its per-session shim (FR-S8).
        env.push((crate::agents::proc::SESSION_ENV.to_string(), id.to_string()));
        let mut req = StartRequest {
            cwd: PathBuf::from(&rec.cwd),
            resume_native_id: rec.native_id.clone(),
            model: rec.model.clone(),
            env,
            instructions: (!instructions.is_empty()).then(|| instructions.join("\n\n")),
            remote: None,
            mcp_servers: Vec::new(),
            computer: None,
            fork_from: None,
            mem_limit_mb: self.mem_limit_mb(rec.agent),
            allowed_tools: Vec::new(),
        };
        let prepare_hooks: Vec<PrepareHook> = self.prepare_hooks.lock().unwrap().clone();
        for hook in &prepare_hooks {
            hook(rec.clone()).await?;
        }
        let config_hooks: Vec<StartConfigHook> = self.start_config_hooks.lock().unwrap().clone();
        for hook in &config_hooks {
            hook(&rec, &mut req)?;
        }
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        // The shim output of this session is acknowledged only once the pump below has stored
        // the events it produced (FR-S8).
        let gate = crate::agents::proc::require_commit(id);
        // A fork whose agent forks natively (FR-S5): fork instead of starting fresh. If the
        // agent cannot after all, start fresh and put the copied conversation in front of it.
        let native_fork = if rec.native_id.is_none() { self.store.pending_native_fork(id)? } else { None };
        let mut run = match native_fork {
            Some(from) => {
                let mut forked = req.clone();
                forked.fork_from = Some(from);
                match adapter.start(forked, tx.clone()).await {
                    Ok(run) => {
                        self.store.settle_native_fork(id, true)?;
                        run
                    }
                    Err(e) => {
                        tracing::warn!(session = %id, "native fork failed, copying the conversation: {e:#}");
                        self.store.settle_native_fork(id, false)?;
                        let _ = self.record(
                            id,
                            &AgentEvent::Notice {
                                message: format!("The agent could not fork its session ({e:#}); the conversation was copied instead."),
                            },
                        );
                        adapter.start(req, tx.clone()).await?
                    }
                }
            }
            None => adapter.start(req, tx.clone()).await?,
        };
        // A rejoined agent may wait on a prompt the previous server stored (FR-S8).
        let pending = self.pending_approvals(id)?;
        if !pending.is_empty() {
            run.restore_pending_approvals(pending).await;
        }
        let questions = self.pending_questions(id)?;
        if !questions.is_empty() {
            run.restore_pending_questions(questions).await;
        }
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let run = LiveRun { run: Arc::new(Mutex::new(run)), events: tx, generation };
        live.insert(id.to_string(), run.clone());

        // Pump: store then push every event, until the agent's channel closes.
        let this = self.clone();
        let sid = id.to_string();
        tokio::spawn(async move {
            loop {
                // Everything sent so far is stored when nothing waits in the channel and no
                // event is being handled: that is when the agent's output may be acknowledged.
                gate.commit_if_idle(|| rx.is_empty());
                let event = tokio::select! {
                    ev = rx.recv() => match ev {
                        Some(ev) => ev,
                        None => break,
                    },
                    // The agent's reader moved on, with the channel already drained.
                    _ = gate.notified() => continue,
                };
                // A resumed agent re-announces the native id we already have; skip the duplicate.
                if let AgentEvent::NativeSession { native_id } = &event {
                    if this.store.session(&sid).ok().flatten().and_then(|s| s.native_id).as_ref()
                        == Some(native_id)
                    {
                        continue;
                    }
                }
                if let Err(e) = this.record(&sid, &event) {
                    tracing::error!(session = %sid, "failed to store event: {e:#}");
                    gate.store_failed();
                }
                // Approve-everything mode answers a new request itself (FR-A5). Spawned: the
                // answer goes back through this very channel.
                if let AgentEvent::ApprovalRequested { approval_id, tool, needs_person, .. } = &event {
                    if this.approve_all(&sid) {
                        let policy = approval_policy(tool, *needs_person);
                        if policy.auto_in_approve_all {
                            if let Some(message) = policy.auto_notice {
                                let note = AgentEvent::Notice { message: message.into() };
                                if let Err(e) = this.record(&sid, &note) {
                                    tracing::warn!(session = %sid, "failed to store notice: {e:#}");
                                }
                            }
                            let (this, sid, approval_id) = (this.clone(), sid.clone(), approval_id.clone());
                            tokio::spawn(async move {
                                if let Err(e) = this.answer(&sid, &approval_id, ApprovalDecision::AllowOnce).await {
                                    tracing::warn!(session = %sid, "auto-approve failed: {e}");
                                }
                            });
                        } else {
                            // A question is for a person; no automatic mode answers it (FR-A5).
                            let note = AgentEvent::Notice { message: QUESTION_WAITS_NOTICE.into() };
                            if let Err(e) = this.record(&sid, &note) {
                                tracing::warn!(session = %sid, "failed to store notice: {e:#}");
                            }
                        }
                    }
                }
            }
            // The channel is closed and drained: all of it is stored.
            gate.commit_ready();
            crate::agents::proc::release_commit_of(&sid, &gate);
            let mut live = this.live.lock().await;
            if live.get(&sid).is_some_and(|l| l.generation == generation) {
                live.remove(&sid);
            }
        });
        Ok(run)
    }

    /// Prompts of `id` that were asked and not answered or withdrawn by a turn end: `(approval id,
    /// tool, input, needs_person)` (FR-S8). `needs_person` is the stored flag of the request, so a
    /// rejoined adapter keeps the safety net for a tool the CLI marked as needing a person.
    fn pending_approvals(&self, id: &str) -> anyhow::Result<Vec<(String, String, serde_json::Value, bool)>> {
        let events: Vec<AgentEvent> = self.store.events_after(id, 0)?.into_iter().map(|e| e.event).collect();
        Ok(pending_approvals_of(events))
    }

    /// Questions of `id` that were asked and not answered, skipped or withdrawn by a turn end:
    /// `(question id, questions)`, oldest first (FR-A5).
    pub fn pending_questions(&self, id: &str) -> anyhow::Result<Vec<(String, Vec<Question>)>> {
        let mut pending: Vec<(String, Vec<Question>)> = Vec::new();
        for e in self.store.events_after(id, 0)? {
            match e.event {
                AgentEvent::QuestionAsked { question_id, questions } => pending.push((question_id, questions)),
                AgentEvent::QuestionAnswered { question_id, .. } | AgentEvent::PromptWithdrawn { prompt_id: question_id } => {
                    pending.retain(|(p, _)| *p != question_id)
                }
                AgentEvent::TurnEnded { .. } => pending.clear(),
                _ => {}
            }
        }
        Ok(pending)
    }

    fn touch(&self, id: &str) {
        self.last_active.lock().unwrap().insert(id.to_string(), Instant::now());
    }

    /// A client is viewing `id`: keep its agent alive for `ttl` (FR-S6).
    pub fn lease(&self, id: &str, ttl: Duration) -> Result<(), SessionError> {
        self.store.session(id)?.ok_or_else(|| SessionError::NotFound(id.into()))?;
        self.leases.lock().unwrap().insert(id.to_string(), Instant::now() + ttl);
        Ok(())
    }

    /// Release agents that have been quiet for `idle`, are not mid-turn and are not leased
    /// (FR-S6). Their next message resumes them natively. Returns the released session ids.
    pub async fn reap_idle(self: &Arc<Self>, idle: Duration) -> Vec<String> {
        let now = Instant::now();
        let candidates: Vec<String> = self.live.lock().await.keys().cloned().collect();
        let mut released = Vec::new();
        for id in candidates {
            let leased = self.leases.lock().unwrap().get(&id).is_some_and(|until| *until > now);
            let quiet = self
                .last_active
                .lock()
                .unwrap()
                .get(&id)
                .is_none_or(|at| now.duration_since(*at) >= idle);
            let busy = matches!(
                self.store.session(&id).ok().flatten().map(|s| s.status),
                Some(SessionStatus::Running | SessionStatus::WaitingForApproval)
            );
            if leased || !quiet || busy {
                continue;
            }
            if let Err(e) = self.release(&id).await {
                tracing::warn!(session = %id, "idle release failed: {e:#}");
                continue;
            }
            released.push(id);
        }
        self.leases.lock().unwrap().retain(|_, until| *until > now);
        released
    }

    /// FR-S5: fork `id` after its completed turn `turn_seq` (the last one when `None`).
    pub fn fork(
        &self,
        id: &str,
        turn_seq: Option<i64>,
        title: Option<&str>,
    ) -> Result<SessionRecord, crate::fork::ForkError> {
        let parent = self.store.session(id)?;
        let rec = self.store.fork_session(id, turn_seq, title)?;
        // The agent's own fork first, the conversation copy as the fallback (FR-S5, #78).
        if let (Some(parent), Some(origin)) = (parent, self.store.fork_origin(&rec.id)?) {
            let mode = self.adapters.get(&parent.agent).map(|a| a.native_fork());
            let events = self.store.events_after(id, 0)?;
            let turns = crate::fork::completed_turns(&events);
            if let (Some(mode), Some(ordinal)) = (mode, turns.iter().position(|s| *s == origin.turn_seq)) {
                let trailing = events
                    .iter()
                    .any(|e| e.seq > origin.turn_seq && matches!(e.event, AgentEvent::UserMessage { .. }));
                if let Some(from) = crate::fork::plan_native_fork(
                    mode,
                    parent.native_id.as_deref(),
                    ordinal + 1,
                    turns.len(),
                    trailing,
                ) {
                    self.store.set_native_fork(&rec.id, &from)?;
                }
            }
        }
        let _ = self.push.send(Push::SessionCreated { session: rec.clone() });
        Ok(rec)
    }

    pub async fn send(self: &Arc<Self>, id: &str, text: &str) -> Result<(), SessionError> {
        self.touch(id);
        let live = self.ensure_live(id).await?;
        let hooks: Vec<MessageHook> = self.message_hooks.lock().unwrap().clone();
        // A fork's copied conversation goes first, once (FR-S5).
        let mut notes: Vec<String> = self.store.take_fork_seed(id)?.into_iter().collect();
        notes.extend(hooks.iter().filter_map(|h| h(id)));
        for note in &notes {
            live.events
                .send(AgentEvent::SystemNotice { text: note.clone() })
                .await
                .map_err(|_| anyhow::anyhow!("agent for session {id} has exited"))?;
        }
        live.events
            .send(AgentEvent::UserMessage { text: text.to_string() })
            .await
            .map_err(|_| anyhow::anyhow!("agent for session {id} has exited"))?;
        // The agent sees the notes first, then the user's words; the store keeps them apart.
        let agent_text = if notes.is_empty() {
            text.to_string()
        } else {
            format!("{}\n\n{text}", notes.join("\n\n"))
        };
        live.run.lock().await.send(&agent_text).await?;
        Ok(())
    }

    /// A message the user typed: [`Sessions::send`], then the user-message hooks (mentions,
    /// FR-T6). The HTTP API's `POST /sessions/{id}/messages` comes here.
    pub async fn send_from_user(self: &Arc<Self>, id: &str, text: &str) -> Result<(), SessionError> {
        self.send(id, text).await?;
        let hooks: Vec<UserMessageHook> = self.user_message_hooks.lock().unwrap().clone();
        for hook in hooks {
            hook(id.to_string(), text.to_string()).await;
        }
        Ok(())
    }

    pub async fn answer(
        self: &Arc<Self>,
        id: &str,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), SessionError> {
        let live = self.live.lock().await.get(id).cloned().ok_or_else(|| {
            SessionError::Other(anyhow::anyhow!("session {id} has no running agent"))
        })?;
        live.events
            .send(AgentEvent::ApprovalResolved { approval_id: approval_id.to_string(), decision })
            .await
            .map_err(|_| anyhow::anyhow!("agent for session {id} has exited"))?;
        live.run.lock().await.answer(approval_id, decision).await?;
        Ok(())
    }

    /// Answer a pending question (FR-A5). Checked against the stored question, recorded as
    /// [`AgentEvent::QuestionAnswered`] (values of a `secret` question as `***`), then sent to
    /// the agent with the real values.
    pub async fn answer_question(
        self: &Arc<Self>,
        id: &str,
        question_id: &str,
        answers: Vec<QuestionAnswer>,
    ) -> Result<(), QuestionError> {
        self.resolve_question(id, question_id, Some(answers)).await
    }

    /// [`Sessions::answer_question`] or [`Sessions::decline_question`] as the body of a question
    /// route chooses; `"answered"` or `"declined"` is the `status` the routes answer with. The body
    /// is checked before the session is looked at.
    pub async fn resolve_question_body(
        self: &Arc<Self>,
        id: &str,
        question_id: &str,
        body: QuestionAnswerBody,
    ) -> Result<&'static str, QuestionError> {
        match body.choice()? {
            QuestionChoice::Answer(answers) => {
                self.answer_question(id, question_id, answers).await?;
                Ok("answered")
            }
            QuestionChoice::Decline => {
                self.decline_question(id, question_id).await?;
                Ok("declined")
            }
        }
    }

    /// Skip a pending question: the agent is told that the person did not answer (FR-A5). An
    /// explicit choice, recorded as [`AgentEvent::QuestionAnswered`] with no answers.
    pub async fn decline_question(self: &Arc<Self>, id: &str, question_id: &str) -> Result<(), QuestionError> {
        self.resolve_question(id, question_id, None).await
    }

    async fn resolve_question(
        self: &Arc<Self>,
        id: &str,
        question_id: &str,
        answers: Option<Vec<QuestionAnswer>>,
    ) -> Result<(), QuestionError> {
        self.store.session(id)?.ok_or_else(|| QuestionError::NoSession(id.into()))?;
        let questions = self
            .pending_questions(id)?
            .into_iter()
            .find(|(q, _)| q == question_id)
            .map(|(_, questions)| questions)
            .ok_or_else(|| QuestionError::NotPending(question_id.into()))?;
        if let Some(answers) = &answers {
            validate_question_answers(&questions, answers)?;
        }
        let live = self
            .live
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| QuestionError::NoAgent(id.into()))?;
        // (a) Take the key: a second answer to the same question is refused while this one is in
        // flight. (b) The agent is asked first. (c) Only an answer it accepted is recorded.
        // (d) Any failure releases the key, so a retry or a decline can follow; the key is also
        // released when the question is recorded as answered or withdrawn (see `release_questions`).
        let key = (id.to_string(), question_id.to_string());
        if !self.questions_taken.lock().unwrap().insert(key.clone()) {
            return Err(QuestionError::NotPending(question_id.into()));
        }
        let stored = answers.as_deref().map(|a| redact_secret_answers(&questions, a)).unwrap_or_default();
        let sent = {
            let mut run = live.run.lock().await;
            match &answers {
                Some(answers) => run.answer_question(question_id, answers).await,
                None => run.decline_question(question_id).await,
            }
        };
        if let Err(e) = sent {
            self.questions_taken.lock().unwrap().remove(&key);
            return Err(e.into());
        }
        if live
            .events
            .send(AgentEvent::QuestionAnswered { question_id: question_id.to_string(), answers: stored })
            .await
            .is_err()
        {
            self.questions_taken.lock().unwrap().remove(&key);
            return Err(QuestionError::Other(anyhow::anyhow!("agent for session {id} has exited")));
        }
        Ok(())
    }

    /// Release the double-answer keys of `session_id`: all of them when `question_id` is `None`,
    /// else that question's.
    fn release_questions(&self, session_id: &str, question_id: Option<&str>) {
        self.questions_taken
            .lock()
            .unwrap()
            .retain(|(s, q)| s != session_id || question_id.is_some_and(|only| only != q));
    }

    /// Number of questions held by the double-answer guard (tests: the set must not grow).
    #[doc(hidden)]
    pub fn questions_taken_len(&self) -> usize {
        self.questions_taken.lock().unwrap().len()
    }

    /// Whether `id` is in approve-everything mode (FR-A5).
    pub fn approve_all(&self, id: &str) -> bool {
        self.approve_all.lock().unwrap().contains(id)
    }

    /// Switch approve-everything mode for `id` (FR-A5). Switching it on also allows every approval
    /// the running turn is waiting on; later requests are allowed as they arrive (see the event
    /// pump). It works for every agent kind because it only answers through [`Sessions::answer`].
    pub async fn set_approve_all(self: &Arc<Self>, id: &str, enabled: bool) -> Result<(), SessionError> {
        self.store.session(id)?.ok_or_else(|| SessionError::NotFound(id.into()))?;
        let changed = {
            let mut set = self.approve_all.lock().unwrap();
            if enabled { set.insert(id.to_string()) } else { set.remove(id) }
        };
        if !changed {
            return Ok(());
        }
        let _ = self.push.send(Push::ApproveAll { session_id: id.to_string(), enabled });
        let message = if enabled {
            "Approve everything is on: tool requests in this session are allowed without asking."
        } else {
            "Approve everything is off: tool requests ask again."
        };
        self.record(id, &AgentEvent::Notice { message: message.into() })?;
        if enabled {
            let mut pending: Vec<(String, Option<&'static str>)> = Vec::new();
            for e in self.store.events_after(id, 0)? {
                match e.event {
                    AgentEvent::ApprovalRequested { approval_id, tool, needs_person, .. } => {
                        // A question is never answered by the mode (FR-A5, one policy table).
                        let policy = approval_policy(&tool, needs_person);
                        if policy.auto_in_approve_all {
                            pending.push((approval_id, policy.auto_notice));
                        }
                    }
                    AgentEvent::ApprovalResolved { approval_id, .. }
                    | AgentEvent::PromptWithdrawn { prompt_id: approval_id } => {
                        pending.retain(|(p, _)| *p != approval_id)
                    }
                    AgentEvent::TurnEnded { .. } => pending.clear(),
                    _ => {}
                }
            }
            for (approval_id, notice) in pending {
                if let Some(message) = notice {
                    self.record(id, &AgentEvent::Notice { message: message.into() })?;
                }
                if let Err(e) = self.answer(id, &approval_id, ApprovalDecision::AllowOnce).await {
                    tracing::warn!(session = %id, "approve-all of pending {approval_id} failed: {e}");
                }
            }
        }
        Ok(())
    }

    pub async fn interrupt(self: &Arc<Self>, id: &str) -> Result<(), SessionError> {
        if let Some(live) = self.live.lock().await.get(id).cloned() {
            live.run.lock().await.interrupt().await?;
        }
        Ok(())
    }

    /// Stop an agent process (idle release, FR-S6). Its native state stays for resume.
    pub async fn release(self: &Arc<Self>, id: &str) -> Result<(), SessionError> {
        if let Some(live) = self.live.lock().await.remove(id) {
            live.run.lock().await.shutdown().await?;
        }
        Ok(())
    }

    /// Server startup (FR-S8): rejoin the sessions whose agent outlived the previous server.
    /// Agents that can be rejoined as they run are attached from their stored output offset, so
    /// the rest of an interrupted turn arrives; for the others the old agent is stopped and the
    /// session resumes natively on its next message. Returns the rejoined session ids.
    pub async fn rejoin_shims(self: &Arc<Self>, rt: &crate::agents::proc::ShimRuntime) -> Vec<String> {
        let ids = match crate::agents::proc::live_shim_sessions(rt).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("could not scan session shims: {e:#}");
                return Vec::new();
            }
        };
        // All sessions at once: an update with many running sessions must not attach them one
        // after the other.
        let results = futures::future::join_all(ids.into_iter().map(|id| self.rejoin_one(rt, id))).await;
        results.into_iter().flatten().collect()
    }

    /// [`Sessions::rejoin_shims`] for one session: `Some(id)` when its agent was rejoined.
    async fn rejoin_one(self: &Arc<Self>, rt: &crate::agents::proc::ShimRuntime, id: String) -> Option<String> {
        let rec = self.store.session(&id).ok().flatten()?;
        let adapter = self.adapters.get(&rec.agent)?.clone();
        if adapter.rejoins_shim() {
            return match self.ensure_live(&id).await {
                Ok(_) => Some(id),
                Err(e) => {
                    tracing::warn!(session = %id, "could not rejoin its agent: {e}");
                    None
                }
            };
        }
        // Not rejoinable: a turn that was running is read back from the surviving agent, which
        // is then stopped; the session resumes natively on its next message.
        if adapter.drains_shim()
            && matches!(rec.status, SessionStatus::Running | SessionStatus::WaitingForApproval)
        {
            if let Err(e) = self.drain_turn(rt, &id, adapter.as_ref()).await {
                tracing::warn!(session = %id, "could not drain the surviving turn: {e:#}");
            }
        }
        None
    }

    /// FR-S8: store what a surviving agent wrote while no server was attached, then stop it.
    async fn drain_turn(
        self: &Arc<Self>,
        rt: &crate::agents::proc::ShimRuntime,
        id: &str,
        adapter: &dyn AgentAdapter,
    ) -> anyhow::Result<()> {
        let max = Duration::from_secs(
            std::env::var("EMBER_SHIM_DRAIN_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(600),
        );
        let lines = crate::agents::proc::drain_surviving_lines(rt, id, max, |l| adapter.drain_turn_end(l)).await?;
        for event in adapter.translate_drained(&lines) {
            self.record(id, &event)?;
        }
        // A turn that did not finish cannot continue: end it, so the session is not stuck running.
        let status = self.store.session(id)?.map(|s| s.status);
        if matches!(status, Some(SessionStatus::Running | SessionStatus::WaitingForApproval)) {
            self.record(
                id,
                &AgentEvent::Error { message: "the agent was stopped while the server restarted".into() },
            )?;
            self.record(id, &AgentEvent::TurnEnded { outcome: crate::events::TurnOutcome::Failed })?;
        }
        Ok(())
    }

    pub async fn is_live(&self, id: &str) -> bool {
        self.live.lock().await.contains_key(id)
    }
}

/// The approvals still pending in `events` (oldest first): asked, not resolved, and not withdrawn
/// by a turn end. `needs_person` is the stored flag, so it survives a server restart (FR-S8, FR-A5).
fn pending_approvals_of(events: Vec<AgentEvent>) -> Vec<(String, String, serde_json::Value, bool)> {
    let mut pending: Vec<(String, String, serde_json::Value, bool)> = Vec::new();
    for event in events {
        match event {
            AgentEvent::ApprovalRequested { approval_id, tool, input, needs_person } => {
                pending.push((approval_id, tool, input, needs_person))
            }
            AgentEvent::ApprovalResolved { approval_id, .. } | AgentEvent::PromptWithdrawn { prompt_id: approval_id } => {
                pending.retain(|(p, _, _, _)| *p != approval_id)
            }
            AgentEvent::TurnEnded { .. } => pending.clear(),
            _ => {}
        }
    }
    pending
}

#[cfg(test)]
mod pending_tests {
    use super::*;
    use crate::events::TurnOutcome;

    fn asked(id: &str, tool: &str, needs_person: bool) -> AgentEvent {
        AgentEvent::ApprovalRequested {
            approval_id: id.into(),
            tool: tool.into(),
            input: serde_json::json!({ "k": id }),
            needs_person,
        }
    }

    /// Review finding of #675: the rebuilt pending list used to drop the `needs_person` flag, so
    /// after a rejoin an unknown flagged tool lost its safety net and could be remembered.
    #[test]
    fn fr_a5_pending_approvals_keep_the_stored_needs_person_flag() {
        let pending = pending_approvals_of(vec![
            asked("a1", "SomeNewTool", true),
            asked("a2", "Bash", false),
            AgentEvent::ApprovalResolved { approval_id: "a2".into(), decision: ApprovalDecision::AllowOnce },
        ]);
        assert_eq!(pending.len(), 1);
        assert_eq!((pending[0].0.as_str(), pending[0].1.as_str(), pending[0].3), ("a1", "SomeNewTool", true));
        // Both flags survive side by side, and a turn end withdraws everything.
        let pending = pending_approvals_of(vec![asked("a1", "X", true), asked("a2", "Y", false)]);
        assert_eq!(pending.iter().map(|p| p.3).collect::<Vec<_>>(), vec![true, false]);
        let pending = pending_approvals_of(vec![
            asked("a1", "X", true),
            AgentEvent::TurnEnded { outcome: TurnOutcome::Completed },
            asked("a3", "Z", true),
        ]);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, "a3");
    }

    /// A row stored before the flag existed has no `needs_person` and reads as false.
    #[test]
    fn fr_a5_a_stored_row_without_the_flag_restores_as_false() {
        let old: AgentEvent = serde_json::from_str(
            r#"{"kind":"approval_requested","approval_id":"o1","tool":"Bash","input":{}}"#,
        )
        .unwrap();
        let pending = pending_approvals_of(vec![old]);
        assert_eq!(pending.len(), 1);
        assert!(!pending[0].3);
    }
}
