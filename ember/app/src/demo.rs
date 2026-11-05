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
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use ember_client::Client;
use ember_client::state::{Computer, ConnectionState, Input, Reachability};
use ember_client::wire::{
    AgentEvent, ApprovalDecision, DailyUsage, DetectedAgent, Project, Push, SessionPatch,
    SessionRecord, SessionStatus, StoredEvent, TurnOutcome,
};

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
    ide_launch_with(session, project, cwd, demo_ose_from_env())
}

/// Env var that names a running OSE server (`host:port`, started without a connection token)
/// for the demo's Ember 에디터 to connect to, since the demo has no node to start one.
pub const DEMO_OSE_ENV: &str = "EMBER_DEMO_OSE";

fn demo_ose_from_env() -> Option<ember_client::wire::EmberOseConnection> {
    let raw = std::env::var(DEMO_OSE_ENV).ok()?;
    let raw = raw.trim();
    (!raw.is_empty()).then(|| ember_client::wire::EmberOseConnection {
        endpoint: format!("http://{raw}"),
        connection_token: String::new(),
    })
}

/// [`ide_launch`] with the OSE connection given. Pure, for tests.
pub fn ide_launch_with(
    session: &str,
    project: &str,
    cwd: &str,
    ose: Option<ember_client::wire::EmberOseConnection>,
) -> crate::server::IdeLaunch {
    use crate::server::{IdeComputer, IdeLaunch, IdeTarget};
    let target = |kind: &str, available: bool| IdeTarget {
        kind: kind.to_string(),
        available,
        url: None,
        command: None,
        launch: (kind == "ember" && ose.is_some()).then(|| ember_client::wire::EmberLaunch {
            session: session.to_string(),
            project: project.to_string(),
            computer: "이 컴퓨터".to_string(),
            folder: cwd.to_string(),
            server_url: None,
            ose: ose.clone(),
        }),
        reason: (!available).then(|| UNAVAILABLE.to_string()),
    };
    IdeLaunch {
        session: session.to_string(),
        project: project.to_string(),
        folder: cwd.to_string(),
        computer: IdeComputer {
            name: "이 컴퓨터".to_string(),
            local: true,
        },
        targets: vec![
            target("ember", true),
            target("vscode", false),
            target("gateway", false),
        ],
    }
}

/// Seed missing files without replacing files edited in an earlier demo run.
pub fn prepare_folder(data_dir: &Path, project: &str) -> io::Result<PathBuf> {
    let folder = data_dir
        .join("demo")
        .join(crate::config::safe_file_stem(project));
    std::fs::create_dir_all(folder.join("src"))?;
    for (name, contents) in [
        ("README.md", "# 데모 프로젝트\n\nEmber 편집기 예제입니다.\n"),
        (
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        ),
        (
            "src/main.rs",
            "fn main() {\n    println!(\"안녕하세요!\");\n}\n",
        ),
        (
            "package.json",
            "{\n  \"name\": \"demo\",\n  \"version\": \"0.1.0\"\n}\n",
        ),
    ] {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(folder.join(name))
        {
            Ok(mut file) => file.write_all(contents.as_bytes())?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(folder)
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
        Project {
            name: "demo-project".into(),
            created_at: 0,
            computers: vec!["local".into()],
        },
        Project {
            name: "second-project".into(),
            created_at: 0,
            computers: vec!["local".into()],
        },
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
        DetectedAgent {
            kind: "claude-code".into(),
            installed: true,
            version: Some("2.1.0".into()),
        },
        DetectedAgent {
            kind: "codex".into(),
            installed: true,
            version: Some("0.50.0".into()),
        },
        DetectedAgent {
            kind: "antigravity".into(),
            installed: false,
            version: None,
        },
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
    let today = usage(now)
        .first()
        .map(|u| u.input_tokens + u.output_tokens)
        .unwrap_or(0);
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

fn record(
    data_dir: &Path,
    id: &str,
    project: &str,
    agent: &str,
    title: &str,
    status: SessionStatus,
    now: i64,
    last_seq: i64,
) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        project: project.into(),
        agent: agent.into(),
        cwd: data_dir
            .join("demo")
            .join(crate::config::safe_file_stem(project))
            .to_string_lossy()
            .into_owned(),
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
            AgentEvent::UserMessage {
                text: "src/config.rs 에 포트 설정을 추가하고 테스트를 돌려 줘".into(),
            },
            AgentEvent::AssistantMessage {
                text: "먼저 설정 파일을 읽어 보겠습니다.".into(),
            },
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
            AgentEvent::AssistantMessage {
                text: "포트 필드가 이미 있습니다. 테스트를 실행해 확인하겠습니다.".into(),
            },
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
            AgentEvent::UserMessage {
                text: "README 에 실행 방법을 한 줄 추가해 줘".into(),
            },
            AgentEvent::AssistantMessage {
                text: "README.md 에 실행 방법을 추가했습니다.".into(),
            },
            AgentEvent::Usage {
                input_tokens: 1_200,
                output_tokens: 300,
            },
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed,
            },
        ],
        now,
    )
}

/// The demo source: the scripted data and the transcripts it keeps, so a conversation opened
/// again shows what happened in it.
pub struct DemoSource {
    events: Mutex<HashMap<String, Vec<StoredEvent>>>,
    created: Mutex<u32>,
    data_dir: PathBuf,
    git: Mutex<HashMap<String, GitState>>,
}

