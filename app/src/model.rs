//! Pure view models: what the screens show, computed from the client's [`State`] (which the
//! main server keeps current, including pins, archive marks, titles and computer assignments)
//! and the few server answers the client does not hold (accounts, search hits). No UI types
//! and no I/O here, so all of it is unit-tested.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use ember_client::state::{Computer, ConnectionState, Reachability, State};
use ember_client::transcript::{Transcript, TranscriptItem};
use ember_client::wire::{MentionCandidate, SearchHit, SessionStatus, TaskStatus, TeamRole, TeamTask, TeamView};
use ember_client::LauncherStatus;

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
    /// account id → label.
    pub account_labels: &'a HashMap<String, String>,
}

/// The label of a session's account, or `None` for the server's own login.
pub fn account_label(account_id: Option<&str>, labels: &HashMap<String, String>) -> Option<String> {
    account_id.map(|aid| labels.get(aid).cloned().unwrap_or_else(|| aid.to_string()))
}

pub fn session_row(state: &State, id: &str, cx: &RowContext<'_>) -> Option<SessionRow> {
    let v = state.session(id)?;
    let r = v.record;
    Some(SessionRow {
        id: r.id.clone(),
        project: r.project.clone(),
        title: r.title.clone(),
        agent: r.agent.clone(),
        model: r.model.clone(),
        account: account_label(r.account_id.as_deref(), cx.account_labels),
        cwd: r.cwd.clone(),
        updated_at: r.updated_at,
        preview: state.transcript(id).and_then(preview),
        status: v.status,
        pinned: r.pinned,
        archived: r.archived,
    })
}

