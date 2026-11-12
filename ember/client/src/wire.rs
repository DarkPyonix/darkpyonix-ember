//! The main server's wire types, as the client sees them (PR-1, `ember/server/src/{events,store}.rs`).
//!
//! Mirrored rather than shared so the client never links the server. Decoding is tolerant where
//! the server may grow: unknown event kinds and statuses decode to `Unknown` instead of failing,
//! so an additive server change never breaks an older client..

use serde::{Deserialize, Serialize};

/// OSE connection details delivered with an Ember editor launch.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmberOseConnection {
    pub endpoint: String,
    pub connection_token: String,
}

impl std::fmt::Debug for EmberOseConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmberOseConnection").field("endpoint", &self.endpoint)
            .field("connection_token", &"[REDACTED]").finish()
    }
}

/// Launch data for the native editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmberLaunch {
    pub session: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub computer: String,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ose: Option<EmberOseConnection>,
}

#[cfg(test)]
mod ide_launch_tests {
    use super::*;

    #[test]
    fn fr_l7_fr_l31_fr_w4_connection_round_trip() {
        let value = serde_json::json!({
            "session": "s", "project": "p", "computer": "local", "folder": "/w",
            "server_url": null,
            "ose": { "endpoint": "http://localhost:9000", "connection_token": "secret" }
        });
        let launch: EmberLaunch = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(launch.ose.as_ref().unwrap().connection_token, "secret");
        assert!(!format!("{launch:?}").contains("secret"));
        assert_eq!(serde_json::to_value(launch).unwrap(), value);
    }

    #[test]
    fn fr_l7_fr_l31_fr_w4_older_launch_has_no_connection() {
        let launch: EmberLaunch = serde_json::from_value(serde_json::json!({"session": "s"})).unwrap();
        assert!(launch.ose.is_none());
        assert!(serde_json::to_value(&launch).unwrap().get("ose").is_none());
    }
}

/// One thing that happened in a session (server `AgentEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    NativeSession { native_id: String },
    UserMessage { text: String },
    AssistantDelta { text: String },
    AssistantMessage { text: String },
    ToolCall { call_id: String, name: String, input: serde_json::Value },
    ToolResult { call_id: String, output: String, is_error: bool },
    ApprovalRequested { approval_id: String, tool: String, input: serde_json::Value },
    ApprovalResolved { approval_id: String, decision: ApprovalDecision },
    Usage { input_tokens: u64, output_tokens: u64 },
    TurnEnded { outcome: TurnOutcome },
    Error { message: String },
    /// Something Ember shows in the session, not part of the agent's conversation (an A2A
    /// message refused by loop protection, the outcome of a mention, a teammate ended).
    Notice { message: String },
    /// The agent asks the person one or more questions and waits (FR-T11, FR-A5).
    QuestionAsked { question_id: String, questions: Vec<Question> },
    /// A question was answered (`answers` empty: skipped). Values of a secret question arrive as
    /// `***`; the client never holds the typed secret after sending it.
    QuestionAnswered { question_id: String, #[serde(default)] answers: Vec<QuestionAnswer> },
    /// The agent withdrew an approval or a question that was still waiting (server
    /// `PromptWithdrawn`). Nothing can be answered for `prompt_id` any more.
    PromptWithdrawn { prompt_id: String },
    /// An event kind this client does not know yet. Ignored by the reducer.
    #[serde(other)]
    Unknown,
}

/// One question of a `QuestionAsked` event (server `Question`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    #[serde(default)]
    pub header: Option<String>,
    pub text: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub allow_other: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// The answer to one question. `Debug` never prints a value: an answer may be a password
/// (FR-T11).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub question_id: String,
    #[serde(default)]
    pub values: Vec<String>,
}

impl std::fmt::Debug for QuestionAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionAnswer")
            .field("question_id", &self.question_id)
            .field("values", &vec!["***"; self.values.len()])
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowAlways,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed,
}

/// Session status as stored on the server. The launcher's status adds "finished-unread" on top
/// (see [`crate::state::LauncherStatus`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForApproval,
    Finished,
    Failed,
    #[serde(other)]
    Unknown,
}

/// A session as the server lists it (server `SessionRecord`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub project: String,
    /// Agent kind (`claude-code`, `codex`, …). A string so new agents need no client change.
    pub agent: String,
    pub cwd: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub native_id: Option<String>,
    pub status: SessionStatus,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seq: i64,
    /// The account the session runs under (FR-U2); `None` = the server's own agent login.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Why that account was chosen (FR-U3).
    #[serde(default)]
    pub account_reason: Option<String>,
    /// FR-L9. Absent from servers older than the metadata API: `false`.
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
}

