//! Antigravity adapter: process and approval-hook plumbing against a fake CLI (the hook really
//! goes through `curl` to the hook route), and an opt-in end-to-end run against the real `agy`
//! (FR-A2 acceptance: read, edit, run a command; native resume, FR-S2; approvals, FR-A5).
//!
//! The end-to-end test uses the user's Antigravity login, so it is ignored by default:
//! `EMBER_E2E_AGY=1 cargo test --test antigravity -- --ignored`
//! (`EMBER_AGY_BIN` picks the binary, `EMBER_E2E_AGY_MODEL` the model; default
//! `gemini-3.8-flash-low`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ember_server::agents::antigravity::{self, AntigravityAdapter, HookBroker};
use ember_server::agents::{AgentAdapter, AgentRun, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, TurnOutcome};
use tokio::sync::mpsc;

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agy/fake-agy.sh")
}

/// A hook broker whose route is served on an ephemeral loopback port.
async fn serve_hooks() -> Arc<HookBroker> {
    let broker = HookBroker::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    broker.set_base_url(&format!("http://{}", listener.local_addr().unwrap()));
    let app = antigravity::router(broker.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    broker
}

async fn next(rx: &mut mpsc::Receiver<AgentEvent>, secs: u64) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(secs), rx.recv())
        .await
        .expect("timed out waiting for an agent event")
        .expect("agent event channel closed")
}

/// Events up to the next approval request: `(approval id, events before it)`.
async fn until_approval(rx: &mut mpsc::Receiver<AgentEvent>) -> (String, Vec<AgentEvent>) {
    let mut before = Vec::new();
    loop {
        match next(rx, 20).await {
            AgentEvent::ApprovalRequested { approval_id, tool, input } => {
                assert_eq!(tool, "run_command");
                assert_eq!(input["CommandLine"], "echo hi");
                return (approval_id, before);
            }
            other => before.push(other),
        }
    }
}

#[tokio::test]
async fn detect_reports_version_or_not_installed() {
    let home = tempfile::tempdir().unwrap();
    let d = AntigravityAdapter::new(fake_bin(), home.path(), HookBroker::new()).detect().await;
    assert!(d.installed);
    assert_eq!(d.version.as_deref(), Some("9.9.9"));
    let d = AntigravityAdapter::new("/nonexistent/agy", home.path(), HookBroker::new()).detect().await;
    assert!(!d.installed);
}

