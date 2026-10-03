//! Real Codex end to end (SPEC FR-A2 acceptance): a turn that reads a file, edits it and runs a
//! command behind an approval, then a native resume by thread id after the process stopped.
//!
//! Uses the real `codex` CLI and the user's account, so it is ignored by default:
//! `EMBER_E2E_CODEX=1 cargo test --test codex -- --ignored`
//! (`EMBER_CODEX_BIN` picks the binary, `EMBER_E2E_CODEX_MODEL` the model; default `gpt-6-luna`).

use std::time::Duration;

use ember_server::agents::codex::CodexAdapter;
use ember_server::agents::{AgentAdapter, AgentRun, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, TurnOutcome};
use tokio::sync::mpsc;

/// Collect events until `TurnEnded`, answering every approval with `decision`.
async fn run_turn(
    run: &mut Box<dyn AgentRun>,
    rx: &mut mpsc::Receiver<AgentEvent>,
    text: &str,
    decision: ApprovalDecision,
) -> Vec<AgentEvent> {
    run.send(text).await.unwrap();
    let mut events = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(240), rx.recv())
            .await
            .expect("turn timed out")
            .expect("agent channel closed");
        if let AgentEvent::ApprovalRequested { approval_id, .. } = &ev {
            run.answer(approval_id, decision).await.unwrap();
        }
        let done = matches!(ev, AgentEvent::TurnEnded { .. });
        if !matches!(ev, AgentEvent::AssistantDelta { .. }) {
            eprintln!("{}", serde_json::to_string(&ev).unwrap());
        }
        events.push(ev);
        if done {
            return events;
        }
    }
}

fn last_message(events: &[AgentEvent]) -> String {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::AssistantMessage { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
#[ignore = "runs the real codex CLI against the user's account; set EMBER_E2E_CODEX=1"]
async fn codex_read_edit_run_with_approval_then_resume() {
    if std::env::var("EMBER_E2E_CODEX").as_deref() != Ok("1") {
        eprintln!("EMBER_E2E_CODEX!=1, skipping");
        return;
    }
    let adapter = CodexAdapter::from_env()
        // Ask before any command that is not known-safe, so the turn needs an approval.
        .with_approval_policy("untrusted")
        .with_sandbox("workspace-write");
    let detected = adapter.detect().await;
    assert!(detected.installed, "codex not installed");
    eprintln!("codex {:?}", detected.version);

    let dir = tempfile::Builder::new()
        .prefix("ember-codex-e2e")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "alpha\n").unwrap();
    let model =
        Some(std::env::var("EMBER_E2E_CODEX_MODEL").unwrap_or_else(|_| "gpt-6-luna".into()));

    // Turn 1: read, edit, run a command that needs approval.
    let (tx, mut rx) = mpsc::channel(1024);
    let req = StartRequest {
        cwd: dir.path().to_path_buf(),
        resume_native_id: None,
        model: model.clone(),
        env: Vec::new(),
        instructions: None,
        remote: None,
    };
    let mut run = adapter.start(req, tx).await.unwrap();
    let thread_id = match rx.recv().await.unwrap() {
        AgentEvent::NativeSession { native_id } => native_id,
        other => panic!("expected NativeSession first, got {other:?}"),
    };
    let events = run_turn(
        &mut run,
        &mut rx,
        "Read notes.txt. Then edit notes.txt so it contains `beta` instead of `alpha`. \
         Then run the shell command `cat notes.txt | tee out.txt`. Reply with one short sentence.",
        ApprovalDecision::AllowAlways,
    )
    .await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ApprovalRequested { .. })),
        "no approval"
    );
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolCall { .. })), "no tool call");
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ToolResult { is_error: false, .. })),
        "no successful tool result"
    );
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })), "no usage");
    assert!(!last_message(&events).is_empty(), "no assistant message");
    assert!(std::fs::read_to_string(dir.path().join("notes.txt")).unwrap().contains("beta"));
    assert!(std::fs::read_to_string(dir.path().join("out.txt")).unwrap().contains("beta"));
    run.shutdown().await.unwrap();
    drop(run);

    // Turn 2, in a new process: native resume by thread id keeps the conversation.
    let (tx, mut rx) = mpsc::channel(1024);
    let req = StartRequest {
        cwd: dir.path().to_path_buf(),
        resume_native_id: Some(thread_id.clone()),
        model,
        env: Vec::new(),
        instructions: None,
        remote: None,
    };
    let mut run = adapter.start(req, tx).await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), AgentEvent::NativeSession { native_id: thread_id });
    let events = run_turn(
        &mut run,
        &mut rx,
        "Without running any tools: which word did you write into notes.txt? Answer with the word only.",
        ApprovalDecision::AllowOnce,
    )
    .await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(last_message(&events).to_lowercase().contains("beta"), "resume lost the conversation");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_binary_is_reported_not_installed() {
    let d = CodexAdapter::new("/nonexistent/codex-for-ember-test").detect().await;
    assert!(!d.installed);
    assert_eq!(d.version, None);
}
