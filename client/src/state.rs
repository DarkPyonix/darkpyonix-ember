//! The client's whole view of the main server, as a pure reducer (FR-L1–FR-L3, FR-L5).
//!
//! [`State::apply`] takes one [`Input`] — something the network said or the user did — and
//! returns the [`Changes`] it caused: which projects/sessions/transcripts a UI must redraw, and
//! which transcripts the sync layer must refetch. No I/O happens here; the connection layer
//! ([`crate::client`]) feeds inputs in and acts on the requested resyncs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::transcript::{Applied, Transcript};
use crate::wire::{AgentEvent, Push, SessionRecord, SessionStatus, StoredEvent};

/// Session status as the launcher shows it (FR-L2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherStatus {
    Idle,
    Running,
    /// Outranks running (the server already resolves this in its stored status).
    WaitingForApproval,
    /// Finished with events the user has not seen; stays until the session is opened.
    FinishedUnread,
    Finished,
    Failed,
    Unknown,
}

/// Reachability of a computer (FR-L3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    #[default]
    Unknown,
    Online,
    Offline,
}

/// A computer listed at the bottom of the launcher (FR-L3). Placeholder: the main server has no
/// computer API yet, so this is only ever filled by [`Input::ComputersLoaded`] (and the cache).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Computer {
    pub id: String,
    pub name: String,
    pub reachability: Reachability,
    /// Projects this computer is assigned to (FR-L4).
    pub projects: Vec<String>,
}

/// Push connection state, shown to the user (PR-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    /// Not started, or stopped.
    Offline,
    Connecting { attempt: u32 },
    Connected,
    /// Lost or failed; retrying after a backoff.
    Reconnecting { attempt: u32, retry_in_ms: u64, error: String },
    /// The server speaks a push version this client does not. Not retried: the user must update
    /// one side.
    Incompatible { server: u32, client: u32 },
}

/// Something that happened, fed to [`State::apply`].
#[derive(Debug, Clone)]
pub enum Input {
    /// `GET /sessions` answered.
    SessionsLoaded(Vec<SessionRecord>),
    /// `GET /sessions/{id}` or `POST /sessions` answered.
    SessionLoaded(SessionRecord),
    Push(Push),
    /// `GET /sessions/{id}/events?after=` answered.
    EventsFetched { session_id: String, events: Vec<StoredEvent> },
    /// The UI opened a conversation view.
    Opened(String),
    /// The UI closed it.
    Closed(String),
    Connection(ConnectionState),
    ComputersLoaded(Vec<Computer>),
}

/// What an input changed. A UI redraws the named parts; the sync layer acts on `resync*`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// The project list or a project's session list/order changed.
    pub projects: bool,
    /// Sessions whose record or launcher status changed.
    pub sessions: BTreeSet<String>,
    /// Sessions whose transcript changed.
    pub transcripts: BTreeSet<String>,
    pub computers: bool,
    pub connection: bool,
    /// Open sessions whose transcript has a gap: fetch `events?after=last_seq`.
    pub resync: BTreeSet<String>,
    /// The server may have dropped messages, or an unknown session appeared: reload the session
    /// list and resync every open session.
    pub resync_all: bool,
    /// Something persisted in the cache changed.
    pub persist: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }

    /// Fold `other` into `self`.
    pub fn merge(&mut self, other: Changes) {
        self.projects |= other.projects;
        self.sessions.extend(other.sessions);
        self.transcripts.extend(other.transcripts);
        self.computers |= other.computers;
        self.connection |= other.connection;
        self.resync.extend(other.resync);
        self.resync_all |= other.resync_all;
        self.persist |= other.persist;
    }
}

/// A project and its sessions, most recently active first.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectView {
    pub name: String,
    pub sessions: Vec<String>,
}

/// One session as the launcher shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionView<'a> {
    pub record: &'a SessionRecord,
    pub status: LauncherStatus,
    pub open: bool,
}

/// What is cached on disk for an offline cold start (FR-L1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheSnapshot {
    pub format: u32,
    /// Whether a session list has ever been loaded; before that, existing history counts as read.
    pub baselined: bool,
    pub sessions: Vec<SessionRecord>,
    /// Highest sequence the user has seen per session.
    pub last_seen: BTreeMap<String, i64>,
    pub computers: Vec<Computer>,
}

pub const CACHE_FORMAT: u32 = 1;

