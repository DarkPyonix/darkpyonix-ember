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
            // No server and no registry in the demo: there is no computer id, so no terminal relay is attempted.
            computer: String::new(),
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

/// The 14-day token summary of `GET /api/usage/summary` for the demo, oldest day first, with
/// sample numbers. The cost stays unknown, as on the server.
/// The Home GitHub card's sample in the demo (FR-L35). The card marks it (예시).
pub fn github_state() -> crate::ui::home::GithubState {
    crate::ui::home::sample_github_state(crate::ui::home::GithubSampleKind::Ok)
}

pub fn usage_summary(now: i64) -> ember_client::api::UsageSummary {
    use ember_client::api::{AgentUsageRow, UsageDayRow, UsageSummary};
    let row = |agent: &str, input: u64, output: u64, turns: u64| AgentUsageRow {
        agent: agent.into(),
        input_tokens: input,
        output_tokens: output,
        total_tokens: input + output,
        turns,
        cost_usd: None,
    };
    let mut days: Vec<UsageDayRow> = usage(now)
        .into_iter()
        .map(|u| {
            let codex = row("codex", u.input_tokens, u.output_tokens, u.reports);
            let claude = row("claude-code", u.input_tokens * 2, u.output_tokens * 2, u.reports + 1);
            UsageDayRow {
                day: u.day,
                input_tokens: codex.input_tokens + claude.input_tokens,
                output_tokens: codex.output_tokens + claude.output_tokens,
                total_tokens: codex.total_tokens + claude.total_tokens,
                turns: codex.turns + claude.turns,
                cost_usd: None,
                sessions: 2,
                by_agent: vec![claude, codex],
            }
        })
        .collect();
    days.reverse();
    let sum = |f: fn(&UsageDayRow) -> u64| days.iter().map(f).sum::<u64>();
    UsageSummary {
        from: days.first().map(|d| d.day.clone()).unwrap_or_default(),
        to: days.last().map(|d| d.day.clone()).unwrap_or_default(),
        input_tokens: sum(|d| d.input_tokens),
        output_tokens: sum(|d| d.output_tokens),
        total_tokens: sum(|d| d.total_tokens),
        turns: sum(|d| d.turns),
        cost_usd: None,
        by_agent: Vec::new(),
        cost_note: Some("no price table".into()),
        days,
    }
}

/// The second project's team: every demo member has its own name, so the box's deck card reads
/// `김하늘, 이서연` and not the repeated `second-project-codex`. Demo data only; ids stay stable.
pub fn second_team() -> ember_client::wire::TeamView {
    use ember_client::wire::{TeamMember, TeamRole, TeamView};
    let names = [
        ("demo-s3", "박도윤", "codex"),
        ("demo-s4", "최민준", "claude-code"),
        ("demo-s5", "정수아", "codex"),
        ("demo-s6", "한지우", "claude-code"),
        ("demo-s7", "김하늘", "codex"),
        ("demo-s8", "이서연", "claude-code"),
        ("demo-s9", "오준서", "codex"),
    ];
    TeamView {
        id: "demo-team-second".into(),
        project: "second-project".into(),
        leader: "demo-s3".into(),
        created_at: 0,
        members: names
            .iter()
            .map(|(id, name, agent)| TeamMember {
                session_id: (*id).into(),
                name: (*name).into(),
                role: if *id == "demo-s3" { TeamRole::Leader } else { TeamRole::Teammate },
                joined_at: 0,
                ended_at: None,
                title: String::new(),
                agent: (*agent).into(),
                status: None,
            })
            .collect(),
        tasks: Vec::new(),
    }
}

pub fn projects() -> Vec<Project> {
    vec![
        Project {
            name: "demo-project".into(),
            created_at: 0,
            computers: vec!["local".into()],
            ..Default::default()
        },
        Project {
            name: "second-project".into(),
            created_at: 0,
            computers: vec!["local".into()],
            ..Default::default()
        },
    ]
}

/// A plausible metrics sample for the demo computer (FR-L21, `GET /api/computers/{id}/metrics`).
/// The values move with `now_ms` so the cards and rates change on screen; the network counters
/// grow steadily, so two samples give a rate. Nothing here is measured.
pub fn metrics_sample(now_ms: u64) -> ember_client::api::MetricsSample {
    use ember_client::api::*;
    let t = (now_ms / 1000) as f32;
    let wave = |period: f32, lo: f32, hi: f32, phase: f32| lo + (hi - lo) * (0.5 + 0.5 * ((t / period) + phase).sin());
    let cores: Vec<f32> = (0..8).map(|i| wave(7.0 + i as f32, 4.0, 80.0, i as f32)).collect();
    let total = cores.iter().sum::<f32>() / cores.len() as f32;
    const GB: u64 = 1 << 30;
    let procs = [("ember-server", 14.0, 420u64), ("claude", 9.5, 880), ("rust-analyzer", 6.0, 1500), ("node", 3.2, 310), ("Code Helper", 2.1, 640)];
    MetricsSample {
        at_ms: now_ms,
        cpu: CpuMetrics { total_percent: Some(total), cores },
        memory: Some(MemoryMetrics {
            total_bytes: 16 * GB,
            used_bytes: (9.0 * GB as f32 + wave(30.0, 0.0, 2.0, 0.0) * GB as f32) as u64,
            swap_total_bytes: 4 * GB,
            swap_used_bytes: GB / 2,
        }),
        disks: vec![DiskMetrics { name: "Macintosh HD".into(), mount: "/".into(), total_bytes: 500 * GB, available_bytes: 210 * GB }],
        network: vec![NetMetrics { interface: "en0".into(), rx_bytes: now_ms / 1000 * 2_400_000, tx_bytes: now_ms / 1000 * 380_000 }],
        temperatures: vec![TempMetrics { label: "CPU".into(), celsius: wave(20.0, 48.0, 62.0, 1.0) }],
        gpu: GpuReport { devices: vec![], reason: Some("이 컴퓨터에서는 GPU 값을 읽지 못했습니다".into()) },
        processes: procs
            .iter()
            .enumerate()
            .map(|(i, (name, cpu, mb))| ProcessMetrics {
                pid: 100 + i as u32 * 37,
                name: (*name).into(),
                cpu_percent: *cpu + wave(5.0, 0.0, 2.0, i as f32),
                memory_bytes: mb << 20,
            })
            .collect(),
        process_count: 412,
    }
}

