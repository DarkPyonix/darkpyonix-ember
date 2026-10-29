//! Demo only (#199): the data ember-app serves itself when there is no server.
//!
//! Replaced when a server is given. It is on only when `EMBER_DEMO_DATA=1` and `EMBER_SERVER_URL`
//! is unset, so a single Windows PC without ember-server (the server and node are not Windows
//! targets yet) can still show every screen. [`active`] is the one place that decides between
//! this source and the server; [`crate::services::Services::demo`] holds the answer.
//!
//! The source fills the same places the server fills: the client's [`State`](ember_client::State)
//! through [`Input`]s (projects, sessions, computers, transcripts) and the bridge's live signals
//! (accounts, ChatGPT plan account, usage, agents). Routines and agent settings already fall back
//! to their local stores when no server answers. Actions that would reach the server (send a
//! message, answer an approval, create or rename a session) change this state instead, so
//! nothing waits for a server that is not there.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::json;

use ember_client::state::{Computer, ConnectionState, Input, Reachability};
use ember_client::wire::{
    AgentEvent, ApprovalDecision, DailyUsage, DetectedAgent, Project, Push, SessionPatch, SessionRecord,
    SessionStatus, StoredEvent, TurnOutcome,
};
use ember_client::Client;

use crate::server::{Account, ChatGptAccount, CreateSession};

/// The switch the demo launch sets (the server and the editor read the same one).
pub const DEMO_ENV: &str = "EMBER_DEMO_DATA";
/// The fixed id of the demo ChatGPT plan account (the same as the server's demo seed).
pub const CHATGPT_ID: &str = "demo-chatgpt-plan";
/// How many days of usage the demo carries.
pub const DAYS: i64 = 14;
/// The line the toast shows for actions the demo cannot do.
pub const DEMO_NOTICE: &str = "데모 모드입니다";

const DAY_MS: i64 = 86_400_000;
const APPROVAL_ID: &str = "demo-approval-1";

/// The toast for actions that need a server.
pub const UNAVAILABLE: &str = "데모에서는 쓸 수 없습니다";

/// The message to show instead of calling the server: `Some` in the demo, `None` otherwise.
/// Pure, for tests.
pub fn unavailable(demo_active: bool) -> Option<&'static str> {
    demo_active.then_some(UNAVAILABLE)
}

/// The IDE list the demo serves itself (#218): Ember 에디터 opens the session's folder, the
/// other two say they are not available. Pure, for tests.
pub fn ide_launch(session: &str, project: &str, cwd: &str) -> crate::server::IdeLaunch {
    use crate::server::{IdeComputer, IdeLaunch, IdeTarget};
    let target = |kind: &str, available: bool| IdeTarget {
        kind: kind.to_string(),
        available,
        url: None,
        command: None,
        launch: None,
        reason: (!available).then(|| UNAVAILABLE.to_string()),
    };
    IdeLaunch {
        session: session.to_string(),
        project: project.to_string(),
        folder: cwd.to_string(),
        computer: IdeComputer { name: "이 컴퓨터".to_string(), local: true },
        targets: vec![target("ember", true), target("vscode", false), target("gateway", false)],
    }
}

/// The name for a new demo project: trimmed, not empty, not already taken. Pure, for tests.
pub fn new_project_name(raw: &str, existing: &[String]) -> Result<String, &'static str> {
    let name = raw.trim();
    if name.is_empty() {
        Err("프로젝트 이름을 입력하세요")
    } else if existing.iter().any(|e| e == name) {
        Err("같은 이름의 프로젝트가 이미 있습니다")
    } else {
        Ok(name.to_string())
    }
}

/// Whether the demo source is used: the switch is `1` and no server URL is given (an empty one
/// counts as not given, like `AppConfig`). Pure, for tests.
pub fn active(demo_switch: Option<&str>, server_url: Option<&str>) -> bool {
    demo_switch == Some("1") && server_url.map(str::trim).is_none_or(str::is_empty)
}

