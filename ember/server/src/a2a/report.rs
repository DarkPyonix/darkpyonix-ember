//! Completion, failure, needs-input and stall reports to the delegating session (SPEC FR-T8,
//! #995). Design approved [leader, 2026-11-22].
//!
//! - **Delegators.** An active teammate reports every turn end to its leader. Any session also
//!   reports to the sessions whose A2A messages were delivered into it since its last report,
//!   except a reply (`--reply-to`), a report, a person's channel message, and a teammate's message
//!   to its own leader (a teammate does not delegate to its leader). A turn woken only by reports,
//!   or started by the user outside a team, reports to nobody, so reports do not beget reports.
//! - **Triggers.** A turn end, once the session stays idle for the settle time (an automatic
//!   follow-up turn reports once at its real end); an approval or question still open after the
//!   settle time (once per prompt); no event for the stall time while running (once per stall).
//! - **The report** is built by the server from the stored turn: outcome, error, last reply,
//!   files changed, commits and pull requests, and the transcript range. It is stored as an A2A
//!   message of kind `report` and counts toward loop protection (FR-T3).
//! - **Wake.** A done report waits the batch time before it wakes the receiver, so reports that
//!   arrive together start one turn; any other report wakes at once. A busy receiver gets its
//!   reports batched at its turn end: mid-turn delivery needs per-harness hooks (#997).
//! - **Acknowledgement.** A report delivered into a turn that fails is queued again, once.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use super::store::CHANNEL_SENDER_PREFIX;
use super::{is_busy, A2a, A2aMessage};
use crate::events::{AgentEvent, SessionStatus, TurnOutcome};
use crate::store::{now_ms, StoredEvent};

/// Characters of the last reply kept in a report.
pub const SUMMARY_CHARS: usize = 600;
/// Files and commands listed in a report at most.
pub const MAX_LISTED: usize = 20;

/// Timings of FR-T8 reports.
#[derive(Debug, Clone, Copy)]
pub struct ReportTimes {
    /// How long a session must stay idle after a turn end before the turn is reported, and how
    /// long an approval or question must stay open before it is reported.
    pub settle: Duration,
    /// How long a done report waits before it wakes the receiver.
    pub batch: Duration,
    /// No event for this long while running is a stall (`EMBER_A2A_STALL_SECS`, default 600).
    pub stall: Duration,
    /// How often running sessions are checked for a stall.
    pub stall_check: Duration,
}

impl Default for ReportTimes {
    fn default() -> ReportTimes {
        let stall = std::env::var("EMBER_A2A_STALL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(600);
        ReportTimes {
            settle: Duration::from_secs(2),
            batch: Duration::from_secs(2),
            stall: Duration::from_secs(stall),
            stall_check: Duration::from_secs(30),
        }
    }
}

/// Where and as whom a session ran, for the report (#82 P3). Each `None` leaves its line out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnContext {
    /// The name of the computer the session is on now (the main server is "this server").
    pub computer: Option<String>,
    /// The account's display label, never its id.
    pub account_label: Option<String>,
}

/// Looks up a session's [`TurnContext`]; installed by `main.rs` from the computers and the
/// accounts (FR-T8 report fields of #82 P3).
pub type ContextLookup = Arc<dyn Fn(&crate::store::SessionRecord) -> TurnContext + Send + Sync>;

/// The place lines of one report: [`TurnContext`] plus the computer of the previous report when
/// it differs (a move happens only between turns, FR-X3, so it is "since the last report").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    pub computer: Option<String>,
    pub moved_from: Option<String>,
    pub account_label: Option<String>,
}

/// What the reporter remembers per session.
#[derive(Debug, Default)]
pub struct ReportState {
    /// Delegating senders delivered into the session since its last turn-end report.
    senders: HashMap<String, BTreeSet<String>>,
    /// Report ids delivered into the receiver's current turn (re-queued if that turn fails).
    inflight: HashMap<String, Vec<String>>,
    /// Report ids already re-queued once.
    requeued: HashSet<String>,
    /// The last event seq a turn-end report covered.
    reported_upto: HashMap<String, i64>,
    /// Approval and question ids already reported as needs-input.
    waiting_sent: HashSet<String>,
    /// The `last_seq` at which a stall was reported, per session.
    stalled_at: HashMap<String, i64>,
    /// The computer named in the session's last report (#82 P3).
    last_computer: HashMap<String, String>,
}