/// The sample one poll interval before `now_ms`. The monitor feed takes it first in demo mode,
/// so the network card shows a rate from the first frame instead of waiting for a second poll.
pub fn metrics_previous(now_ms: u64) -> ember_client::api::MetricsSample {
    metrics_sample(now_ms.saturating_sub(crate::ui::monitor::POLL_SECS * 1000))
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

/// The listening ports of the demo computer (FR-P7): two LAN ports and one loopback port, so the
/// 웹 페이지 list shows both an openable row and a note-only row. The demo computer reads as
/// `DEMO_HOST`, not local.
pub fn ports() -> ember_client::api::Ports {
    use ember_client::api::{ListeningPort, PortScope};
    let lp = |port, scope, process: &str| ListeningPort { port, scope, process: process.into(), ..Default::default() };
    ember_client::api::Ports {
        ports: vec![lp(3000, PortScope::Lan, "node"), lp(6006, PortScope::Lan, "storybook"), lp(5173, PortScope::Loopback, "vite")],
        supported: true,
    }
}

/// The address the demo computer's ports are served on.
pub const DEMO_HOST: &str = "192.168.0.10";

pub fn agents() -> Vec<DetectedAgent> {
    vec![
        DetectedAgent {
            kind: "claude-code".into(),
            installed: true,
            version: Some("2.1.0".into()),
            ..Default::default()
        },
        DetectedAgent {
            kind: "codex".into(),
            installed: true,
            version: Some("0.50.0".into()),
            ..Default::default()
        },
        DetectedAgent {
            kind: "antigravity".into(),
            installed: false,
            version: None,
            ..Default::default()
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

fn user(text: &str) -> AgentEvent {
    AgentEvent::UserMessage { text: text.into() }
}

fn say(text: &str) -> AgentEvent {
    AgentEvent::AssistantMessage { text: text.into() }
}

fn call(call_id: &str, name: &str, input: Value) -> AgentEvent {
    AgentEvent::ToolCall {
        call_id: call_id.into(),
        name: name.into(),
        input,
    }
}

fn result(call_id: &str, output: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: output.into(),
        is_error: false,
    }
}

fn ask(approval_id: &str, command: &str, description: &str) -> AgentEvent {
    AgentEvent::ApprovalRequested {
        approval_id: approval_id.into(),
        tool: "Bash".into(),
        input: json!({ "command": command, "description": description }),
    }
}

/// A sample question marked 예시: answering it calls nothing (FR-T11).
fn sample_question() -> AgentEvent {
    use ember_client::wire::{Question, QuestionOption};
    let opt = |l: &str, d: &str| QuestionOption { label: l.into(), description: Some(d.into()) };
    AgentEvent::QuestionAsked {
        question_id: "demo-question-1".into(),
        questions: vec![Question {
            id: "q0".into(),
            header: Some("예시".into()),
            text: "(예시) 링크 목록을 어떤 순서로 정리할까요?".into(),
            options: vec![opt("가나다순", "제목 기준으로 정렬합니다."), opt("최근 수정순", "마지막 수정 시각 기준으로 정렬합니다.")],
            multi_select: false,
            secret: false,
            allow_other: true,
        }],
    }
}

fn decided(approval_id: &str, decision: ApprovalDecision) -> AgentEvent {
    AgentEvent::ApprovalResolved {
        approval_id: approval_id.into(),
        decision,
    }
}

/// The scripted conversation of the first session: a read, an edit with its unified diff, a
/// shell tool call, an approval already allowed, then a `Bash` approval that waits for the user.
/// The events go through the same transcript reducer the server's events do (#505).
fn script_waiting(now: i64) -> Vec<StoredEvent> {
    stored(
        "demo-s1",
        vec![
            user("src/config.rs 에 포트 설정을 추가하고 테스트를 돌려 줘"),
            say("먼저 설정 파일을 읽어 보겠습니다."),
            call("c1", "Read", json!({ "file_path": "src/config.rs" })),
            result("c1", "pub struct Config {\n    pub port: u16,\n}\n"),
            say("포트 필드가 이미 있습니다. 기본값을 넣는 생성 함수를 추가하겠습니다."),
            call(
                "c2",
                "Edit",
                json!({
                    "file_path": "src/config.rs",
                    "old_string": "    pub port: u16,\n}\n",
                    "new_string": "    pub port: u16,\n}\n\nimpl Config {\n    pub fn new() -> Config {\n        Config { port: 8080 }\n    }\n}\n",
                }),
            ),
            result(
                "c2",
                "--- a/src/config.rs\n+++ b/src/config.rs\n@@ -1,3 +1,9 @@\n pub struct Config {\n     pub port: u16,\n }\n+\n+impl Config {\n+    pub fn new() -> Config {\n+        Config { port: 8080 }\n+    }\n+}\n",
            ),
            ask("demo-approval-0", "cargo fmt --check", "Check formatting"),
            decided("demo-approval-0", ApprovalDecision::AllowOnce),
            call(
                "c3",
                "Bash",
                json!({ "command": "cargo fmt --check", "description": "Check formatting" }),
            ),
            result("c3", ""),
            say("서식은 문제가 없습니다. 테스트를 실행해 확인하겠습니다."),
            ask(APPROVAL_ID, "cargo test --locked", "Run the tests"),
        ],
        now,
    )
}

/// The second session: an edit with a diff, one command denied and one allowed, then done.
fn script_done(now: i64) -> Vec<StoredEvent> {
    stored(
        "demo-s2",
        vec![
            user("README 에 실행 방법을 한 줄 추가해 줘"),
            say("README.md 를 먼저 확인합니다."),
            call("d1", "Read", json!({ "file_path": "README.md" })),
            result("d1", "# demo-project\n\n작은 예제 프로젝트입니다.\n"),
            call(
                "d2",
                "Edit",
                json!({
                    "file_path": "README.md",
                    "old_string": "작은 예제 프로젝트입니다.\n",
                    "new_string": "작은 예제 프로젝트입니다.\n\n실행: `cargo run`\n",
                }),
            ),
            result(
                "d2",
                "--- a/README.md\n+++ b/README.md\n@@ -1,3 +1,5 @@\n # demo-project\n \n 작은 예제 프로젝트입니다.\n+\n+실행: `cargo run`\n",
            ),
            ask("demo-approval-d1", "rm -rf target", "Clean the build output"),
            decided("demo-approval-d1", ApprovalDecision::Deny),
            say("빌드 결과를 지우는 명령은 거부되어 건너뛰었습니다."),
            ask("demo-approval-d2", "git diff --stat", "Show the change"),
            decided("demo-approval-d2", ApprovalDecision::AllowOnce),
            call(
                "d3",
                "Bash",
                json!({ "command": "git diff --stat", "description": "Show the change" }),
            ),
            result("d3", " README.md | 2 ++\n 1 file changed, 2 insertions(+)\n"),
            say("README.md 에 실행 방법을 추가했습니다."),
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

/// A short conversation for the other demo sessions, so no session opens to an empty page:
/// the prompt, one reply, and the tail that matches the status.
fn script_brief(id: &str, status: SessionStatus, prompt: &str, reply: &str, now: i64) -> Vec<StoredEvent> {
    let mut events = vec![user(prompt), say(reply)];
    match status {
        SessionStatus::Running => events.push(call(
            "b1",
            "Bash",
            json!({ "command": "ls", "description": "List files" }),
        )),
        SessionStatus::WaitingForApproval => {
            events.push(ask(&format!("demo-approval-{id}"), "git status", "Show the status"));
            if id == "demo-s6" {
                events.push(sample_question());
            }
        }
        SessionStatus::Failed => {
            events.push(AgentEvent::Error {
                message: "테스트가 실패했습니다 (데모).".into(),
            });
            events.push(AgentEvent::TurnEnded {
                outcome: TurnOutcome::Failed,
            });
        }
        _ => events.push(AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed,
        }),
    }
    stored(id, events, now)
}

/// Every demo transcript, keyed by session id.
fn seed_events(now: i64) -> HashMap<String, Vec<StoredEvent>> {
    let mut events = HashMap::new();
    events.insert("demo-s1".to_string(), script_waiting(now));
    events.insert("demo-s2".to_string(), script_done(now - 600_000));
    let brief: [(&str, SessionStatus, &str, &str, i64); 7] = [
        ("demo-s3", SessionStatus::Running, "빌드 스크립트가 맞는지 점검해 줘", "스크립트를 확인하고 있습니다.", 60_000),
        ("demo-s4", SessionStatus::Finished, "릴리스 노트 초안을 써 줘", "초안을 작성했습니다.", 900_000),
        ("demo-s5", SessionStatus::Failed, "플레이크가 나는 테스트를 조사해 줘", "테스트를 반복 실행해 보겠습니다.", 1_200_000),
        ("demo-s6", SessionStatus::WaitingForApproval, "문서 링크를 정리해 줘", "링크 상태를 확인하겠습니다.", 120_000),
        ("demo-s7", SessionStatus::Finished, "의존성을 업데이트해 줘", "의존성을 갱신했습니다.", 1_800_000),
        ("demo-s8", SessionStatus::Running, "CI 캐시를 정리해 줘", "캐시 키를 살펴보고 있습니다.", 240_000),
        ("demo-s9", SessionStatus::Finished, "로그 형식을 통일해 줘", "로그 형식을 맞췄습니다.", 2_400_000),
    ];
    for (id, status, prompt, reply, ago) in brief {
        events.insert(id.to_string(), script_brief(id, status, prompt, reply, now - ago));
    }
    events
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
        DemoSource {
            events: Mutex::new(seed_events(now)),
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
            *self.events.lock().unwrap() = seed_events(now);
        }
        *self.created.lock().unwrap() = 0;
        self.git.lock().unwrap().clear();
        for input in self.inputs(now) {
            client.apply(input);
        }
        for id in (1..=9).map(|n| format!("demo-s{n}")) {
            let id = id.as_str();
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
                    n("demo-s3"),
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
                n("demo-s4"),
            ),
            record(
                &self.data_dir,
                "demo-s5",
                "second-project",
                "codex",
                "테스트 플레이크 조사",
                SessionStatus::Failed,
                now - 1_200_000,
                n("demo-s5"),
            ),
            record(
                &self.data_dir,
                "demo-s6",
                "second-project",
                "claude-code",
                "문서 링크 정리",
                SessionStatus::WaitingForApproval,
                now - 120_000,
                n("demo-s6"),
            ),
            record(
                &self.data_dir,
                "demo-s7",
                "second-project",
                "codex",
                "의존성 업데이트",
                SessionStatus::Finished,
                now - 1_800_000,
                n("demo-s7"),
            ),
            record(
                &self.data_dir,
                "demo-s8",
                "second-project",
                "claude-code",
                "CI 캐시 정리",
                SessionStatus::Running,
                now - 240_000,
                n("demo-s8"),
            ),
            record(
                &self.data_dir,
                "demo-s9",
                "second-project",
                "codex",
                "로그 형식 통일",
                SessionStatus::Finished,
                now - 2_400_000,
                n("demo-s9"),
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
            Input::TeamLoaded { session_id: "demo-s3".into(), team: Some(second_team()) },
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

    /// The user answered or skipped the sample question (FR-T11). Nothing leaves the process;
    /// the sample has no secret question, and a secret value would be stored as `***` anyway.
    pub fn answer_question(
        &self,
        client: &Client,
        id: &str,
        question_id: &str,
        reply: &ember_client::api::QuestionReply,
    ) {
        let answers = match reply {
            ember_client::api::QuestionReply::Answers(a) => a.clone(),
            ember_client::api::QuestionReply::Skip => Vec::new(),
        };
        self.emit(
            client,
            id,
            SessionStatus::WaitingForApproval,
            AgentEvent::QuestionAnswered { question_id: question_id.into(), answers },
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
            ..Default::default()
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

    /// The GitHub card's data (`GET /api/integrations/github`, FR-G13): two repositories, one
    /// with a failing run, the numbers a connected `gh` would give.
    pub fn github_summary(&self) -> ember_client::api::GithubSummary {
        use ember_client::api::{GithubRepo, GithubState, GithubSummary, GithubTotals};
        let ci = |runs: &[&str]| Some(runs.iter().map(|r| r.to_string()).collect::<Vec<_>>());
        GithubSummary {
            state: GithubState::On,
            reason: None,
            account: Some("demo-user".into()),
            repos: vec![
                GithubRepo {
                    name_with_owner: Some("demo-user/ember".into()),
                    default_branch: Some("develop".into()),
                    open_prs: Some(4),
                    review_requested: Some(1),
                    capped: false,
                    ci: ci(&["success", "success", "failure", "success", "pending", "success", "cancelled", "success"]),
                    error: None,
                    projects: vec!["ember".into()],
                },
                GithubRepo {
                    name_with_owner: Some("demo-user/notes".into()),
                    default_branch: Some("main".into()),
                    open_prs: Some(1),
                    review_requested: Some(0),
                    capped: false,
                    ci: ci(&["success", "success", "success", "failure"]),
                    error: None,
                    projects: vec!["notes".into()],
                },
            ],
            totals: GithubTotals { repos: Some(2), open_prs: Some(5), review_requested: Some(1), ci_pass_percent: Some(80) },
            checked_at: 0,
        }
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

    /// Update a project's settings in this demo run only (FR-L26, FR-L22, #489).
    pub fn patch_project(
        &self,
        client: &Client,
        name: &str,
        patch: &ember_client::wire::ProjectPatch,
    ) -> Result<Project, ember_client::api::ProjectPatchError> {
        let Some(mut project) = client.read(|s| s.project(name).cloned()) else {
            return Err(ember_client::api::ProjectPatchError::Rejected {
                status: 404,
                code: Some("not_found".into()),
                message: "not found".into(),
            });
        };
        if let Some(new_name) = &patch.name {
            let trimmed = new_name.trim();
            if trimmed != name {
                if name == crate::ui::project::DEFAULT_PROJECT_NAME {
                    return Err(ember_client::api::ProjectPatchError::Rejected {
                        status: 403,
                        code: Some("protected_project".into()),
                        message: "protected".into(),
                    });
                }
                client.apply(Input::ProjectRenamed { old: name.to_string(), new: trimmed.to_string() });
                project.name = trimmed.to_string();
            }
        }
        if let Some(r) = &patch.repository {
            project.repository = (!r.trim().is_empty()).then(|| r.trim().to_string());
        }
        if let Some(b) = &patch.default_branch {
            project.default_branch = (!b.trim().is_empty()).then(|| b.trim().to_string());
        }
        if let Some(md) = &patch.member_defaults {
            project.member_defaults = md.clone();
        }
        if let Some(st) = &patch.character_style {
            project.character_style = (!st.trim().is_empty()).then(|| st.trim().to_string());
        }
        if let Some(po) = &patch.profile_overrides {
            project.profile_overrides = po.iter().filter_map(|(k, v)| v.clone().map(|a| (k.clone(), Some(a)))).collect();
        }
        if let Some(co) = &patch.character_overrides {
            project.character_overrides = co
                .iter()
                .filter_map(|(k, v)| v.clone().filter(|o| !o.is_empty()).map(|o| (k.clone(), o)))
                .collect();
        }
        client.apply(Input::Push(Push::ProjectUpdated { project: project.clone() }));
        Ok(project)
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

/// The files each seeded commit of [`demo_log`] changed, by row (FR-G14): (path, old path, status,
/// additions, deletions, binary). The merge commit (row 4) lists what it brought in over its
/// first parent; the last row is the root commit.
type DemoCommitFile = (&'static str, Option<&'static str>, &'static str, u64, u64, bool);
const DEMO_COMMIT_FILES: &[&[DemoCommitFile]] = &[
    &[("ember/node/src/git_log.rs", None, "added", 85, 0, false), ("ember/node/tests/git_log.rs", None, "added", 120, 0, false), ("ember/server/src/api/git.rs", None, "modified", 3, 1, false)],
    &[("ember/app/src/ui/vcs.rs", None, "modified", 18, 6, false), ("ember/app/src/ui/cr/vcs.rs", None, "modified", 9, 2, false)],
    &[("docs/SPEC.md", None, "modified", 14, 2, false), ("AGENTS.md", None, "modified", 4, 1, false), ("docs/INTENT.md", None, "modified", 6, 0, false)],
    &[("ember/node/tests/git_log.rs", None, "modified", 22, 0, false), ("ember/node/src/git.rs", None, "modified", 2, 0, false)],
    &[("ember/app/src/ui/hunks.rs", None, "added", 58, 0, false), ("ember/app/src/ui/vcs.rs", None, "modified", 53, 10, false), ("ember/node/src/git.rs", None, "modified", 30, 4, false)],
    &[("ember/app/src/ui/hunks.rs", None, "added", 58, 0, false), ("ember/app/src/ui/vcs.rs", None, "modified", 12, 3, false)],
    &[("ember/node/src/git.rs", None, "modified", 88, 131, false), ("ember/node/src/git_status.rs", None, "added", 140, 0, false)],
    &[("ember/node/src/git.rs", None, "modified", 9, 3, false), ("ember/node/tests/git_api.rs", None, "modified", 31, 0, false), ("docs/notes.md", Some("docs/old-notes.md"), "renamed", 2, 1, false)],
    &[("Cargo.lock", None, "modified", 120, 118, false), ("ember/node/Cargo.toml", None, "modified", 3, 3, false), ("ember/app/assets/icon.png", None, "modified", 0, 0, true)],
    &[("ember/node/src/git.rs", None, "modified", 64, 2, false), ("ember/client/src/api.rs", None, "modified", 11, 0, false)],
    &[("ember/node/src/git.rs", None, "modified", 140, 0, false), ("ember/app/src/ui/vcs.rs", None, "modified", 33, 5, false), ("ember/node/src/old_commit.rs", None, "deleted", 0, 40, false)],
    &[("README.md", None, "added", 20, 0, false), ("Cargo.toml", None, "added", 12, 0, false), ("ember/node/src/main.rs", None, "added", 45, 0, false), (".gitignore", None, "added", 6, 0, false)],
];

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

/// What the node answers for a path that is not unmerged (409).
const NOT_UNMERGED: &str = "병합 충돌 중인 파일이 아닙니다.";

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
    /// Unmerged files (FR-G7), in path order; resolving one removes it.
    conflicts: Vec<DemoConflict>,
    /// A merge of `origin/main` is in progress (FR-G16); abort and commit end it.
    merging: bool,
    /// Files staged by resolving a conflict during the merge; abort drops them and the merge commit takes them.
    merge_resolved: Vec<String>,
    /// Pull requests (FR-G8, FR-G10 to FR-G12), each as the node's detail JSON; the list op
    /// strips the detail-only keys. Review, merge and create change this in memory.
    prs: Vec<Value>,
    /// Commits made by revert and cherry-pick (FR-G15), newest first; the log lists them above the
    /// seeded history.
    applied: Vec<DemoApplied>,
}

/// One commit a demo revert or cherry-pick made: its sha, subject, time (unix seconds) and the
/// seeded commit whose files it carries (`invert` swaps additions and deletions).
#[derive(Debug, Clone)]
struct DemoApplied {
    sha: String,
    subject: String,
    time: i64,
    source: usize,
    invert: bool,
}

/// One unmerged demo file: the blocks `git/conflict` answers, as the node's JSON.
#[derive(Debug, Clone)]
struct DemoConflict {
    path: String,
    blocks: Vec<Value>,
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

fn demo_conflicts() -> Vec<DemoConflict> {
    let common = |text: &str| json!({ "kind": "common", "text": text });
    let block = |ours: &str, theirs: &str| {
        json!({ "kind": "conflict", "ours": ours, "theirs": theirs, "ours_label": "HEAD (feat/git-api)", "theirs_label": "origin/main" })
    };
    vec![
        DemoConflict {
            path: "ember/app/src/ui/cr/shell.rs".into(),
            blocks: vec![
                common("use compose_rust::ui as cu;\n"),
                block("use crate::ui::vcs::Section;\n", "use crate::ui::vcs::{Section, SectionNav};\n"),
                common("\npub fn shell() {\n"),
                block("    let pad = 16.0;\n", "    let pad = 24.0;\n"),
                common("}\n"),
            ],
        },
        DemoConflict {
            path: "docs/SPEC.md".into(),
            blocks: vec![
                common("# SPEC\n\n"),
                block("FR-G7 is in progress.\n", "FR-G7 is built.\n"),
                common("\nEnd of file"),
            ],
        },
    ]
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
            conflicts: demo_conflicts(),
            merging: true,
            merge_resolved: Vec::new(),
            prs: demo_pr_details(now_ms() / 1000),
            applied: Vec::new(),
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

    /// The log with the commits of revert and cherry-pick on top (FR-G15). The newest one carries
    /// HEAD and the current branch, which the seeded tip then loses.
    fn full_log(&self, now: i64) -> Vec<Value> {
        let mut base = demo_log(now);
        if self.applied.is_empty() {
            return base;
        }
        if let Some(refs) = base.first_mut().and_then(|c| c["refs"].as_array_mut()) {
            refs.retain(|r| r["kind"] != "head" && r["kind"] != "branch");
        }
        let mut parent = base[0]["sha"].clone();
        let mut top: Vec<Value> = Vec::new();
        for a in self.applied.iter().rev() {
            top.push(json!({
                "sha": a.sha, "parents": [parent], "author": "BREW", "email": "brew@example.com",
                "time": a.time, "subject": a.subject, "refs": [],
            }));
            parent = Value::String(a.sha.clone());
        }
        top.reverse();
        top[0]["refs"] = json!([{ "name": "HEAD", "kind": "head" }, { "name": format!("refs/heads/{}", self.current), "kind": "branch" }]);
        top.extend(base);
        top
    }

    /// The log row (and the seeded row index, or the source of an applied commit) a sha prefix names.
    fn find_commit(&self, sha: &str) -> Option<(Value, usize, bool)> {
        let sha = sha.to_ascii_lowercase();
        if sha.len() < 7 || sha.len() > 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let seeded = demo_log(now_ms() / 1000);
        for a in &self.applied {
            if a.sha.starts_with(&sha) {
                let row = self.full_log(now_ms() / 1000).into_iter().find(|c| c["sha"] == a.sha.as_str())?;
                return Some((row, a.source, a.invert));
            }
        }
        let at = seeded.iter().position(|c| c["sha"].as_str().is_some_and(|s| s.starts_with(&sha)))?;
        Some((self.full_log(now_ms() / 1000).into_iter().find(|c| c["sha"] == seeded[at]["sha"])?, at, false))
    }

    /// `commit/show` (FR-G14).
    fn commit_show(&self, body: &Value) -> Result<Value, GitError> {
        let sha = str_of(body, "sha").unwrap_or("");
        if sha.len() < 7 || sha.len() > 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(GitError::bad("커밋 SHA는 7자에서 40자의 16진수여야 합니다."));
        }
        let (row, source, invert) = self.find_commit(sha).ok_or_else(|| GitError::new(404, "not_found", "그런 커밋을 찾을 수 없습니다."))?;
        let files: Vec<Value> = DEMO_COMMIT_FILES[source]
            .iter()
            .map(|(path, old, status, add, del, binary)| {
                let (add, del) = if invert { (*del, *add) } else { (*add, *del) };
                let mut f = json!({ "path": path, "status": status, "additions": add, "deletions": del, "binary": binary });
                if let Some(old) = old {
                    f["old_path"] = json!(old);
                }
                f
            })
            .collect();
        let full = row["sha"].as_str().unwrap_or_default().to_string();
        let subject = row["subject"].as_str().unwrap_or_default().to_string();
        Ok(json!({
            "sha": full, "short": full.chars().take(7).collect::<String>(), "subject": subject,
            "body": format!("{subject}\n\n데모 커밋입니다."),
            "author": row["author"], "time": iso_time(row["time"].as_i64().unwrap_or(0)),
            "parents": row["parents"], "refs": row["refs"],
            "files_total": files.len(), "files": files, "truncated": false,
        }))
    }

    /// `revert` and `cherry-pick` (FR-G15): add a commit to the demo log, or refuse as the node does.
    fn commit_apply(&mut self, op: &str, body: &Value) -> Result<Value, GitError> {
        let sha = str_of(body, "sha").unwrap_or("");
        if sha.len() < 7 || sha.len() > 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(GitError::bad("커밋 SHA는 7자에서 40자의 16진수여야 합니다."));
        }
        if !body.get("confirm").and_then(Value::as_bool).unwrap_or(false) {
            return Err(GitError::bad("확인이 필요합니다."));
        }
        let (row, source, _) = self.find_commit(sha).ok_or_else(|| GitError::new(404, "not_found", "그런 커밋을 찾을 수 없습니다."))?;
        if self.files.iter().any(|f| !(f.kind == Kind::New && f.staged.is_empty())) {
            return Err(GitError::conflict("작업 중인 변경이 있어 되돌릴 수 없습니다. 먼저 커밋하거나 스태시하세요."));
        }
        let is_merge = row["parents"].as_array().is_some_and(|p| p.len() > 1);
        if op == "cherry-pick" && is_merge {
            return Err(GitError::bad("병합 커밋은 지원하지 않습니다."));
        }
        let subject = row["subject"].as_str().unwrap_or_default();
        let (subject, invert) = if op == "revert" { (format!("Revert \"{subject}\""), true) } else { (subject.to_string(), false) };
        self.commits += 1;
        let new_sha = format!("{:040x}", 0x00de_c0de_0000_u128 + self.commits);
        self.applied.insert(0, DemoApplied { sha: new_sha.clone(), subject, time: now_ms() / 1000, source, invert });
        self.branch_mut().ahead += 1;
        Ok(json!({ "new_sha": new_sha }))
    }

    /// `merge/abort` and `merge/commit` (FR-G17), refusing as the node does.
    fn merge_end(&mut self, op: &str, body: &Value) -> Result<Value, GitError> {
        if !body.get("confirm").and_then(Value::as_bool).unwrap_or(false) {
            return Err(GitError::bad("확인이 필요합니다."));
        }
        if !self.merging {
            return Err(GitError::conflict("진행 중인 병합이 없습니다."));
        }
        let resolved = std::mem::take(&mut self.merge_resolved);
        if op == "merge/abort" {
            self.conflicts.clear();
            self.files.retain(|f| !resolved.contains(&f.path));
            self.merging = false;
            return Ok(json!({ "ok": true }));
        }
        if !self.conflicts.is_empty() {
            self.merge_resolved = resolved;
            return Err(GitError::conflict("충돌이 남아 있어 병합 커밋을 만들 수 없습니다."));
        }
        // The merge commit takes the files the resolutions staged.
        self.files.retain(|f| !resolved.contains(&f.path));
        self.merging = false;
        self.commits += 1;
        let new_sha = format!("{:040x}", 0x00de_c0de_0000_u128 + self.commits);
        let subject = format!("Merge origin/main into {}", self.current);
        self.applied.insert(0, DemoApplied { sha: new_sha.clone(), subject, time: now_ms() / 1000, source: 0, invert: false });
        self.branch_mut().ahead += 1;
        Ok(json!({ "new_sha": new_sha }))
    }

    fn tidy(&mut self) {
        self.files.retain(|f| !(f.staged.is_empty() && f.unstaged.is_empty()));
    }

    /// The pull request ops of FR-G10 to FR-G12 on the in-memory list.
    fn pr_op(&mut self, op: &str, body: &Value) -> Result<Value, GitError> {
        let confirmed = body.get("confirm").and_then(Value::as_bool).unwrap_or(false);
        if op == "pr/create" {
            if !confirmed {
                return Err(GitError::bad("확인이 필요합니다."));
            }
            let title = str_of(body, "title").unwrap_or("").trim();
            if title.is_empty() {
                return Err(GitError::bad("제목을 입력하세요."));
            }
            if title.chars().count() > 256 || title.chars().any(char::is_control) {
                return Err(GitError::bad("제목은 한 줄, 256자 이내여야 합니다."));
            }
            let number = self.prs.iter().filter_map(|p| p["number"].as_u64()).max().unwrap_or(0) + 1;
            let head = str_of(body, "head").map(str::trim).filter(|h| !h.is_empty()).unwrap_or(&self.current).to_string();
            let base = str_of(body, "base").map(str::trim).filter(|b| !b.is_empty()).unwrap_or("develop").to_string();
            let draft = body.get("draft").and_then(Value::as_bool).unwrap_or(false);
            let url = format!("https://github.com/DarkPyonix/darkpyonix-ember/pull/{number}");
            let gate = demo_gate("open", draft, "none", (0, 0, 0));
            self.prs.insert(0, json!({
                "number": number, "title": title, "state": "open", "draft": draft, "author": "demo",
                "head": head, "base": base, "updated_at": iso_time(now_ms() / 1000).replace("+00:00", "Z"),
                "review": "none", "checks": { "passed": 0, "failed": 0, "pending": 0 },
                "merge": gate, "url": url,
                "body": str_of(body, "body").unwrap_or(""), "check_runs": [], "reviews": [], "comments": [],
                "requested_reviewers": [], "additions": 0, "deletions": 0, "changed_files": 0,
                "head_sha": demo_head_sha(number),
            }));
            return Ok(json!({ "number": number, "url": url }));
        }
        let number = body.get("number").and_then(Value::as_u64).unwrap_or(0);
        if number == 0 || number > 999_999 {
            return Err(GitError::bad("풀 리퀘스트 번호는 1에서 999999 사이여야 합니다."));
        }
        let Some(at) = self.prs.iter().position(|p| p["number"] == number) else {
            return Err(GitError::conflict("GitHub에서 저장소를 찾을 수 없습니다 (원격 주소와 접근 권한을 확인하세요).\nno pull requests found"));
        };
        if op == "pr" {
            return Ok(self.prs[at].clone());
        }
        if !confirmed {
            return Err(GitError::bad("확인이 필요합니다."));
        }
        let now = iso_time(now_ms() / 1000).replace("+00:00", "Z");
        let pr = &mut self.prs[at];
        if op == "pr/merge" {
            let method = str_of(body, "method").unwrap_or("");
            if !["merge", "squash", "rebase"].contains(&method) {
                return Err(GitError::bad("method must be merge, squash or rebase"));
            }
            let sha = str_of(body, "head_sha").map(str::trim).unwrap_or("");
            if sha.is_empty() {
                return Err(GitError::bad("확인한 헤드가 필요합니다."));
            }
            if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(GitError::bad("head_sha는 40자리 16진수여야 합니다."));
            }
            if pr["merge"]["can_merge"] != true {
                return Err(GitError::conflict(pr["merge"]["reason"].as_str().unwrap_or("병합할 수 없습니다.")));
            }
            if !pr["head_sha"].as_str().unwrap_or("").eq_ignore_ascii_case(sha) {
                return Err(GitError::conflict("풀 리퀘스트가 바뀌었습니다. 다시 불러온 뒤 병합하세요."));
            }
            pr["state"] = json!("merged");
            pr["merge"] = demo_gate("merged", false, "approved", (0, 0, 0));
            pr["updated_at"] = json!(now);
            return Ok(json!({ "number": number, "merged": true, "method": method }));
        }
        // pr/review
        if pr["state"] != "open" {
            return Err(GitError::conflict("열려 있지 않은 풀 리퀘스트에는 리뷰할 수 없습니다."));
        }
        let action = str_of(body, "action").unwrap_or("");
        let text = str_of(body, "body").unwrap_or("").trim().to_string();
        let state = match action {
            "approve" => "approved",
            "request_changes" => "changes_requested",
            "comment" => "commented",
            _ => return Err(GitError::bad("action must be approve, request_changes or comment")),
        };
        if state != "approved" && text.is_empty() {
            return Err(GitError::bad("내용을 입력하세요. 변경 요청과 댓글에는 본문이 필요합니다."));
        }
        pr["reviews"].as_array_mut().expect("reviews is an array").push(json!({
            "author": "demo", "state": state, "body": text, "submitted_at": now,
        }));
        if state != "commented" {
            pr["review"] = json!(if state == "approved" { "approved" } else { "changes_requested" });
        }
        let c = &pr["checks"];
        let counts = (c["passed"].as_u64().unwrap_or(0) as u32, c["failed"].as_u64().unwrap_or(0) as u32, c["pending"].as_u64().unwrap_or(0) as u32);
        pr["merge"] = demo_gate("open", pr["draft"] == true, pr["review"].as_str().unwrap_or("none"), counts);
        pr["updated_at"] = json!(now);
        Ok(json!({ "number": number, "action": action }))
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
            // The demo folder is its repository's root (FR-L31.9).
            "toplevel" => Ok(json!({ "toplevel": "", "prefix": "" })),
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
                    // The old name of a rename is part of that file here, as `git reset -- new old` treats it.
                    if self.files.iter().any(|f| f.old_path.as_deref() == Some(p.as_str()) && f.path != *p) {
                        continue;
                    }
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
                    if !self.branches.iter().any(|b| b.name == from) && self.find_commit(from).is_none() {
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
            "conflicts" => Ok(json!({ "paths": self.conflicts.iter().map(|c| c.path.clone()).collect::<Vec<_>>() })),
            "conflict" => {
                let path = str_of(body, "path").ok_or_else(|| GitError::bad("path is required"))?;
                let c = self.conflicts.iter().find(|c| c.path == path).ok_or_else(|| GitError::conflict(NOT_UNMERGED))?;
                Ok(json!({ "blocks": c.blocks }))
            }
            "resolve" => {
                let path = str_of(body, "path").ok_or_else(|| GitError::bad("path is required"))?.to_string();
                let content = str_of(body, "content").ok_or_else(|| GitError::bad("content is required"))?.to_string();
                let at = self.conflicts.iter().position(|c| c.path == path).ok_or_else(|| GitError::conflict(NOT_UNMERGED))?;
                self.conflicts.remove(at);
                if self.merging && !self.merge_resolved.contains(&path) {
                    self.merge_resolved.push(path.clone());
                }
                // Resolving stages the file: the whole content as one staged hunk.
                let lines: Vec<String> = content.lines().map(|l| format!("+{l}")).collect();
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                let staged = vec![hunk(&format!("@@ -0,0 +1,{} @@", refs.len().max(1)), &refs)];
                self.files.retain(|f| f.path != path);
                self.files.push(DemoFile { path, old_path: None, kind: Kind::Modified, staged, unstaged: vec![] });
                Ok(Value::Null)
            }
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
                Ok(json!({ "commits": self.full_log(now_ms() / 1000).into_iter().skip(skip).take(limit).collect::<Vec<_>>() }))
            }
            "commit/show" => self.commit_show(body),
            "revert" | "cherry-pick" => self.commit_apply(op, body),
            "merge" => Ok(json!({
                "in_progress": self.merging,
                "from": if self.merging { json!("origin/main") } else { Value::Null },
                "into": self.current,
                "conflict_files": self.conflicts.len(),
                "can_commit": self.merging && self.conflicts.is_empty(),
            })),
            "merge/abort" | "merge/commit" => self.merge_end(op, body),
            "branches" => Ok(json!({ "branches": demo_branches(now_ms() / 1000) })),
            "prs" => {
                let state = body.get("state").and_then(Value::as_str).unwrap_or("open");
                if !["open", "closed", "merged", "all"].contains(&state) {
                    return Err(GitError::bad("state must be open, closed, merged or all"));
                }
                let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(30);
                if limit == 0 || limit > 200 {
                    return Err(GitError::bad("limit은 1에서 200 사이여야 합니다."));
                }
                let prs: Vec<Value> = self
                    .prs
                    .iter()
                    .filter(|p| state == "all" || p["state"] == state)
                    .take(limit as usize)
                    .map(|p| {
                        let mut list = p.clone();
                        for key in PR_DETAIL_KEYS {
                            list.as_object_mut().expect("a pull request is an object").remove(*key);
                        }
                        list
                    })
                    .collect();
                Ok(json!({ "prs": prs }))
            }
            "pr" | "pr/review" | "pr/merge" | "pr/create" => self.pr_op(op, body),
            _ => Err(GitError::new(404, "not_found", "unknown git operation")),
        }
    }
}

/// Local branches as the node answers them (`/git/branches`, FR-G9): the current one with an
/// upstream ahead 2 behind 1, the default, a plain one and one checked out in a worktree.
/// `now` is unix seconds.
fn demo_branches(now: i64) -> Vec<Value> {
    // (name, current, default, upstream, ahead, behind, sha, subject, minutes ago, worktree)
    type Row = (&'static str, bool, bool, Option<&'static str>, u32, u32, &'static str, &'static str, i64, Option<&'static str>);
    const ROWS: &[Row] = &[
        ("feat/git-api", true, false, Some("origin/feat/git-api"), 2, 1, "a1b2c3d", "Feat: Git log API and history list", 12, Some("/Users/demo/darkpyonix-ember")),
        ("main", false, true, Some("origin/main"), 0, 0, "7f6e5d4", "Merge pull request #398 from docs/commit-rules", 60 * 26, None),
        ("develop", false, false, Some("origin/develop"), 0, 3, "3c2b1a0", "Docs: describe the commit message rules", 60 * 5, None),
        ("feat/ui-phase-6", false, false, None, 0, 0, "9e8d7c6", "Feat: screen 6 layout", 60 * 30, Some("/Users/demo/worktrees/ui-phase-6")),
    ];
    ROWS.iter()
        .map(|(name, current, default, upstream, ahead, behind, sha, subject, minutes, worktree)| {
            json!({
                "name": name,
                "is_current": current,
                "is_default": default,
                "upstream": upstream,
                "ahead": ahead,
                "behind": behind,
                "last_sha": sha,
                "last_subject": subject,
                "last_time": iso_time(now - minutes * 60),
                "worktree": worktree,
            })
        })
        .collect()
}

/// Pull requests as the node answers them (`/git/prs`, FR-G8): an approved one with green checks,
/// one with a failing check, a draft and a merged one. `now` is unix seconds.
fn demo_prs(now: i64) -> Vec<Value> {
    // (number, title, state, draft, author, head, minutes ago, review, (passed, failed, pending), (can_merge, reason))
    type Row = (u64, &'static str, &'static str, bool, &'static str, &'static str, i64, &'static str, (u32, u32, u32), (bool, &'static str));
    const ROWS: &[Row] = &[
        (412, "Feat: attach the session shim before routing", "open", false, "BREW", "feat/session-shim", 12, "approved", (3, 0, 0), (true, "병합할 수 있습니다.")),
        (409, "Fix: stop egress when a session detaches", "open", false, "Minji Kim", "fix/egress-stop", 60 * 5, "review_required", (2, 1, 0), (false, "CI 체크가 실패했습니다 (실패 1개). 통과해야 병합할 수 있습니다.")),
        (405, "Feat: channel feed with mentions", "open", true, "Jisoo Park", "feat/channel-feed", 60 * 26, "none", (1, 0, 2), (false, "초안 상태라 병합할 수 없습니다. 검토 준비 후 초안을 해제하세요.")),
        (398, "Docs: describe the commit message rules", "merged", false, "Minji Kim", "docs/commit-rules", 60 * 24 * 3, "approved", (3, 0, 0), (false, "이미 병합되었습니다.")),
    ];
    ROWS.iter()
        .map(|(number, title, state, draft, author, head, minutes, review, checks, merge)| {
            json!({
                "number": number,
                "title": title,
                "state": state,
                "draft": draft,
                "author": author,
                "head": head,
                "base": "develop",
                "updated_at": iso_time(now - minutes * 60).replace("+00:00", "Z"),
                "review": review,
                "checks": { "passed": checks.0, "failed": checks.1, "pending": checks.2 },
                "merge": { "can_merge": merge.0, "reason": merge.1 },
                "url": format!("https://github.com/DarkPyonix/darkpyonix-ember/pull/{number}"),
            })
        })
        .collect()
}

/// Keys the detail has and the list does not.
const PR_DETAIL_KEYS: &[&str] = &["body", "check_runs", "reviews", "comments", "requested_reviewers", "additions", "deletions", "changed_files", "head_sha"];

/// The fixed fake head commit of a demo pull request: 40 hex characters.
fn demo_head_sha(number: u64) -> String {
    format!("{:040x}", 0xdec0de00_0000_0000u64 + number)
}

/// The merge gate of FR-L19 with the node's Korean texts (`ember/node/src/git_prs.rs`).
fn demo_gate(state: &str, draft: bool, review: &str, checks: (u32, u32, u32)) -> Value {
    let (can, reason) = if state == "merged" {
        (false, "이미 병합되었습니다.".to_string())
    } else if state != "open" {
        (false, "닫힌 풀 리퀘스트입니다.".to_string())
    } else if draft {
        (false, "초안 상태라 병합할 수 없습니다. 검토 준비 후 초안을 해제하세요.".to_string())
    } else if checks.1 > 0 {
        (false, format!("CI 체크가 실패했습니다 (실패 {}개). 통과해야 병합할 수 있습니다.", checks.1))
    } else if checks.2 > 0 {
        (false, format!("CI 체크가 아직 진행 중입니다 (대기 {}개). 모두 통과해야 합니다.", checks.2))
    } else if review == "approved" {
        (true, "병합할 수 있습니다.".to_string())
    } else if review == "changes_requested" {
        (false, "변경 요청이 있습니다. 수정 후 다시 승인을 받아야 합니다.".to_string())
    } else {
        (false, "승인이 필요합니다. 리뷰어의 승인을 받아야 합니다.".to_string())
    };
    json!({ "can_merge": can, "reason": reason })
}

/// The demo pull requests with the detail the node adds (`/git/pr`, FR-G10): three checks per
/// pull request (passed, then failed, then pending, named build, lint, e2e), reviews, comments.
fn demo_pr_details(now: i64) -> Vec<Value> {
    let at = |minutes: i64| iso_time(now - minutes * 60).replace("+00:00", "Z");
    demo_prs(now)
        .into_iter()
        .map(|mut pr| {
            let number = pr["number"].as_u64().unwrap_or(0);
            let c = pr["checks"].clone();
            let mut runs = Vec::new();
            let names = ["build", "lint", "e2e"];
            let mut k = 0;
            for (state, n) in [("passed", c["passed"].as_u64()), ("failed", c["failed"].as_u64()), ("pending", c["pending"].as_u64())] {
                for _ in 0..n.unwrap_or(0) {
                    let name = names[k % 3];
                    k += 1;
                    runs.push(json!({ "name": name, "state": state, "detail_url": format!("https://github.com/DarkPyonix/darkpyonix-ember/actions/runs/{number}{k}") }));
                }
            }
            let (reviews, comments, requested): (Vec<Value>, Vec<Value>, Vec<&str>) = match number {
                412 => (
                    vec![json!({ "author": "Minji Kim", "state": "approved", "body": "shim 순서가 명확해졌습니다. 병합해도 좋습니다.", "submitted_at": at(30) }),
                         json!({ "author": "Jisoo Park", "state": "commented", "body": "", "submitted_at": at(50) })],
                    vec![json!({ "author": "Jisoo Park", "body": "라우팅 전에 attach가 끝나는지 테스트가 있나요?", "created_at": at(90) }),
                         json!({ "author": "BREW", "body": "fr_s3_* 테스트에 추가했습니다.", "created_at": at(60) })],
                    vec![],
                ),
                409 => (
                    vec![json!({ "author": "BREW", "state": "changes_requested", "body": "lint 실패를 먼저 고쳐 주세요.", "submitted_at": at(200) }),
                         json!({ "author": "Jisoo Park", "state": "commented", "body": "분리된 세션에서도 재현됩니다.", "submitted_at": at(260) })],
                    vec![json!({ "author": "Minji Kim", "body": "lint 수정 후 다시 올리겠습니다.", "created_at": at(240) }),
                         json!({ "author": "BREW", "body": "확인했습니다.", "created_at": at(220) })],
                    vec!["BREW"],
                ),
                405 => (
                    vec![json!({ "author": "BREW", "state": "commented", "body": "초안이지만 방향은 좋습니다.", "submitted_at": at(1000) }),
                         json!({ "author": "Minji Kim", "state": "pending", "body": "", "submitted_at": "" })],
                    vec![json!({ "author": "Jisoo Park", "body": "멘션 파서는 다음 커밋에서 붙입니다.", "created_at": at(1200) }),
                         json!({ "author": "BREW", "body": "e2e가 끝나면 초안을 해제하세요.", "created_at": at(1100) })],
                    vec!["Minji Kim"],
                ),
                _ => (
                    vec![json!({ "author": "BREW", "state": "approved", "body": "", "submitted_at": at(4400) }),
                         json!({ "author": "Jisoo Park", "state": "approved", "body": "좋습니다.", "submitted_at": at(4350) })],
                    vec![json!({ "author": "Minji Kim", "body": "병합 규칙 문서 반영했습니다.", "created_at": at(4380) }),
                         json!({ "author": "BREW", "body": "병합합니다.", "created_at": at(4320) })],
                    vec![],
                ),
            };
            let o = pr.as_object_mut().expect("a pull request is an object");
            o.insert("body".into(), json!(format!("데모 풀 리퀘스트 #{number}의 설명입니다.\n\n- 변경 범위\n- 확인 방법")));
            o.insert("check_runs".into(), json!(runs));
            o.insert("reviews".into(), json!(reviews));
            o.insert("comments".into(), json!(comments));
            o.insert("requested_reviewers".into(), json!(requested));
            o.insert("additions".into(), json!(40 + number % 90));
            o.insert("deletions".into(), json!(number % 30));
            o.insert("changed_files".into(), json!(2 + number % 7));
            o.insert("head_sha".into(), json!(demo_head_sha(number)));
            pr
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn fr_l18_demo_ports_show_open_and_note_only_rows() {
        use crate::ui::session::{web_pages_from, ComputerHost, WebPagesLoad};
        let host = ComputerHost { local: false, host: Some(crate::demo::DEMO_HOST.into()) };
        let WebPagesLoad::Ready(pages) = web_pages_from(Ok(crate::demo::ports()), &host) else {
            panic!("demo ports are ready");
        };
        let urls: Vec<_> = pages.iter().map(|p| p.url.as_deref()).collect();
        assert_eq!(
            urls,
            [Some("http://192.168.0.10:3000"), None, Some("http://192.168.0.10:6006")]
        );
        assert!(pages[1].note.is_some());
    }

    #[test]
    fn fr_l31_demo_ember_target_carries_the_given_ose_connection() {
        let ose = ember_client::wire::EmberOseConnection {
            endpoint: "http://127.0.0.1:18765".into(),
            connection_token: String::new(),
        };
        let l = ide_launch_with("s", "p", "/w", Some(ose));
        let c = crate::server::IdeLaunch::editor_connection(Some(&Ok(l)));
        assert_eq!(c.endpoint.as_deref(), Some("127.0.0.1:18765"));
        assert_eq!(c.computer, None, "the demo has no computer id, so the editor must not try a terminal relay");
        let none = ide_launch_with("s", "p", "/w", None);
        assert!(none.target("ember").unwrap().launch.is_none());
    }

    use super::*;

    #[test]
    fn fr_l21_demo_metrics_are_plausible_and_give_a_network_rate() {
        let a = metrics_sample(1_000_000);
        let b = metrics_sample(1_003_000);
        assert!(a.cpu.total_percent.unwrap() <= 100.0 && !a.cpu.cores.is_empty());
        let m = a.memory.as_ref().unwrap();
        assert!(m.used_bytes < m.total_bytes);
        assert!(a.gpu.devices.is_empty() && a.gpu.reason.is_some());
        let (rx, _) = crate::ui::monitor::net_rates(&a, &b).unwrap();
        assert_eq!(rx, 2_400_000);
    }

    #[test]
    fn fr_l21_demo_feed_has_a_network_rate_after_the_first_poll() {
        let now = 1_000_000u64;
        let mut f = crate::ui::monitor::MonitorFeed::default();
        f.push(metrics_previous(now));
        f.push(metrics_sample(now));
        let (rx, tx) = f.rates().unwrap();
        assert!(rx > 1 << 20 && tx > 0);
        let net = crate::ui::monitor::card_view(crate::ui::monitor::Metric::Network, &f).unwrap();
        assert!(net.headline.contains("MB/s"), "{}", net.headline);
        assert!(metrics_previous(0).at_ms == 0);
    }
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

    fn clean_demo() -> DemoSource {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        gs(&d, "stash/push", json!({})).unwrap();
        d
    }

    #[test]
    fn fr_g14_demo_commit_show_lists_two_to_four_files_for_every_log_commit() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let log = gs(&d, "log", json!({})).unwrap();
        for c in log["commits"].as_array().unwrap() {
            let sha = c["sha"].as_str().unwrap();
            let v = gs(&d, "commit/show", json!({ "sha": &sha[..9] })).unwrap();
            assert_eq!(v["sha"], sha);
            assert_eq!(v["short"], &sha[..7]);
            assert_eq!(v["parents"], c["parents"]);
            assert!(v["time"].as_str().unwrap().contains('T'));
            let n = v["files"].as_array().unwrap().len();
            assert!((2..=4).contains(&n), "{sha}: {n}");
            assert_eq!(v["files_total"], n);
        }
    }

    #[test]
    fn fr_g14_demo_commit_show_decodes_with_the_client_type_and_has_a_merge_and_a_rename() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let log = gs(&d, "log", json!({})).unwrap();
        let commits = log["commits"].as_array().unwrap();
        let merge = commits.iter().find(|c| c["parents"].as_array().unwrap().len() == 2).unwrap();
        let v = gs(&d, "commit/show", json!({ "sha": merge["sha"] })).unwrap();
        let decoded: ember_client::api::GitCommitDetail = serde_json::from_value(v).unwrap();
        assert_eq!(decoded.parents.len(), 2);
        assert!(!decoded.files.is_empty());
        let renamed = commits.iter().filter_map(|c| gs(&d, "commit/show", json!({ "sha": c["sha"] })).ok())
            .any(|v| v["files"].as_array().unwrap().iter().any(|f| f["status"] == "renamed" && f["old_path"].is_string()));
        assert!(renamed);
    }

    #[test]
    fn fr_g14_demo_commit_show_unknown_is_404_and_bad_is_400() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        assert_eq!(gs(&d, "commit/show", json!({ "sha": "deadbeef" })).unwrap_err().status, 404);
        for sha in ["abc12", "--all", "zzzzzzz"] {
            assert_eq!(gs(&d, "commit/show", json!({ "sha": sha })).unwrap_err().status, 400, "{sha}");
        }
    }

    #[test]
    fn fr_g14_demo_branch_can_start_from_a_commit_sha() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        gs(&d, "branch", json!({ "name": "from-sha", "from": "9c8d7e6" })).unwrap();
        assert_eq!(status(&d)["branch"], "from-sha");
        assert_eq!(gs(&d, "branch", json!({ "name": "other", "from": "deadbeef" })).unwrap_err().status, 400);
    }

    #[test]
    fn fr_g15_demo_revert_adds_a_commit_on_top_of_the_log() {
        let d = clean_demo();
        let before = gs(&d, "log", json!({})).unwrap()["commits"].as_array().unwrap().clone();
        let r = gs(&d, "revert", json!({ "sha": "9c8d7e6f", "confirm": true })).unwrap();
        let after = gs(&d, "log", json!({})).unwrap()["commits"].as_array().unwrap().clone();
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(after[0]["sha"], r["new_sha"]);
        assert_eq!(after[0]["subject"], "Revert \"Docs: describe the commit message rules\"");
        assert_eq!(after[0]["parents"], json!([before[0]["sha"]]));
        let heads = after.iter().filter(|c| c["refs"].as_array().unwrap().iter().any(|r| r["kind"] == "head")).count();
        assert_eq!(heads, 1);
        let shown = gs(&d, "commit/show", json!({ "sha": r["new_sha"] })).unwrap();
        assert_eq!(shown["files"][0]["deletions"], 14);
    }

    #[test]
    fn fr_g15_demo_cherry_pick_adds_a_commit_and_refuses_a_merge() {
        let d = clean_demo();
        let r = gs(&d, "cherry-pick", json!({ "sha": "5d6e7f8", "confirm": true })).unwrap();
        let log = gs(&d, "log", json!({ "limit": 1 })).unwrap();
        assert_eq!(log["commits"][0]["sha"], r["new_sha"]);
        assert_eq!(log["commits"][0]["subject"], "Test: cover unborn branch history");
        let e = gs(&d, "cherry-pick", json!({ "sha": "2b3c4d5", "confirm": true })).unwrap_err();
        assert_eq!((e.status, e.message.as_str()), (400, "병합 커밋은 지원하지 않습니다."));
    }

    fn resolve_all(d: &DemoSource) {
        for p in conflict_paths(d) {
            gs(d, "resolve", json!({ "path": p, "content": "merged\n" })).unwrap();
        }
    }

    #[test]
    fn fr_g16_demo_merge_state_reports_origin_main_into_the_current_branch() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let v = gs(&d, "merge", json!({})).unwrap();
        assert_eq!(v, json!({ "in_progress": true, "from": "origin/main", "into": status(&d)["branch"], "conflict_files": 2, "can_commit": false }));
        let path = conflict_paths(&d)[0].clone();
        gs(&d, "resolve", json!({ "path": path, "content": "x\n" })).unwrap();
        assert_eq!(gs(&d, "merge", json!({})).unwrap()["conflict_files"], 1);
        resolve_all(&d);
        let v = gs(&d, "merge", json!({})).unwrap();
        assert_eq!((v["conflict_files"].clone(), v["can_commit"].clone()), (json!(0), json!(true)));
    }

    #[test]
    fn fr_g17_demo_abort_clears_the_conflicts_and_the_merge() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let path = conflict_paths(&d)[0].clone();
        gs(&d, "resolve", json!({ "path": path.clone(), "content": "x\n" })).unwrap();
        assert_eq!(gs(&d, "merge/abort", json!({ "confirm": true })).unwrap(), json!({ "ok": true }));
        assert!(conflict_paths(&d).is_empty());
        let v = gs(&d, "merge", json!({})).unwrap();
        assert_eq!((v["in_progress"].clone(), v["from"].clone(), v["can_commit"].clone()), (json!(false), Value::Null, json!(false)));
        assert!(status(&d)["files"].as_array().unwrap().iter().all(|f| f["path"] != path.as_str()));
        for op in ["merge/abort", "merge/commit"] {
            let e = gs(&d, op, json!({ "confirm": true })).unwrap_err();
            assert_eq!((e.status, e.message.as_str()), (409, "진행 중인 병합이 없습니다."), "{op}");
        }
    }

    #[test]
    fn fr_g17_demo_commit_needs_every_conflict_resolved_then_adds_a_commit() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let e = gs(&d, "merge/commit", json!({ "confirm": true })).unwrap_err();
        assert_eq!((e.status, e.message.as_str()), (409, "충돌이 남아 있어 병합 커밋을 만들 수 없습니다."));
        assert_eq!(conflict_paths(&d).len(), 2);
        resolve_all(&d);
        let before = gs(&d, "log", json!({})).unwrap()["commits"].as_array().unwrap().len();
        let r = gs(&d, "merge/commit", json!({ "confirm": true })).unwrap();
        let log = gs(&d, "log", json!({})).unwrap();
        assert_eq!(log["commits"].as_array().unwrap().len(), before + 1);
        assert_eq!(log["commits"][0]["sha"], r["new_sha"]);
        assert!(log["commits"][0]["subject"].as_str().unwrap().starts_with("Merge origin/main into "));
        assert_eq!(gs(&d, "merge", json!({})).unwrap()["in_progress"], false);
    }

    #[test]
    fn fr_g17_demo_confirm_is_required() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        for op in ["merge/abort", "merge/commit"] {
            let e = gs(&d, op, json!({})).unwrap_err();
            assert_eq!((e.status, e.message.as_str()), (400, "확인이 필요합니다."), "{op}");
        }
        assert_eq!(conflict_paths(&d).len(), 2);
    }

    #[test]
    fn fr_g15_demo_dirty_tree_is_409_and_confirm_is_required() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        for op in ["revert", "cherry-pick"] {
            let e = gs(&d, op, json!({ "sha": "9c8d7e6", "confirm": true })).unwrap_err();
            assert_eq!(e.status, 409, "{op}");
            assert_eq!(e.message, "작업 중인 변경이 있어 되돌릴 수 없습니다. 먼저 커밋하거나 스태시하세요.");
            assert_eq!(gs(&d, op, json!({ "sha": "9c8d7e6" })).unwrap_err().status, 400);
        }
        assert_eq!(gs(&d, "log", json!({})).unwrap()["commits"].as_array().unwrap().len(), 12);
    }

    #[test]
    fn fr_g9_demo_branches_have_the_node_shape_and_order() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let list = gs(&d, "branches", json!({})).unwrap();
        let decoded: ember_client::api::GitBranches = serde_json::from_value(list).unwrap();
        let names: Vec<&str> = decoded.branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names[..2], ["feat/git-api", "main"]);
        let cur = &decoded.branches[0];
        assert!(cur.is_current && !cur.is_default);
        assert_eq!((cur.upstream.as_deref(), cur.ahead, cur.behind), (Some("origin/feat/git-api"), 2, 1));
        assert!(decoded.branches[1].is_default);
        let wt = decoded.branches.iter().find(|b| b.name == "feat/ui-phase-6").unwrap();
        assert_eq!(wt.worktree.as_deref(), Some("/Users/demo/worktrees/ui-phase-6"));
        assert!(wt.upstream.is_none());
        assert!(decoded.branches.iter().all(|b| b.last_time.as_deref().is_some_and(|t| t.contains('T')) && !b.last_sha.is_empty()));
    }

    #[test]
    fn fr_g13_demo_github_summary_is_connected_with_two_repos_and_a_ci_mix() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let g = d.github_summary();
        assert_eq!(g.state, ember_client::api::GithubState::On);
        assert_eq!(g.account.as_deref(), Some("demo-user"));
        assert_eq!(g.repos.len(), 2);
        assert_eq!(g.totals.repos, Some(2));
        let sum = |f: fn(&ember_client::api::GithubRepo) -> Option<u64>| g.repos.iter().filter_map(f).sum::<u64>();
        assert_eq!(g.totals.open_prs, Some(sum(|r| r.open_prs)));
        assert_eq!(g.totals.review_requested, Some(sum(|r| r.review_requested)));
        let runs: Vec<&str> = g.repos.iter().flat_map(|r| r.ci.iter().flatten().map(String::as_str)).collect();
        let ok = runs.iter().filter(|r| **r == "success").count() as u64;
        let bad = runs.iter().filter(|r| **r == "failure").count() as u64;
        assert!(bad > 0 && ok > bad);
        assert_eq!(g.totals.ci_pass_percent, Some((100.0 * ok as f64 / (ok + bad) as f64).round() as u64));
        // It decodes as the server's JSON would.
        let back: ember_client::api::GithubSummary = serde_json::from_value(serde_json::to_value(&g).unwrap()).unwrap();
        assert_eq!(back, g);
    }

    fn prs(d: &DemoSource, body: Value) -> Vec<Value> {
        gs(d, "prs", body).unwrap()["prs"].as_array().unwrap().clone()
    }

    #[test]
    fn fr_g8_demo_prs_default_to_the_open_ones_with_the_node_shape() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let list = prs(&d, json!({}));
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|p| p["state"] == "open"));
        let first = &list[0];
        for key in ["number", "title", "state", "draft", "author", "head", "base", "updated_at", "review", "checks", "merge", "url"] {
            assert!(!first[key].is_null(), "missing {key}");
        }
        assert!(first["updated_at"].as_str().unwrap().ends_with('Z'));
        // The client decodes the same JSON.
        let decoded: ember_client::api::GitPrs = serde_json::from_value(json!({ "prs": list })).unwrap();
        assert_eq!(decoded.prs.len(), 3);
    }

    #[test]
    fn fr_g8_demo_prs_cover_green_failing_draft_and_merged() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let all = prs(&d, json!({ "state": "all" }));
        assert_eq!(all.len(), 4);
        assert_eq!(all[0]["merge"]["can_merge"], true);
        assert_eq!(all[0]["review"], "approved");
        assert_eq!(all[1]["checks"]["failed"], 1);
        assert!(all[1]["merge"]["reason"].as_str().unwrap().contains("CI"));
        assert_eq!(all[2]["draft"], true);
        assert_eq!(all[3]["state"], "merged");
        assert!(all.iter().filter(|p| p["merge"]["can_merge"] == true).count() == 1);
        let merged = prs(&d, json!({ "state": "merged" }));
        assert_eq!(merged.len(), 1);
        assert!(prs(&d, json!({ "state": "closed" })).is_empty());
        assert_eq!(prs(&d, json!({ "state": "all", "limit": 2 })).len(), 2);
    }

    #[test]
    fn fr_g8_demo_prs_refuse_a_bad_state_or_limit() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        assert_eq!(gs(&d, "prs", json!({ "state": "nope" })).unwrap_err().status, 400);
        assert_eq!(gs(&d, "prs", json!({ "limit": 0 })).unwrap_err().status, 400);
    }

    fn pr_detail(d: &DemoSource, number: u64) -> Value {
        gs(d, "pr", json!({ "number": number })).unwrap()
    }

    #[test]
    fn fr_g10_demo_detail_has_three_checks_two_reviews_two_comments_in_the_node_shape() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        for number in [412, 409, 405, 398] {
            let p = pr_detail(&d, number);
            assert_eq!(p["check_runs"].as_array().unwrap().len(), 3, "{number}");
            assert_eq!(p["reviews"].as_array().unwrap().len(), 2, "{number}");
            assert_eq!(p["comments"].as_array().unwrap().len(), 2, "{number}");
            assert!(p["body"].as_str().unwrap().contains(&number.to_string()));
            let c = &p["checks"];
            assert_eq!(c["passed"].as_u64().unwrap() + c["failed"].as_u64().unwrap() + c["pending"].as_u64().unwrap(), 3);
            // The client decodes the same JSON.
            let decoded: ember_client::api::GitPrDetail = serde_json::from_value(p).unwrap();
            assert_eq!(decoded.pr.number, number);
        }
        let failing = pr_detail(&d, 409);
        assert!(failing["check_runs"].as_array().unwrap().iter().any(|c| c["state"] == "failed"));
        assert_eq!(gs(&d, "pr", json!({ "number": 0 })).unwrap_err().status, 400);
        assert_eq!(gs(&d, "pr", json!({ "number": 77 })).unwrap_err().status, 409);
        // The list does not carry the detail keys.
        let list = prs(&d, json!({}));
        assert!(list[0].get("check_runs").is_none() && list[0].get("reviews").is_none() && list[0].get("body").is_none());
    }

    #[test]
    fn fr_g11_demo_review_changes_the_decision_and_the_gate() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let need = gs(&d, "pr/review", json!({ "number": 409, "action": "approve" })).unwrap_err();
        assert_eq!(need.status, 400);
        assert_eq!(gs(&d, "pr/review", json!({ "number": 409, "action": "comment", "confirm": true })).unwrap_err().status, 400);
        let out = gs(&d, "pr/review", json!({ "number": 405, "action": "request_changes", "body": "고쳐 주세요", "confirm": true })).unwrap();
        assert_eq!(out, json!({ "number": 405, "action": "request_changes" }));
        let p = pr_detail(&d, 405);
        assert_eq!(p["review"], "changes_requested");
        assert_eq!(p["reviews"].as_array().unwrap().len(), 3);
        assert_eq!(p["reviews"][2]["state"], "changes_requested");
        // Approving 409 does not open the gate while a check fails.
        gs(&d, "pr/review", json!({ "number": 409, "action": "approve", "confirm": true })).unwrap();
        let p = pr_detail(&d, 409);
        assert_eq!(p["review"], "approved");
        assert_eq!(p["merge"]["can_merge"], false);
        assert!(p["merge"]["reason"].as_str().unwrap().contains("CI"));
        assert_eq!(prs(&d, json!({}))[1]["review"], "approved");
    }

    #[test]
    fn fr_g11_demo_merge_follows_the_gate() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let h = |n: u64| pr_detail(&d, n)["head_sha"].as_str().unwrap().to_string();
        let closed = gs(&d, "pr/merge", json!({ "number": 409, "method": "squash", "delete_branch": true, "head_sha": h(409), "confirm": true })).unwrap_err();
        assert_eq!(closed.status, 409);
        assert!(closed.message.contains("CI"), "{}", closed.message);
        assert_eq!(gs(&d, "pr/merge", json!({ "number": 412, "method": "squash", "head_sha": h(412) })).unwrap_err().status, 400);
        assert_eq!(gs(&d, "pr/merge", json!({ "number": 412, "method": "nope", "head_sha": h(412), "confirm": true })).unwrap_err().status, 400);
        let out = gs(&d, "pr/merge", json!({ "number": 412, "method": "squash", "delete_branch": true, "head_sha": h(412), "confirm": true })).unwrap();
        assert_eq!(out, json!({ "number": 412, "merged": true, "method": "squash" }));
        assert_eq!(pr_detail(&d, 412)["state"], "merged");
        assert_eq!(pr_detail(&d, 405)["head_sha"].as_str().unwrap().len(), 40);
        assert_eq!(prs(&d, json!({})).len(), 2, "the open list lost it");
        assert_eq!(prs(&d, json!({ "state": "merged" })).len(), 2);
        let again = gs(&d, "pr/merge", json!({ "number": 412, "method": "merge", "head_sha": h(412), "confirm": true })).unwrap_err();
        assert!(again.message.contains("병합"));
    }

    #[test]
    fn fr_g11_demo_merge_without_a_head_sha_is_400() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        for body in [json!({ "number": 412, "method": "squash", "confirm": true }), json!({ "number": 412, "method": "squash", "head_sha": "", "confirm": true })] {
            let e = gs(&d, "pr/merge", body).unwrap_err();
            assert_eq!(e.status, 400);
            assert_eq!(e.message, "확인한 헤드가 필요합니다.");
        }
        assert_eq!(pr_detail(&d, 412)["state"], "open", "nothing merged");
    }

    #[test]
    fn fr_g11_demo_merge_with_a_stale_head_sha_is_409() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let stale = gs(&d, "pr/merge", json!({ "number": 412, "method": "squash", "head_sha": "f".repeat(40), "confirm": true })).unwrap_err();
        assert_eq!(stale.status, 409);
        assert!(stale.message.contains("풀 리퀘스트가 바뀌었습니다"), "{}", stale.message);
        assert_eq!(pr_detail(&d, 412)["state"], "open", "nothing merged");
        let sha = pr_detail(&d, 412)["head_sha"].as_str().unwrap().to_string();
        gs(&d, "pr/merge", json!({ "number": 412, "method": "squash", "head_sha": sha, "confirm": true })).unwrap();
        assert_eq!(pr_detail(&d, 412)["state"], "merged");
    }

    #[test]
    fn fr_g12_demo_create_adds_an_open_pull_request_to_the_list() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        assert_eq!(gs(&d, "pr/create", json!({ "title": "T" })).unwrap_err().status, 400);
        assert_eq!(gs(&d, "pr/create", json!({ "title": "  ", "confirm": true })).unwrap_err().status, 400);
        let out = gs(&d, "pr/create", json!({ "title": "Feat: demo", "body": "why", "draft": true, "confirm": true })).unwrap();
        assert_eq!(out["number"], 413);
        assert!(out["url"].as_str().unwrap().ends_with("/pull/413"));
        let list = prs(&d, json!({}));
        assert_eq!(list.len(), 4);
        assert_eq!(list[0]["number"], 413);
        assert_eq!((list[0]["draft"].clone(), list[0]["head"].clone(), list[0]["base"].clone()), (json!(true), json!("feat/git-api"), json!("develop")));
        assert_eq!(pr_detail(&d, 413)["body"], "why");
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

    fn conflict_paths(d: &DemoSource) -> Vec<String> {
        gs(d, "conflicts", json!({})).unwrap()["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect()
    }

    fn conflict_blocks(d: &DemoSource, path: &str) -> usize {
        let v = gs(d, "conflict", json!({ "path": path })).unwrap();
        v["blocks"].as_array().unwrap().iter().filter(|b| b["kind"] == "conflict").count()
    }

    #[test]
    fn fr_g7_demo_lists_two_unmerged_files_with_two_blocks_and_one() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let paths = conflict_paths(&d);
        assert_eq!(paths.len(), 2);
        let mut counts: Vec<usize> = paths.iter().map(|p| conflict_blocks(&d, p)).collect();
        counts.sort();
        assert_eq!(counts, vec![1, 2]);
        let v = gs(&d, "conflict", json!({ "path": paths[0] })).unwrap();
        let first = v["blocks"].as_array().unwrap().iter().find(|b| b["kind"] == "conflict").unwrap();
        assert!(first["ours_label"].as_str().unwrap().contains("feat/git-api"));
        assert_eq!(first["theirs_label"], "origin/main");
    }

    #[test]
    fn fr_g7_demo_resolve_removes_the_file_from_conflicts_and_stages_it() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        let path = conflict_paths(&d)[1].clone();
        gs(&d, "resolve", json!({ "path": path, "content": "a\nb" })).unwrap();
        assert_eq!(conflict_paths(&d).len(), 1);
        assert!(!conflict_paths(&d).contains(&path));
        let f = file(&status(&d), &path).clone();
        assert!(!f.is_null(), "resolved file is in the work tree status");
        assert_eq!(f["index"], "M");
    }

    #[test]
    fn fr_g7_demo_not_unmerged_path_is_a_korean_409() {
        let d = DemoSource::new(0, std::env::temp_dir().join("ember-demo-test"));
        for (op, body) in [("conflict", json!({ "path": "nope.rs" })), ("resolve", json!({ "path": "nope.rs", "content": "x" }))] {
            let e = gs(&d, op, body).unwrap_err();
            assert_eq!(e.status, 409);
            assert_eq!(e.message, "병합 충돌 중인 파일이 아닙니다.");
        }
        let path = conflict_paths(&d)[0].clone();
        gs(&d, "resolve", json!({ "path": path, "content": "x" })).unwrap();
        assert_eq!(gs(&d, "resolve", json!({ "path": path, "content": "x" })).unwrap_err().status, 409);
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
        assert_eq!(gs(&d, "conflicts", json!({})).unwrap()["paths"].as_array().unwrap().len(), 2);
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

    /// Replay a demo transcript through the same reducer the server's events go through.
    fn replay(demo: &DemoSource, id: &str) -> ember_client::transcript::Transcript {
        let mut t = ember_client::transcript::Transcript::new();
        for ev in demo.events_for(id) {
            t.apply(&ev);
        }
        t
    }

    #[test]
    fn demo_source_199_each_demo_session_carries_every_conversation_event_kind() {
        use ember_client::transcript::{ApprovalState, TranscriptItem};
        let demo = DemoSource::new(1_788_000_000_000, std::env::temp_dir());
        for id in ["demo-s1", "demo-s2"] {
            let t = replay(&demo, id);
            let has = |f: &dyn Fn(&TranscriptItem) -> bool| t.items.iter().any(f);
            assert!(has(&|i| matches!(i, TranscriptItem::User { .. })), "{id} user");
            assert!(has(&|i| matches!(i, TranscriptItem::Assistant { .. })), "{id} assistant");
            for tool in ["Read", "Bash", "Edit"] {
                assert!(
                    has(&|i| matches!(i, TranscriptItem::ToolCall { name, result: Some(_), .. } if name == tool)),
                    "{id} {tool} call with a result"
                );
            }
            assert!(
                has(&|i| matches!(i, TranscriptItem::ToolCall { name, result: Some(r), .. }
                    if name == "Edit" && r.output.contains("@@ ") && r.output.contains("\n+"))),
                "{id} unified diff"
            );
            assert!(has(&|i| matches!(i, TranscriptItem::Approval { .. })), "{id} approval");
        }
        let s1 = replay(&demo, "demo-s1");
        assert_eq!(s1.pending_approvals().count(), 1);
        let s2 = replay(&demo, "demo-s2");
        let states: Vec<_> = s2
            .items
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::Approval { state, .. } => Some(*state),
                _ => None,
            })
            .collect();
        assert!(states.contains(&ApprovalState::Resolved(ApprovalDecision::AllowOnce)));
        assert!(states.contains(&ApprovalState::Resolved(ApprovalDecision::Deny)));
    }

    #[test]
    fn demo_source_199_every_demo_session_has_events_matching_its_last_seq() {
        let now = 1_788_000_000_000;
        let demo = DemoSource::new(now, std::env::temp_dir());
        for s in demo.sessions(now) {
            let ev = demo.events_for(&s.id);
            assert!(!ev.is_empty(), "{} has a transcript", s.id);
            assert_eq!(ev.len() as i64, s.last_seq, "{}", s.id);
        }
    }
}