impl DemoSource {
    pub fn new(now: i64, data_dir: PathBuf) -> DemoSource {
        let mut events = HashMap::new();
        events.insert("demo-s1".to_string(), script_waiting(now));
        events.insert("demo-s2".to_string(), script_done(now - 600_000));
        DemoSource {
            events: Mutex::new(events),
            created: Mutex::new(0),
            data_dir,
            git: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_data_dir(now: i64, data_dir: PathBuf) -> io::Result<DemoSource> {
        for project in ["demo-project", "second-project"] {
            prepare_folder(&data_dir, project)?;
        }
        Ok(Self::new(now, data_dir))
    }

    /// Reset all demo transcripts, created-session counter, and client state to the initial seed.
    pub fn reset(&self, client: &Client, now: i64) {
        {
            let mut ev = self.events.lock().unwrap();
            ev.clear();
            ev.insert("demo-s1".to_string(), script_waiting(now));
            ev.insert("demo-s2".to_string(), script_done(now - 600_000));
        }
        *self.created.lock().unwrap() = 0;
        self.git.lock().unwrap().clear();
        for input in self.inputs(now) {
            client.apply(input);
        }
        for id in ["demo-s1", "demo-s2"] {
            client.apply(Input::EventsFetched {
                session_id: id.to_string(),
                events: self.events_for(id),
            });
        }
    }

    /// The sessions, as the server would list them.
    pub fn sessions(&self, now: i64) -> Vec<SessionRecord> {
        let ev = self.events.lock().unwrap();
        let n = |id: &str| ev.get(id).map_or(0, |v| v.len() as i64);
        let sessions = vec![
            record(
                &self.data_dir,
                "demo-s1",
                "demo-project",
                "claude-code",
                "포트 설정 추가",
                SessionStatus::WaitingForApproval,
                now,
                n("demo-s1"),
            ),
            record(
                &self.data_dir,
                "demo-s2",
                "demo-project",
                "codex",
                "README 정리",
                SessionStatus::Finished,
                now - 600_000,
                n("demo-s2"),
            ),
            // A fuller team in the second project, so the box's "외 N명" deck, the pinned chips
            // and every status group show in review captures (no transcripts).
            SessionRecord {
                pinned: true,
                ..record(
                    &self.data_dir,
                    "demo-s3",
                    "second-project",
                    "codex",
                    "빌드 스크립트 점검",
                    SessionStatus::Running,
                    now - 60_000,
                    0,
                )
            },
            record(
                &self.data_dir,
                "demo-s4",
                "second-project",
                "claude-code",
                "릴리스 노트 초안",
                SessionStatus::Finished,
                now - 900_000,
                0,
            ),
            record(
                &self.data_dir,
                "demo-s5",
                "second-project",
                "codex",
                "테스트 플레이크 조사",
                SessionStatus::Failed,
                now - 1_200_000,
                0,
            ),
            record(
                &self.data_dir,
                "demo-s6",
                "second-project",
                "claude-code",
                "문서 링크 정리",
                SessionStatus::WaitingForApproval,
                now - 120_000,
                0,
            ),
            record(
                &self.data_dir,
                "demo-s7",
                "second-project",
                "codex",
                "의존성 업데이트",
                SessionStatus::Finished,
                now - 1_800_000,
                0,
            ),
            record(
                &self.data_dir,
                "demo-s8",
                "second-project",
                "claude-code",
                "CI 캐시 정리",
                SessionStatus::Running,
                now - 240_000,
                0,
            ),
            record(
                &self.data_dir,
                "demo-s9",
                "second-project",
                "codex",
                "로그 형식 통일",
                SessionStatus::Finished,
                now - 2_400_000,
                0,
            ),
        ];
        sessions
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
        self.events
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// The UI opened `id`: show its transcript.
    pub fn open(&self, client: &Client, id: &str) {
        client.apply(Input::Opened(id.to_string()));
        client.apply(Input::EventsFetched {
            session_id: id.to_string(),
            events: self.events_for(id),
        });
    }

    /// Append one event to a session's transcript and tell the client.
    fn emit(&self, client: &Client, id: &str, status: SessionStatus, event: AgentEvent) {
        let seq = client.read(|s| s.session(id).map_or(0, |v| v.record.last_seq)) + 1;
        let ev = StoredEvent {
            session_id: id.into(),
            seq,
            at: now_ms(),
            event,
        };
        self.events
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .push(ev.clone());
        client.apply(Input::Push(Push::Event { status, event: ev }));
    }

    /// A message the user sent: the demo answers with a fixed line.
    pub fn send_message(&self, client: &Client, id: &str, text: &str) {
        self.emit(
            client,
            id,
            SessionStatus::Running,
            AgentEvent::UserMessage { text: text.into() },
        );
        self.emit(
            client,
            id,
            SessionStatus::Running,
            AgentEvent::AssistantMessage {
                text: format!("{DEMO_NOTICE}: 서버가 없어 답변은 정해진 문장입니다."),
            },
        );
        self.emit(
            client,
            id,
            SessionStatus::Finished,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed,
            },
        );
    }

    /// The user answered an approval card: record the decision, then finish the turn.
    pub fn answer(&self, client: &Client, id: &str, approval_id: &str, decision: ApprovalDecision) {
        self.emit(
            client,
            id,
            SessionStatus::Running,
            AgentEvent::ApprovalResolved {
                approval_id: approval_id.into(),
                decision,
            },
        );
        let text = match decision {
            ApprovalDecision::Deny => "거부하셨습니다. 명령을 실행하지 않았습니다.".to_string(),
            _ => "테스트가 모두 통과했습니다 (데모).".to_string(),
        };
        self.emit(
            client,
            id,
            SessionStatus::Running,
            AgentEvent::AssistantMessage { text },
        );
        self.emit(
            client,
            id,
            SessionStatus::Finished,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed,
            },
        );
    }

    /// A new session, kept in this run only.
    pub fn create_session(&self, client: &Client, body: &CreateSession) -> SessionRecord {
        let n = {
            let mut c = self.created.lock().unwrap();
            *c += 1;
            *c
        };
        let title = body.title.clone().unwrap_or_else(|| "새 세션".to_string());
        let mut rec = record(
            &self.data_dir,
            &format!("demo-new-{n}"),
            &body.project,
            &body.agent,
            &title,
            SessionStatus::Idle,
            now_ms(),
            0,
        );
        if !body.cwd.is_empty() {
            rec.cwd = body.cwd.clone();
        }
        rec.model = body.model.clone();
        client.apply(Input::SessionLoaded(rec.clone()));
        rec
    }

    /// A new project on this computer, kept in this run only (#218).
    pub fn create_project(&self, client: &Client, raw: &str) -> Result<Project, String> {
        let existing: Vec<String> =
            client.read(|s| s.projects().into_iter().map(|p| p.name).collect());
        let name = new_project_name(raw, &existing).map_err(str::to_string)?;
        let project = Project {
            name,
            created_at: now_ms() / 1000,
            computers: vec!["local".into()],
        };
        client.apply(Input::Push(Push::ProjectUpdated {
            project: project.clone(),
        }));
        Ok(project)
    }

    /// Answer `POST /projects/{id}/git/<op>` with the contract's JSON (`.scratch/git/contract.md`,
    /// FR-G4). Operations that answer 204 return `Value::Null`. Each project keeps its own
    /// in-memory repository, seeded on first use and dropped by [`DemoSource::reset`].
    pub fn git(&self, project: &str, op: &str, body: &Value) -> Result<Value, GitError> {
        if project.is_empty() {
            return Err(GitError::new(404, "not_a_repo", "not a git repository"));
        }
        let mut all = self.git.lock().unwrap();
        all.entry(project.to_string()).or_insert_with(GitState::seed).run(op, body)
    }

    /// Rename, pin or archive in this run only.
    pub fn patch_session(&self, client: &Client, id: &str, patch: &SessionPatch) {
        let Some(mut rec) = client.read(|s| s.session(id).map(|v| v.record.clone())) else {
            return;
        };
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

/// The demo history, newest first, as the node's `/git/log` commits. `c7` is a merge of `c8`
/// (first parent) and `b2` (the side branch). `now` is unix seconds. Pure, for tests.
fn demo_log(now: i64) -> Vec<Value> {
    // (sha, parents, author, minutes ago, subject, refs as (name, kind))
    type Row = (&'static str, &'static [&'static str], &'static str, i64, &'static str, &'static [(&'static str, &'static str)]);
    const ROWS: &[Row] = &[
        ("a1b2c3d4e5f60718293a4b5c6d7e8f9012345601", &["e4f5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4c02"], "BREW", 12, "Feat: Git log API and history list", &[("HEAD", "head"), ("refs/heads/feat/git-api", "branch")]),
        ("e4f5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4c02", &["9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c03"], "BREW", 95, "Fix: keep the staged selection after a refresh", &[("refs/remotes/origin/feat/git-api", "remote")]),
        ("9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c03", &["5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f04"], "Minji Kim", 60 * 5, "Docs: describe the commit message rules", &[]),
        ("5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f04", &["2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d05"], "BREW", 60 * 26, "Test: cover unborn branch history", &[]),
        ("2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d05", &["c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f06", "b2a1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a407"], "BREW", 60 * 30, "Merge branch 'feat/stage-lines' into feat/git-api", &[]),
        ("b2a1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a407", &["c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f06"], "Minji Kim", 60 * 40, "Feat: stage single lines from the diff", &[("refs/heads/feat/stage-lines", "branch")]),
        ("c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f06", &["3e4f5a6b7c8d9e0f1a2b3c4d5e6f708192a3b408"], "BREW", 60 * 52, "Refactor: split the status parser", &[]),
        ("3e4f5a6b7c8d9e0f1a2b3c4d5e6f708192a3b408", &["8f9012345678901234567890abcdef1234567809"], "Jisoo Park", 60 * 24 * 3, "Fix: handle renamed files in status", &[]),
        ("8f9012345678901234567890abcdef1234567809", &["1f2e3d4c5b6a79880716253443526170898a0b10"], "Jisoo Park", 60 * 24 * 4, "Chore: bump dependencies", &[("refs/tags/v0.3.0", "tag")]),
        ("1f2e3d4c5b6a79880716253443526170898a0b10", &["0a1b2c3d4e5f60718293a4b5c6d7e8f901234511"], "Minji Kim", 60 * 24 * 6, "Feat: show ahead and behind counts", &[("refs/heads/main", "branch"), ("refs/remotes/origin/main", "remote")]),
        ("0a1b2c3d4e5f60718293a4b5c6d7e8f901234511", &["7a8b9c0d1e2f30415263748596a7b8c9d0e1f212"], "BREW", 60 * 24 * 9, "Feat: commit from the staged files", &[]),
        ("7a8b9c0d1e2f30415263748596a7b8c9d0e1f212", &[], "BREW", 60 * 24 * 14, "Chore: initial commit", &[]),
    ];
    ROWS.iter()
        .map(|(sha, parents, author, mins, subject, refs)| {
            json!({
                "sha": sha,
                "parents": parents,
                "author": author,
                "email": format!("{}@example.com", author.to_lowercase().replace(' ', ".")),
                "time": now - mins * 60,
                "subject": subject,
                "refs": refs.iter().map(|(name, kind)| json!({ "name": name, "kind": kind })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// An error answer of a demo git route: the status and the `ErrorBody` fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl GitError {
    fn new(status: u16, code: &str, message: &str) -> GitError {
        GitError { status, code: code.into(), message: message.into() }
    }
    fn bad(message: &str) -> GitError {
        GitError::new(400, "bad_input", message)
    }
    fn conflict(message: &str) -> GitError {
        GitError::new(409, "conflict", message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Modified,
    /// Untracked until a hunk is staged, then added.
    New,
    Renamed,
    Deleted,
}

#[derive(Debug, Clone)]
struct DemoFile {
    path: String,
    old_path: Option<String>,
    kind: Kind,
    /// Hunk texts (each starts with its `@@` line and ends with a newline).
    staged: Vec<String>,
    unstaged: Vec<String>,
}

impl DemoFile {
    fn chars(&self) -> (char, char) {
        let w = |m: char| if self.unstaged.is_empty() { '.' } else { m };
        match self.kind {
            Kind::Modified => (if self.staged.is_empty() { '.' } else { 'M' }, w('M')),
            Kind::Deleted => {
                if self.staged.is_empty() {
                    ('.', 'D')
                } else {
                    ('D', '.')
                }
            }
            Kind::New => {
                if self.staged.is_empty() {
                    ('?', '?')
                } else {
                    ('A', w('M'))
                }
            }
            Kind::Renamed => ('R', w('M')),
        }
    }

    fn diff(&self, staged: bool) -> String {
        let hunks = if staged { &self.staged } else { &self.unstaged };
        if hunks.is_empty() {
            return String::new();
        }
        let p = &self.path;
        let mut out = String::new();
        let renamed_side = self.kind == Kind::Renamed && staged;
        let from = if renamed_side { self.old_path.as_deref().unwrap_or(p) } else { p };
        out.push_str(&format!("diff --git a/{from} b/{p}\n"));
        let new_side = self.kind == Kind::New && (staged || self.staged.is_empty());
        if new_side {
            out.push_str("new file mode 100644\n--- /dev/null\n");
            out.push_str(&format!("+++ b/{p}\n"));
        } else if self.kind == Kind::Deleted {
            out.push_str(&format!("deleted file mode 100644\n--- a/{p}\n+++ /dev/null\n"));
        } else {
            if renamed_side {
                out.push_str(&format!("similarity index 88%\nrename from {from}\nrename to {p}\n"));
            }
            out.push_str(&format!("--- a/{from}\n+++ b/{p}\n"));
        }
        for h in hunks {
            out.push_str(h);
        }
        out
    }
}

#[derive(Debug, Clone)]
struct DemoBranch {
    name: String,
    upstream: Option<String>,
    ahead: u32,
    behind: u32,
}

#[derive(Debug, Clone)]
struct GitState {
    current: String,
    branches: Vec<DemoBranch>,
    files: Vec<DemoFile>,
    fetched_age_secs: Option<u64>,
    commits: u128,
    /// Newest first, as `git stash list` (index 0 is the newest).
    stashes: Vec<DemoStash>,
}

/// One demo stash. `files` are the changes it holds, put back by apply and pop.
#[derive(Debug, Clone)]
struct DemoStash {
    message: String,
    branch: String,
    /// Unix seconds.
    time: i64,
    files: Vec<DemoFile>,
}

/// A unix time as the ISO 8601 text of `git stash list --format=%cI` (UTC offset written `+00:00`).
fn iso_time(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00", secs / 3_600, secs % 3_600 / 60, secs % 60)
}

fn hunk(header: &str, lines: &[&str]) -> String {
    let mut s = format!("{header}\n");
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// The old-start number of a hunk header (`@@ -12,6 +12,8 @@` gives `12`).
fn old_start(h: &str) -> Option<&str> {
    let line = h.lines().next()?;
    line.strip_prefix("@@ -")?.split([',', ' ']).next()
}

fn split_patch(patch: &str) -> (Option<String>, Vec<String>) {
    let mut path = None;
    let mut hunks: Vec<String> = Vec::new();
    for line in patch.lines() {
        if line.starts_with("@@") {
            hunks.push(format!("{line}\n"));
        } else if let Some(last) = hunks.last_mut() {
            last.push_str(line);
            last.push('\n');
        } else if let Some(p) = line.strip_prefix("+++ b/") {
            path = Some(p.to_string());
        } else if path.is_none() {
            if let Some(p) = line.strip_prefix("--- a/") {
                path = Some(p.to_string());
            }
        }
    }
    (path, hunks)
}

fn take_hunk(from: &mut Vec<String>, wanted: &str) -> Option<String> {
    let header = wanted.lines().next().unwrap_or("");
    let at = from
        .iter()
        .position(|h| h.lines().next() == Some(header))
        .or_else(|| from.iter().position(|h| old_start(h).is_some() && old_start(h) == old_start(wanted)))?;
    Some(from.remove(at))
}

fn str_of<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

impl GitState {
    fn seed() -> GitState {
        let b = |name: &str, up: Option<&str>, ahead, behind| DemoBranch { name: name.into(), upstream: up.map(Into::into), ahead, behind };
        let f = |path: &str, old: Option<&str>, kind, staged: Vec<String>, unstaged: Vec<String>| DemoFile {
            path: path.into(),
            old_path: old.map(Into::into),
            kind,
            staged,
            unstaged,
        };
        let vcs = "ember/app/src/ui/vcs.rs";
        GitState {
            current: "feat/git-api".into(),
            branches: vec![
                b("feat/git-api", Some("origin/feat/git-api"), 2, 1),
                b("main", Some("origin/main"), 0, 0),
                b("develop", Some("origin/develop"), 0, 0),
                b("feat/ui-phase-6", None, 0, 0),
            ],
            files: vec![
                f(
                    vcs,
                    None,
                    Kind::Modified,
                    vec![hunk("@@ -20,5 +20,6 @@ use crate::ui::{use_ui, Route, Ui};", &[" use crate::ui::theme::*;", "+use crate::ui::git::Snapshot;", " ", " /// What a view shows.", " pub const SOON: &str = \"준비 중입니다\";", " "])],
                    vec![
                        hunk("@@ -88,6 +89,6 @@ pub enum Section {", &["     Changes,", "     History,", "-    Branches,", "+    Branches2,", "     PullRequests,", "     Stash,", "     Conflicts,"]),
                        hunk("@@ -140,0 +141,3 @@ impl Section {", &["+    pub fn demo(self) -> bool {", "+        true", "+    }"]),
                    ],
                ),
                f(
                    "ember/server/src/git.rs",
                    None,
                    Kind::New,
                    vec![hunk("@@ -0,0 +1,6 @@", &["+//! Git routes (FR-G4).", "+", "+pub fn routes() -> Vec<&'static str> {", "+    vec![\"status\", \"diff\"]", "+}", "+"])],
                    vec![],
                ),
                f(
                    "docs/GIT.md",
                    None,
                    Kind::New,
                    vec![],
                    vec![hunk("@@ -0,0 +1,4 @@", &["+# Git", "+", "+노드가 git CLI로 저장소를 읽습니다.", "+"])],
                ),
                f(
                    "ember/app/src/ui/diff_view.rs",
                    Some("ember/app/src/ui/review.rs"),
                    Kind::Renamed,
                    vec![],
                    vec![hunk("@@ -1,4 +1,4 @@", &["-//! The review view.", "+//! The diff view.", " ", " use dioxus_compose::prelude::*;", " "])],
                ),
                f(
                    "ember/app/src/legacy.rs",
                    None,
                    Kind::Deleted,
                    vec![],
                    vec![hunk("@@ -1,3 +0,0 @@", &["-//! Old helpers.", "-", "-pub fn old() {}"])],
                ),
            ],
            fetched_age_secs: Some(300),
            commits: 0,
            stashes: {
                let now = now_ms() / 1000;
                let held = |path: &str, line: &str| DemoFile {
                    path: path.into(),
                    old_path: None,
                    kind: Kind::Modified,
                    staged: vec![],
                    unstaged: vec![hunk("@@ -1,1 +1,2 @@", &[" //! demo", &format!("+{line}")])],
                };
                vec![
                    DemoStash { message: "세션 shim 작업 중".into(), branch: "feat/git-api".into(), time: now - 60 * 40, files: vec![held("ember/node/src/shim.rs", "// wip")] },
                    DemoStash { message: "릴리스 문서 초안".into(), branch: "main".into(), time: now - 86_400 * 2, files: vec![held("docs/RELEASE.md", "초안")] },
                ]
            },
        }
    }

    fn branch(&self) -> &DemoBranch {
        self.branches.iter().find(|b| b.name == self.current).expect("current branch exists")
    }

    fn branch_mut(&mut self) -> &mut DemoBranch {
        let cur = self.current.clone();
        self.branches.iter_mut().find(|b| b.name == cur).expect("current branch exists")
    }

    fn find(&mut self, path: &str) -> Result<&mut DemoFile, GitError> {
        self.files
            .iter_mut()
            .find(|f| f.path == path)
            .ok_or_else(|| GitError::bad(&format!("pathspec '{path}' did not match any files")))
    }

    fn tidy(&mut self) {
        self.files.retain(|f| !(f.staged.is_empty() && f.unstaged.is_empty()));
    }

    fn run(&mut self, op: &str, body: &Value) -> Result<Value, GitError> {
        match op {
            "status" => {
                let b = self.branch();
                let files: Vec<Value> = self
                    .files
                    .iter()
                    .map(|f| {
                        let (i, w) = f.chars();
                        json!({"path": f.path, "old_path": f.old_path, "index": i.to_string(), "worktree": w.to_string()})
                    })
                    .collect();
                Ok(json!({
                    "branch": self.current,
                    "upstream": b.upstream,
                    "ahead": b.ahead,
                    "behind": b.behind,
                    "fetched_age_secs": self.fetched_age_secs,
                    "branches": self.branches.iter().map(|b| b.name.clone()).collect::<Vec<_>>(),
                    "files": files,
                }))
            }
            "diff" => {
                let staged = body.get("staged").and_then(Value::as_bool).unwrap_or(false);
                let only = str_of(body, "path");
                let patch: String = self.files.iter().filter(|f| only.is_none_or(|p| p == f.path)).map(|f| f.diff(staged)).collect();
                Ok(json!({ "patch": patch }))
            }
            "stage" | "unstage" => {
                let paths: Vec<String> = body
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .ok_or_else(|| GitError::bad("paths is required"))?;
                for p in &paths {
                    let f = self.find(p)?;
                    if op == "stage" {
                        let mut moved = std::mem::take(&mut f.unstaged);
                        f.staged.append(&mut moved);
                    } else {
                        let mut moved = std::mem::take(&mut f.staged);
                        f.unstaged.append(&mut moved);
                    }
                }
                self.tidy();
                Ok(Value::Null)
            }
            "apply" => {
                let patch = str_of(body, "patch").ok_or_else(|| GitError::bad("patch is required"))?;
                let reverse = body.get("reverse").and_then(Value::as_bool).unwrap_or(false);
                let (path, hunks) = split_patch(patch);
                let path = path.ok_or_else(|| GitError::bad("patch names no file"))?;
                if hunks.is_empty() {
                    return Err(GitError::bad("patch has no hunks"));
                }
                let f = self.find(&path)?;
                for h in &hunks {
                    let (from, to) = if reverse { (&mut f.staged, &mut f.unstaged) } else { (&mut f.unstaged, &mut f.staged) };
                    let moved = take_hunk(from, h).ok_or_else(|| GitError::conflict("error: patch does not apply"))?;
                    to.push(moved);
                }
                self.tidy();
                Ok(Value::Null)
            }
            "commit" => {
                let summary = str_of(body, "summary").unwrap_or("").trim();
                if summary.is_empty() {
                    return Err(GitError::bad("summary is required"));
                }
                let amend = body.get("amend").and_then(Value::as_bool).unwrap_or(false);
                if !amend && self.files.iter().all(|f| f.staged.is_empty()) {
                    return Err(GitError::conflict("nothing to commit"));
                }
                for f in &mut self.files {
                    if f.staged.is_empty() {
                        continue;
                    }
                    f.staged.clear();
                    f.old_path = None;
                    if f.kind != Kind::Deleted {
                        f.kind = Kind::Modified;
                    }
                }
                self.tidy();
                if !amend {
                    self.branch_mut().ahead += 1;
                }
                self.commits += 1;
                Ok(json!({ "sha": format!("{:040x}", 0x00de_c0de_0000_u128 + self.commits) }))
            }
            "checkout" => {
                let name = str_of(body, "branch").ok_or_else(|| GitError::bad("branch is required"))?;
                if !self.branches.iter().any(|b| b.name == name) {
                    return Err(GitError::bad(&format!("pathspec '{name}' did not match any branch")));
                }
                self.current = name.to_string();
                Ok(Value::Null)
            }
            "branch" => {
                let name = str_of(body, "name").unwrap_or("").trim();
                if name.is_empty() || name.contains(char::is_whitespace) {
                    return Err(GitError::bad("invalid branch name"));
                }
                if self.branches.iter().any(|b| b.name == name) {
                    return Err(GitError::conflict(&format!("a branch named '{name}' already exists")));
                }
                if let Some(from) = str_of(body, "from") {
                    if !self.branches.iter().any(|b| b.name == from) {
                        return Err(GitError::bad(&format!("not a valid object name: '{from}'")));
                    }
                }
                self.branches.push(DemoBranch { name: name.into(), upstream: None, ahead: 0, behind: 0 });
                self.current = name.to_string();
                Ok(Value::Null)
            }
            "fetch" => {
                self.fetched_age_secs = Some(0);
                // A fetch learns of one new upstream commit on a branch that tracks something.
                let b = self.branch_mut();
                if b.upstream.is_some() && b.behind == 0 {
                    b.behind = 1;
                }
                Ok(json!({ "output": "From origin\n   a1b2c3d..e4f5a6b  main       -> origin/main\n" }))
            }
            "pull" => {
                let b = self.branch_mut();
                if b.upstream.is_none() {
                    return Err(GitError::conflict("There is no tracking information for the current branch."));
                }
                let output = if b.behind == 0 { "Already up to date.\n" } else { "Updating a1b2c3d..e4f5a6b\nFast-forward\n 1 file changed, 3 insertions(+)\n" };
                b.behind = 0;
                self.fetched_age_secs = Some(0);
                Ok(json!({ "output": output }))
            }
            "push" => {
                let name = self.current.clone();
                let b = self.branch_mut();
                let output = format!("To origin\n   a1b2c3d..e4f5a6b  {name} -> {name}\n");
                b.upstream = Some(format!("origin/{name}"));
                b.ahead = 0;
                b.behind = 0;
                Ok(json!({ "output": output }))
            }
            "conflicts" => Ok(json!({ "paths": Vec::<String>::new() })),
            "stash/list" => Ok(Value::Array(
                self.stashes
                    .iter()
                    .enumerate()
                    .map(|(i, st)| json!({ "index": i, "message": st.message, "branch": st.branch, "time": iso_time(st.time) }))
                    .collect(),
            )),
            "stash/push" => {
                let untracked = body.get("include_untracked").and_then(Value::as_bool).unwrap_or(false);
                let (held, kept): (Vec<DemoFile>, Vec<DemoFile>) =
                    std::mem::take(&mut self.files).into_iter().partition(|f| untracked || !(f.kind == Kind::New && f.staged.is_empty()));
                self.files = kept;
                if held.is_empty() {
                    return Err(GitError::conflict("stash할 로컬 변경 사항이 없습니다."));
                }
                let message = str_of(body, "message").map(str::trim).filter(|m| !m.is_empty());
                let message = message.map(str::to_string).unwrap_or_else(|| format!("{} 작업 저장", self.current));
                self.stashes.insert(0, DemoStash { message, branch: self.current.clone(), time: now_ms() / 1000, files: held });
                Ok(Value::Null)
            }
            "stash/apply" | "stash/pop" | "stash/drop" => {
                let at = body.get("index").and_then(Value::as_u64).map(|i| i as usize).filter(|i| *i < self.stashes.len());
                let Some(at) = at else {
                    return Err(GitError::conflict("그런 stash가 없습니다."));
                };
                if op != "stash/drop" {
                    let held = if op == "stash/pop" { self.stashes.remove(at).files } else { self.stashes[at].files.clone() };
                    for f in held {
                        if !self.files.iter().any(|x| x.path == f.path) {
                            self.files.push(f);
                        }
                    }
                } else {
                    self.stashes.remove(at);
                }
                Ok(Value::Null)
            }
            "log" => {
                let limit = body["limit"].as_u64().unwrap_or(50) as usize;
                let skip = body["skip"].as_u64().unwrap_or(0) as usize;
                Ok(json!({ "commits": demo_log(now_ms() / 1000).into_iter().skip(skip).take(limit).collect::<Vec<_>>() }))
            }
            _ => Err(GitError::new(404, "not_found", "unknown git operation")),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fr_l31_demo_ember_target_carries_the_given_ose_connection() {
        let ose = ember_client::wire::EmberOseConnection {
            endpoint: "http://127.0.0.1:18765".into(),
            connection_token: String::new(),
        };
        let l = ide_launch_with("s", "p", "/w", Some(ose));
        let c = crate::server::IdeLaunch::editor_connection(Some(&Ok(l)));
        assert_eq!(c.endpoint.as_deref(), Some("127.0.0.1:18765"));
        let none = ide_launch_with("s", "p", "/w", None);
        assert!(none.target("ember").unwrap().launch.is_none());
    }

    use super::*;
    use ember_client::state::State;

    #[test]
    fn fr_l31_demo_folder_contains_sample_files() {
        let data = tempfile::tempdir().unwrap();
        let folder = prepare_folder(data.path(), "demo-project").unwrap();
        for name in ["README.md", "Cargo.toml", "src/main.rs", "package.json"] {
            assert!(folder.join(name).is_file(), "missing {name}");
        }
        assert!(folder.starts_with(data.path()));
    }

    #[test]
    fn fr_l31_demo_folder_creation_preserves_edits() {
        let data = tempfile::tempdir().unwrap();
        let folder = prepare_folder(data.path(), "demo-project").unwrap();
        std::fs::write(folder.join("README.md"), "수정한 내용").unwrap();
        assert_eq!(prepare_folder(data.path(), "demo-project").unwrap(), folder);
        assert_eq!(
            std::fs::read_to_string(folder.join("README.md")).unwrap(),
            "수정한 내용"
        );
    }

    #[test]
    fn server_only_actions_are_blocked_only_in_the_demo() {
        assert_eq!(super::unavailable(true), Some("데모에서는 쓸 수 없습니다"));
        assert_eq!(super::unavailable(false), None);
    }

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
        assert_eq!(
            l.target("gateway").unwrap().reason.as_deref(),
            Some(UNAVAILABLE)
        );
        let (p, f) = crate::server::IdeLaunch::editor_target(Some(&Ok(l)));
        assert_eq!(
            (p.as_str(), f.as_str()),
            ("demo-project", "C:/demo/demo-project")
        );
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
        assert!(
            u.iter()
                .all(|r| r.total_tokens() > 0 && r.account_id.as_deref() == Some(CHATGPT_ID))
        );
    }

    #[test]
    fn projects_computers_agents_and_accounts_have_the_demo_shape() {
        let p = projects();
        assert_eq!(p[0].name, "demo-project");
        assert_eq!(p.len(), 2);
        assert_eq!(
            computers()
                .iter()
                .filter(|c| c.reachability == Reachability::Online)
                .count(),
            1
        );
        assert!(agents().iter().any(|a| a.installed));
        let g = chatgpt_accounts(1_788_000_000_000);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].status, "signed_in");
        assert!(g[0].tokens_today > 0);
        assert!(accounts().iter().any(|a| a.is_default));
    }

    fn gs(d: &DemoSource, op: &str, body: Value) -> Result<Value, GitError> {
        d.git("demo-project", op, &body)
    }

    fn status(d: &DemoSource) -> Value {
        gs(d, "status", json!({})).unwrap()
    }

    fn file<'a>(st: &'a Value, path: &str) -> &'a Value {
        st["files"].as_array().unwrap().iter().find(|f| f["path"] == path).unwrap_or(&Value::Null)
    }

    #[test]
    fn fr_g5_demo_log_lists_newest_first_with_one_merge_and_refs() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let log = gs(&d, "log", json!({ "limit": 50, "skip": 0 })).unwrap();
        let commits = log["commits"].as_array().unwrap();
        assert_eq!(commits.len(), 12);
        let times: Vec<i64> = commits.iter().map(|c| c["time"].as_i64().unwrap()).collect();
        assert!(times.windows(2).all(|w| w[0] > w[1]), "newest first");
        let merges: Vec<_> = commits.iter().filter(|c| c["parents"].as_array().unwrap().len() == 2).collect();
        assert_eq!(merges.len(), 1);
        assert_eq!(commits.last().unwrap()["parents"].as_array().unwrap().len(), 0);
        let kinds: Vec<&str> = commits.iter().flat_map(|c| c["refs"].as_array().unwrap()).map(|r| r["kind"].as_str().unwrap()).collect();
        for k in ["head", "branch", "tag"] {
            assert!(kinds.contains(&k), "missing ref kind {k}");
        }
    }

    #[test]
    fn fr_g5_demo_log_honors_limit_and_skip() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let all = gs(&d, "log", json!({})).unwrap()["commits"].as_array().unwrap().clone();
        let page = gs(&d, "log", json!({ "limit": 5, "skip": 3 })).unwrap();
        let page = page["commits"].as_array().unwrap();
        assert_eq!(page.len(), 5);
        assert_eq!(page[0]["sha"], all[3]["sha"]);
        let past = gs(&d, "log", json!({ "limit": 5, "skip": 100 })).unwrap();
        assert!(past["commits"].as_array().unwrap().is_empty());
    }

    fn stash_list(d: &DemoSource) -> Vec<Value> {
        gs(d, "stash/list", json!({})).unwrap().as_array().unwrap().clone()
    }

    #[test]
    fn fr_g6_demo_stash_lists_two_newest_first_with_iso_times() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let list = stash_list(&d);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["index"], 0);
        assert_eq!(list[1]["index"], 1);
        assert!(list[0]["time"].as_str().unwrap().contains('T'));
        assert!(!list[0]["message"].as_str().unwrap().is_empty());
        assert_eq!(iso_time(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(iso_time(1_788_000_000), "2026-08-29T10:40:00+00:00");
    }

    #[test]
    fn fr_g6_demo_stash_push_takes_tracked_changes_and_adds_an_entry() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let before = status(&d)["files"].as_array().unwrap().len();
        gs(&d, "stash/push", json!({ "message": "  내 작업  ", "include_untracked": false })).unwrap();
        let list = stash_list(&d);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0]["message"], "내 작업");
        let after = status(&d)["files"].as_array().unwrap().len();
        assert!(after < before, "tracked changes left the work tree");
        assert!(after > 0, "untracked files stay without include_untracked");
    }

    #[test]
    fn fr_g6_demo_stash_push_with_untracked_clears_everything_then_refuses_when_clean() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        gs(&d, "stash/push", json!({ "include_untracked": true })).unwrap();
        assert!(status(&d)["files"].as_array().unwrap().is_empty());
        let e = gs(&d, "stash/push", json!({})).unwrap_err();
        assert_eq!(e.status, 409);
        assert_eq!(stash_list(&d).len(), 3);
    }

    #[test]
    fn fr_g6_demo_stash_pop_removes_the_entry_and_restores_its_changes() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        gs(&d, "stash/pop", json!({ "index": 0 })).unwrap();
        assert_eq!(stash_list(&d).len(), 1);
        assert_eq!(stash_list(&d)[0]["message"], "릴리스 문서 초안");
        assert!(!file(&status(&d), "ember/node/src/shim.rs").is_null());
    }

    #[test]
    fn fr_g6_demo_stash_apply_keeps_the_entry_and_drop_removes_it() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        gs(&d, "stash/apply", json!({ "index": 1 })).unwrap();
        assert_eq!(stash_list(&d).len(), 2);
        assert!(!file(&status(&d), "docs/RELEASE.md").is_null());
        gs(&d, "stash/drop", json!({ "index": 1 })).unwrap();
        assert_eq!(stash_list(&d).len(), 1);
    }

    #[test]
    fn fr_g6_demo_stash_invalid_index_is_a_korean_conflict_and_changes_nothing() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        for op in ["stash/apply", "stash/pop", "stash/drop"] {
            for body in [json!({ "index": 9 }), json!({}), json!({ "index": "x" })] {
                let e = gs(&d, op, body).unwrap_err();
                assert_eq!(e.status, 409);
                assert_eq!(e.message, "그런 stash가 없습니다.");
            }
        }
        assert_eq!(stash_list(&d).len(), 2);
    }

