//! The sync engine: keeps a [`State`] in step with one main server (PR-1, FR-L1, FR-S6).
//!
//! - **Push**: one WebSocket, reconnected with exponential backoff. Before each connect the
//!   server is probed (`/health`). Unknown message types and fields are skipped.
//! - **Gap recovery**: after every (re)connect and on `lagged`, the session list is reloaded and
//!   every open session fetches `events?after=<last applied seq>`. Transcripts drop duplicates
//!   and hold early events by sequence number, so nothing is lost or applied twice.
//! - **Leases**: an open session's lease is renewed every `lease_interval` until it is closed.
//! - **Cache**: the launcher snapshot is written (debounced) whenever it changes and read back
//!   by [`Client::new`] before any network request.
//!
//! Needs a tokio runtime; all I/O is async.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{broadcast, mpsc, watch, Notify};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::api::{Api, ApiResult};
use crate::cache;
use crate::push::{self, Backoff, DecodeError};
use crate::state::{Changes, ConnectionState, Input, State};
use crate::wire::{
    ApprovalDecision, DetectedAgent, MentionCandidate, NewSession, Project, Push, SearchHit, SessionPatch,
    SessionRecord, TeamMail, TeamMember, TeamView,
};

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Main server base URL, e.g. `http://127.0.0.1:8740`.
    pub base_url: String,
    /// Where the cold-start snapshot lives; `None` disables caching.
    pub cache_path: Option<PathBuf>,
    /// How often an open session's lease is renewed (server TTL is 90 s).
    pub lease_interval: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// After a connect, resync once more after this delay: the server subscribes a push socket
    /// only after the upgrade completes, so an event stored in between would otherwise be missed
    /// until the next event of that session.
    pub resync_settle: Duration,
    /// Minimum spacing of cache writes.
    pub cache_debounce: Duration,
}

impl ClientConfig {
    pub fn new(base_url: impl Into<String>) -> ClientConfig {
        ClientConfig {
            base_url: base_url.into(),
            cache_path: None,
            lease_interval: Duration::from_secs(30),
            backoff_min: Duration::from_millis(250),
            backoff_max: Duration::from_secs(30),
            resync_settle: Duration::from_millis(250),
            cache_debounce: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushMode {
    Run,
    Paused,
}

#[derive(Debug)]
enum Resync {
    All,
    Session(String),
}

struct Inner {
    api: Api,
    config: ClientConfig,
    state: SyncMutex<State>,
    changes: broadcast::Sender<Arc<Changes>>,
    revision: watch::Sender<u64>,
    resync: mpsc::UnboundedSender<Resync>,
    persist: Notify,
    /// Drop the current push socket and reconnect (a resync fetch failed).
    reconnect: Notify,
    push_mode: watch::Sender<PushMode>,
    leases: SyncMutex<HashMap<String, JoinHandle<()>>>,
    tasks: SyncMutex<Vec<JoinHandle<()>>>,
    resync_rx: SyncMutex<Option<mpsc::UnboundedReceiver<Resync>>>,
}

/// Handle to the sync engine. Cheap to clone; all clones share one state.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Client {
    /// Build a client and load the cached snapshot (FR-L1). No network yet; call
    /// [`Client::start`] to connect.
    pub async fn new(config: ClientConfig) -> ApiResult<Client> {
        let api = Api::new(&config.base_url)?;
        Ok(Self::with_api(config, api).await)
    }

    /// Like [`Client::new`], with a ready [`Api`], e.g. [`Api::over_transport`] to reach the
    /// server over the peer-to-peer transport. `config.base_url` is then unused.
    pub async fn with_api(config: ClientConfig, api: Api) -> Client {
        let state = match &config.cache_path {
            Some(p) => cache::load(p).await.map(State::from_cache).unwrap_or_default(),
            None => State::new(),
        };
        let (changes, _) = broadcast::channel(256);
        let (revision, _) = watch::channel(state.revision());
        let (resync, resync_rx) = mpsc::unbounded_channel();
        let (push_mode, _) = watch::channel(PushMode::Run);
        Client {
            inner: Arc::new(Inner {
                api,
                config,
                state: SyncMutex::new(state),
                changes,
                revision,
                resync,
                persist: Notify::new(),
                reconnect: Notify::new(),
                push_mode,
                leases: SyncMutex::new(HashMap::new()),
                tasks: SyncMutex::new(Vec::new()),
                resync_rx: SyncMutex::new(Some(resync_rx)),
            }),
        }
    }

    /// Start the push connection, the resync worker and the cache writer. Idempotent.
    pub fn start(&self) {
        let Some(rx) = self.inner.resync_rx.lock().unwrap().take() else { return };
        let mut tasks = self.inner.tasks.lock().unwrap();
        tasks.push(tokio::spawn(push_loop(self.inner.clone())));
        tasks.push(tokio::spawn(resync_worker(self.inner.clone(), rx)));
        if self.inner.config.cache_path.is_some() {
            tasks.push(tokio::spawn(cache_writer(self.inner.clone())));
        }
    }

    /// Stop every background task and lease renewal. The state stays readable.
    pub fn stop(&self) {
        for t in self.inner.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        for (_, t) in self.inner.leases.lock().unwrap().drain() {
            t.abort();
        }
        self.inner.apply(Input::Connection(ConnectionState::Offline));
    }

    /// Drop the push connection and stay disconnected (e.g. app backgrounded on mobile).
    pub fn pause_push(&self) {
        self.inner.push_mode.send_replace(PushMode::Paused);
    }

    /// Reconnect after [`Client::pause_push`]; catches up on everything missed meanwhile.
    pub fn resume_push(&self) {
        self.inner.push_mode.send_replace(PushMode::Run);
    }

    /// Drop the current push socket and reconnect right away (e.g. the network changed). The
    /// reconnect catches up on anything missed, like any other.
    pub fn reconnect_push(&self) {
        self.inner.reconnect.notify_one();
    }

    // ---- observing ---------------------------------------------------------------------------

    /// Read the current state. Keep `f` short: it holds the state lock.
    pub fn read<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        f(&self.inner.state.lock().unwrap())
    }

    /// Every non-empty [`Changes`], in order. A receiver that lags should redraw everything.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Changes>> {
        self.inner.changes.subscribe()
    }