impl SessionRecord {
    /// Take the user-editable metadata (title, pin, archive) from `other`, keeping this
    /// record's activity fields. Returns whether anything changed.
    pub fn take_meta(&mut self, other: &SessionRecord) -> bool {
        let changed = self.title != other.title || self.pinned != other.pinned || self.archived != other.archived;
        self.title.clone_from(&other.title);
        self.pinned = other.pinned;
        self.archived = other.archived;
        changed
    }
}

/// A project and the computers assigned to it (server `projects::Project`, FR-L4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub created_at: i64,
    /// Assigned computer ids (`local` = the main server itself), sorted.
    #[serde(default)]
    pub computers: Vec<String>,
    /// Repository address (FR-L26); `None` when none is set or the server is older.
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Defaults for new members; `None` or `null` reads as no defaults.
    #[serde(default)]
    pub member_defaults: Option<MemberDefaults>,
    /// `mongle`, `malang`, `crew`, `pocket`, `cream` or `keyring`.
    #[serde(default)]
    pub character_style: Option<String>,
    #[serde(default)]
    pub archived: bool,
    /// Harness to account id (FR-L26); `null` for a harness whose account was deleted. `{}`, a
    /// missing field or `null` reads as no overrides.
    #[serde(default, deserialize_with = "null_as_default")]
    pub profile_overrides: std::collections::BTreeMap<String, Option<String>>,
    /// Per-member character overrides saved from the team character editor (FR-L22, #489).
    #[serde(default, deserialize_with = "null_as_default")]
    pub character_overrides: std::collections::BTreeMap<String, CharacterOverride>,
}

/// A saved character override for one member name in a project (FR-L22, #489).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silhouette: Option<usize>,
}

impl CharacterOverride {
    pub fn is_empty(&self) -> bool {
        *self == CharacterOverride::default()
    }
}

fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// New member defaults of a project (FR-L26): harness kind, model id and permission mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl MemberDefaults {
    pub fn is_empty(&self) -> bool {
        self.agent.is_none() && self.model.is_none() && self.mode.is_none()
    }
}

/// `PATCH /projects/{name}` body (FR-L26). `None` fields are omitted. For the text fields an
/// empty string clears; `member_defaults: Some(None)` sends `null` and clears. The client never
/// sends `leader_session` or `archived` (the server answers 400 `not_yet`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProjectPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_defaults: Option<Option<MemberDefaults>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_style: Option<String>,
    /// The whole map, harness to account id (`Some(map)` replaces the stored one, an empty map
    /// clears all). The field is never sent as `null` (the server answers 400).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_overrides: Option<std::collections::BTreeMap<String, Option<String>>>,
    /// Per-member character overrides (`Some(map)` replaces the stored one, an empty map clears
    /// all; FR-L22, #489).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_overrides: Option<std::collections::BTreeMap<String, Option<CharacterOverride>>>,
    /// Archive (`Some(true)`) or restore (`Some(false)`) the project (FR-L26, #384). Never `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

impl ProjectPatch {
    /// True when nothing changed, so there is nothing to send.
    pub fn is_empty(&self) -> bool {
        *self == ProjectPatch::default()
    }
}

/// `PATCH /sessions/{id}` body (FR-L9); `None` fields are left alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

/// `GET /usage` row (FR-U7): tokens one account reported on one UTC day. The server lists the
/// ChatGPT plan accounts (FR-U4) next to the agent accounts, keyed by account id.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DailyUsage {
    #[serde(default)]
    pub account_id: Option<String>,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Number of `usage` events (about one per turn).
    #[serde(default)]
    pub reports: u64,
}

impl DailyUsage {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// Start of a matched term in [`SearchHit::snippet`].
pub const SNIPPET_OPEN: char = '\u{ab}';
/// End of a matched term in [`SearchHit::snippet`].
pub const SNIPPET_CLOSE: char = '\u{bb}';

/// `GET /search` hit (FR-S4): one message, by session and sequence number.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SearchHit {
    pub session_id: String,
    pub seq: i64,
    /// `user_message` or `assistant_message`.
    #[serde(default)]
    pub kind: String,
    /// Text around the match; matched terms between [`SNIPPET_OPEN`] and [`SNIPPET_CLOSE`].
    pub snippet: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub archived: bool,
}

impl SearchHit {
    /// The snippet without match markers.
    pub fn plain_snippet(&self) -> String {
        self.snippet.chars().filter(|c| *c != SNIPPET_OPEN && *c != SNIPPET_CLOSE).collect()
    }
}

/// One stored event with its per-session sequence number (server `StoredEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub session_id: String,
    pub seq: i64,
    pub at: i64,
    pub event: AgentEvent,
}