    #[test]
    fn git_status_has_the_contract_shape() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let st = status(&d);
        assert_eq!(st["branch"], "feat/git-api");
        assert_eq!(st["upstream"], "origin/feat/git-api");
        assert_eq!(st["ahead"], 2);
        assert_eq!(st["behind"], 1);
        assert!(st["fetched_age_secs"].is_u64());
        assert!(st["branches"].as_array().unwrap().len() >= 3);
        let files = st["files"].as_array().unwrap();
        assert!((4..=6).contains(&files.len()));
        let ch = |p: &str| (file(&st, p)["index"].as_str().unwrap().to_string(), file(&st, p)["worktree"].as_str().unwrap().to_string());
        assert_eq!(ch("ember/app/src/ui/vcs.rs"), ("M".into(), "M".into()));
        assert_eq!(ch("ember/server/src/git.rs"), ("A".into(), ".".into()));
        assert_eq!(ch("docs/GIT.md"), ("?".into(), "?".into()));
        assert_eq!(ch("ember/app/src/ui/diff_view.rs").0, "R");
        assert_eq!(file(&st, "ember/app/src/ui/diff_view.rs")["old_path"], "ember/app/src/ui/review.rs");
        assert_eq!(ch("ember/app/src/legacy.rs"), (".".into(), "D".into()));
    }

    #[test]
    fn demo_hunk_line_counts_match_their_headers() {
        let g = GitState::seed();
        for f in &g.files {
            for staged in [false, true] {
                let text = f.diff(staged);
                for file in crate::ui::vcs::parse_unified(&text) {
                    for h in &file.hunks {
                        let old = h.lines.iter().filter(|l| l.kind != crate::ui::vcs::LineKind::Add).count() as u32;
                        let new = h.lines.iter().filter(|l| l.kind != crate::ui::vcs::LineKind::Del).count() as u32;
                        assert_eq!((old, new), (h.old_len, h.new_len), "{} {}", f.path, h.header);
                        assert!(h.lines.iter().all(|l| !l.text.starts_with("@@ ")), "{} {}", f.path, h.header);
                    }
                }
            }
        }
    }

    #[test]
    fn git_diff_has_hunks_for_staged_unstaged_and_deleted() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let p = |staged: bool, path: Option<&str>| gs(&d, "diff", json!({"path": path, "staged": staged})).unwrap()["patch"].as_str().unwrap().to_string();
        let un = p(false, Some("ember/app/src/ui/vcs.rs"));
        assert_eq!(un.matches("\n@@ ").count() + usize::from(un.starts_with("@@")), 2);
        assert!(un.contains("@@ -140,0 +141,3 @@"));
        let st = p(true, Some("ember/app/src/ui/vcs.rs"));
        assert_eq!(st.matches("\n@@ ").count(), 1);
        assert!(p(false, Some("ember/app/src/legacy.rs")).contains("deleted file mode"));
        assert!(p(true, Some("ember/server/src/git.rs")).contains("--- /dev/null"));
        let all = p(false, None);
        assert!(all.contains("diff --git a/docs/GIT.md") && all.contains("diff --git a/ember/app/src/legacy.rs"));
        assert!(!all.contains("ember/server/src/git.rs"));
    }

    #[test]
    fn git_diff_accepts_include_untracked_and_lists_the_untracked_file_once() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let body = json!({"path": null, "staged": false, "include_untracked": true});
        let all = gs(&d, "diff", body).unwrap()["patch"].as_str().unwrap().to_string();
        // docs/GIT.md is the demo's untracked file: one header, in the one patch.
        assert_eq!(all.matches("diff --git a/docs/GIT.md").count(), 1);
    }

    #[test]
    fn git_stage_unstage_and_apply_change_the_status() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        gs(&d, "stage", json!({"paths": ["docs/GIT.md"]})).unwrap();
        assert_eq!(file(&status(&d), "docs/GIT.md")["index"], "A");
        gs(&d, "unstage", json!({"paths": ["docs/GIT.md"]})).unwrap();
        assert_eq!(file(&status(&d), "docs/GIT.md")["index"], "?");
        // Stage one hunk by patch, leaving the other unstaged, then reverse it.
        let patch = "diff --git a/ember/app/src/ui/vcs.rs b/ember/app/src/ui/vcs.rs\n--- a/ember/app/src/ui/vcs.rs\n+++ b/ember/app/src/ui/vcs.rs\n@@ -140,0 +141,3 @@ impl Section {\n+    pub fn demo(self) -> bool {\n+        true\n+    }\n";
        gs(&d, "apply", json!({"patch": patch, "reverse": false})).unwrap();
        let staged = gs(&d, "diff", json!({"path": "ember/app/src/ui/vcs.rs", "staged": true})).unwrap();
        assert!(staged["patch"].as_str().unwrap().contains("@@ -140,0 +141,3 @@"));
        let unstaged = gs(&d, "diff", json!({"path": "ember/app/src/ui/vcs.rs", "staged": false})).unwrap();
        assert!(!unstaged["patch"].as_str().unwrap().contains("@@ -140,0 +141,3 @@"));
        gs(&d, "apply", json!({"patch": patch, "reverse": true})).unwrap();
        let again = gs(&d, "diff", json!({"path": "ember/app/src/ui/vcs.rs", "staged": false})).unwrap();
        assert!(again["patch"].as_str().unwrap().contains("@@ -140,0 +141,3 @@"));
        let err = gs(&d, "apply", json!({"patch": patch, "reverse": true})).unwrap_err();
        assert_eq!(err.status, 409);
        assert_eq!(gs(&d, "stage", json!({"paths": ["nope.rs"]})).unwrap_err().status, 400);
    }

    #[test]
    fn git_commit_clears_the_index_and_counts_ahead() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        assert_eq!(gs(&d, "commit", json!({"summary": " "})).unwrap_err().status, 400);
        let r = gs(&d, "commit", json!({"summary": "Feat: git demo", "description": null, "amend": false})).unwrap();
        assert_eq!(r["sha"].as_str().unwrap().len(), 40);
        let st = status(&d);
        assert_eq!(st["ahead"], 3);
        // The staged added file is gone from the list as a change; the unstaged parts remain.
        assert!(file(&st, "ember/server/src/git.rs").is_null());
        assert_eq!(file(&st, "ember/app/src/ui/vcs.rs")["index"], ".");
        assert_eq!(file(&st, "ember/app/src/ui/vcs.rs")["worktree"], "M");
        assert!(st["files"].as_array().unwrap().iter().all(|f| f["index"] != "A" && f["index"] != "M"));
        // Nothing staged now: a plain commit is a conflict, an amend is allowed and keeps ahead.
        assert_eq!(gs(&d, "commit", json!({"summary": "x"})).unwrap_err().status, 409);
        gs(&d, "commit", json!({"summary": "x", "amend": true})).unwrap();
        assert_eq!(status(&d)["ahead"], 3);
    }

    #[test]
    fn git_branches_and_remote_actions_update_the_status() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        gs(&d, "checkout", json!({"branch": "main"})).unwrap();
        let st = status(&d);
        assert_eq!((st["branch"].as_str().unwrap(), st["ahead"].as_u64().unwrap()), ("main", 0));
        assert_eq!(gs(&d, "checkout", json!({"branch": "ghost"})).unwrap_err().status, 400);
        gs(&d, "branch", json!({"name": "feat/new", "from": "develop"})).unwrap();
        let st = status(&d);
        assert_eq!(st["branch"], "feat/new");
        assert!(st["upstream"].is_null());
        assert!(st["branches"].as_array().unwrap().iter().any(|b| b == "feat/new"));
        assert_eq!(gs(&d, "branch", json!({"name": "feat/new"})).unwrap_err().status, 409);
        assert_eq!(gs(&d, "pull", json!({})).unwrap_err().status, 409);
        gs(&d, "push", json!({})).unwrap();
        assert_eq!(status(&d)["upstream"], "origin/feat/new");
        // Back on the seeded branch: pull clears behind, push clears ahead, fetch is reported.
        gs(&d, "checkout", json!({"branch": "feat/git-api"})).unwrap();
        assert!(gs(&d, "pull", json!({})).unwrap()["output"].is_string());
        assert_eq!(status(&d)["behind"], 0);
        assert!(gs(&d, "push", json!({"remote": null})).unwrap()["output"].is_string());
        assert_eq!(status(&d)["ahead"], 0);
        gs(&d, "fetch", json!({})).unwrap();
        assert_eq!(status(&d)["fetched_age_secs"], 0);
        assert_eq!(gs(&d, "conflicts", json!({})).unwrap(), json!({"paths": []}));
        assert_eq!(gs(&d, "bogus", json!({})).unwrap_err().status, 404);
    }

    #[test]
    fn git_state_resets_with_the_demo() {
        let _g = crate::services::test_guard();
        let s = crate::services::test_services();
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        gs(&d, "commit", json!({"summary": "x"})).unwrap();
        gs(&d, "checkout", json!({"branch": "main"})).unwrap();
        d.reset(&s.client, 0);
        let st = status(&d);
        assert_eq!((st["branch"].as_str().unwrap(), st["ahead"].as_u64().unwrap()), ("feat/git-api", 2));
        assert_eq!(file(&st, "ember/server/src/git.rs")["index"], "A");
        // Each project has its own repository.
        gs(&d, "checkout", json!({"branch": "main"})).unwrap();
        assert_eq!(d.git("second-project", "status", &json!({})).unwrap()["branch"], "feat/git-api");
    }

    #[test]
    fn the_seed_gives_sessions_with_a_pending_approval() {
        let data = tempfile::tempdir().unwrap();
        let demo = DemoSource::new(1_788_000_000_000, data.path().to_path_buf());
        assert!(
            demo.sessions(1_788_000_000_000)
                .iter()
                .all(|s| !s.cwd.is_empty())
        );
        let mut st = State::new();
        for i in demo.inputs(1_788_000_000_000) {
            st.apply(i);
        }
        let s1 = st.session("demo-s1").expect("session");
        assert_eq!(s1.record.project, "demo-project");
        assert_eq!(s1.status, ember_client::LauncherStatus::WaitingForApproval);
        assert!(st.session("demo-s2").is_some());
        // The second project has more than the five tiles a box shows before its deck.
        let second = st
            .projects()
            .into_iter()
            .find(|p| p.name == "second-project")
            .expect("project");
        assert!(second.sessions.len() > 5);
        assert_eq!(st.computers().len(), 2);
        let ev = demo.events_for("demo-s1");
        assert_eq!(ev.len() as i64, s1.record.last_seq);
        assert!(matches!(
            ev.last().unwrap().event,
            AgentEvent::ApprovalRequested { .. }
        ));
        // Sequence numbers are consecutive from 1, as the transcript requires.
        assert!(ev.iter().enumerate().all(|(i, e)| e.seq == i as i64 + 1));
    }
}