    /// The state revision; never lags, for "something changed, re-read" bindings.
    pub fn watch_revision(&self) -> watch::Receiver<u64> {
        self.inner.revision.subscribe()
    }

    pub fn api(&self) -> &Api {
        &self.inner.api
    }

    /// Feed an input directly (e.g. computers from another source, FR-L3).
    pub fn apply(&self, input: Input) -> Changes {
        self.inner.apply(input)
    }

    /// Write the cache now (e.g. before the app is suspended).
    pub async fn flush_cache(&self) -> Result<(), cache::CacheError> {
        self.inner.save_cache().await
    }

    // ---- conversation view -------------------------------------------------------------------

    /// The UI shows `id`: load its transcript, mark it seen, and renew its lease until
    /// [`Client::close_session`] (FR-L5, FR-S6).
    pub fn open_session(&self, id: &str) {
        self.inner.apply(Input::Opened(id.to_string()));
        let inner = self.inner.clone();
        let sid = id.to_string();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(inner.config.lease_interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(e) = inner.api.lease(&sid).await {
                    tracing::debug!(session = %sid, "lease renewal failed: {e}");
                }
            }
        });
        if let Some(old) = self.inner.leases.lock().unwrap().insert(id.to_string(), task) {
            old.abort();
        }
    }

    pub fn close_session(&self, id: &str) {
        if let Some(t) = self.inner.leases.lock().unwrap().remove(id) {
            t.abort();
        }
        self.inner.apply(Input::Closed(id.to_string()));
    }

    // ---- actions -----------------------------------------------------------------------------

    pub async fn create_session(&self, new: &NewSession) -> ApiResult<SessionRecord> {
        let rec = self.inner.api.create_session(new).await?;
        self.inner.apply(Input::SessionLoaded(rec.clone()));
        Ok(rec)
    }

    pub async fn send_message(&self, id: &str, text: &str) -> ApiResult<()> {
        self.inner.api.send_message(id, text).await
    }

    pub async fn answer(&self, id: &str, approval_id: &str, decision: ApprovalDecision) -> ApiResult<()> {
        self.inner.api.answer(id, approval_id, decision).await
    }

    pub async fn interrupt(&self, id: &str) -> ApiResult<()> {
        self.inner.api.interrupt(id).await
    }

    pub async fn agents(&self) -> ApiResult<Vec<DetectedAgent>> {
        self.inner.api.agents().await
    }

    /// Rename, pin or archive a session (FR-L9). The state is updated from the answer at once;
    /// the server's push tells every other client.
    pub async fn patch_session(&self, id: &str, patch: &SessionPatch) -> ApiResult<SessionRecord> {
        let rec = self.inner.api.patch_session(id, patch).await?;
        self.inner.apply(Input::Push(Push::SessionUpdated { session: rec.clone() }));
        Ok(rec)
    }

    /// Reload the project list (with computer assignments) into the state.
    pub async fn load_projects(&self) -> ApiResult<Vec<Project>> {
        let list = self.inner.api.projects().await?;
        self.inner.apply(Input::ProjectsLoaded(list.clone()));
        Ok(list)
    }

    /// Assign a computer to a project (FR-L4).
    pub async fn assign_computer(&self, project: &str, computer_id: &str) -> ApiResult<Project> {
        let p = self.inner.api.assign_computer(project, computer_id).await?;
        self.inner.apply(Input::Push(Push::ProjectUpdated { project: p.clone() }));
        Ok(p)
    }

    /// Unassign a computer from a project (FR-L4).
    pub async fn unassign_computer(&self, project: &str, computer_id: &str) -> ApiResult<Project> {
        let p = self.inner.api.unassign_computer(project, computer_id).await?;
        self.inner.apply(Input::Push(Push::ProjectUpdated { project: p.clone() }));
        Ok(p)
    }

    /// Full-text search on the server across every session's messages (FR-S4).
    pub async fn search(&self, query: &str, limit: Option<usize>) -> ApiResult<Vec<SearchHit>> {
        self.inner.api.search(query, limit).await
    }

    // ---- teams and mentions (FR-T6, FR-T7) ---------------------------------------------------

    /// Load the team `session_id` leads or belongs to into the state; push keeps it current.
    pub async fn load_team(&self, session_id: &str) -> ApiResult<Option<TeamView>> {
        self.inner.fetch_team(session_id).await
    }

    /// A team's mail (not kept in the state).
    pub async fn team_mail(&self, team_id: &str, after: i64, limit: Option<usize>) -> ApiResult<Vec<TeamMail>> {
        self.inner.api.team_mail(team_id, after, limit).await
    }

    /// End a teammate as the user. The server pushes the changed team.
    pub async fn end_teammate(&self, team_id: &str, session_id: &str) -> ApiResult<TeamMember> {
        self.inner.api.end_teammate(team_id, session_id).await
    }

    /// Sessions a message typed in `session_id` may mention.
    pub async fn mention_candidates(&self, session_id: &str, query: &str, limit: Option<usize>) -> ApiResult<Vec<MentionCandidate>> {
        self.inner.api.mention_candidates(session_id, query, limit).await
    }

    /// Reload the session list and resync open sessions now.
    pub fn refresh(&self) {
        let _ = self.inner.resync.send(Resync::All);
    }
}

