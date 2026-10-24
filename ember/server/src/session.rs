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

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use crate::events::{AgentEvent, ApprovalDecision, SessionStatus};
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

pub struct Sessions {
    store: Arc<Store>,
    start_hooks: std::sync::Mutex<Vec<StartHook>>,
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
}

impl Sessions {
    pub fn new(store: Arc<Store>, adapters: Vec<Arc<dyn AgentAdapter>>) -> Arc<Sessions> {
        let (push, _) = broadcast::channel(1024);
        Arc::new(Sessions {
            store,
            start_hooks: std::sync::Mutex::new(Vec::new()),
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
        })
    }

    /// Register a hook that adds environment to every agent process this server starts.
    pub fn add_start_hook(&self, hook: StartHook) {
        self.start_hooks.lock().unwrap().push(hook);
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
        // A fork whose agent forks natively (FR-S5): fork instead of starting fresh. If the
        // agent cannot after all, start fresh and put the copied conversation in front of it.
        let native_fork = if rec.native_id.is_none() { self.store.pending_native_fork(id)? } else { None };
        let run = match native_fork {
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
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let run = LiveRun { run: Arc::new(Mutex::new(run)), events: tx, generation };
        live.insert(id.to_string(), run.clone());

        // Pump: store then push every event, until the agent's channel closes.
        let this = self.clone();
        let sid = id.to_string();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
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
                }
                // Approve-everything mode answers a new request itself (FR-A5). Spawned: the
                // answer goes back through this very channel.
                if let AgentEvent::ApprovalRequested { approval_id, .. } = &event {
                    if this.approve_all(&sid) {
                        let (this, sid, approval_id) = (this.clone(), sid.clone(), approval_id.clone());
                        tokio::spawn(async move {
                            if let Err(e) = this.answer(&sid, &approval_id, ApprovalDecision::AllowOnce).await {
                                tracing::warn!(session = %sid, "auto-approve failed: {e}");
                            }
                        });
                    }
                }
            }
            let mut live = this.live.lock().await;
            if live.get(&sid).is_some_and(|l| l.generation == generation) {
                live.remove(&sid);
            }
        });
        Ok(run)
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
            let mut pending: Vec<String> = Vec::new();
            for e in self.store.events_after(id, 0)? {
                match e.event {
                    AgentEvent::ApprovalRequested { approval_id, .. } => pending.push(approval_id),
                    AgentEvent::ApprovalResolved { approval_id, .. } => pending.retain(|p| *p != approval_id),
                    AgentEvent::TurnEnded { .. } => pending.clear(),
                    _ => {}
                }
            }
            for approval_id in pending {
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

    pub async fn is_live(&self, id: &str) -> bool {
        self.live.lock().await.contains_key(id)
    }
}