#[derive(Debug, Clone)]
pub struct State {
    sessions: HashMap<String, SessionRecord>,
    transcripts: HashMap<String, Transcript>,
    open: BTreeSet<String>,
    last_seen: HashMap<String, i64>,
    baselined: bool,
    computers: Vec<Computer>,
    connection: ConnectionState,
    revision: u64,
}

impl Default for State {
    fn default() -> State {
        State {
            sessions: HashMap::new(),
            transcripts: HashMap::new(),
            open: BTreeSet::new(),
            last_seen: HashMap::new(),
            baselined: false,
            computers: Vec::new(),
            connection: ConnectionState::Offline,
            revision: 0,
        }
    }
}

impl State {
    pub fn new() -> State {
        State::default()
    }

    /// State as it was cached, before the network answers (FR-L1).
    pub fn from_cache(c: CacheSnapshot) -> State {
        State {
            sessions: c.sessions.into_iter().map(|s| (s.id.clone(), s)).collect(),
            last_seen: c.last_seen.into_iter().collect(),
            baselined: c.baselined,
            computers: c.computers,
            ..State::default()
        }
    }

    pub fn to_cache(&self) -> CacheSnapshot {
        let mut sessions: Vec<_> = self.sessions.values().cloned().collect();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        CacheSnapshot {
            format: CACHE_FORMAT,
            baselined: self.baselined,
            sessions,
            last_seen: self.last_seen.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            computers: self.computers.clone(),
        }
    }

    // ---- reads -------------------------------------------------------------------------------