impl Inner {
    fn apply(&self, input: Input) -> Changes {
        let (ch, rev) = {
            let mut st = self.state.lock().unwrap();
            let ch = st.apply(input);
            (ch, st.revision())
        };
        if ch.is_empty() {
            return ch;
        }
        if ch.resync_all {
            let _ = self.resync.send(Resync::All);
        }
        for id in &ch.resync {
            let _ = self.resync.send(Resync::Session(id.clone()));
        }
        if ch.persist {
            self.persist.notify_one();
        }
        self.revision.send_replace(rev);
        let _ = self.changes.send(Arc::new(ch.clone()));
        ch
    }

    async fn save_cache(&self) -> Result<(), cache::CacheError> {
        let Some(path) = &self.config.cache_path else { return Ok(()) };
        let snap = self.state.lock().unwrap().to_cache();
        cache::save(path, &snap).await
    }

    /// Fetch events after the transcript's last applied seq. Returns how many arrived.
    async fn fetch_session(&self, id: &str) -> ApiResult<usize> {
        let after = {
            let st = self.state.lock().unwrap();
            match st.transcript(id) {
                Some(t) if st.is_open(id) => t.last_seq,
                _ => return Ok(0),
            }
        };
        let events = self.api.events(id, after).await?;
        let n = events.len();
        if n > 0 {
            self.apply(Input::EventsFetched { session_id: id.to_string(), events });
        }
        Ok(n)
    }

    async fn fetch_team(&self, session_id: &str) -> ApiResult<Option<TeamView>> {
        let team = self.api.session_team(session_id).await?;
        self.apply(Input::TeamLoaded { session_id: session_id.to_string(), team: team.clone() });
        Ok(team)
    }

    async fn resync_all(&self) -> ApiResult<()> {
        let list = self.api.sessions(None).await?;
        self.apply(Input::SessionsLoaded(list));
        match self.api.projects().await {
            Ok(projects) => {
                self.apply(Input::ProjectsLoaded(projects));
            }
            // A server older than the projects API (FR-L4): projects come from sessions only.
            Err(e) if e.status() == Some(404) => {}
            Err(e) => return Err(e),
        }
        let open: Vec<String> = self.state.lock().unwrap().open_sessions().cloned().collect();
        for id in open {
            match self.fetch_session(&id).await {
                Ok(_) => {}
                // Deleted or unknown on this server: nothing to catch up.
                Err(e) if e.status() == Some(404) => {}
                Err(e) => return Err(e),
            }
            // Team changes may have been missed too. Best effort: an older server has no teams.
            if let Err(e) = self.fetch_team(&id).await {
                tracing::debug!(session = %id, "team refresh failed: {e}");
            }
        }
        Ok(())
    }
}

