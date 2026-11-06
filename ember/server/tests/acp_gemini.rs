//! The real Gemini CLI over ACP (SPEC FR-A10g). **UNVERIFIED until run on a machine with the CLI.**
//!
//! Ignored by default, and returns early unless `EMBER_E2E_GEMINI=1` and `gemini` is on `PATH`
//! (`EMBER_GEMINI_BIN` picks another binary). Run with
//! `EMBER_E2E_GEMINI=1 cargo test --test acp_gemini -- --ignored --nocapture`
//! with `GEMINI_API_KEY` set (or an Enterprise sign-in); `EMBER_E2E_GEMINI_MODEL` picks a model.
//!
//! What it checks, in order, and prints as it goes (the printed lines are the evidence to paste
//! into the SPEC "Gemini CLI facts" block):
//! 1. `--version` parses to a version (FR-A10a).
//! 2. The start with the preset's `--acp` succeeds, or fails and the `--experimental-acp` retry
//!    succeeds; which flag was accepted is printed (the fact the SPEC leaves open).
//! 3. A turn that reads a file, edits it and runs a command behind approvals completes
//!    (FR-A10g), and whether the turn reported `Usage` is printed (FR-A10d).
//! 4. A second process resumes the session by id (`session/load` or `session/resume`).
//! 5. With `GEMINI_API_KEY` removed from the environment, a start and turn end with
//!    `auth_required` and not a crash (FR-A10b), when `EMBER_E2E_GEMINI_NOKEY=1`.
//!
//! The raw `initialize` answer (capabilities, `authMethods` ids) is not printed by the adapter;
//! run with `RUST_LOG=acp=debug` to see the agent's stderr, and read the answer from a proxy
//! script if the `authMethods` id `gemini-api-key` needs confirming.

use std::time::Duration;

use ember_server::agents::acp::{AcpAdapter, AcpConfig};
use ember_server::agents::{resolve_executable, AgentAdapter, AgentRun, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, TurnOutcome};
use tokio::sync::mpsc;

async fn run_turn(
    run: &mut Box<dyn AgentRun>,
    rx: &mut mpsc::Receiver<AgentEvent>,
    text: &str,
) -> Vec<AgentEvent> {
    run.send(text).await.unwrap();
    let mut events = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(300), rx.recv())
            .await
            .expect("turn timed out")
            .expect("agent channel closed");
        if let AgentEvent::ApprovalRequested { approval_id, .. } = &ev {
            run.answer(approval_id, ApprovalDecision::AllowOnce).await.unwrap();
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
#[ignore = "runs the real gemini CLI against the user's credentials; set EMBER_E2E_GEMINI=1"]
async fn gemini_read_edit_run_with_approval_then_resume() {
    if std::env::var("EMBER_E2E_GEMINI").as_deref() != Ok("1") {
        eprintln!("EMBER_E2E_GEMINI!=1, skipping");
        return;
    }
    let bin = std::env::var_os("EMBER_GEMINI_BIN").map(std::path::PathBuf::from);
    let command = bin.clone().unwrap_or_else(|| "gemini".into());
    if resolve_executable(&command).is_none() {
        eprintln!("gemini is not on PATH, skipping");
        return;
    }
    let adapter = AcpAdapter::new(AcpConfig::preset("gemini", bin).unwrap()).unwrap();
    let detected = adapter.detect().await;
    assert!(detected.installed, "gemini --version failed");
    eprintln!("gemini version: {:?}", detected.version);
    assert!(detected.version.is_some(), "the version output did not parse");

    let dir = tempfile::Builder::new()
        .prefix("ember-gemini-e2e")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "alpha\n").unwrap();
    let req = |resume: Option<String>| StartRequest {
        cwd: dir.path().to_path_buf(),
        resume_native_id: resume,
        model: std::env::var("EMBER_E2E_GEMINI_MODEL").ok(),
        ..Default::default()
    };

    let (tx, mut rx) = mpsc::channel(1024);
    let mut run = adapter.start(req(None), tx).await.unwrap();
    let session_id = match rx.recv().await.unwrap() {
        AgentEvent::NativeSession { native_id } => native_id,
        other => panic!("expected NativeSession first (auth_required? {other:?})"),
    };
    let events = run_turn(
        &mut run,
        &mut rx,
        "Read notes.txt, then replace the word alpha in it with beta, then run `cat notes.txt` in \
         the shell. Reply with the command's output only.",
    )
    .await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolCall { .. })), "no tool calls");
    assert_eq!(std::fs::read_to_string(dir.path().join("notes.txt")).unwrap().trim(), "beta");
    assert!(last_message(&events).contains("beta"));
    eprintln!("usage reported: {}", events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    run.shutdown().await.unwrap();

    let (tx, mut rx) = mpsc::channel(1024);
    let mut run = adapter.start(req(Some(session_id.clone())), tx).await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), AgentEvent::NativeSession { native_id: session_id });
    let events = run_turn(
        &mut run,
        &mut rx,
        "Without running any tools: which word did you write into notes.txt? Answer with the word only.",
    )
    .await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(last_message(&events).to_lowercase().contains("beta"), "resume lost the conversation");
    run.shutdown().await.unwrap();

    if std::env::var("EMBER_E2E_GEMINI_NOKEY").as_deref() == Ok("1") {
        // Authentication missing: the key is removed from the process environment (an
        // Enterprise sign-in cached on disk would still authenticate; then this step is moot).
        std::env::remove_var("GEMINI_API_KEY");
        let (tx, mut rx) = mpsc::channel(1024);
        let mut run = adapter.start(req(None), tx).await.unwrap();
        let events = run_turn(&mut run, &mut rx, "hello").await;
        eprintln!("no-key turn: {events:?}");
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnError { code, .. } if code == "auth_required")));
        run.shutdown().await.unwrap();
    }
}