    /// Increments on every input that changed something; cheap "did anything change" check.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn connection(&self) -> &ConnectionState {
        &self.connection
    }

    /// Projects by name, each with its sessions most recently active first.
    pub fn projects(&self) -> Vec<ProjectView> {
        let mut by_project: BTreeMap<&str, Vec<&SessionRecord>> = BTreeMap::new();
        for s in self.sessions.values() {
            by_project.entry(&s.project).or_default().push(s);
        }
        by_project
            .into_iter()
            .map(|(name, mut list)| {
                list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.id.cmp(&b.id)));
                ProjectView { name: name.to_string(), sessions: list.into_iter().map(|s| s.id.clone()).collect() }
            })
            .collect()
    }

    pub fn session(&self, id: &str) -> Option<SessionView<'_>> {
        let record = self.sessions.get(id)?;
        Some(SessionView { record, status: self.launcher_status(record), open: self.open.contains(id) })
    }

    pub fn transcript(&self, id: &str) -> Option<&Transcript> {
        self.transcripts.get(id)
    }

    pub fn computers(&self) -> &[Computer] {
        &self.computers
    }

    pub fn open_sessions(&self) -> impl Iterator<Item = &String> {
        self.open.iter()
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.open.contains(id)
    }

    /// Highest sequence the user has seen in `id`.
    pub fn last_seen(&self, id: &str) -> i64 {
        self.last_seen.get(id).copied().unwrap_or(0)
    }

    fn launcher_status(&self, s: &SessionRecord) -> LauncherStatus {
        match s.status {
            SessionStatus::Idle => LauncherStatus::Idle,
            SessionStatus::Running => LauncherStatus::Running,
            SessionStatus::WaitingForApproval => LauncherStatus::WaitingForApproval,
            SessionStatus::Finished if s.last_seq > self.last_seen(&s.id) && !self.open.contains(&s.id) => {
                LauncherStatus::FinishedUnread
            }
            SessionStatus::Finished => LauncherStatus::Finished,
            SessionStatus::Failed => LauncherStatus::Failed,
            SessionStatus::Unknown => LauncherStatus::Unknown,
        }
    }

    // ---- the reducer -------------------------------------------------------------------------

    pub fn apply(&mut self, input: Input) -> Changes {
        let mut ch = Changes::default();
        match input {
            Input::SessionsLoaded(list) => {
                for rec in list {
                    self.upsert(rec, &mut ch);
                }
                if !self.baselined {
                    self.baselined = true;
                    ch.persist = true;
                }
            }
            Input::SessionLoaded(rec) => self.upsert(rec, &mut ch),
            Input::Push(Push::SessionCreated { session }) => {
                // Created while we watched: nothing in it has been seen yet.
                if !self.sessions.contains_key(&session.id) {
                    self.last_seen.entry(session.id.clone()).or_insert(0);
                }
                self.upsert(session, &mut ch);
            }
            Input::Push(Push::Event { status, event }) => self.on_event(Some(status), event, &mut ch),
            Input::Push(Push::Lagged { .. }) => ch.resync_all = true,
            Input::EventsFetched { session_id, events } => {
                for ev in events {
                    if ev.session_id == session_id {
                        self.on_event(None, ev, &mut ch);
                    }
                }
            }
            Input::Opened(id) => {
                self.open.insert(id.clone());
                self.transcripts.entry(id.clone()).or_default();
                self.mark_seen(&id, &mut ch);
                ch.sessions.insert(id.clone());
                ch.transcripts.insert(id.clone());
                ch.resync.insert(id);
            }
            Input::Closed(id) => {
                if self.open.remove(&id) {
                    self.mark_seen(&id, &mut ch);
                    ch.sessions.insert(id);
                }
            }
            Input::Connection(c) => {
                if c != self.connection {
                    self.connection = c;
                    ch.connection = true;
                }
            }
            Input::ComputersLoaded(list) => {
                if list != self.computers {
                    self.computers = list;
                    ch.computers = true;
                    ch.persist = true;
                }
            }
        }
        if !ch.is_empty() {
            self.revision += 1;
        }
        ch
    }

    /// Take a session record from the server unless we already hold a newer one (a push can
    /// overtake a list request).
    fn upsert(&mut self, rec: SessionRecord, ch: &mut Changes) {
        let id = rec.id.clone();
        if !self.last_seen.contains_key(&id) {
            // Before the first list load, existing history counts as read; after it, a session
            // we never saw is entirely unread.
            let seen = if self.baselined { 0 } else { rec.last_seq };
            self.last_seen.insert(id.clone(), seen);
        }
        match self.sessions.get(&id) {
            Some(cur) if cur.last_seq > rec.last_seq => return,
            Some(cur) if *cur == rec => return,
            Some(cur) => {
                if cur.project != rec.project || cur.updated_at != rec.updated_at {
                    ch.projects = true;
                }
            }
            None => ch.projects = true,
        }
        self.sessions.insert(id.clone(), rec);
        if self.open.contains(&id) {
            self.mark_seen(&id, ch);
            // The record may be ahead of the transcript (events we have not received).
            if self.transcripts.get(&id).is_some_and(|t| t.last_seq < self.sessions[&id].last_seq) {
                ch.resync.insert(id.clone());
            }
        }
        ch.sessions.insert(id);
        ch.persist = true;
    }

    /// One stored event, from push (`status` known) or a fetch (`status` unknown).
    fn on_event(&mut self, status: Option<SessionStatus>, ev: StoredEvent, ch: &mut Changes) {
        let id = ev.session_id.clone();
        match self.sessions.get_mut(&id) {
            Some(rec) => {
                if ev.seq > rec.last_seq {
                    rec.last_seq = ev.seq;
                    if rec.updated_at != ev.at {
                        rec.updated_at = ev.at;
                        ch.projects = true;
                    }
                    if let Some(st) = status {
                        rec.status = st;
                    }
                    if let AgentEvent::NativeSession { native_id } = &ev.event {
                        rec.native_id = Some(native_id.clone());
                    }
                    ch.sessions.insert(id.clone());
                    ch.persist = true;
                }
            }
            // A session we have not heard of (its creation was missed): reload the list.
            None => ch.resync_all = true,
        }
        if let Some(t) = self.transcripts.get_mut(&id) {
            match t.apply(&ev) {
                Applied::Changed => {
                    ch.transcripts.insert(id.clone());
                }
                Applied::Duplicate => {}
                // Closed sessions are caught up when reopened; only open ones fetch now.
                Applied::Gap { .. } if self.open.contains(&id) => {
                    ch.resync.insert(id.clone());
                }
                Applied::Gap { .. } => {}
            }
        }
        if self.open.contains(&id) {
            self.mark_seen(&id, ch);
        }
    }

    fn mark_seen(&mut self, id: &str, ch: &mut Changes) {
        let Some(rec) = self.sessions.get(id) else { return };
        let seen = self.last_seen.entry(id.to_string()).or_insert(0);
        if *seen < rec.last_seq {
            *seen = rec.last_seq;
            ch.sessions.insert(id.to_string());
            ch.persist = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{ApprovalDecision, TurnOutcome};
    use serde_json::json;

    fn rec(id: &str, project: &str, status: SessionStatus, last_seq: i64) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            project: project.into(),
            agent: "scripted".into(),
            cwd: "/tmp".into(),
            model: None,
            native_id: None,
            status,
            title: "t".into(),
            created_at: 0,
            updated_at: last_seq,
            last_seq,
        }
    }

    fn push(id: &str, seq: i64, status: SessionStatus, event: AgentEvent) -> Input {
        Input::Push(Push::Event {
            status,
            event: StoredEvent { session_id: id.into(), seq, at: seq, event },
        })
    }

    #[test]
    fn status_transitions_follow_push() {
        let mut s = State::new();
        s.apply(Input::SessionsLoaded(vec![]));
        s.apply(Input::Push(Push::SessionCreated { session: rec("a", "p", SessionStatus::Idle, 0) }));
        assert_eq!(s.session("a").unwrap().status, LauncherStatus::Idle);
        let ch = s.apply(push("a", 1, SessionStatus::Running, AgentEvent::UserMessage { text: "hi".into() }));
        assert!(ch.sessions.contains("a"));
        assert_eq!(s.session("a").unwrap().status, LauncherStatus::Running);
        s.apply(push(
            "a",
            2,
            SessionStatus::WaitingForApproval,
            AgentEvent::ApprovalRequested { approval_id: "x".into(), tool: "Bash".into(), input: json!({}) },
        ));
        assert_eq!(s.session("a").unwrap().status, LauncherStatus::WaitingForApproval);
        s.apply(push(
            "a",
            3,
            SessionStatus::Running,
            AgentEvent::ApprovalResolved { approval_id: "x".into(), decision: ApprovalDecision::AllowOnce },
        ));
        s.apply(push("a", 4, SessionStatus::Finished, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
        assert_eq!(s.session("a").unwrap().status, LauncherStatus::FinishedUnread);
        s.apply(push("a", 5, SessionStatus::Running, AgentEvent::UserMessage { text: "again".into() }));
        s.apply(push("a", 6, SessionStatus::Failed, AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }));
        assert_eq!(s.session("a").unwrap().status, LauncherStatus::Failed);
    }

    #[test]
    fn finished_stays_unread_until_opened_and_survives_cache() {
        let mut s = State::new();
        s.apply(Input::SessionsLoaded(vec![]));
        s.apply(Input::Push(Push::SessionCreated { session: rec("a", "p", SessionStatus::Idle, 0) }));
        s.apply(push("a", 1, SessionStatus::Running, AgentEvent::UserMessage { text: "hi".into() }));
        s.apply(push("a", 2, SessionStatus::Finished, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
        assert_eq!(s.session("a").unwrap().status, LauncherStatus::FinishedUnread);

        // Persisted unread survives a restart.
        let mut s2 = State::from_cache(s.to_cache());
        assert_eq!(s2.session("a").unwrap().status, LauncherStatus::FinishedUnread);

        let ch = s2.apply(Input::Opened("a".into()));
        assert!(ch.persist && ch.resync.contains("a"));
        s2.apply(Input::Closed("a".into()));
        assert_eq!(s2.session("a").unwrap().status, LauncherStatus::Finished);
        assert_eq!(s2.last_seen("a"), 2);
        let s3 = State::from_cache(s2.to_cache());
        assert_eq!(s3.session("a").unwrap().status, LauncherStatus::Finished);

        // Events arriving while the session is open are seen as they arrive.
        s2.apply(Input::Opened("a".into()));
        s2.apply(push("a", 3, SessionStatus::Running, AgentEvent::UserMessage { text: "x".into() }));
        s2.apply(push("a", 4, SessionStatus::Finished, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
        s2.apply(Input::Closed("a".into()));
        assert_eq!(s2.session("a").unwrap().status, LauncherStatus::Finished);
    }

    #[test]
    fn first_list_load_counts_history_as_read_later_ones_do_not() {
        let mut s = State::new();
        s.apply(Input::SessionsLoaded(vec![rec("old", "p", SessionStatus::Finished, 9)]));
        assert_eq!(s.session("old").unwrap().status, LauncherStatus::Finished);
        // Appeared while we were away, after the baseline: unread.
        s.apply(Input::SessionsLoaded(vec![rec("new", "p", SessionStatus::Finished, 4)]));
        assert_eq!(s.session("new").unwrap().status, LauncherStatus::FinishedUnread);
    }

    #[test]
    fn projects_group_sessions_most_recent_first() {
        let mut s = State::new();
        let ch = s.apply(Input::SessionsLoaded(vec![
            rec("a", "beta", SessionStatus::Idle, 1),
            rec("b", "alpha", SessionStatus::Idle, 2),
            rec("c", "beta", SessionStatus::Idle, 5),
        ]));
        assert!(ch.projects);
        let p = s.projects();
        assert_eq!(p[0], ProjectView { name: "alpha".into(), sessions: vec!["b".into()] });
        assert_eq!(p[1], ProjectView { name: "beta".into(), sessions: vec!["c".into(), "a".into()] });
        // Activity reorders.
        let ch = s.apply(push("a", 2, SessionStatus::Running, AgentEvent::UserMessage { text: "x".into() }));
        assert!(ch.projects);
        // updated_at comes from the event's `at` (= seq in these fixtures), so "a" is still older.
        s.apply(push("a", 3, SessionStatus::Running, AgentEvent::AssistantDelta { text: "y".into() }));
        s.apply(Input::Push(Push::Event {
            status: SessionStatus::Running,
            event: StoredEvent { session_id: "a".into(), seq: 4, at: 100, event: AgentEvent::Unknown },
        }));
        assert_eq!(s.projects()[1].sessions, vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn stale_list_does_not_overwrite_newer_push() {
        let mut s = State::new();
        s.apply(Input::SessionsLoaded(vec![rec("a", "p", SessionStatus::Idle, 0)]));
        s.apply(push("a", 1, SessionStatus::Running, AgentEvent::UserMessage { text: "hi".into() }));
        let ch = s.apply(Input::SessionsLoaded(vec![rec("a", "p", SessionStatus::Idle, 0)]));
        assert!(ch.sessions.is_empty());
        assert_eq!(s.session("a").unwrap().status, LauncherStatus::Running);
    }

    #[test]
    fn transcript_gap_requests_resync_only_when_open() {
        let mut s = State::new();
        s.apply(Input::SessionsLoaded(vec![rec("a", "p", SessionStatus::Idle, 0)]));
        s.apply(Input::Opened("a".into()));
        let ch = s.apply(push("a", 2, SessionStatus::Running, AgentEvent::AssistantDelta { text: "x".into() }));
        assert!(ch.resync.contains("a"));
        assert!(!ch.transcripts.contains("a"));
        let ch = s.apply(Input::EventsFetched {
            session_id: "a".into(),
            events: vec![
                StoredEvent { session_id: "a".into(), seq: 1, at: 1, event: AgentEvent::UserMessage { text: "hi".into() } },
                StoredEvent { session_id: "a".into(), seq: 2, at: 2, event: AgentEvent::AssistantDelta { text: "x".into() } },
            ],
        });
        assert!(ch.transcripts.contains("a"));
        let t = s.transcript("a").unwrap();
        assert_eq!(t.last_seq, 2);
        assert_eq!(t.items.len(), 2);

        s.apply(Input::Closed("a".into()));
        let ch = s.apply(push("a", 5, SessionStatus::Running, AgentEvent::AssistantDelta { text: "z".into() }));
        assert!(ch.resync.is_empty());
    }

    #[test]
    fn lagged_and_unknown_sessions_request_full_resync() {
        let mut s = State::new();
        assert!(s.apply(Input::Push(Push::Lagged { missed: 4 })).resync_all);
        assert!(s.apply(push("ghost", 1, SessionStatus::Running, AgentEvent::Unknown)).resync_all);
    }

    #[test]
    fn connection_and_computers() {
        let mut s = State::new();
        assert!(s.apply(Input::Connection(ConnectionState::Connecting { attempt: 0 })).connection);
        assert!(!s.apply(Input::Connection(ConnectionState::Connecting { attempt: 0 })).connection);
        let rev = s.revision();
        let pc = Computer { id: "m".into(), name: "mini".into(), reachability: Reachability::Unknown, projects: vec!["p".into()] };
        assert!(s.apply(Input::ComputersLoaded(vec![pc.clone()])).computers);
        assert_eq!(s.revision(), rev + 1);
        assert_eq!(State::from_cache(s.to_cache()).computers(), &[pc]);
    }
}
