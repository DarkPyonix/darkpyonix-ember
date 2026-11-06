//! Claude Code adapter: process plumbing against a fake CLI, and an opt-in end-to-end run against
//! the real `claude` (FR-A2 acceptance: read, edit, run a command; then native resume, FR-S2).

use std::path::{Path, PathBuf};
use std::time::Duration;

use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::{AgentAdapter, AgentRun, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, QuestionAnswer, TurnOutcome};
use tokio::sync::mpsc;

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/fake-claude.sh")
}

async fn next(rx: &mut mpsc::Receiver<AgentEvent>, secs: u64) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(secs), rx.recv())
        .await
        .expect("timed out waiting for an agent event")
        .expect("agent event channel closed")
}

#[tokio::test]
async fn detect_reports_version_or_not_installed() {
    let d = ClaudeCodeAdapter::new(fake_bin()).detect().await;
    assert!(d.installed);
    assert_eq!(d.version.as_deref(), Some("9.9.9"));
    let d = ClaudeCodeAdapter::new("/nonexistent/claude").detect().await;
    assert!(!d.installed);
    assert_eq!(d.version, None);
}

#[tokio::test]
async fn fake_cli_turn_with_allow_always_and_deny() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let mut run = ClaudeCodeAdapter::new(fake_bin())
        .start(
            StartRequest {
                cwd: dir.path().to_path_buf(),
                resume_native_id: Some("prev".into()),
                model: Some("haiku".into()),
                env: Vec::new(),
                instructions: None,
                remote: None,
                mcp_servers: Vec::new(),
                computer: None,
                fork_from: None,
                mem_limit_mb: None,
                allowed_tools: Vec::new(),
            },
            tx,
        )
        .await
        .unwrap();
    run.send("hello").await.unwrap();

    assert_eq!(
        next(&mut rx, 10).await,
        AgentEvent::NativeSession { native_id: "fake-session".into() }
    );
    assert!(
        matches!(next(&mut rx, 10).await, AgentEvent::ToolCall { name, .. } if name == "Write")
    );
    let AgentEvent::ApprovalRequested { approval_id, tool, .. } = next(&mut rx, 10).await else {
        panic!("expected approval");
    };
    assert_eq!((approval_id.as_str(), tool.as_str()), ("r1", "Write"));
    run.answer("r1", ApprovalDecision::AllowAlways).await.unwrap();
    // r2 (Write again) is auto-allowed without an event; r3 (Bash) asks.
    let AgentEvent::ApprovalRequested { approval_id, tool, .. } = next(&mut rx, 10).await else {
        panic!("expected approval");
    };
    assert_eq!((approval_id.as_str(), tool.as_str()), ("r3", "Bash"));
    assert!(run.answer("nope", ApprovalDecision::Deny).await.is_err());
    run.answer("r3", ApprovalDecision::Deny).await.unwrap();
    assert!(matches!(next(&mut rx, 10).await, AgentEvent::ToolResult { .. }));
    assert_eq!(next(&mut rx, 10).await, AgentEvent::Usage { input_tokens: 3, output_tokens: 4 });
    assert_eq!(next(&mut rx, 10).await, AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
    run.shutdown().await.unwrap();

    let args = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
    assert!(args.contains("--permission-prompt-tool stdio"), "{args}");
    assert!(args.contains("--model haiku") && args.contains("--resume=prev"), "{args}");
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(dir.path().join("stdin.log"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(lines[0]["type"], "user");
    assert_eq!(lines[0]["message"]["content"], "hello");
    for (line, id) in lines[1..3].iter().zip(["r1", "r2"]) {
        assert_eq!(line["type"], "control_response");
        assert_eq!(line["response"]["request_id"], id);
        assert_eq!(line["response"]["response"]["behavior"], "allow");
    }
    assert_eq!(lines[2]["response"]["response"]["updatedInput"]["file_path"], "b");
    assert_eq!(lines[3]["response"]["request_id"], "r3");
    assert_eq!(lines[3]["response"]["response"]["behavior"], "deny");
}

/// Collect events until `TurnEnded`, answering approvals with `decide`; interrupt shortly after
/// the first call of `interrupt_on_tool`.
async fn run_turn(
    run: &mut Box<dyn AgentRun>,
    rx: &mut mpsc::Receiver<AgentEvent>,
    decide: ApprovalDecision,
    interrupt_on_tool: Option<&str>,
) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    loop {
        let ev = next(rx, 240).await;
        if !matches!(ev, AgentEvent::AssistantDelta { .. }) {
            eprintln!("event: {ev:?}");
        }
        match &ev {
            AgentEvent::ApprovalRequested { approval_id, .. } => {
                run.answer(approval_id, decide).await.unwrap();
            }
            AgentEvent::ToolCall { name, .. } if Some(name.as_str()) == interrupt_on_tool => {
                seen.push(ev);
                let id = approval_pending_or_running(rx, run, decide, &mut seen).await;
                if matches!(seen.last(), Some(AgentEvent::TurnEnded { .. })) {
                    return seen;
                }
                eprintln!("interrupting after {id}");
                run.interrupt().await.unwrap();
                continue;
            }
            _ => {}
        }
        let done = matches!(ev, AgentEvent::TurnEnded { .. });
        seen.push(ev);
        if done {
            return seen;
        }
    }
}

/// After a tool call we want to interrupt: answer its approval if one comes, then give the tool
/// two seconds to be running.
async fn approval_pending_or_running(
    rx: &mut mpsc::Receiver<AgentEvent>,
    run: &mut Box<dyn AgentRun>,
    decide: ApprovalDecision,
    seen: &mut Vec<AgentEvent>,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut what = "tool call".to_string();
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        eprintln!("event: {ev:?}");
        if let AgentEvent::ApprovalRequested { approval_id, .. } = &ev {
            run.answer(approval_id, decide).await.unwrap();
            what = format!("approval {approval_id}");
        }
        let done = matches!(ev, AgentEvent::TurnEnded { .. });
        seen.push(ev);
        if done {
            break;
        }
    }
    what
}