#[tokio::test]
async fn fake_cli_turns_with_hook_approvals_resume_and_interrupt() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let broker = serve_hooks().await;
    let adapter = AntigravityAdapter::new(fake_bin(), home.path(), broker);
    let (tx, mut rx) = mpsc::channel(64);
    let mut run = adapter
        .start(
            StartRequest {
                cwd: cwd.path().to_path_buf(),
                model: Some("m1".into()),
                instructions: Some("Use ember-a2a.".into()),
                ..Default::default()
            },
            tx,
        )
        .await
        .unwrap();

    // Turn 1: the hook asks Ember, which waits for the answer. The tool call (stdout) and the
    // approval request (hook route) race, so their order is not asserted.
    run.send("hello").await.unwrap();
    let (approval_id, before) = until_approval(&mut rx).await;
    assert_eq!(approval_id, "fake-conv:2");
    run.answer(&approval_id, ApprovalDecision::AllowOnce).await.unwrap();
    let mut rest = before;
    loop {
        let ev = next(&mut rx, 20).await;
        let done = matches!(ev, AgentEvent::TurnEnded { .. });
        rest.push(ev);
        if done {
            break;
        }
    }
    assert!(rest.contains(&AgentEvent::NativeSession { native_id: "fake-conv".into() }));
    assert!(rest.iter().any(|e| matches!(e,
        AgentEvent::ToolCall { call_id, name, .. } if call_id == "fake-conv:2" && name == "run_command")));
    let tail: Vec<&AgentEvent> = rest
        .iter()
        .filter(|e| !matches!(e, AgentEvent::ToolCall { .. } | AgentEvent::NativeSession { .. }))
        .collect();
    assert_eq!(
        tail,
        [
            &AgentEvent::ToolResult { call_id: "fake-conv:2".into(), output: String::new(), is_error: false },
            // Per-turn usage from the steps (output + thinking), not the conversation total.
            &AgentEvent::Usage { input_tokens: 3, output_tokens: 5 },
            &AgentEvent::AssistantMessage { text: "done".into() },
            &AgentEvent::TurnEnded { outcome: TurnOutcome::Completed },
        ]
    );

    // Turn 2 resumes the conversation natively; the user denies.
    run.send("again").await.unwrap();
    let (approval_id, _) = until_approval(&mut rx).await;
    run.answer(&approval_id, ApprovalDecision::Deny).await.unwrap();
    loop {
        if let AgentEvent::TurnEnded { outcome } = next(&mut rx, 20).await {
            assert_eq!(outcome, TurnOutcome::Completed);
            break;
        }
    }

    // Turn 3: allowed always, then interrupted while running.
    run.send("slow").await.unwrap();
    let (approval_id, _) = until_approval(&mut rx).await;
    run.answer(&approval_id, ApprovalDecision::AllowAlways).await.unwrap();
    while !matches!(next(&mut rx, 20).await, AgentEvent::ToolResult { .. }) {}
    run.interrupt().await.unwrap();
    assert_eq!(next(&mut rx, 20).await, AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted });

    // Turn 4: run_command is now allowed without asking.
    run.send("fourth").await.unwrap();
    loop {
        match next(&mut rx, 20).await {
            AgentEvent::ApprovalRequested { .. } => panic!("AllowAlways should have covered this"),
            AgentEvent::TurnEnded { outcome } => {
                assert_eq!(outcome, TurnOutcome::Completed);
                break;
            }
            _ => {}
        }
    }
    run.shutdown().await.unwrap();

    let args = std::fs::read_to_string(cwd.path().join("args.log")).unwrap();
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines.len(), 5, "hook check + 4 turns: {args}");
    assert!(lines[0].starts_with("-p /hooks --add-dir "), "{args}");
    assert!(lines[1].contains("--dangerously-skip-permissions") && lines[1].contains("--model m1"));
    assert!(!lines[1].contains("--conversation"), "{args}");
    for l in &lines[2..] {
        assert!(l.contains("--conversation=fake-conv"), "{args}");
    }
    let decisions = std::fs::read_to_string(cwd.path().join("decisions.log")).unwrap();
    let decisions: Vec<serde_json::Value> =
        decisions.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let words: Vec<&str> = decisions.iter().map(|d| d["decision"].as_str().unwrap()).collect();
    assert_eq!(words, ["allow", "deny", "allow", "allow"]);
}

#[tokio::test]
async fn a_hook_that_was_not_loaded_fails_the_turn() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    // A fake that lists no hooks.
    let bin = cwd.path().join("agy-without-hooks.sh");
    std::fs::write(&bin, "#!/bin/sh\necho \"$*\" >> args.log\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let adapter = AntigravityAdapter::new(&bin, home.path(), serve_hooks().await);
    let (tx, mut rx) = mpsc::channel(16);
    let mut run =
        adapter.start(StartRequest { cwd: cwd.path().into(), ..Default::default() }, tx).await.unwrap();
    run.send("hello").await.unwrap();
    assert!(matches!(next(&mut rx, 20).await, AgentEvent::Error { message } if message.contains("approval hook")));
    assert_eq!(next(&mut rx, 20).await, AgentEvent::TurnEnded { outcome: TurnOutcome::Failed });
    let args = std::fs::read_to_string(cwd.path().join("args.log")).unwrap();
    assert_eq!(args.lines().count(), 1, "only the check ran: {args}");
}

