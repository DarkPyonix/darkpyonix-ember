//! Pure view models: what the screens show, computed from the client's [`State`] and the
//! app's own preferences. No UI types and no I/O here, so all of it is unit-tested.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use ember_client::state::{Computer, ConnectionState, Reachability, State};
use ember_client::transcript::{Transcript, TranscriptItem};
use ember_client::LauncherStatus;

use crate::prefs::Prefs;
use crate::server::{Account, ComputerStatus};

// ---- status ------------------------------------------------------------------------------------

/// How urgent a status is when several sessions are summarised into one project badge
/// (FR-L2: waiting-for-approval outranks running).
pub fn status_rank(s: LauncherStatus) -> u8 {
    match s {
        LauncherStatus::WaitingForApproval => 6,
        LauncherStatus::Running => 5,
        LauncherStatus::Failed => 4,
        LauncherStatus::FinishedUnread => 3,
        LauncherStatus::Finished => 2,
        LauncherStatus::Idle => 1,
        LauncherStatus::Unknown => 0,
    }
}

pub fn status_label(s: LauncherStatus) -> &'static str {
    match s {
        LauncherStatus::Idle => "Idle",
        LauncherStatus::Running => "Running",
        LauncherStatus::WaitingForApproval => "Needs approval",
        LauncherStatus::FinishedUnread => "Finished, unread",
        LauncherStatus::Finished => "Finished",
        LauncherStatus::Failed => "Failed",
        LauncherStatus::Unknown => "Unknown",
    }
}

/// The agent is working: a new message would be queued (FR-L6), the interrupt button shows.
pub fn is_busy(s: LauncherStatus) -> bool {
    matches!(s, LauncherStatus::Running | LauncherStatus::WaitingForApproval)
}

/// Badge tone; the UI maps it to colour roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Warning,
    Error,
    Success,
}

/// Per-project counts for the project card's badges (FR-L2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSummary {
    pub total: usize,
    pub running: usize,
    pub waiting: usize,
    pub unread: usize,
    pub failed: usize,
    /// The most urgent status in the project.
    pub top: Option<LauncherStatus>,
}

impl ProjectSummary {
    pub fn add(&mut self, s: LauncherStatus) {
        self.total += 1;
        match s {
            LauncherStatus::Running => self.running += 1,
            LauncherStatus::WaitingForApproval => self.waiting += 1,
            LauncherStatus::FinishedUnread => self.unread += 1,
            LauncherStatus::Failed => self.failed += 1,
            _ => {}
        }
        if self.top.is_none_or(|t| status_rank(s) > status_rank(t)) {
            self.top = Some(s);
        }
    }

    /// Badges in urgency order: approval first, then running, failed, unread.
    pub fn badges(&self) -> Vec<(String, Tone)> {
        let mut out = Vec::new();
        if self.waiting > 0 {
            out.push((format!("{} need approval", self.waiting), Tone::Warning));
        }
        if self.running > 0 {
            out.push((format!("{} running", self.running), Tone::Accent));
        }
        if self.failed > 0 {
            out.push((format!("{} failed", self.failed), Tone::Error));
        }
        if self.unread > 0 {
            out.push((format!("{} unread", self.unread), Tone::Success));
        }
        out
    }
}