#[tokio::test]
#[ignore = "runs the real claude CLI; set EMBER_E2E_CLAUDE=1 and pass --ignored"]
async fn real_claude_read_edit_run_then_resume() {
    if std::env::var("EMBER_E2E_CLAUDE").as_deref() != Ok("1") {
        eprintln!("EMBER_E2E_CLAUDE != 1; skipping");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter("ember::claude=debug")
        .with_test_writer()
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    std::fs::write(cwd.join("notes.txt"), "colour: alpha\n").unwrap();
    let adapter = ClaudeCodeAdapter::from_env();
    assert!(adapter.detect().await.installed, "claude not installed");
    let req = |resume: Option<String>| StartRequest {
        cwd: cwd.clone(),
        resume_native_id: resume,
        model: Some("haiku".into()),
        env: Vec::new(),
        instructions: None,
        remote: None,
        mcp_servers: Vec::new(),
        computer: None,
        fork_from: None,
        mem_limit_mb: None,
        allowed_tools: Vec::new(),
    };

    // Turn 1: read, edit (needs approval in the default permission mode), run a command.
    let (tx, mut rx) = mpsc::channel(1024);
    let mut run = adapter.start(req(None), tx).await.unwrap();
    run.send(
        "Use the Read tool on notes.txt, then use the Edit tool to change alpha to beta in it, \
         then run the shell command `grep -c beta notes.txt` with the Bash tool. Reply briefly.",
    )
    .await
    .unwrap();
    let events = run_turn(&mut run, &mut rx, ApprovalDecision::AllowOnce, None).await;
    let native_id = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::NativeSession { native_id } => Some(native_id.clone()),
            _ => None,
        })
        .expect("native session id");
    let calls: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for tool in ["Read", "Edit", "Bash"] {
        assert!(calls.contains(&tool), "missing {tool} in {calls:?}");
    }
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ApprovalRequested { .. })));
    let results = events.iter().filter(|e| matches!(e, AgentEvent::ToolResult { .. })).count();
    assert!(results >= 3, "{results} tool results");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AssistantDelta { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AssistantMessage { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(std::fs::read_to_string(cwd.join("notes.txt")).unwrap().contains("beta"));
    run.shutdown().await.unwrap();

    // Turn 2: native resume remembers the conversation.
    let (tx, mut rx) = mpsc::channel(1024);
    let mut run = adapter.start(req(Some(native_id.clone())), tx).await.unwrap();
    run.send("Which word did you replace alpha with? Answer with that one word only.")
        .await
        .unwrap();
    let events = run_turn(&mut run, &mut rx, ApprovalDecision::AllowOnce, None).await;
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AssistantMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.to_lowercase().contains("beta"), "resumed reply: {text}");
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));

    // Turn 3, same process: interrupt a long-running command.
    run.send(
        "Run the shell command `ping -c 60 127.0.0.1` with the Bash tool (not in the background). \
         Do nothing else.",
    )
    .await
    .unwrap();
    let events = run_turn(&mut run, &mut rx, ApprovalDecision::AllowAlways, Some("Bash")).await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }));
    run.shutdown().await.unwrap();
}

