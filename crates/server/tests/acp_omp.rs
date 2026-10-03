//! Real OMP (oh-my-pi) over ACP end to end (SPEC FR-A2 acceptance): a turn that reads a file,
//! edits it and runs a command behind approvals, then a native resume by session id after the
//! process stopped.
//!
//! Uses the real `omp` CLI (`omp acp`) and the user's configured provider, so it is ignored by
//! default: `EMBER_E2E_OMP=1 cargo test --test acp_omp -- --ignored`
//! (`EMBER_OMP_BIN` picks the binary, `EMBER_E2E_OMP_MODEL` the model; default: omp's own).

use std::time::Duration;

use ember_server::agents::acp::{AcpAdapter, AcpConfig};
use ember_server::agents::{AgentAdapter, AgentRun, StartRequest};
use ember_server::events::{AgentEvent, ApprovalDecision, TurnOutcome};
use tokio::sync::mpsc;

async fn run_turn(
    run: &mut Box<dyn AgentRun>,
    rx: &mut mpsc::Receiver<AgentEvent>,
    text: &str,
    decision: ApprovalDecision,
) -> Vec<AgentEvent> {
    run.send(text).await.unwrap();
    let mut events = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(300), rx.recv())
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
#[ignore = "runs the real omp CLI against the user's provider; set EMBER_E2E_OMP=1"]
async fn omp_read_edit_run_with_approval_then_resume() {
    if std::env::var("EMBER_E2E_OMP").as_deref() != Ok("1") {
        eprintln!("EMBER_E2E_OMP!=1, skipping");
        return;
    }
    let bin = std::env::var_os("EMBER_OMP_BIN").map(Into::into);
    let adapter = AcpAdapter::new(AcpConfig::preset("omp", bin).unwrap()).unwrap();
    let detected = adapter.detect().await;
    assert!(detected.installed, "omp not installed");
    eprintln!("omp {:?}", detected.version);

    let dir = tempfile::Builder::new()
        .prefix("ember-omp-e2e")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "alpha\n").unwrap();
    let req = |resume: Option<String>| StartRequest {
        cwd: dir.path().to_path_buf(),
        resume_native_id: resume,
        model: std::env::var("EMBER_E2E_OMP_MODEL").ok(),
        ..Default::default()
    };

    let (tx, mut rx) = mpsc::channel(1024);
    let mut run = adapter.start(req(None), tx).await.unwrap();
    let session_id = match rx.recv().await.unwrap() {
        AgentEvent::NativeSession { native_id } => native_id,
        other => panic!("expected NativeSession first, got {other:?}"),
    };
    let events = run_turn(
        &mut run,
        &mut rx,
        "Read notes.txt, then replace the word alpha in it with beta, then run `cat notes.txt` in \
         the shell. Reply with the command's output only.",
        ApprovalDecision::AllowOnce,
    )
    .await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolCall { .. })), "no tool calls");
    assert_eq!(std::fs::read_to_string(dir.path().join("notes.txt")).unwrap().trim(), "beta");
    assert!(last_message(&events).contains("beta"));
    run.shutdown().await.unwrap();

    // Native resume in a new process (session/resume or session/load).
    let (tx, mut rx) = mpsc::channel(1024);
    let mut run = adapter.start(req(Some(session_id.clone())), tx).await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), AgentEvent::NativeSession { native_id: session_id });
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