/// How a report's turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Failed,
    Interrupted,
    /// Waiting on a person: what for.
    NeedsInput(String),
    /// No event for this many seconds.
    Stalled(u64),
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Failed => "failed",
            Outcome::Interrupted => "interrupted",
            Outcome::NeedsInput(_) => "needs input",
            Outcome::Stalled(_) => "stalled",
        }
    }

    /// Only a done report waits for the batch window.
    fn urgent(&self) -> bool {
        !matches!(self, Outcome::Done)
    }
}

/// The report text, built from the events of the reported span (oldest first).
pub fn build_report(session_id: &str, title: &str, outcome: &Outcome, place: &Place, events: &[StoredEvent]) -> String {
    let first = events.first().map(|e| e.seq).unwrap_or(0);
    let last = events.last().map(|e| e.seq).unwrap_or(0);
    let mut out = format!("Report: \"{title}\" (session {session_id}) {}.\n", match outcome {
        Outcome::Done => "finished its turn",
        Outcome::Failed => "ended its turn with a failure",
        Outcome::Interrupted => "was interrupted",
        Outcome::NeedsInput(_) => "is waiting on a person",
        Outcome::Stalled(_) => "has made no progress",
    });
    out.push_str(&format!("Outcome: {}\n", outcome.label()));
    match outcome {
        Outcome::NeedsInput(what) => out.push_str(&format!("Waiting on: {what}\n")),
        Outcome::Stalled(secs) if *secs < 60 => out.push_str(&format!("No event for {secs} s while running\n")),
        Outcome::Stalled(secs) => out.push_str(&format!("No event for {} min while running\n", secs.div_ceil(60))),
        _ => {}
    }
    if let Some(c) = &place.computer {
        match &place.moved_from {
            Some(from) => out.push_str(&format!("Computer: {c} (moved from {from} since the last report)\n")),
            None => out.push_str(&format!("Computer: {c}\n")),
        }
    }
    if let Some(a) = &place.account_label {
        out.push_str(&format!("Account: {a}\n"));
    }
    if let Some(err) = last_error(events) {
        out.push_str(&format!("Error: {}\n", clip(&err, 300)));
    }
    match last_reply(events) {
        Some(text) => out.push_str(&format!("Last reply: {}\n", clip(&text, SUMMARY_CHARS))),
        None => out.push_str("Last reply: none\n"),
    }
    let files = files_changed(events);
    if files.is_empty() {
        out.push_str("Files changed: none recorded\n");
    } else {
        let more = files.len().saturating_sub(MAX_LISTED);
        let mut line = files.iter().take(MAX_LISTED).cloned().collect::<Vec<_>>().join(", ");
        if more > 0 {
            line.push_str(&format!(" and {more} more"));
        }
        out.push_str(&format!("Files changed: {line}\n"));
    }
    let commits = commits(events);
    if !commits.is_empty() {
        let lines: Vec<String> = commits.iter().take(MAX_LISTED).map(|c| clip(c, 160)).collect();
        out.push_str(&format!("Commits: {}\n", lines.join(" | ")));
    }
    let prs = pull_requests(events);
    if !prs.is_empty() {
        out.push_str(&format!("Pull requests: {}\n", prs.join(", ")));
    }
    out.push_str(&format!("Transcript: session {session_id}, events {first} to {last}"));
    out
}

fn clip(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let mut cut: String = one_line.chars().take(max).collect();
    cut.push_str("...");
    cut
}

fn last_reply(events: &[StoredEvent]) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.event {
        AgentEvent::AssistantMessage { text } if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    })
}

fn last_error(events: &[StoredEvent]) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.event {
        AgentEvent::TurnError { message, .. } | AgentEvent::Error { message } => Some(message.clone()),
        _ => None,
    })
}