/// Serialises catch-up fetches; coalesces bursts of requests.
async fn resync_worker(inner: Arc<Inner>, mut rx: mpsc::UnboundedReceiver<Resync>) {
    while let Some(first) = rx.recv().await {
        let mut all = false;
        let mut ids = BTreeSet::new();
        let mut take = |r: Resync| match r {
            Resync::All => all = true,
            Resync::Session(id) => {
                ids.insert(id);
            }
        };
        take(first);
        while let Ok(r) = rx.try_recv() {
            take(r);
        }
        let result = if all {
            inner.resync_all().await
        } else {
            let mut r = Ok(());
            for id in &ids {
                if let Err(e) = inner.fetch_session(id).await {
                    if e.status() != Some(404) {
                        r = Err(e);
                        break;
                    }
                }
            }
            r
        };
        if let Err(e) = result {
            // Catch-up failed while push may still be up: reconnect, which resyncs everything.
            tracing::warn!("resync failed, reconnecting: {e}");
            inner.reconnect.notify_one();
        }
    }
}

async fn cache_writer(inner: Arc<Inner>) {
    loop {
        inner.persist.notified().await;
        if let Err(e) = inner.save_cache().await {
            tracing::warn!("cache write failed: {e}");
        }
        tokio::time::sleep(inner.config.cache_debounce).await;
    }
}

enum Ended {
    /// Connection lost or failed: retry.
    Lost(String),
    /// Paused by the app.
    Paused,
}

async fn push_loop(inner: Arc<Inner>) {
    let mut backoff = Backoff::new(inner.config.backoff_min, inner.config.backoff_max);
    let mut mode = inner.push_mode.subscribe();
    loop {
        if *mode.borrow_and_update() == PushMode::Paused {
            inner.apply(Input::Connection(ConnectionState::Offline));
            if mode.changed().await.is_err() {
                return;
            }
            continue;
        }
        inner.apply(Input::Connection(ConnectionState::Connecting { attempt: backoff.attempt() }));
        let ended = connect_and_run(&inner, &mut mode, &mut backoff).await;
        match ended {
            Ended::Paused => {}
            Ended::Lost(error) => {
                let delay = backoff.next_delay();
                inner.apply(Input::Connection(ConnectionState::Reconnecting {
                    attempt: backoff.attempt(),
                    retry_in_ms: delay.as_millis() as u64,
                    error,
                }));
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    r = mode.changed() => if r.is_err() { return },
                }
            }
        }
    }
}

async fn connect_and_run(
    inner: &Arc<Inner>,
    mode: &mut watch::Receiver<PushMode>,
    backoff: &mut Backoff,
) -> Ended {
    match inner.api.health().await {
        Ok(_) => {}
        Err(e) => return Ended::Lost(e.to_string()),
    }
    let mut ws = match inner.api.open_push().await {
        Ok(ws) => ws,
        Err(e) => return Ended::Lost(e.to_string()),
    };
    backoff.reset();
    inner.apply(Input::Connection(ConnectionState::Connected));
    // Catch up on everything missed while disconnected, and once more after the server has
    // certainly subscribed this socket.
    let _ = inner.resync.send(Resync::All);
    let settle = {
        let inner = inner.clone();
        tokio::spawn(async move {
            tokio::time::sleep(inner.config.resync_settle).await;
            let _ = inner.resync.send(Resync::All);
        })
    };
    let ended = loop {
        tokio::select! {
            msg = ws.next() => match msg {
                Some(Ok(Message::Text(text))) => match push::decode(&text) {
                    Ok(p) => {
                        inner.apply(Input::Push(p));
                    }
                    Err(DecodeError::Skipped(kind)) => tracing::trace!("ignoring push {kind}"),
                    // An unknown push type is additive: skip it.
                    Err(e) => tracing::warn!("skipping push message: {e}"),
                },
                Some(Ok(Message::Close(_))) | None => break Ended::Lost("push connection closed".into()),
                Some(Ok(_)) => {}
                Some(Err(e)) => break Ended::Lost(e.to_string()),
            },
            r = mode.changed() => {
                if r.is_err() || *mode.borrow() == PushMode::Paused {
                    break Ended::Paused;
                }
            }
            _ = inner.reconnect.notified() => break Ended::Lost("reconnect requested".into()),
        }
    };
    settle.abort();
    let _ = ws.close(None).await;
    ended
}