fn start_req(cwd: &Path) -> StartRequest {
    StartRequest {
        cwd: cwd.to_path_buf(),
        resume_native_id: None,
        model: None,
        env: Vec::new(),
        instructions: None,
        remote: None,
        mcp_servers: Vec::new(),
        computer: None,
        fork_from: None,
        mem_limit_mb: None,
        allowed_tools: Vec::new(),
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude").join(name)
}

fn stdin_log(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("stdin.log"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn answer_for(q: &str, value: &str) -> Vec<QuestionAnswer> {
    vec![QuestionAnswer { question_id: q.into(), values: vec![value.into()] }]
}

/// FR-A5: a question is its own event; answering it never enters the "allow always" memory, and an
/// ordinary tool (Write) still does.
#[tokio::test]
async fn fr_a5_a_question_is_a_question_event_and_allow_always_does_not_apply_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let mut run = ClaudeCodeAdapter::new(fixture("fake-claude-question.sh"))
        .start(start_req(dir.path()), tx)
        .await
        .unwrap();
    run.send("hello").await.unwrap();
    loop {
        if let AgentEvent::QuestionAsked { question_id, questions } = next(&mut rx, 10).await {
            assert_eq!(question_id, "q1");
            assert_eq!(questions[0].text, "Which?");
            break;
        }
    }
    // The approval route cannot answer a question.
    assert!(run.answer("q1", ApprovalDecision::AllowAlways).await.is_err());
    run.answer_question("q1", &answer_for("q0", "a")).await.unwrap();
    // q2 is a question again: it asks, it is not answered from any memory.
    loop {
        if let AgentEvent::QuestionAsked { question_id, .. } = next(&mut rx, 10).await {
            assert_eq!(question_id, "q2");
            break;
        }
    }
    run.decline_question("q2").await.unwrap();
    // w1 (Write) asks; after allow-always, w2 (Write) is answered without an event.
    loop {
        if let AgentEvent::ApprovalRequested { approval_id, tool, needs_person, .. } = next(&mut rx, 10).await {
            assert_eq!((approval_id.as_str(), tool.as_str()), ("w1", "Write"));
            assert!(!needs_person);
            break;
        }
    }
    run.answer("w1", ApprovalDecision::AllowAlways).await.unwrap();
    loop {
        match next(&mut rx, 10).await {
            AgentEvent::ApprovalRequested { approval_id, .. } => panic!("{approval_id} should auto-allow"),
            AgentEvent::TurnEnded { outcome } => {
                assert_eq!(outcome, TurnOutcome::Completed);
                break;
            }
            _ => {}
        }
    }
    run.shutdown().await.unwrap();
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(dir.path().join("stdin.log"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let ids: Vec<_> = lines[1..].iter().map(|l| l["response"]["request_id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, ["q1", "q2", "w1", "w2"], "{lines:?}");
}

/// FR-A5: the recorded question reaches the session as `QuestionAsked`, and the person's answer
/// goes to the CLI as the frame measured on Claude Code 2.1.319.
#[tokio::test]
async fn fr_a5_a_recorded_question_is_answered_with_the_measured_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let mut run = ClaudeCodeAdapter::new(fixture("fake-claude-question-reply.sh"))
        .start(start_req(dir.path()), tx)
        .await
        .unwrap();
    run.send("which colour?").await.unwrap();
    let id = loop {
        match next(&mut rx, 10).await {
            AgentEvent::QuestionAsked { question_id, questions } => {
                assert_eq!(question_id, "364602b5-0a1e-4b36-9dc1-8910841b10ac");
                assert_eq!(questions[0].text, "Which colour do you prefer?");
                break question_id;
            }
            AgentEvent::ApprovalRequested { .. } => panic!("a question must not be an approval"),
            _ => {}
        }
    };
    // A bad answer is refused and leaves the question pending.
    assert!(run.answer_question(&id, &[]).await.is_err());
    run.answer_question(&id, &answer_for("q0", "blue")).await.unwrap();
    loop {
        if let AgentEvent::TurnEnded { outcome } = next(&mut rx, 10).await {
            assert_eq!(outcome, TurnOutcome::Completed);
            break;
        }
    }
    run.shutdown().await.unwrap();
    let lines = stdin_log(dir.path());
    let reply = &lines[1];
    assert_eq!(reply["type"], "control_response");
    assert_eq!(reply["response"]["request_id"], id.as_str());
    assert_eq!(reply["response"]["response"]["behavior"], "allow");
    assert_eq!(
        reply["response"]["response"]["updatedInput"]["answers"],
        serde_json::json!({"Which colour do you prefer?": "blue"})
    );
    assert_eq!(lines.len(), 2, "exactly one reply: {lines:?}");
}

/// FR-A5: skipping a question replies allow without `answers`.
#[tokio::test]
async fn fr_a5_a_declined_question_replies_allow_without_answers() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let mut run = ClaudeCodeAdapter::new(fixture("fake-claude-question-reply.sh"))
        .start(start_req(dir.path()), tx)
        .await
        .unwrap();
    run.send("which colour?").await.unwrap();
    let id = loop {
        if let AgentEvent::QuestionAsked { question_id, .. } = next(&mut rx, 10).await {
            break question_id;
        }
    };
    run.decline_question(&id).await.unwrap();
    assert!(run.decline_question(&id).await.is_err(), "no longer pending");
    loop {
        if let AgentEvent::TurnEnded { .. } = next(&mut rx, 10).await {
            break;
        }
    }
    run.shutdown().await.unwrap();
    let reply = &stdin_log(dir.path())[1];
    assert_eq!(reply["response"]["response"]["behavior"], "allow");
    assert!(reply["response"]["response"]["updatedInput"].get("answers").is_none());
}

/// FR-A5: a plan approval (`requires_user_interaction`, measured on 2.1.319) carries the mark, and
/// "allow always" on it is an allow once: the next plan asks again.
#[tokio::test]
async fn fr_a5_allow_always_on_a_plan_is_not_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let mut run = ClaudeCodeAdapter::new(fixture("fake-claude-plan.sh"))
        .start(start_req(dir.path()), tx)
        .await
        .unwrap();
    run.send("plan it").await.unwrap();
    for want in ["p1", "p2"] {
        loop {
            if let AgentEvent::ApprovalRequested { approval_id, tool, needs_person, .. } = next(&mut rx, 10).await {
                assert_eq!((approval_id.as_str(), tool.as_str()), (want, "ExitPlanMode"));
                assert!(needs_person);
                break;
            }
        }
        run.answer(want, ApprovalDecision::AllowAlways).await.unwrap();
    }
    run.shutdown().await.unwrap();
    let ids: Vec<_> = stdin_log(dir.path())[1..]
        .iter()
        .map(|l| l["response"]["request_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["p1", "p2"]);
}