/// Tools whose input names a file they change: Claude Code's edit tools (`file_path`,
/// `notebook_path`) and Codex's `file_change` (`changes[].path`).
fn changed_paths(name: &str, input: &serde_json::Value) -> Vec<String> {
    match name {
        "Edit" | "Write" | "MultiEdit" => input["file_path"].as_str().map(String::from).into_iter().collect(),
        "NotebookEdit" => input["notebook_path"].as_str().map(String::from).into_iter().collect(),
        "file_change" => input["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["path"].as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

/// Call ids whose result was not an error (a call without a result counts as done).
fn failed_calls(events: &[StoredEvent]) -> HashSet<&str> {
    events
        .iter()
        .filter_map(|e| match &e.event {
            AgentEvent::ToolResult { call_id, is_error: true, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect()
}

/// Files the span's successful tool calls changed, each once, in order.
pub fn files_changed(events: &[StoredEvent]) -> Vec<String> {
    let failed = failed_calls(events);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in events {
        if let AgentEvent::ToolCall { call_id, name, input } = &e.event {
            if failed.contains(call_id.as_str()) {
                continue;
            }
            for p in changed_paths(name, input) {
                if seen.insert(p.clone()) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Successful shell commands of the span that ran `git commit` (Claude Code `Bash`, Codex
/// `command`).
pub fn commits(events: &[StoredEvent]) -> Vec<String> {
    let failed = failed_calls(events);
    events
        .iter()
        .filter_map(|e| match &e.event {
            AgentEvent::ToolCall { call_id, name, input } if (name == "Bash" || name == "command") && !failed.contains(call_id.as_str()) => {
                input["command"].as_str().filter(|c| c.contains("git commit")).map(String::from)
            }
            _ => None,
        })
        .collect()
}

/// GitHub pull request URLs in the span's tool results and replies, each once.
pub fn pull_requests(events: &[StoredEvent]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        let text = match &e.event {
            AgentEvent::ToolResult { output, is_error: false, .. } => output.as_str(),
            AgentEvent::AssistantMessage { text } => text.as_str(),
            _ => continue,
        };
        for url in pr_urls(text) {
            if !out.contains(&url) {
                out.push(url);
            }
        }
    }
    out
}

fn pr_urls(text: &str) -> Vec<String> {
    const START: &str = "https://github.com/";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(START) {
        let tail = &rest[i + START.len()..];
        let parts: Vec<&str> = tail.splitn(4, '/').collect();
        if parts.len() >= 4 && parts[2] == "pull" {
            let number: String = parts[3].chars().take_while(|c| c.is_ascii_digit()).collect();
            let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
            if !number.is_empty() && ok(parts[0]) && ok(parts[1]) {
                out.push(format!("{START}{}/{}/pull/{number}", parts[0], parts[1]));
            }
        }
        rest = tail;
    }
    out
}

impl A2a {
    /// Remember who delegated work by the messages just delivered into `target`, and which of
    /// them were reports (FR-T8). Called by `flush` after a successful send.
    pub(super) fn note_delivered(&self, target: &str, delivered: &[A2aMessage]) {
        let leader_of_target = self.leader_of(target);
        let mut st = self.report_state.lock().unwrap();
        let mut reports = Vec::new();
        for m in delivered {
            if m.is_report() {
                reports.push(m.id.clone());
                continue;
            }
            let from = m.from_session.as_str();
            let teammate_to_its_leader = self.leader_of(from).as_deref() == Some(target);
            if m.reply_to.is_some() || from == target || from.starts_with(CHANNEL_SENDER_PREFIX) || teammate_to_its_leader {
                continue;
            }
            if leader_of_target.as_deref() == Some(from) {
                continue; // the leader is a delegator of its teammate anyway
            }
            st.senders.entry(target.to_string()).or_default().insert(from.to_string());
        }
        st.inflight.insert(target.to_string(), reports);
    }

    /// The leader of `session` when it is an active teammate.
    fn leader_of(&self, session: &str) -> Option<String> {
        let store = self.sessions.store();
        let m = store.team_member(session).ok().flatten().filter(|m| m.is_active_teammate())?;
        let team = store.team_row(&m.team_id).ok().flatten()?;
        (team.leader_session != session).then_some(team.leader_session)
    }

    /// Who `session` reports to; `take` drops the message senders (a turn-end report).
    fn delegators(&self, session: &str, take: bool) -> Vec<String> {
        let mut out: BTreeSet<String> = {
            let mut st = self.report_state.lock().unwrap();
            if take { st.senders.remove(session).unwrap_or_default() } else { st.senders.get(session).cloned().unwrap_or_default() }
        };
        if let Some(l) = self.leader_of(session) {
            out.insert(l);
        }
        out.remove(session);
        out.into_iter().collect()
    }

    fn title_of(&self, session: &str) -> String {
        self.sessions.store().session(session).ok().flatten().map(|r| r.title).unwrap_or_default()
    }

    /// The place lines for a report of `session` now, remembering its computer for the next.
    fn place_of(&self, session: &str) -> Place {
        let lookup = self.report_context.lock().unwrap().clone();
        let Some(lookup) = lookup else { return Place::default() };
        let Ok(Some(rec)) = self.sessions.store().session(session) else { return Place::default() };
        let ctx = lookup(&rec);
        let moved_from = ctx.computer.as_ref().and_then(|now| {
            let mut st = self.report_state.lock().unwrap();
            let before = st.last_computer.insert(session.to_string(), now.clone());
            before.filter(|b| b != now)
        });
        Place { computer: ctx.computer, moved_from, account_label: ctx.account_label }
    }

    /// Store one report and wake the receiver: at once, or after the batch time for a done one.
    async fn send_report(self: &Arc<Self>, from: &str, to: &str, text: &str, urgent: bool) {
        {
            let _guard = self.send_lock.lock().await;
            if self.check_limits(from, &[to.to_string()]).is_err() {
                return;
            }
            if let Err(e) = self.store.insert_report(from, to, text) {
                tracing::warn!(from = %from, to = %to, "storing an FR-T8 report failed: {e:#}");
                return;
            }
        }
        if urgent {
            self.flush_logged(to).await;
        } else {
            let (this, to, wait) = (self.clone(), to.to_string(), self.config.reports.batch);
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                this.flush_logged(&to).await;
            });
        }
    }

    /// A turn of `session` ended at `end_seq` (FR-T8).
    pub(super) async fn on_turn_ended(self: &Arc<Self>, session: &str, end_seq: i64, outcome: TurnOutcome) {
        // A report delivered into a failed turn goes back in the queue, once.
        let inflight = self.report_state.lock().unwrap().inflight.remove(session).unwrap_or_default();
        if outcome == TurnOutcome::Failed && !inflight.is_empty() {
            let again: Vec<String> = {
                let mut st = self.report_state.lock().unwrap();
                inflight.into_iter().filter(|id| st.requeued.insert(id.clone())).collect()
            };
            if !again.is_empty() {
                match self.store.mark_undelivered(&again) {
                    Ok(()) => self.flush_logged(session).await,
                    Err(e) => tracing::warn!(session = %session, "re-queueing FR-T8 reports failed: {e:#}"),
                }
            }
        }
        tokio::time::sleep(self.config.reports.settle).await;
        let store = self.sessions.store();
        let Ok(Some(rec)) = store.session(session) else { return };
        // A follow-up turn started, or ended later: that one reports.
        if is_busy(rec.status) {
            return;
        }
        let from = self.report_state.lock().unwrap().reported_upto.get(session).copied().unwrap_or(0);
        let Ok(events) = store.events_after(session, from) else { return };
        // A later turn end reports instead, and so does a follow-up turn still running (an
        // automatic continuation has turn activity after this end but no user message).
        let continued = events.iter().any(|e| {
            e.seq > end_seq
                && matches!(
                    e.event,
                    AgentEvent::TurnEnded { .. }
                        | AgentEvent::UserMessage { .. }
                        | AgentEvent::ToolCall { .. }
                        | AgentEvent::AssistantDelta { .. }
                        | AgentEvent::AssistantMessage { .. }
                )
        });
        if continued {
            return;
        }
        let upto = events.last().map(|e| e.seq).unwrap_or(end_seq);
        self.report_state.lock().unwrap().reported_upto.insert(session.to_string(), upto);
        let to = self.delegators(session, true);
        if to.is_empty() {
            return;
        }
        let outcome = match outcome {
            TurnOutcome::Completed => Outcome::Done,
            TurnOutcome::Failed => Outcome::Failed,
            TurnOutcome::Interrupted => Outcome::Interrupted,
        };
        let text = build_report(session, &rec.title, &outcome, &self.place_of(session), &events);
        for d in to {
            self.send_report(session, &d, &text, outcome.urgent()).await;
        }
    }

    /// An approval or question (`prompt_id`) was asked at `seq`; report it when it is still open
    /// after the settle time (FR-T8 needs input), once per prompt.
    pub(super) async fn on_prompt(self: &Arc<Self>, session: &str, prompt_id: &str, seq: i64, what: String) {
        tokio::time::sleep(self.config.reports.settle).await;
        let Ok(later) = self.sessions.store().events_after(session, seq) else { return };
        let answered = later.iter().any(|e| match &e.event {
            AgentEvent::ApprovalResolved { approval_id, .. } => approval_id == prompt_id,
            AgentEvent::QuestionAnswered { question_id, .. } => question_id == prompt_id,
            AgentEvent::PromptWithdrawn { prompt_id: p } => p == prompt_id,
            AgentEvent::TurnEnded { .. } => true,
            _ => false,
        });
        if answered || !self.report_state.lock().unwrap().waiting_sent.insert(prompt_id.to_string()) {
            return;
        }
        let to = self.delegators(session, false);
        if to.is_empty() {
            return;
        }
        let from = self.report_state.lock().unwrap().reported_upto.get(session).copied().unwrap_or(0);
        let events = self.sessions.store().events_after(session, from).unwrap_or_default();
        let text = build_report(session, &self.title_of(session), &Outcome::NeedsInput(what), &self.place_of(session), &events);
        for d in to {
            self.send_report(session, &d, &text, true).await;
        }
    }

    /// Report sessions that have delegators and no event for the stall time while running.
    pub(super) async fn check_stalls(self: &Arc<Self>) {
        let mut candidates: BTreeSet<String> = self.report_state.lock().unwrap().senders.keys().cloned().collect();
        if let Ok(members) = self.sessions.store().active_team_members() {
            candidates.extend(members.into_iter().filter(|(_, m)| m.is_active_teammate()).map(|(id, _)| id));
        }
        let stall = self.config.reports.stall;
        for sid in candidates {
            let Ok(Some(rec)) = self.sessions.store().session(&sid) else { continue };
            if rec.status != SessionStatus::Running {
                continue;
            }
            if self.report_state.lock().unwrap().stalled_at.get(&sid) == Some(&rec.last_seq) {
                continue;
            }
            let last_at = self
                .sessions
                .store()
                .events_after(&sid, rec.last_seq - 1)
                .ok()
                .and_then(|evs| evs.last().map(|e| e.at))
                .unwrap_or(rec.updated_at);
            let idle_ms = now_ms() - last_at;
            if idle_ms < stall.as_millis() as i64 {
                continue;
            }
            let to = self.delegators(&sid, false);
            self.report_state.lock().unwrap().stalled_at.insert(sid.clone(), rec.last_seq);
            if to.is_empty() {
                continue;
            }
            let from = self.report_state.lock().unwrap().reported_upto.get(&sid).copied().unwrap_or(0);
            let events = self.sessions.store().events_after(&sid, from).unwrap_or_default();
            let text = build_report(&sid, &rec.title, &Outcome::Stalled((idle_ms / 1000) as u64), &self.place_of(&sid), &events);
            for d in to {
                self.send_report(&sid, &d, &text, true).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: i64, event: AgentEvent) -> StoredEvent {
        StoredEvent { session_id: "s".into(), seq, at: 0, event }
    }

    fn call(seq: i64, id: &str, name: &str, input: serde_json::Value) -> StoredEvent {
        ev(seq, AgentEvent::ToolCall { call_id: id.into(), name: name.into(), input })
    }

    fn result(seq: i64, id: &str, output: &str, is_error: bool) -> StoredEvent {
        ev(seq, AgentEvent::ToolResult { call_id: id.into(), output: output.into(), is_error })
    }

    #[test]
    fn fr_t8_report_lists_files_commits_and_pull_requests_of_successful_calls() {
        let events = vec![
            ev(3, AgentEvent::UserMessage { text: "go".into() }),
            call(4, "c1", "Edit", json!({ "file_path": "/w/a.rs" })),
            result(5, "c1", "ok", false),
            call(6, "c2", "Write", json!({ "file_path": "/w/b.rs" })),
            result(7, "c2", "denied", true),
            call(8, "c3", "file_change", json!({ "changes": [{ "path": "/w/c.rs" }, { "path": "/w/a.rs" }] })),
            call(9, "c4", "Bash", json!({ "command": "git commit -m 'Fix: x'" })),
            result(10, "c4", "[feat/x 1a2b3c] Fix: x", false),
            call(11, "c5", "command", json!({ "command": "gh pr create --fill" })),
            result(12, "c5", "https://github.com/acme/repo/pull/42\n", false),
            ev(13, AgentEvent::AssistantMessage { text: "Done.\nOpened https://github.com/acme/repo/pull/42 for review.".into() }),
            ev(14, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }),
        ];
        assert_eq!(files_changed(&events), vec!["/w/a.rs", "/w/c.rs"]);
        assert_eq!(commits(&events), vec!["git commit -m 'Fix: x'"]);
        assert_eq!(pull_requests(&events), vec!["https://github.com/acme/repo/pull/42"]);
        let text = build_report("s1", "alice", &Outcome::Done, &Place::default(), &events);
        assert!(text.starts_with("Report: \"alice\" (session s1) finished its turn.\nOutcome: done\n"), "{text}");
        assert!(text.contains("Last reply: Done. Opened https://github.com/acme/repo/pull/42 for review.\n"), "{text}");
        assert!(text.contains("Files changed: /w/a.rs, /w/c.rs\n"), "{text}");
        assert!(text.contains("Commits: git commit -m 'Fix: x'\n"), "{text}");
        assert!(text.contains("Pull requests: https://github.com/acme/repo/pull/42\n"), "{text}");
        assert!(text.ends_with("Transcript: session s1, events 3 to 14"), "{text}");
        assert!(!text.contains("Error:"), "{text}");
    }

    #[test]
    fn fr_t8_failed_needs_input_and_stalled_reports_say_why() {
        let events = vec![
            ev(1, AgentEvent::AssistantMessage { text: "x".repeat(SUMMARY_CHARS + 50) }),
            ev(2, AgentEvent::TurnError { code: "rate_limit".into(), message: "usage limit reached".into() }),
        ];
        let failed = build_report("s", "t", &Outcome::Failed, &Place::default(), &events);
        assert!(failed.contains("Outcome: failed\nError: usage limit reached\n"), "{failed}");
        let summary = failed.lines().find(|l| l.starts_with("Last reply: ")).unwrap();
        assert_eq!(summary.chars().count(), "Last reply: ".len() + SUMMARY_CHARS + 3);
        let waiting = build_report("s", "t", &Outcome::NeedsInput("approval for Bash".into()), &Place::default(), &[]);
        assert!(waiting.contains("is waiting on a person.\nOutcome: needs input\nWaiting on: approval for Bash\n"), "{waiting}");
        assert!(waiting.contains("Last reply: none\nFiles changed: none recorded\n"), "{waiting}");
        let stalled = build_report("s", "t", &Outcome::Stalled(601), &Place::default(), &[]);
        assert!(stalled.contains("Outcome: stalled\nNo event for 11 min while running\n"), "{stalled}");
    }

    // #82 P3: the report names the computer, a move since the last report and the account label.
    #[test]
    fn fr_t8_report_names_the_computer_and_account() {
        let place = Place { computer: Some("pi".into()), moved_from: None, account_label: Some("Work (Codex)".into()) };
        let text = build_report("s", "t", &Outcome::Done, &place, &[]);
        assert!(text.contains("Outcome: done\nComputer: pi\nAccount: Work (Codex)\n"), "{text}");
        let moved = Place { computer: Some("this server".into()), moved_from: Some("pi".into()), account_label: None };
        let text = build_report("s", "t", &Outcome::Done, &moved, &[]);
        assert!(text.contains("Computer: this server (moved from pi since the last report)\n"), "{text}");
        assert!(!text.contains("Account:"), "{text}");
        let none = build_report("s", "t", &Outcome::Done, &Place::default(), &[]);
        assert!(!none.contains("Computer:") && !none.contains("Account:"), "{none}");
    }

    #[test]
    fn fr_t8_pull_request_urls_need_owner_repo_and_number() {
        assert_eq!(pr_urls("see https://github.com/a/b/pull/7."), vec!["https://github.com/a/b/pull/7"]);
        assert!(pr_urls("https://github.com/a/b/issues/7 https://github.com/a/pull/ https://github.com/a/b/pull/x").is_empty());
    }
}