// ---- time --------------------------------------------------------------------------------------

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// "just now", "5m", "3h", "2d", "5w"; timestamps are unix milliseconds.
pub fn relative_time(now: i64, at: i64) -> String {
    if at <= 0 {
        return String::new();
    }
    let secs = (now - at).max(0) / 1000;
    match secs {
        0..=44 => "just now".into(),
        45..=3599 => format!("{}m", (secs / 60).max(1)),
        3600..=86_399 => format!("{}h", secs / 3600),
        86_400..=604_799 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

// ---- text helpers ------------------------------------------------------------------------------

/// First non-empty line, at most `max` characters (an ellipsis marks the cut).
pub fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let more_lines = text.lines().filter(|l| !l.trim().is_empty()).count() > 1;
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max || more_lines {
        out.push('\u{2026}');
    }
    out
}

/// The latest thing said in a transcript, for the session row (FR-L1 preview).
pub fn preview(t: &Transcript) -> Option<String> {
    t.items.iter().rev().find_map(|i| match i {
        TranscriptItem::Assistant { text, .. } | TranscriptItem::User { text, .. } if !text.trim().is_empty() => {
            Some(one_line(text, 120))
        }
        TranscriptItem::ToolCall { name, .. } if !name.is_empty() => Some(format!("Tool: {name}")),
        TranscriptItem::Approval { tool, .. } => Some(format!("Approval requested: {tool}")),
        TranscriptItem::Error { message, .. } => Some(one_line(message, 120)),
        _ => None,
    })
}

/// A compact one-line rendering of a tool input (`{"command":"ls"}` → `command: ls`).
pub fn json_summary(v: &serde_json::Value, max: usize) -> String {
    let s = match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(m) => m
            .iter()
            .map(|(k, v)| match v {
                serde_json::Value::String(s) => format!("{k}: {s}"),
                other => format!("{k}: {other}"),
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    };
    one_line(&s, max)
}

pub fn json_pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

// ---- launcher rows -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCard {
    pub name: String,
    pub summary: ProjectSummary,
    /// Latest activity in the project, unix ms.
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub project: String,
    pub title: String,
    pub agent: String,
    pub model: Option<String>,
    pub account: Option<String>,
    pub cwd: String,
    pub updated_at: i64,
    pub preview: Option<String>,
    pub status: LauncherStatus,
    pub pinned: bool,
    pub archived: bool,
}

/// Labels for account ids (`label (agent)`), from `GET /accounts`.
pub fn account_labels(accounts: &[Account]) -> HashMap<String, String> {
    accounts.iter().map(|a| (a.id.clone(), a.label.clone())).collect()
}

/// What a session row needs from the outside world besides [`State`].
pub struct RowContext<'a> {
    pub prefs: &'a Prefs,
    /// session id → account id.
    pub session_accounts: &'a HashMap<String, String>,
    /// account id → label.
    pub account_labels: &'a HashMap<String, String>,
}

pub fn session_row(state: &State, id: &str, cx: &RowContext<'_>) -> Option<SessionRow> {
    let v = state.session(id)?;
    let r = v.record;
    let account = cx.session_accounts.get(id).map(|aid| cx.account_labels.get(aid).cloned().unwrap_or_else(|| aid.clone()));
    Some(SessionRow {
        id: r.id.clone(),
        project: r.project.clone(),
        title: cx.prefs.titles.get(id).cloned().unwrap_or_else(|| r.title.clone()),
        agent: r.agent.clone(),
        model: r.model.clone(),
        account,
        cwd: r.cwd.clone(),
        updated_at: r.updated_at,
        preview: state.transcript(id).and_then(preview),
        status: v.status,
        pinned: cx.prefs.pinned.contains(id),
        archived: cx.prefs.archived.contains(id),
    })
}

/// All projects, most recently active first, with their status summary. Archived sessions do
/// not count toward a project's badges.
pub fn project_cards(state: &State, prefs: &Prefs) -> Vec<ProjectCard> {
    let mut cards: Vec<ProjectCard> = state
        .projects()
        .into_iter()
        .map(|p| {
            let mut summary = ProjectSummary::default();
            let mut updated_at = 0;
            for id in &p.sessions {
                if prefs.archived.contains(id) {
                    continue;
                }
                if let Some(v) = state.session(id) {
                    summary.add(v.status);
                    updated_at = updated_at.max(v.record.updated_at);
                }
            }
            ProjectCard { name: p.name, summary, updated_at }
        })
        .collect();
    cards.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.name.cmp(&b.name)));
    cards
}