/// All projects, most recently active first, with their status summary. Archived sessions do
/// not count toward a project's badges.
pub fn project_cards(state: &State) -> Vec<ProjectCard> {
    let mut cards: Vec<ProjectCard> = state
        .projects()
        .into_iter()
        .map(|p| {
            let mut summary = ProjectSummary::default();
            let mut updated_at = 0;
            for id in &p.sessions {
                if let Some(v) = state.session(id).filter(|v| !v.record.archived) {
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

/// One search result (FR-L9): a session, and the message that matched when the match was in
/// the conversation (FR-S4).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub row: SessionRow,
    /// The matching message's sequence number (`None`: the session's details matched).
    pub seq: Option<i64>,
    /// Text around the match, without markers.
    pub snippet: Option<String>,
}

/// Search results: the main server's full-text hits across every session's messages (FR-S4),
/// best first, then sessions whose details (title, project, agent, account, folder) contain
/// every word of `query`, case-insensitively, most recent first. A hit whose session this
/// client does not hold (yet) is skipped; the next session list load brings it.
pub fn search_results(state: &State, query: &str, hits: &[SearchHit], cx: &RowContext<'_>) -> Vec<SearchResult> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<SearchResult> = hits
        .iter()
        .filter_map(|h| {
            let row = session_row(state, &h.session_id, cx)?;
            Some(SearchResult { row, seq: Some(h.seq), snippet: Some(one_line(&h.plain_snippet(), 200)) })
        })
        .collect();
    let with_hits: HashSet<String> = out.iter().map(|r| r.row.id.clone()).collect();
    let mut by_details: Vec<SessionRow> = state
        .projects()
        .into_iter()
        .flat_map(|p| p.sessions)
        .filter(|id| !with_hits.contains(id))
        .filter_map(|id| session_row(state, &id, cx))
        .filter(|row| {
            let hay = format!(
                "{} {} {} {} {}",
                row.title,
                row.project,
                row.agent,
                row.account.as_deref().unwrap_or(""),
                row.cwd
            )
            .to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect();
    by_details.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    out.extend(by_details.into_iter().map(|row| SearchResult { row, seq: None, snippet: None }));
    out
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
/// offline computer stays listed. `projects` is kept from the previous list; the client state
/// re-derives it from the server's project assignments (FR-L4) when it takes the list.
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

/// A computer's assignment to the selected project, for the computer list's toggle (FR-L4).
pub fn is_assigned(state: &State, project: &str, computer_id: &str) -> bool {
    state.project_computers(project).iter().any(|c| c == computer_id)
}

/// Group names for the computers list ("p1, p2").
pub fn join_projects(projects: &[String]) -> String {
    let set: BTreeMap<&str, ()> = projects.iter().map(|p| (p.as_str(), ())).collect();
    set.keys().copied().collect::<Vec<_>>().join(", ")
}

// ---- composer mentions (FR-T6) --------------------------------------------------------------

/// A mention being typed at the end of the draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedMention {
    /// Exactly as typed, `@@` included (`@@bet`, `@@"beta te`).
    pub raw: String,
    /// What to filter candidates by (`bet`, `beta te`).
    pub query: String,
}

/// The `@@` mention the draft ends with, if any. `@@` counts at the start or after whitespace
/// or an opening bracket. An unquoted mention ends at whitespace; a quoted one at its closing
/// quote. A finished mention (followed by a space, or closed) is not "being typed".
pub fn typed_mention(draft: &str) -> Option<TypedMention> {
    let at = draft.rfind("@@")?;
    let before = draft[..at].chars().last();
    if !before.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{')) {
        return None;
    }
    let rest = &draft[at + 2..];
    let query = match rest.strip_prefix('"') {
        Some(q) if !q.contains('"') && !q.contains('\n') => q,
        Some(_) => return None,
        None if rest.chars().any(char::is_whitespace) => return None,
        None => rest,
    };
    Some(TypedMention { raw: draft[at..].to_string(), query: query.to_string() })
}

/// Candidates matching `query` (title substring or id prefix, case-insensitive), at most `n`.
pub fn filter_mentions<'a>(all: &'a [MentionCandidate], query: &str, n: usize) -> Vec<&'a MentionCandidate> {
    let q = query.trim().to_lowercase();
    all.iter()
        .filter(|c| q.is_empty() || c.id.starts_with(&q) || c.title.to_lowercase().contains(&q))
        .take(n)
        .collect()
}

/// What a picked candidate becomes in the sent text: `@@Title` (one word), `@@"Title"` (with
/// spaces), or `@@<id>` when the title is ambiguous among `all`, empty or holds a quote. The
/// server resolves all three the same way.
pub fn mention_token(c: &MentionCandidate, all: &[MentionCandidate]) -> String {
    let title = c.title.trim();
    let unique = all.iter().filter(|o| o.title.trim().eq_ignore_ascii_case(title)).count() == 1;
    if title.is_empty() || title.contains('"') || !unique {
        format!("@@{}", c.id)
    } else if title.chars().any(char::is_whitespace) {
        format!("@@\"{title}\"")
    } else {
        format!("@@{title}")
    }
}

/// Replace each picked mention (`(raw as typed, token)`) in `text`. The text field cannot be
/// edited from code, so a pick is applied when the message is sent; a raw fragment the user
/// typed on from (`@@bet` became `@@beta`) is left for the server to resolve as typed.
pub fn apply_mentions(text: &str, picks: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (raw, token) in picks {
        let mut from = 0;
        while let Some(i) = out[from..].find(raw.as_str()).map(|i| i + from) {
            let end = i + raw.len();
            let ends_cleanly = out[end..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}'));
            if ends_cleanly {
                out.replace_range(i..end, token);
                break;
            }
            from = end;
        }
    }
    out
}

/// One row of the conversation's team panel (FR-T7).
#[derive(Debug, Clone, PartialEq)]
pub struct TeamMemberRow {
    pub session_id: String,
    pub name: String,
    pub leader: bool,
    pub agent: String,
    /// Live status from the session record when the client has it, else the team's snapshot.
    pub status: &'static str,
    pub ended: bool,
    /// This conversation.
    pub this: bool,
    /// Tasks assigned to this member that are not done or cancelled.
    pub open_tasks: usize,
}

/// The team panel's member rows: leader first, then active teammates, then ended ones.
pub fn team_member_rows(state: &State, team: &TeamView, this: &str) -> Vec<TeamMemberRow> {
    let mut rows: Vec<TeamMemberRow> = team
        .members
        .iter()
        .map(|m| {
            let status = match state.session(&m.session_id) {
                Some(v) => status_label(v.status),
                None => match m.status {
                    Some(SessionStatus::Running) => status_label(LauncherStatus::Running),
                    Some(SessionStatus::WaitingForApproval) => status_label(LauncherStatus::WaitingForApproval),
                    Some(SessionStatus::Failed) => status_label(LauncherStatus::Failed),
                    Some(SessionStatus::Finished) => status_label(LauncherStatus::Finished),
                    _ => status_label(LauncherStatus::Idle),
                },
            };
            TeamMemberRow {
                session_id: m.session_id.clone(),
                name: m.name.clone(),
                leader: m.role == TeamRole::Leader,
                agent: m.agent.clone(),
                status,
                ended: !m.active(),
                this: m.session_id == this,
                open_tasks: team
                    .tasks
                    .iter()
                    .filter(|t| {
                        t.assignee.as_deref() == Some(m.session_id.as_str())
                            && !matches!(t.status, TaskStatus::Done | TaskStatus::Cancelled)
                    })
                    .count(),
            }
        })
        .collect();
    rows.sort_by_key(|r| (!r.leader, r.ended));
    rows
}

/// One task line: `#3 [in progress] Write tests · alice`.
pub fn task_line(t: &TeamTask) -> String {
    let who = t.assignee_name.as_deref().unwrap_or(if t.assignee.is_some() { "someone" } else { "unassigned" });
    format!("#{} [{}] {} \u{00b7} {who}", t.number, t.status.label(), t.title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::state::Input;
    use ember_client::wire::{AgentEvent, Project, Push, SessionRecord, SessionStatus, StoredEvent, TurnOutcome};
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
            account_id: None,
            account_reason: None,
            pinned: false,
            archived: false,
        }
    }

    /// Apply a server metadata push (what a PATCH by any client produces).
    fn meta(s: &mut State, id: &str, f: impl FnOnce(&mut SessionRecord)) {
        let mut r = s.session(id).unwrap().record.clone();
        f(&mut r);
        s.apply(Input::Push(Push::SessionUpdated { session: r }));
    }

    fn state(recs: Vec<SessionRecord>) -> State {
        let mut s = State::new();
        s.apply(Input::SessionsLoaded(recs));
        s
    }

    fn cx(al: &HashMap<String, String>) -> RowContext<'_> {
        RowContext { account_labels: al }
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
        let mut a = rec("a", "alpha", SessionStatus::Running, 10);
        a.account_id = Some("acc1".into());
        let mut s = state(vec![
            a,
            rec("b", "alpha", SessionStatus::Finished, 30),
            rec("c", "beta", SessionStatus::WaitingForApproval, 20),
        ]);
        meta(&mut s, "a", |r| r.pinned = true);
        let al = HashMap::from([("acc1".to_string(), "work".to_string())]);
        let cards = project_cards(&s);
        assert_eq!(cards.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
        assert_eq!(cards[1].summary.top, Some(LauncherStatus::WaitingForApproval));
        let rows = project_sessions(&s, "alpha", &cx(&al), false);
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["a", "b"], "pinned first");
        assert_eq!(rows[0].account.as_deref(), Some("work"), "account from the session record");
        assert_eq!(rows[1].account, None);

        meta(&mut s, "b", |r| r.archived = true);
        assert_eq!(project_sessions(&s, "alpha", &cx(&al), false).len(), 1);
        assert_eq!(project_sessions(&s, "alpha", &cx(&al), true).len(), 2);
        assert_eq!(project_cards(&s)[0].summary.total, 1);

        meta(&mut s, "c", |r| r.title = "Renamed".into());
        assert_eq!(latest_cwd(&s, "alpha").as_deref(), Some("/w/alpha"));
        let recent = recent_sessions(&s, &cx(&al), 5);
        assert_eq!(recent[0].id, "a", "pinned first");
        assert!(recent.iter().all(|r| r.id != "b"), "archived sessions are not recent");
    }

    #[test]
    fn fr_l9_search_shows_server_hits_then_detail_matches() {
        let mut s = state(vec![
            rec("a", "alpha", SessionStatus::Finished, 10),
            rec("b", "beta", SessionStatus::Finished, 30),
            rec("c", "beta", SessionStatus::Finished, 20),
        ]);
        meta(&mut s, "c", |r| r.title = "Deploy notes".into());
        let al = HashMap::new();
        let hit = |id: &str, seq: i64, snippet: &str| SearchHit {
            session_id: id.into(),
            seq,
            kind: "user_message".into(),
            snippet: snippet.into(),
            title: String::new(),
            project: String::new(),
            archived: false,
        };
        let hits = vec![
            hit("a", 4, "the \u{ab}deploy\u{bb} failed"),
            hit("ghost", 1, "a session this client has not loaded"),
        ];
        let found = search_results(&s, "DEPLOY", &hits, &cx(&al));
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].row.id.as_str(), found[0].seq), ("a", Some(4)));
        assert_eq!(found[0].snippet.as_deref(), Some("the deploy failed"));
        assert_eq!((found[1].row.id.as_str(), found[1].seq, found[1].row.title.as_str()), ("c", None, "Deploy notes"));
        // A session with a message hit is not listed again for its details.
        let found = search_results(&s, "alpha", &[hit("a", 1, "x")], &cx(&al));
        assert_eq!(found.len(), 1);
        assert!(search_results(&s, "  ", &hits, &cx(&al)).is_empty());
    }

    #[test]
    fn fr_l4_assignment_comes_from_the_server_projects() {
        let mut s = state(vec![rec("a", "alpha", SessionStatus::Idle, 1)]);
        s.apply(Input::ProjectsLoaded(vec![Project { name: "alpha".into(), created_at: 0, computers: vec!["local".into()] }]));
        assert!(is_assigned(&s, "alpha", "local"));
        assert!(!is_assigned(&s, "alpha", "studio"));
        assert!(!is_assigned(&s, "nope", "local"));
    }

    #[test]
    fn fr_l2_unread_shows_until_opened() {
        let mut s = state(vec![]);
        s.apply(Input::Push(Push::SessionCreated { session: rec("a", "p", SessionStatus::Idle, 0) }));
        s.apply(Input::Push(Push::Event {
            status: SessionStatus::Finished,
            event: StoredEvent { session_id: "a".into(), seq: 1, at: 1, event: AgentEvent::TurnEnded { outcome: TurnOutcome::Completed } },
        }));
        let al = HashMap::new();
        assert_eq!(project_cards(&s)[0].summary.unread, 1);
        s.apply(Input::Opened("a".into()));
        s.apply(Input::Closed("a".into()));
        assert_eq!(project_sessions(&s, "p", &cx(&al), false)[0].status, LauncherStatus::Finished);
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

    fn cand(id: &str, title: &str) -> MentionCandidate {
        MentionCandidate { id: id.into(), title: title.into(), project: "p".into(), agent: "codex".into(), status: SessionStatus::Idle }
    }

    #[test]
    fn typed_mentions_at_the_end_of_the_draft() {
        let t = |d: &str| typed_mention(d).map(|m| (m.raw, m.query));
        assert_eq!(t("ask @@bet"), Some(("@@bet".into(), "bet".into())));
        assert_eq!(t("@@"), Some(("@@".into(), String::new())));
        assert_eq!(t("ask (@@\"beta te"), Some(("@@\"beta te".into(), "beta te".into())));
        assert_eq!(t("ask @@beta "), None, "finished by a space");
        assert_eq!(t("ask @@\"beta\""), None, "closed quote");
        assert_eq!(t("mail a@@b"), None);
        assert_eq!(t("no mention"), None);
    }

    #[test]
    fn mention_tokens_and_replacement() {
        let all = vec![cand("id-1", "Beta Tests"), cand("id-2", "gamma"), cand("id-3", "dup"), cand("id-4", "Dup"), cand("id-5", "say \"hi\"")];
        assert_eq!(mention_token(&all[0], &all), "@@\"Beta Tests\"");
        assert_eq!(mention_token(&all[1], &all), "@@gamma");
        assert_eq!(mention_token(&all[2], &all), "@@id-3", "ambiguous titles use the id");
        assert_eq!(mention_token(&all[4], &all), "@@id-5");
        assert_eq!(filter_mentions(&all, "TEST", 5).len(), 1);
        assert_eq!(filter_mentions(&all, "id-", 2).len(), 2);
        assert_eq!(filter_mentions(&all, "", 10).len(), 5);

        let picks = vec![("@@bet".to_string(), "@@\"Beta Tests\"".to_string()), ("@@gam".to_string(), "@@gamma".to_string())];
        assert_eq!(apply_mentions("hi @@bet, and @@gam", &picks), "hi @@\"Beta Tests\", and @@gamma");
        // Typed on after picking: left as typed.
        assert_eq!(apply_mentions("hi @@betamax", &picks[..1]), "hi @@betamax");
        assert_eq!(apply_mentions("plain", &picks), "plain");
    }

    #[test]
    fn team_rows_put_the_leader_first_and_count_open_tasks() {
        use ember_client::wire::{TeamMember, TeamTask};
        let m = |sid: &str, name: &str, role: TeamRole, ended: Option<i64>| TeamMember {
            session_id: sid.into(),
            name: name.into(),
            role,
            joined_at: 0,
            ended_at: ended,
            title: name.into(),
            agent: "codex".into(),
            status: Some(SessionStatus::Running),
        };
        let task = |n: i64, status: TaskStatus, who: Option<&str>, name: Option<&str>| TeamTask {
            id: format!("t{n}"),
            team_id: "team".into(),
            number: n,
            title: format!("task {n}"),
            detail: String::new(),
            status,
            assignee: who.map(String::from),
            assignee_name: name.map(String::from),
            created_by: "l".into(),
            created_at: 0,
            updated_at: 0,
        };
        let team = TeamView {
            id: "team".into(),
            project: "p".into(),
            leader: "l".into(),
            created_at: 0,
            members: vec![m("old", "old", TeamRole::Teammate, Some(1)), m("a", "alice", TeamRole::Teammate, None), m("l", "lead", TeamRole::Leader, None)],
            tasks: vec![
                task(1, TaskStatus::InProgress, Some("a"), Some("alice")),
                task(2, TaskStatus::Done, Some("a"), Some("alice")),
                task(3, TaskStatus::Open, None, None),
            ],
        };
        let rows = team_member_rows(&State::new(), &team, "a");
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["lead", "alice", "old"]);
        assert!(rows[1].this && !rows[0].this && rows[2].ended);
        assert_eq!((rows[1].open_tasks, rows[1].status), (1, "Running"));
        assert_eq!(task_line(&team.tasks[0]), "#1 [in progress] task 1 \u{b7} alice");
        assert_eq!(task_line(&team.tasks[2]), "#3 [open] task 3 \u{b7} unassigned");
    }
}