/// `GET /sessions/{id}` response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionDetail {
    pub session: SessionRecord,
    pub live: bool,
    /// Whether the fork action is offered (FR-S5); disabled, not failing, when `false`.
    #[serde(default)]
    pub can_fork: bool,
    /// Sequence numbers of the completed turns a fork may start after (FR-S5), oldest first.
    #[serde(default)]
    pub fork_turns: Vec<i64>,
    /// Where this session was forked from, if it is a fork (FR-S5).
    #[serde(default)]
    pub fork_of: Option<ForkOrigin>,
    /// Whether the session is in approve-everything mode (FR-A5).
    #[serde(default)]
    pub approve_all: bool,
}

/// Where a fork came from (server `ForkOrigin`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ForkOrigin {
    pub parent_id: String,
    /// Sequence number, in the parent, of the `turn_ended` event the fork starts after.
    pub turn_seq: i64,
    #[serde(default)]
    pub forked_at: i64,
}

/// What the client keeps of `GET /sessions/{id}` for the fork action (FR-S5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForkInfo {
    pub can_fork: bool,
    pub turns: Vec<i64>,
    pub origin: Option<ForkOrigin>,
}

impl From<&SessionDetail> for ForkInfo {
    fn from(d: &SessionDetail) -> ForkInfo {
        ForkInfo { can_fork: d.can_fork, turns: d.fork_turns.clone(), origin: d.fork_of.clone() }
    }
}

/// `GET /agents` entry.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct DetectedAgent {
    pub kind: String,
    pub installed: bool,
    #[serde(default)]
    pub version: Option<String>,
    /// Where the binary was found; `None` when the server did not report one (FR-A6.1).
    #[serde(default)]
    pub path: Option<String>,
    /// When the server last probed (epoch ms); `None` when not reported.
    #[serde(default)]
    pub detected_at: Option<i64>,
}

/// `POST /sessions` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewSession {
    pub project: String,
    pub agent: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A decoded push message (server `Push`, plus the `lagged` notice the push loop sends itself).
#[derive(Debug, Clone, PartialEq)]
pub enum Push {
    SessionCreated { session: SessionRecord },
    Event { status: SessionStatus, event: StoredEvent },
    /// The server dropped `missed` messages for this client; it must resync from the store.
    Lagged { missed: u64 },
    /// A session's title, pin or archive mark changed (FR-L9). Only those fields are taken
    /// from `session`; events own the activity fields.
    SessionUpdated { session: SessionRecord },
    /// A project was created or its computer assignment changed (FR-L4).
    ProjectUpdated { project: Project },
    /// A project was deleted (FR-L26, #384): only its name.
    ProjectDeleted { name: String },
    /// A team's members or tasks changed (FR-T7): the whole team.
    TeamUpdated { team: TeamView },
    /// A session's approve-everything mode was switched (FR-A5).
    ApproveAll { session_id: String, enabled: bool },
    /// A message was posted to a channel (FR-T10, FR-L12).
    ChannelMessage { channel_id: String, message: crate::channels::ChannelMessage },
}

// ---- teams and mentions (FR-T6, FR-T7; server `a2a::team`, `a2a::mention`) -------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Leader,
    Teammate,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    InProgress,
    Blocked,
    Done,
    Cancelled,
    #[serde(other)]
    Unknown,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::InProgress => "in progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Unknown => "unknown",
        }
    }
}

/// A team member. `title`, `agent` and `status` are a snapshot from when the team last
/// changed; a UI takes live status from the session record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamMember {
    pub session_id: String,
    pub name: String,
    pub role: TeamRole,
    #[serde(default)]
    pub joined_at: i64,
    /// Set once the teammate was ended.
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub status: Option<SessionStatus>,
}

impl TeamMember {
    pub fn active(&self) -> bool {
        self.ended_at.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamTask {
    pub id: String,
    #[serde(default)]
    pub team_id: String,
    /// `#n` within the team.
    pub number: i64,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    pub status: TaskStatus,
    /// Assignee session id.
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub assignee_name: Option<String>,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

/// A team: leader first among `members`, then teammates (ended ones included); tasks by number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamView {
    pub id: String,
    #[serde(default)]
    pub project: String,
    /// The leader's session id.
    pub leader: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub members: Vec<TeamMember>,
    #[serde(default)]
    pub tasks: Vec<TeamTask>,
}

impl TeamView {
    pub fn member(&self, session_id: &str) -> Option<&TeamMember> {
        self.members.iter().find(|m| m.session_id == session_id)
    }
}

/// `GET /teams/{id}/mail` entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TeamMail {
    pub id: i64,
    #[serde(default)]
    pub team_id: String,
    pub from_session: String,
    #[serde(default)]
    pub from_name: Option<String>,
    /// `None` = the whole team.
    #[serde(default)]
    pub to_session: Option<String>,
    #[serde(default)]
    pub to_name: Option<String>,
    pub text: String,
    #[serde(default)]
    pub created_at: i64,
}

/// `GET /sessions/{id}/mentions` entry: a session the composer may mention as `@@...`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MentionCandidate {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub agent: String,
    pub status: SessionStatus,
}