/// Collect events until `TurnEnded`, answering approvals with `decide`.
async fn run_turn(
    run: &mut Box<dyn AgentRun>,
    rx: &mut mpsc::Receiver<AgentEvent>,
    decide: ApprovalDecision,
) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    loop {
        let ev = next(rx, 300).await;
        if !matches!(ev, AgentEvent::AssistantDelta { .. }) {
            eprintln!("event: {ev:?}");
        }
        if let AgentEvent::ApprovalRequested { approval_id, .. } = &ev {
            run.answer(approval_id, decide).await.unwrap();
        }
        let done = matches!(ev, AgentEvent::TurnEnded { .. });
        seen.push(ev);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
#[ignore = "runs the real agy CLI against the user's account; set EMBER_E2E_AGY=1"]
async fn real_agy_read_edit_run_then_resume_and_interrupt() {
    if std::env::var("EMBER_E2E_AGY").as_deref() != Ok("1") {
        eprintln!("EMBER_E2E_AGY != 1; skipping");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter("ember::agy=debug")
        .with_test_writer()
        .try_init();
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    std::fs::write(cwd.join("notes.txt"), "colour: alpha\n").unwrap();
    let adapter = AntigravityAdapter::new(
        std::env::var_os("EMBER_AGY_BIN").unwrap_or_else(|| "agy".into()),
        home.path(),
        serve_hooks().await,
    );
    assert!(adapter.detect().await.installed, "agy not installed");
    let model = std::env::var("EMBER_E2E_AGY_MODEL").unwrap_or_else(|_| "gemini-3.8-flash-low".into());
    let req = |resume: Option<String>| StartRequest {
        cwd: cwd.clone(),
        resume_native_id: resume,
        model: Some(model.clone()),
        instructions: Some("When you finish a reply, end it with the word EMBERCHECK.".into()),
        ..Default::default()
    };

    // Turn 1: read, edit, run a command; edits and commands ask through the hook.
    let (tx, mut rx) = mpsc::channel(1024);
    let mut run = adapter.start(req(None), tx).await.unwrap();
    run.send(
        "Read notes.txt, change alpha to beta in it, then run the shell command \
         `grep -c beta notes.txt` and tell me its output. Reply briefly.",
    )
    .await
    .unwrap();
    let events = run_turn(&mut run, &mut rx, ApprovalDecision::AllowOnce).await;
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
    assert!(calls.contains(&"run_command"), "{calls:?}");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ApprovalRequested { tool, .. } if tool == "run_command")));
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Notice { .. })), "nothing soft-denied");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AssistantMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("EMBERCHECK"), "instructions reached the agent: {text}");
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(std::fs::read_to_string(cwd.join("notes.txt")).unwrap().contains("beta"));
    run.shutdown().await.unwrap();

    // Turn 2, a new process: native resume remembers; a denied command does not run.
    let (tx, mut rx) = mpsc::channel(1024);
    let mut run = adapter.start(req(Some(native_id.clone())), tx).await.unwrap();
    run.send(
        "Which word did you replace alpha with? Then run the shell command `touch denied.txt`.",
    )
    .await
    .unwrap();
    let events = run_turn(&mut run, &mut rx, ApprovalDecision::Deny).await;
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AssistantMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.to_lowercase().contains("beta"), "resumed reply: {text}");
    assert!(!cwd.join("denied.txt").exists(), "a denied command ran");

    // Turn 3, same run: interrupt a long command.
    run.send("Run the shell command `sleep 120` (not in the background). Do nothing else.")
        .await
        .unwrap();
    loop {
        let ev = next(&mut rx, 300).await;
        eprintln!("event: {ev:?}");
        match ev {
            AgentEvent::ApprovalRequested { approval_id, .. } => {
                run.answer(&approval_id, ApprovalDecision::AllowOnce).await.unwrap();
                tokio::time::sleep(Duration::from_secs(3)).await;
                run.interrupt().await.unwrap();
            }
            AgentEvent::TurnEnded { outcome } => {
                assert_eq!(outcome, TurnOutcome::Interrupted);
                break;
            }
            _ => {}
        }
    }
    run.shutdown().await.unwrap();
}