/// One project's sessions: pinned first, then most recently active; archived ones only when
/// asked for.
pub fn project_sessions(state: &State, project: &str, cx: &RowContext<'_>, show_archived: bool) -> Vec<SessionRow> {
    let Some(p) = state.projects().into_iter().find(|p| p.name == project) else { return Vec::new() };
    let mut rows: Vec<SessionRow> = p
        .sessions
        .iter()
        .filter_map(|id| session_row(state, id, cx))
        .filter(|r| show_archived || !r.archived)
        .collect();
    // `projects()` is already most-recent-first; a stable sort keeps that within each group.
    rows.sort_by_key(|r| !r.pinned);
    rows
}

/// Search across every session (FR-L9): title, project, agent, account, folder and whatever
/// transcript text the client holds. Case-insensitive; every word must match.
pub fn search(state: &State, query: &str, cx: &RowContext<'_>) -> Vec<SessionRow> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut rows: Vec<SessionRow> = state
        .projects()
        .into_iter()
        .flat_map(|p| p.sessions)
        .filter_map(|id| {
            let row = session_row(state, &id, cx)?;
            let mut hay = format!(
                "{} {} {} {} {}",
                row.title,
                row.project,
                row.agent,
                row.account.as_deref().unwrap_or(""),
                row.cwd
            )
            .to_lowercase();
            if let Some(t) = state.transcript(&id) {
                for item in &t.items {
                    if let TranscriptItem::User { text, .. } | TranscriptItem::Assistant { text, .. } = item {
                        hay.push(' ');
                        hay.push_str(&text.to_lowercase());
                    }
                }
            }
            words.iter().all(|w| hay.contains(w.as_str())).then_some(row)
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    rows
}

/// Sessions for the navigation's "Conversations" section: pinned and recently active,
/// not archived, at most `n`.
pub fn recent_sessions(state: &State, cx: &RowContext<'_>, n: usize) -> Vec<SessionRow> {
    let mut rows: Vec<SessionRow> = state
        .projects()
        .into_iter()
        .flat_map(|p| p.sessions)
        .filter_map(|id| session_row(state, &id, cx))
        .filter(|r| !r.archived)
        .collect();
    rows.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| b.updated_at.cmp(&a.updated_at)));
    rows.truncate(n);
    rows
}

// ---- computers ---------------------------------------------------------------------------------