// ---- schedules (FR-A8, server `schedules`) -----------------------------------------------

/// When a schedule fires (server `ScheduleKind`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleKind {
    /// A five-field cron expression evaluated in `tz` (IANA name).
    Cron { expr: String, tz: String },
    /// Every `seconds`.
    Interval { seconds: u64 },
    /// Once, at `at` (Unix ms).
    Once { at: i64 },
}

/// What each run does (server `ScheduleTarget`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleTarget {
    /// Send the prompt into this existing session.
    Continue { session_id: String },
    /// Start a new session and send the prompt there.
    New {
        #[serde(default)]
        title: Option<String>,
    },
}

/// One schedule (`GET /schedules`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub project: String,
    pub agent: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub computer_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub prompt: String,
    pub kind: ScheduleKind,
    pub target: ScheduleTarget,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub catch_up: bool,
    /// Next planned trigger (Unix ms); `None` when paused or a one-off is done.
    #[serde(default)]
    pub next_run_at: Option<i64>,
    #[serde(default)]
    pub last_run_at: Option<i64>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

/// How a run ended (server `RunStatus`); a status this client does not know decodes to `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
    Skipped,
    Missed,
    #[serde(other)]
    Unknown,
}

/// One run of a schedule (`GET /schedules/{id}/runs`, newest first).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ScheduleRun {
    pub id: String,
    pub schedule_id: String,
    /// The trigger time this run belongs to (Unix ms).
    pub scheduled_at: i64,
    pub status: RunStatus,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `POST /schedules` body. `agent` and `cwd` are required for `New` targets.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewSchedule {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub prompt: String,
    pub kind: ScheduleKind,
    pub target: ScheduleTarget,
    pub catch_up: bool,
    pub paused: bool,
}

/// `PATCH /schedules/{id}` body; `None` fields are left alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SchedulePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// For `new` sessions: `Some("")` clears `computer_id` to `None` (this server).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ScheduleTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ScheduleKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<bool>,
}

#[cfg(test)]
mod question_wire_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fr_t11_question_unknown_fields_and_old_rows_decode() {
        let ev: AgentEvent = serde_json::from_value(json!({
            "kind": "question_asked", "question_id": "r1", "future": 1,
            "questions": [{"id": "q0", "text": "Colour?", "extra": true}]
        }))
        .unwrap();
        let AgentEvent::QuestionAsked { question_id, questions } = ev else { panic!() };
        assert_eq!(question_id, "r1");
        let q = &questions[0];
        assert!(q.header.is_none() && q.options.is_empty());
        assert!(!q.multi_select && !q.secret && !q.allow_other);
        // An approval with the server's newer `needs_person` field still decodes.
        let ev: AgentEvent = serde_json::from_value(json!({
            "kind": "approval_requested", "approval_id": "a", "tool": "T", "input": {}, "needs_person": true
        }))
        .unwrap();
        assert!(matches!(ev, AgentEvent::ApprovalRequested { .. }));
    }

    #[test]
    fn fr_t11_question_answered_decodes_skipped_and_secret_stars() {
        let ev: AgentEvent =
            serde_json::from_value(json!({"kind": "question_answered", "question_id": "r1", "answers": []})).unwrap();
        assert!(matches!(ev, AgentEvent::QuestionAnswered { ref answers, .. } if answers.is_empty()));
        let ev: AgentEvent = serde_json::from_value(json!({
            "kind": "question_answered", "question_id": "r1",
            "answers": [{"question_id": "q0", "values": ["***"]}]
        }))
        .unwrap();
        assert!(matches!(ev, AgentEvent::QuestionAnswered { ref answers, .. } if answers[0].values == ["***"]));
    }

    #[test]
    fn fr_t11_question_answer_debug_never_prints_a_value() {
        let a = QuestionAnswer { question_id: "q0".into(), values: vec!["hunter2".into(), "x".into()] };
        let shown = format!("{a:?}");
        assert!(!shown.contains("hunter2") && !shown.contains("\"x\""), "{shown}");
        assert!(shown.contains("***"));
    }
}