/// [`active`] read from the environment.
pub fn active_from_env() -> bool {
    active(
        std::env::var(DEMO_ENV).ok().as_deref(),
        std::env::var("EMBER_SERVER_URL").ok().as_deref(),
    )
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD` (UTC) of a time in milliseconds.
pub fn day_string(ms: i64) -> String {
    // Civil-from-days (Howard Hinnant's algorithm), so no date crate is needed.
    let z = ms.div_euclid(DAY_MS) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// 14 days of usage for the demo account, today first, same rows as the server's demo seed.
pub fn usage(now: i64) -> Vec<DailyUsage> {
    (0..DAYS)
        .map(|ago| {
            let wave = ((ago * 5 + 3) % 7) as u64 + 1;
            let input = 40_000 * wave + 1_500 * ago as u64;
            DailyUsage {
                account_id: Some(CHATGPT_ID.to_string()),
                day: day_string(now - ago * DAY_MS),
                input_tokens: input,
                output_tokens: input / 4,
                reports: 3 + wave,
            }
        })
        .collect()
}

pub fn projects() -> Vec<Project> {
    vec![
        Project { name: "demo-project".into(), created_at: 0, computers: vec!["local".into()] },
        Project { name: "second-project".into(), created_at: 0, computers: vec!["local".into()] },
    ]
}

pub fn computers() -> Vec<Computer> {
    vec![
        Computer {
            id: "local".into(),
            name: "이 컴퓨터".into(),
            reachability: Reachability::Online,
            projects: vec!["demo-project".into(), "second-project".into()],
        },
        Computer {
            id: "studio".into(),
            name: "스튜디오 PC".into(),
            reachability: Reachability::Offline,
            projects: Vec::new(),
        },
    ]
}

pub fn agents() -> Vec<DetectedAgent> {
    vec![
        DetectedAgent { kind: "claude-code".into(), installed: true, version: Some("2.1.0".into()) },
        DetectedAgent { kind: "codex".into(), installed: true, version: Some("0.50.0".into()) },
        DetectedAgent { kind: "antigravity".into(), installed: false, version: None },
    ]
}

pub fn accounts() -> Vec<Account> {
    vec![
        Account {
            id: "demo-claude".into(),
            agent: "claude-code".into(),
            label: "Claude (demo)".into(),
            status: "signed_in".into(),
            is_default: true,
            limited: false,
        },
        Account {
            id: "demo-codex".into(),
            agent: "codex".into(),
            label: "Codex (demo)".into(),
            status: "signed_in".into(),
            is_default: false,
            limited: false,
        },
    ]
}

pub fn chatgpt_accounts(now: i64) -> Vec<ChatGptAccount> {
    let today = usage(now).first().map(|u| u.input_tokens + u.output_tokens).unwrap_or(0);
    vec![ChatGptAccount {
        id: CHATGPT_ID.into(),
        label: "ChatGPT Plus (demo)".into(),
        email: Some("demo@example.com".into()),
        status: "signed_in".into(),
        limited: false,
        limited_until: None,
        limit_reason: None,
        tokens_today: today,
        weekly_cap: None,
    }]
}

fn record(id: &str, project: &str, agent: &str, title: &str, status: SessionStatus, now: i64, last_seq: i64) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        project: project.into(),
        agent: agent.into(),
        cwd: format!("C:/demo/{project}"),
        model: None,
        native_id: None,
        status,
        title: title.into(),
        created_at: now - 3_600_000,
        updated_at: now,
        last_seq,
        account_id: None,
        account_reason: None,
        pinned: false,
        archived: false,
    }
}

fn stored(id: &str, events: Vec<AgentEvent>, now: i64) -> Vec<StoredEvent> {
    let n = events.len() as i64;
    events
        .into_iter()
        .enumerate()
        .map(|(i, event)| StoredEvent {
            session_id: id.into(),
            seq: i as i64 + 1,
            at: now - (n - i as i64) * 1_000,
            event,
        })
        .collect()
}

/// The scripted conversation of the first session: a tool call that ran, then a `Bash`
/// approval that waits for the user.
fn script_waiting(now: i64) -> Vec<StoredEvent> {
    stored(
        "demo-s1",
        vec![
            AgentEvent::UserMessage { text: "src/config.rs 에 포트 설정을 추가하고 테스트를 돌려 줘".into() },
            AgentEvent::AssistantMessage { text: "먼저 설정 파일을 읽어 보겠습니다.".into() },
            AgentEvent::ToolCall {
                call_id: "c1".into(),
                name: "Read".into(),
                input: json!({ "file_path": "src/config.rs" }),
            },
            AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: "pub struct Config {\n    pub port: u16,\n}\n".into(),
                is_error: false,
            },
            AgentEvent::AssistantMessage { text: "포트 필드가 이미 있습니다. 테스트를 실행해 확인하겠습니다.".into() },
            AgentEvent::ApprovalRequested {
                approval_id: APPROVAL_ID.into(),
                tool: "Bash".into(),
                input: json!({ "command": "cargo test --locked", "description": "Run the tests" }),
            },
        ],
        now,
    )
}

fn script_done(now: i64) -> Vec<StoredEvent> {
    stored(
        "demo-s2",
        vec![
            AgentEvent::UserMessage { text: "README 에 실행 방법을 한 줄 추가해 줘".into() },
            AgentEvent::AssistantMessage { text: "README.md 에 실행 방법을 추가했습니다.".into() },
            AgentEvent::Usage { input_tokens: 1_200, output_tokens: 300 },
            AgentEvent::TurnEnded { outcome: TurnOutcome::Completed },
        ],
        now,
    )
}

/// The demo source: the scripted data and the transcripts it keeps, so a conversation opened
/// again shows what happened in it.
pub struct DemoSource {
    events: Mutex<HashMap<String, Vec<StoredEvent>>>,
    created: Mutex<u32>,
}

impl DemoSource {
    pub fn new(now: i64) -> DemoSource {
        let mut events = HashMap::new();
        events.insert("demo-s1".to_string(), script_waiting(now));
        events.insert("demo-s2".to_string(), script_done(now - 600_000));
        DemoSource { events: Mutex::new(events), created: Mutex::new(0) }
    }

    /// The sessions, as the server would list them.
    pub fn sessions(&self, now: i64) -> Vec<SessionRecord> {
        let ev = self.events.lock().unwrap();
        let n = |id: &str| ev.get(id).map_or(0, |v| v.len() as i64);
        vec![
            record("demo-s1", "demo-project", "claude-code", "포트 설정 추가", SessionStatus::WaitingForApproval, now, n("demo-s1")),
            record("demo-s2", "demo-project", "codex", "README 정리", SessionStatus::Finished, now - 600_000, n("demo-s2")),
            // A fuller team in the second project, so the box's "외 N명" deck, the pinned chips
            // and every status group show in review captures (no transcripts).
            SessionRecord { pinned: true, ..record("demo-s3", "second-project", "codex", "빌드 스크립트 점검", SessionStatus::Running, now - 60_000, 0) },
            record("demo-s4", "second-project", "claude-code", "릴리스 노트 초안", SessionStatus::Finished, now - 900_000, 0),
            record("demo-s5", "second-project", "codex", "테스트 플레이크 조사", SessionStatus::Failed, now - 1_200_000, 0),
            record("demo-s6", "second-project", "claude-code", "문서 링크 정리", SessionStatus::WaitingForApproval, now - 120_000, 0),
            record("demo-s7", "second-project", "codex", "의존성 업데이트", SessionStatus::Finished, now - 1_800_000, 0),
            record("demo-s8", "second-project", "claude-code", "CI 캐시 정리", SessionStatus::Running, now - 240_000, 0),
            record("demo-s9", "second-project", "codex", "로그 형식 통일", SessionStatus::Finished, now - 2_400_000, 0),
        ]
    }

    /// What the client is told at start: projects, sessions, computers, a live connection.
    pub fn inputs(&self, now: i64) -> Vec<Input> {
        vec![
            Input::SessionsLoaded(self.sessions(now)),
            Input::ProjectsLoaded(projects()),
            Input::ComputersLoaded(computers()),
            Input::Connection(ConnectionState::Connected),
        ]
    }

    /// The transcript the server would return for `id`.
    pub fn events_for(&self, id: &str) -> Vec<StoredEvent> {
        self.events.lock().unwrap().get(id).cloned().unwrap_or_default()
    }

    /// The UI opened `id`: show its transcript.
    pub fn open(&self, client: &Client, id: &str) {
        client.apply(Input::Opened(id.to_string()));
        client.apply(Input::EventsFetched { session_id: id.to_string(), events: self.events_for(id) });
    }

    /// Append one event to a session's transcript and tell the client.
    fn emit(&self, client: &Client, id: &str, status: SessionStatus, event: AgentEvent) {
        let seq = client.read(|s| s.session(id).map_or(0, |v| v.record.last_seq)) + 1;
        let ev = StoredEvent { session_id: id.into(), seq, at: now_ms(), event };
        self.events.lock().unwrap().entry(id.to_string()).or_default().push(ev.clone());
        client.apply(Input::Push(Push::Event { status, event: ev }));
    }

    /// A message the user sent: the demo answers with a fixed line.
    pub fn send_message(&self, client: &Client, id: &str, text: &str) {
        self.emit(client, id, SessionStatus::Running, AgentEvent::UserMessage { text: text.into() });
        self.emit(
            client,
            id,
            SessionStatus::Running,
            AgentEvent::AssistantMessage { text: format!("{DEMO_NOTICE}: 서버가 없어 답변은 정해진 문장입니다.") },
        );
        self.emit(client, id, SessionStatus::Finished, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
    }

    /// The user answered an approval card: record the decision, then finish the turn.
    pub fn answer(&self, client: &Client, id: &str, approval_id: &str, decision: ApprovalDecision) {
        self.emit(
            client,
            id,
            SessionStatus::Running,
            AgentEvent::ApprovalResolved { approval_id: approval_id.into(), decision },
        );
        let text = match decision {
            ApprovalDecision::Deny => "거부하셨습니다. 명령을 실행하지 않았습니다.".to_string(),
            _ => "테스트가 모두 통과했습니다 (데모).".to_string(),
        };
        self.emit(client, id, SessionStatus::Running, AgentEvent::AssistantMessage { text });
        self.emit(client, id, SessionStatus::Finished, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
    }

    /// A new session, kept in this run only.
    pub fn create_session(&self, client: &Client, body: &CreateSession) -> SessionRecord {
        let n = {
            let mut c = self.created.lock().unwrap();
            *c += 1;
            *c
        };
        let title = body.title.clone().unwrap_or_else(|| "새 세션".to_string());
        let mut rec = record(&format!("demo-new-{n}"), &body.project, &body.agent, &title, SessionStatus::Idle, now_ms(), 0);
        rec.cwd = body.cwd.clone();
        rec.model = body.model.clone();
        client.apply(Input::SessionLoaded(rec.clone()));
        rec
    }

    /// A new project on this computer, kept in this run only (#218).
    pub fn create_project(&self, client: &Client, raw: &str) -> Result<Project, String> {
        let existing: Vec<String> = client.read(|s| s.projects().into_iter().map(|p| p.name).collect());
        let name = new_project_name(raw, &existing).map_err(str::to_string)?;
        let project = Project { name, created_at: now_ms() / 1000, computers: vec!["local".into()] };
        client.apply(Input::Push(Push::ProjectUpdated { project: project.clone() }));
        Ok(project)
    }

    /// Rename, pin or archive in this run only.
    pub fn patch_session(&self, client: &Client, id: &str, patch: &SessionPatch) {
        let Some(mut rec) = client.read(|s| s.session(id).map(|v| v.record.clone())) else { return };
        if let Some(t) = &patch.title {
            rec.title.clone_from(t);
        }
        if let Some(p) = patch.pinned {
            rec.pinned = p;
        }
        if let Some(a) = patch.archived {
            rec.archived = a;
        }
        client.apply(Input::Push(Push::SessionUpdated { session: rec }));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn server_only_actions_are_blocked_only_in_the_demo() {
        assert_eq!(super::unavailable(true), Some("데모에서는 쓸 수 없습니다"));
        assert_eq!(super::unavailable(false), None);
    }

    use super::*;
    use ember_client::state::State;

    #[test]
    fn the_switch_off_means_no_demo() {
        assert!(!active(None, None));
        assert!(!active(Some("0"), None));
        assert!(!active(Some("true"), None));
        assert!(!active(Some(""), Some("")));
    }

    #[test]
    fn the_switch_on_without_a_url_uses_the_demo() {
        assert!(active(Some("1"), None));
        assert!(active(Some("1"), Some("")));
        assert!(active(Some("1"), Some("  ")));
    }

    #[test]
    fn a_given_server_url_keeps_the_server_path() {
        assert!(!active(Some("1"), Some("http://127.0.0.1:8740")));
        assert!(!active(None, Some("http://127.0.0.1:8740")));
    }

    #[test]
    fn day_strings_are_utc_dates() {
        assert_eq!(day_string(0), "1970-01-01");
        assert_eq!(day_string(951_782_400_000), "2000-02-29");
        assert_eq!(day_string(1_788_000_000_000), "2026-08-29");
    }

    #[test]
    fn ide_list_serves_ember_and_blocks_the_others() {
        let l = ide_launch("demo-s1", "demo-project", "C:/demo/demo-project");
        assert!(l.target("ember").unwrap().available);
        assert!(!l.target("vscode").unwrap().available);
        assert_eq!(l.target("gateway").unwrap().reason.as_deref(), Some(UNAVAILABLE));
        let (p, f) = crate::server::IdeLaunch::editor_target(Some(&Ok(l)));
        assert_eq!((p.as_str(), f.as_str()), ("demo-project", "C:/demo/demo-project"));
    }

    #[test]
    fn new_project_name_is_trimmed_and_unique() {
        let ex = vec!["a".to_string()];
        assert_eq!(new_project_name("  b ", &ex), Ok("b".to_string()));
        assert!(new_project_name("   ", &ex).is_err());
        assert!(new_project_name("a", &ex).is_err());
    }

    #[test]
    fn usage_has_fourteen_distinct_days_never_zero() {
        let u = usage(1_788_000_000_000);
        assert_eq!(u.len(), 14);
        assert_eq!(u[0].day, "2026-08-29");
        let mut days: Vec<_> = u.iter().map(|r| r.day.clone()).collect();
        days.dedup();
        assert_eq!(days.len(), 14);
        assert!(u.iter().all(|r| r.total_tokens() > 0 && r.account_id.as_deref() == Some(CHATGPT_ID)));
    }

    #[test]
    fn projects_computers_agents_and_accounts_have_the_demo_shape() {
        let p = projects();
        assert_eq!(p[0].name, "demo-project");
        assert_eq!(p.len(), 2);
        assert_eq!(computers().iter().filter(|c| c.reachability == Reachability::Online).count(), 1);
        assert!(agents().iter().any(|a| a.installed));
        let g = chatgpt_accounts(1_788_000_000_000);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].status, "signed_in");
        assert!(g[0].tokens_today > 0);
        assert!(accounts().iter().any(|a| a.is_default));
    }

    #[test]
    fn the_seed_gives_sessions_with_a_pending_approval() {
        let demo = DemoSource::new(1_788_000_000_000);
        let mut st = State::new();
        for i in demo.inputs(1_788_000_000_000) {
            st.apply(i);
        }
        let s1 = st.session("demo-s1").expect("session");
        assert_eq!(s1.record.project, "demo-project");
        assert_eq!(s1.status, ember_client::LauncherStatus::WaitingForApproval);
        assert!(st.session("demo-s2").is_some());
        // The second project has more than the five tiles a box shows before its deck.
        let second = st.projects().into_iter().find(|p| p.name == "second-project").expect("project");
        assert!(second.sessions.len() > 5);
        assert_eq!(st.computers().len(), 2);
        let ev = demo.events_for("demo-s1");
        assert_eq!(ev.len() as i64, s1.record.last_seq);
        assert!(matches!(ev.last().unwrap().event, AgentEvent::ApprovalRequested { .. }));
        // Sequence numbers are consecutive from 1, as the transcript requires.
        assert!(ev.iter().enumerate().all(|(i, e)| e.seq == i as i64 + 1));
    }
}