/// Turn `GET /computers` into the client's computer list (FR-L3). A probe that did not run
/// (`reachable: None`) keeps the last-known reachability instead of forgetting it; an
/// offline computer stays listed. `projects` is kept from the previous list: the main server
/// has no project assignment API yet (FR-L4).
pub fn merge_computers(previous: &[Computer], fresh: &[ComputerStatus]) -> Vec<Computer> {
    let prev: HashMap<&str, &Computer> = previous.iter().map(|c| (c.id.as_str(), c)).collect();
    fresh
        .iter()
        .map(|c| {
            let old = prev.get(c.id.as_str());
            let reachability = match c.reachable {
                Some(true) => Reachability::Online,
                Some(false) => Reachability::Offline,
                None => old.map(|o| o.reachability).unwrap_or(Reachability::Unknown),
            };
            Computer {
                id: c.id.clone(),
                name: c.name.clone(),
                reachability,
                projects: old.map(|o| o.projects.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

pub fn reachability_label(r: Reachability) -> &'static str {
    match r {
        Reachability::Online => "Online",
        Reachability::Offline => "Offline",
        Reachability::Unknown => "Checking\u{2026}",
    }
}

/// `None` when connected (nothing to say).
pub fn connection_banner(c: &ConnectionState) -> Option<(String, Tone)> {
    match c {
        ConnectionState::Connected => None,
        ConnectionState::Offline => Some(("Offline: showing cached data".into(), Tone::Neutral)),
        ConnectionState::Connecting { .. } => Some(("Connecting to the main server\u{2026}".into(), Tone::Neutral)),
        ConnectionState::Reconnecting { retry_in_ms, error, .. } => Some((
            format!("Disconnected ({error}). Retrying in {}s", retry_in_ms.div_ceil(1000)),
            Tone::Warning,
        )),
        ConnectionState::Incompatible { server, client } => Some((
            format!("This app speaks push v{client}, the server v{server}. Update one of them."),
            Tone::Error,
        )),
    }
}

// ---- message queue (FR-L6) ---------------------------------------------------------------------

/// Messages typed while the agent is busy, delivered one per turn boundary, in order.
///
/// Every send goes through here, so a message typed while idle is delivered at once and the
/// next one waits for that turn to finish. A message counts as delivered once the session is
/// seen busy, or once its sequence number has moved past where it was when the message was
/// sent (a turn so short that no busy status was observed).
#[derive(Debug, Default)]
pub struct Outbox {
    queues: HashMap<String, VecDeque<String>>,
    /// session → `last_seq` when the in-flight message was sent.
    in_flight: HashMap<String, i64>,
    errors: HashMap<String, String>,
}

impl Outbox {
    pub fn push(&mut self, id: &str, text: String) {
        self.errors.remove(id);
        self.queues.entry(id.to_string()).or_default().push_back(text);
    }

    /// Messages waiting (not counting one already sent).
    pub fn queued(&self, id: &str) -> usize {
        self.queues.get(id).map_or(0, VecDeque::len)
    }

    pub fn queued_texts(&self, id: &str) -> Vec<String> {
        self.queues.get(id).map(|q| q.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn clear(&mut self, id: &str) {
        self.queues.remove(id);
    }

    pub fn error(&self, id: &str) -> Option<&str> {
        self.errors.get(id).map(String::as_str)
    }

    /// Given the session's current status, the message to send now, if any.
    pub fn next(&mut self, id: &str, busy: bool, last_seq: i64) -> Option<String> {
        if busy {
            self.in_flight.remove(id);
            return None;
        }
        if let Some(&at) = self.in_flight.get(id) {
            if last_seq <= at {
                return None;
            }
            self.in_flight.remove(id);
        }
        let q = self.queues.get_mut(id)?;
        let text = q.pop_front()?;
        if q.is_empty() {
            self.queues.remove(id);
        }
        self.in_flight.insert(id.to_string(), last_seq);
        Some(text)
    }

    /// The send failed: put the message back at the front and remember why.
    pub fn failed(&mut self, id: &str, text: String, error: String) {
        self.in_flight.remove(id);
        self.queues.entry(id.to_string()).or_default().push_front(text);
        self.errors.insert(id.to_string(), error);
    }
}

// ---- new-session defaults (FR-L8) --------------------------------------------------------------

/// The folder a new session in `project` starts in when nothing was remembered: the most
/// recent session's.
pub fn latest_cwd(state: &State, project: &str) -> Option<String> {
    let p = state.projects().into_iter().find(|p| p.name == project)?;
    p.sessions.first().and_then(|id| state.session(id)).map(|v| v.record.cwd.clone())
}

/// Index of `value` in `options`, else 0.
pub fn index_of(options: &[String], value: Option<&str>) -> usize {
    value.and_then(|v| options.iter().position(|o| o == v)).unwrap_or(0)
}

/// Deduplicate preserving order.
pub fn dedup(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|s| seen.insert(s.clone())).collect()
}

/// Group names for the computers list ("p1, p2").
pub fn join_projects(projects: &[String]) -> String {
    let set: BTreeMap<&str, ()> = projects.iter().map(|p| (p.as_str(), ())).collect();
    set.keys().copied().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::state::Input;
    use ember_client::wire::{AgentEvent, Push, SessionRecord, SessionStatus, StoredEvent, TurnOutcome};
    use serde_json::json;

    fn rec(id: &str, project: &str, status: SessionStatus, at: i64) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            project: project.into(),
            agent: "claude-code".into(),
            cwd: format!("/w/{project}"),
            model: None,
            native_id: None,
            status,
            title: format!("title {id}"),
            created_at: 0,
            updated_at: at,
            last_seq: at,
        }
    }

    fn state(recs: Vec<SessionRecord>) -> State {
        let mut s = State::new();
        s.apply(Input::SessionsLoaded(recs));
        s
    }

    fn cx<'a>(prefs: &'a Prefs, sa: &'a HashMap<String, String>, al: &'a HashMap<String, String>) -> RowContext<'a> {
        RowContext { prefs, session_accounts: sa, account_labels: al }
    }

    #[test]
    fn fr_l2_waiting_outranks_running_in_project_summary() {
        let mut sum = ProjectSummary::default();
        sum.add(LauncherStatus::Running);
        sum.add(LauncherStatus::WaitingForApproval);
        sum.add(LauncherStatus::Running);
        sum.add(LauncherStatus::FinishedUnread);
        assert_eq!(sum.top, Some(LauncherStatus::WaitingForApproval));
        let badges = sum.badges();
        assert_eq!(badges[0], ("1 need approval".to_string(), Tone::Warning));
        assert_eq!(badges[1], ("2 running".to_string(), Tone::Accent));
        assert_eq!(badges[2], ("1 unread".to_string(), Tone::Success));
        assert!(status_rank(LauncherStatus::WaitingForApproval) > status_rank(LauncherStatus::Running));
    }

    #[test]
    fn relative_times() {
        let now = 10_000_000_000;
        assert_eq!(relative_time(now, now - 1000), "just now");
        assert_eq!(relative_time(now, now - 5 * 60_000), "5m");
        assert_eq!(relative_time(now, now - 3 * 3_600_000), "3h");
        assert_eq!(relative_time(now, now - 2 * 86_400_000), "2d");
        assert_eq!(relative_time(now, now - 15 * 86_400_000), "2w");
        assert_eq!(relative_time(now, 0), "");
        assert_eq!(relative_time(now, now + 5000), "just now");
    }

    #[test]
    fn previews_and_summaries() {
        assert_eq!(one_line("  \nhello world\nsecond", 5), "hello\u{2026}");
        assert_eq!(one_line("short", 10), "short");
        let mut t = Transcript::new();
        t.apply_seq(1, &AgentEvent::UserMessage { text: "fix it".into() });
        t.apply_seq(2, &AgentEvent::AssistantDelta { text: "Done.".into() });
        assert_eq!(preview(&t).as_deref(), Some("Done."));
        assert_eq!(json_summary(&json!({ "command": "ls -la" }), 40), "command: ls -la");
        assert_eq!(json_summary(&json!(null), 40), "");
    }

    #[test]
    fn fr_l1_project_cards_and_rows() {
        let s = state(vec![
            rec("a", "alpha", SessionStatus::Running, 10),
            rec("b", "alpha", SessionStatus::Finished, 30),
            rec("c", "beta", SessionStatus::WaitingForApproval, 20),
        ]);
        let mut prefs = Prefs::default();
        prefs.pinned.insert("a".into());
        let sa = HashMap::from([("a".to_string(), "acc1".to_string())]);
        let al = HashMap::from([("acc1".to_string(), "work".to_string())]);
        let cards = project_cards(&s, &prefs);
        assert_eq!(cards.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
        assert_eq!(cards[1].summary.top, Some(LauncherStatus::WaitingForApproval));
        let rows = project_sessions(&s, "alpha", &cx(&prefs, &sa, &al), false);
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["a", "b"], "pinned first");
        assert_eq!(rows[0].account.as_deref(), Some("work"));

        prefs.archived.insert("b".into());
        assert_eq!(project_sessions(&s, "alpha", &cx(&prefs, &sa, &al), false).len(), 1);
        assert_eq!(project_sessions(&s, "alpha", &cx(&prefs, &sa, &al), true).len(), 2);
        assert_eq!(project_cards(&s, &prefs)[0].summary.total, 1);

        prefs.titles.insert("c".into(), "Renamed".into());
        let found = search(&s, "renamed BETA", &cx(&prefs, &sa, &al));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Renamed");
        assert!(search(&s, "  ", &cx(&prefs, &sa, &al)).is_empty());
        assert_eq!(latest_cwd(&s, "alpha").as_deref(), Some("/w/alpha"));
        let recent = recent_sessions(&s, &cx(&prefs, &sa, &al), 5);
        assert_eq!(recent[0].id, "a", "pinned first");
    }

    #[test]
    fn fr_l2_unread_shows_until_opened() {
        let mut s = state(vec![]);
        s.apply(Input::Push(Push::SessionCreated { session: rec("a", "p", SessionStatus::Idle, 0) }));
        s.apply(Input::Push(Push::Event {
            status: SessionStatus::Finished,
            event: StoredEvent { session_id: "a".into(), seq: 1, at: 1, event: AgentEvent::TurnEnded { outcome: TurnOutcome::Completed } },
        }));
        let prefs = Prefs::default();
        let (sa, al) = (HashMap::new(), HashMap::new());
        assert_eq!(project_cards(&s, &prefs)[0].summary.unread, 1);
        s.apply(Input::Opened("a".into()));
        s.apply(Input::Closed("a".into()));
        assert_eq!(project_sessions(&s, "p", &cx(&prefs, &sa, &al), false)[0].status, LauncherStatus::Finished);
    }

    #[test]
    fn fr_l3_computers_keep_last_known_reachability() {
        let fresh = |r: Option<bool>| ComputerStatus {
            id: "c1".into(),
            name: "studio".into(),
            url: String::new(),
            local: false,
            reachable: r,
            error: None,
        };
        let first = merge_computers(&[], &[fresh(None)]);
        assert_eq!(first[0].reachability, Reachability::Unknown);
        let probed = merge_computers(&first, &[fresh(Some(false))]);
        assert_eq!(probed[0].reachability, Reachability::Offline);
        let unprobed = merge_computers(&probed, &[fresh(None)]);
        assert_eq!(unprobed[0].reachability, Reachability::Offline, "an offline computer keeps its last-known state");
    }

    #[test]
    fn fr_l6_outbox_delivers_one_per_turn_in_order() {
        let mut o = Outbox::default();
        o.push("s", "one".into());
        o.push("s", "two".into());
        // Busy: nothing goes out.
        assert_eq!(o.next("s", true, 5), None);
        // Idle: the first goes out; the second waits for that turn.
        assert_eq!(o.next("s", false, 5).as_deref(), Some("one"));
        assert_eq!(o.queued("s"), 1);
        assert_eq!(o.next("s", false, 5), None, "still waiting for the turn to start");
        assert_eq!(o.next("s", true, 6), None);
        assert_eq!(o.next("s", false, 9).as_deref(), Some("two"));
        // A turn too quick to be seen busy still releases the queue once seq moves.
        o.push("s", "three".into());
        assert_eq!(o.next("s", false, 9), None);
        assert_eq!(o.next("s", false, 12).as_deref(), Some("three"));
        // A failed send goes back to the front.
        o.push("s", "four".into());
        o.failed("s", "three".into(), "boom".into());
        assert_eq!(o.queued_texts("s"), vec!["three".to_string(), "four".to_string()]);
        assert_eq!(o.error("s"), Some("boom"));
        o.clear("s");
        assert_eq!(o.queued("s"), 0);
    }

    #[test]
    fn helpers() {
        let opts = vec!["a".to_string(), "b".to_string()];
        assert_eq!(index_of(&opts, Some("b")), 1);
        assert_eq!(index_of(&opts, Some("z")), 0);
        assert_eq!(dedup(vec!["a".into(), "b".into(), "a".into()]), opts);
        assert_eq!(join_projects(&["b".into(), "a".into(), "b".into()]), "a, b");
        assert!(connection_banner(&ConnectionState::Connected).is_none());
        assert_eq!(
            connection_banner(&ConnectionState::Reconnecting { attempt: 1, retry_in_ms: 1500, error: "x".into() }).unwrap().1,
            Tone::Warning
        );
    }
}
