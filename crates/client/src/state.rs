//! The client's whole view of the main server, as a pure reducer (FR-L1–FR-L3, FR-L5).
//!
//! [`State::apply`] takes one [`Input`] (something the network said or the user did) and
//! returns the [`Changes`] it caused: which projects/sessions/transcripts a UI must redraw, and
//! which transcripts the sync layer must refetch. No I/O happens here; the connection layer
//! ([`crate::client`]) feeds inputs in and acts on the requested resyncs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::transcript::{Applied, Transcript};
use crate::wire::{AgentEvent, Project, Push, SessionRecord, SessionStatus, StoredEvent, TeamView};

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

/// A computer listed at the bottom of the launcher (FR-L3), filled by [`Input::ComputersLoaded`]
/// (and the cache). Its `projects` are derived from the server's project assignments (FR-L4)
/// once those have been loaded; before that they are kept as given.
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
    /// `GET /projects` answered (FR-L4).
    ProjectsLoaded(Vec<Project>),
    /// `GET /sessions/{id}/team` answered (FR-T7): the team `session_id` leads or belongs to,
    /// or `None`.
    TeamLoaded { session_id: String, team: Option<TeamView> },
}

/// What an input changed. A UI redraws the named parts; the sync layer acts on `resync*`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// The project list, a project's session list/order, or a session's pin/archive mark
    /// changed.
    pub projects: bool,
    /// Sessions whose record or launcher status changed.
    pub sessions: BTreeSet<String>,
    /// Sessions whose transcript changed.
    pub transcripts: BTreeSet<String>,
    pub computers: bool,
    pub connection: bool,
    /// Teams (by id) whose members or tasks changed, or that a session joined or left.
    pub teams: BTreeSet<String>,
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
        self.teams.extend(other.teams);
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
    /// Projects with their computer assignments (FR-L4); empty from an older cache.
    #[serde(default)]
    pub projects: Vec<Project>,
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
    /// Projects as the server lists them (FR-L1, FR-L4), by name.
    project_records: BTreeMap<String, Project>,
    /// Whether `project_records` came from the server (or a cache of it); until then computer
    /// assignments are not derived from it.
    projects_known: bool,
    /// Teams by id (FR-T7). Not cached: loaded when a conversation opens and kept by push.
    teams: BTreeMap<String, TeamView>,
    /// Which team each member session is in (leader and teammates, ended ones included).
    team_of: HashMap<String, String>,
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
            project_records: BTreeMap::new(),
            projects_known: false,
            teams: BTreeMap::new(),
            team_of: HashMap::new(),
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
            projects_known: !c.projects.is_empty(),
            project_records: c.projects.into_iter().map(|p| (p.name.clone(), p)).collect(),
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
            projects: self.project_records.values().cloned().collect(),
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

    /// Projects by name, each with its sessions most recently active first. Includes projects
    /// the server lists that have no sessions yet.
    pub fn projects(&self) -> Vec<ProjectView> {
        let mut by_project: BTreeMap<&str, Vec<&SessionRecord>> = BTreeMap::new();
        for name in self.project_records.keys() {
            by_project.entry(name.as_str()).or_default();
        }
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

    /// A project as the server lists it, with its assigned computers (FR-L4).
    pub fn project(&self, name: &str) -> Option<&Project> {
        self.project_records.get(name)
    }

    /// Computer ids assigned to `project` (empty when none or unknown).
    pub fn project_computers(&self, project: &str) -> &[String] {
        self.project_records.get(project).map(|p| p.computers.as_slice()).unwrap_or(&[])
    }

    /// The team `session_id` leads or belongs to, if known.
    pub fn team_for_session(&self, session_id: &str) -> Option<&TeamView> {
        self.teams.get(self.team_of.get(session_id)?)
    }

    pub fn team(&self, team_id: &str) -> Option<&TeamView> {
        self.teams.get(team_id)
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
            Input::Push(Push::SessionUpdated { session }) => match self.sessions.get_mut(&session.id) {
                Some(cur) => {
                    if cur.take_meta(&session) {
                        // Pin and archive change list order and membership.
                        ch.projects = true;
                        ch.sessions.insert(session.id.clone());
                        ch.persist = true;
                    }
                }
                // Missed its creation: reload the list.
                None => ch.resync_all = true,
            },
            Input::Push(Push::ProjectUpdated { project }) => {
                self.projects_known = true;
                if self.project_records.get(&project.name) != Some(&project) {
                    self.project_records.insert(project.name.clone(), project);
                    ch.projects = true;
                    ch.persist = true;
                    ch.computers |= self.derive_computer_projects();
                }
            }
            Input::ProjectsLoaded(list) => {
                let fresh: BTreeMap<String, Project> = list.into_iter().map(|p| (p.name.clone(), p)).collect();
                self.projects_known = true;
                if fresh != self.project_records {
                    self.project_records = fresh;
                    ch.projects = true;
                    ch.persist = true;
                }
                ch.computers |= self.derive_computer_projects();
                ch.persist |= ch.computers;
            }
            Input::Push(Push::TeamUpdated { team }) => self.put_team(team, &mut ch),
            Input::TeamLoaded { session_id, team: Some(team) } => {
                if team.member(&session_id).is_none() {
                    // An answer for a session the team no longer lists: only the team counts.
                    self.unlink_session(&session_id, &mut ch);
                }
                self.put_team(team, &mut ch);
            }
            Input::TeamLoaded { session_id, team: None } => self.unlink_session(&session_id, &mut ch),
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
                    self.derive_computer_projects();
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

    /// Take a whole team (push or fetch) and re-point its members at it.
    fn put_team(&mut self, team: TeamView, ch: &mut Changes) {
        if self.teams.get(&team.id) == Some(&team) {
            return;
        }
        let id = team.id.clone();
        // Members the new view no longer lists leave the index.
        self.team_of.retain(|sid, tid| *tid != id || team.member(sid).is_some());
        for m in &team.members {
            if let Some(old) = self.team_of.insert(m.session_id.clone(), id.clone()) {
                if old != id {
                    ch.teams.insert(old);
                }
            }
        }
        self.teams.insert(id.clone(), team);
        ch.teams.insert(id);
    }

    fn unlink_session(&mut self, session_id: &str, ch: &mut Changes) {
        if let Some(old) = self.team_of.remove(session_id) {
            ch.teams.insert(old);
        }
    }

    /// Set each computer's `projects` from the project assignments, once those are known.
    /// Returns whether any computer changed.
    fn derive_computer_projects(&mut self) -> bool {
        if !self.projects_known {
            return false;
        }
        let mut changed = false;
        for c in &mut self.computers {
            let projects: Vec<String> = self
                .project_records
                .values()
                .filter(|p| p.computers.contains(&c.id))
                .map(|p| p.name.clone())
                .collect();
            if c.projects != projects {
                c.projects = projects;
                changed = true;
            }
        }
        changed
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
            account_id: None,
            account_reason: None,
            pinned: false,
            archived: false,
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

    #[test]
    fn session_updated_push_changes_metadata_only() {
        let mut s = State::new();
        s.apply(Input::SessionsLoaded(vec![rec("a", "p", SessionStatus::Idle, 0)]));
        s.apply(push("a", 3, SessionStatus::Running, AgentEvent::UserMessage { text: "hi".into() }));

        // The pushed record was read before the last event: its activity fields are stale.
        let mut pushed = rec("a", "p", SessionStatus::Idle, 1);
        pushed.title = "Renamed".into();
        pushed.pinned = true;
        let ch = s.apply(Input::Push(Push::SessionUpdated { session: pushed.clone() }));
        assert!(ch.sessions.contains("a") && ch.projects && ch.persist);
        let v = s.session("a").unwrap();
        assert_eq!((v.record.title.as_str(), v.record.pinned, v.record.archived), ("Renamed", true, false));
        assert_eq!((v.record.last_seq, v.status), (3, LauncherStatus::Running), "activity is kept");

        // Same metadata again: nothing to redraw.
        assert!(s.apply(Input::Push(Push::SessionUpdated { session: pushed.clone() })).is_empty());

        pushed.archived = true;
        s.apply(Input::Push(Push::SessionUpdated { session: pushed }));
        assert!(s.session("a").unwrap().record.archived);
        // Survives the cache.
        assert!(State::from_cache(s.to_cache()).session("a").unwrap().record.archived);

        // An update for a session we never saw: reload the list.
        assert!(s.apply(Input::Push(Push::SessionUpdated { session: rec("ghost", "p", SessionStatus::Idle, 0) })).resync_all);
    }

    #[test]
    fn project_updates_drive_computer_assignments() {
        let mut s = State::new();
        let pc = |id: &str, projects: &[&str]| Computer {
            id: id.into(),
            name: id.into(),
            reachability: Reachability::Unknown,
            projects: projects.iter().map(|p| p.to_string()).collect(),
        };
        // Before projects are known, a computer's projects are kept as given.
        s.apply(Input::ComputersLoaded(vec![pc("local", &["cached"]), pc("studio", &[])]));
        assert_eq!(s.computers()[0].projects, vec!["cached".to_string()]);

        let proj = |name: &str, computers: &[&str]| Project {
            name: name.into(),
            created_at: 0,
            computers: computers.iter().map(|c| c.to_string()).collect(),
        };
        let ch = s.apply(Input::ProjectsLoaded(vec![proj("alpha", &["studio"]), proj("empty", &[])]));
        assert!(ch.projects && ch.computers);
        assert!(s.computers()[0].projects.is_empty());
        assert_eq!(s.computers()[1].projects, vec!["alpha".to_string()]);
        // A project with no sessions is still listed.
        assert_eq!(s.projects().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["alpha", "empty"]);

        // Pushed assignment (from another client) updates in place.
        let ch = s.apply(Input::Push(Push::ProjectUpdated { project: proj("empty", &["local", "studio"]) }));
        assert!(ch.projects && ch.computers && ch.persist);
        assert_eq!(s.project_computers("empty"), ["local".to_string(), "studio".to_string()]);
        assert_eq!(s.computers()[0].projects, vec!["empty".to_string()]);
        assert_eq!(s.computers()[1].projects, vec!["alpha".to_string(), "empty".to_string()]);
        // Repeating it changes nothing.
        assert!(s.apply(Input::Push(Push::ProjectUpdated { project: proj("empty", &["local", "studio"]) })).is_empty());

        // Fresh computer lists keep the derived assignment.
        s.apply(Input::ComputersLoaded(vec![pc("local", &[]), pc("studio", &[]), pc("new", &[])]));
        assert_eq!(s.computers()[1].projects, vec!["alpha".to_string(), "empty".to_string()]);

        // Projects and assignments survive the cache.
        let c = State::from_cache(s.to_cache());
        assert_eq!(c.project_computers("alpha"), ["studio".to_string()]);
        assert_eq!(c.computers()[0].projects, vec!["empty".to_string()]);
    }

    fn member(sid: &str, name: &str, role: crate::wire::TeamRole, ended: Option<i64>) -> crate::wire::TeamMember {
        crate::wire::TeamMember {
            session_id: sid.into(),
            name: name.into(),
            role,
            joined_at: 0,
            ended_at: ended,
            title: name.into(),
            agent: "scripted".into(),
            status: Some(SessionStatus::Idle),
        }
    }

    fn team(id: &str, members: Vec<crate::wire::TeamMember>, tasks: Vec<crate::wire::TeamTask>) -> TeamView {
        TeamView { id: id.into(), project: "p".into(), leader: members[0].session_id.clone(), created_at: 0, members, tasks }
    }

    fn task(n: i64, status: crate::wire::TaskStatus, assignee: Option<&str>) -> crate::wire::TeamTask {
        crate::wire::TeamTask {
            id: format!("task_{n}"),
            team_id: "t1".into(),
            number: n,
            title: format!("task {n}"),
            detail: String::new(),
            status,
            assignee: assignee.map(String::from),
            assignee_name: None,
            created_by: "lead".into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn team_pushes_index_members_and_tasks() {
        use crate::wire::{TaskStatus, TeamRole};
        let mut s = State::new();
        assert!(s.team_for_session("lead").is_none());

        // Spawned: leader and alice point at the team.
        let t = team("t1", vec![member("lead", "lead", TeamRole::Leader, None), member("a", "alice", TeamRole::Teammate, None)], vec![]);
        let rev = s.revision();
        let ch = s.apply(Input::Push(Push::TeamUpdated { team: t.clone() }));
        assert_eq!(ch.teams, BTreeSet::from(["t1".to_string()]));
        assert_eq!(s.revision(), rev + 1);
        assert_eq!(s.team_for_session("a").map(|t| t.id.as_str()), Some("t1"));
        assert_eq!(s.team_for_session("lead").unwrap().members.len(), 2);
        // The same team again changes nothing.
        assert!(s.apply(Input::Push(Push::TeamUpdated { team: t })).is_empty());

        // A task appears and alice is ended: still a member of the record, not active.
        let t = team(
            "t1",
            vec![member("lead", "lead", TeamRole::Leader, None), member("a", "alice", TeamRole::Teammate, Some(9))],
            vec![task(1, TaskStatus::InProgress, Some("a"))],
        );
        let ch = s.apply(Input::Push(Push::TeamUpdated { team: t }));
        assert!(ch.teams.contains("t1"));
        let view = s.team_for_session("a").unwrap();
        assert!(!view.member("a").unwrap().active());
        assert_eq!(view.tasks[0].status, TaskStatus::InProgress);

        // A member the team no longer lists leaves the index.
        let t = team("t1", vec![member("lead", "lead", TeamRole::Leader, None)], vec![]);
        s.apply(Input::Push(Push::TeamUpdated { team: t }));
        assert!(s.team_for_session("a").is_none());
        assert!(s.team_for_session("lead").is_some());
        assert!(s.team("t1").is_some());
    }

    #[test]
    fn team_loads_link_and_unlink_sessions() {
        use crate::wire::TeamRole;
        let mut s = State::new();
        let t = team("t1", vec![member("lead", "lead", TeamRole::Leader, None), member("b", "bob", TeamRole::Teammate, None)], vec![]);
        let ch = s.apply(Input::TeamLoaded { session_id: "b".into(), team: Some(t.clone()) });
        assert!(ch.teams.contains("t1"));
        assert_eq!(s.team_for_session("b").map(|t| t.id.as_str()), Some("t1"));
        // Loading it again for the leader changes nothing.
        assert!(s.apply(Input::TeamLoaded { session_id: "lead".into(), team: Some(t) }).is_empty());
        // A session with no team.
        assert!(s.apply(Input::TeamLoaded { session_id: "x".into(), team: None }).is_empty());
        let ch = s.apply(Input::TeamLoaded { session_id: "b".into(), team: None });
        assert!(ch.teams.contains("t1"));
        assert!(s.team_for_session("b").is_none());
        // Teams are not part of the cache.
        assert!(State::from_cache(s.to_cache()).team_for_session("lead").is_none());
    }
}
