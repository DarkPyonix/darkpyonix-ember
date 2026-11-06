//! ACP adapter tests against the in-process fake agent ([`super::fake_agent`]) and unit tests of
//! the pieces (configuration, mapping, file slicing, terminal output).

use std::path::Path;
use std::time::Duration;

use super::fake_agent::{self, FakeOptions};
use super::*;

type Events = mpsc::Receiver<AgentEvent>;

fn setup(cwd: &Path) -> Setup {
    Setup {
        cwd: cwd.to_path_buf(),
        resume_native_id: None,
        fork_from: None,
        mcp_servers: Vec::new(),
        computer: None,
        prepend_instructions: None,
        agent: "acp".into(),
    }
}

async fn start(opts: FakeOptions, setup: Setup) -> anyhow::Result<(AcpRun, Events)> {
    let (client, agent) = tokio::io::duplex(1 << 20);
    tokio::spawn(fake_agent::run(agent, opts));
    let (r, w) = tokio::io::split(client);
    let (tx, rx) = mpsc::channel(1024);
    let run = connect(r, w, None, setup, tx).await?;
    Ok((run, rx))
}

async fn next(rx: &mut Events) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("event channel closed")
}

/// Send `text` and collect events through `TurnEnded`, answering approvals with `decision`.
async fn turn(run: &mut AcpRun, rx: &mut Events, text: &str, decision: ApprovalDecision) -> Vec<AgentEvent> {
    run.send(text).await.unwrap();
    collect_turn(run, rx, decision).await
}

async fn collect_turn(run: &mut AcpRun, rx: &mut Events, decision: ApprovalDecision) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    loop {
        let ev = next(rx).await;
        if let AgentEvent::ApprovalRequested { approval_id, .. } = &ev {
            run.answer(approval_id, decision).await.unwrap();
        }
        let done = matches!(ev, AgentEvent::TurnEnded { .. });
        out.push(ev);
        if done {
            return out;
        }
    }
}

fn messages(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AssistantMessage { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn last_message(events: &[AgentEvent]) -> String {
    messages(events).pop().unwrap_or_default()
}

async fn new_session(opts: FakeOptions, cwd: &Path) -> (AcpRun, Events) {
    let (run, mut rx) = start(opts, setup(cwd)).await.unwrap();
    assert_eq!(next(&mut rx).await, AgentEvent::NativeSession { native_id: "fake-session-1".into() });
    (run, rx)
}

#[tokio::test]
async fn prompt_streams_a_message_with_usage() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, "hello", ApprovalDecision::AllowOnce).await;
    assert_eq!(
        events,
        vec![
            AgentEvent::AssistantDelta { text: "Hi ".into() },
            AgentEvent::AssistantDelta { text: "there".into() },
            AgentEvent::AssistantMessage { text: "Hi there".into() },
            AgentEvent::Usage { input_tokens: 10, output_tokens: 5 },
            AgentEvent::TurnEnded { outcome: TurnOutcome::Completed },
        ]
    );
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn allowed_tool_call_reads_and_writes_the_file_here() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "alpha\n").unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, &format!("edit {}", file.display()), ApprovalDecision::AllowOnce).await;

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha\nbeta\n");
    // The message before the tool call is finished before the call is shown.
    assert_eq!(messages(&events), vec!["Editing.".to_string(), "chosen allow-once".to_string()]);
    let path = file.to_string_lossy().into_owned();
    let call = events.iter().position(|e| matches!(e, AgentEvent::ToolCall { .. })).unwrap();
    assert_eq!(
        events[call],
        AgentEvent::ToolCall {
            call_id: "t1".into(),
            name: "edit".into(),
            input: json!({ "title": format!("Edit {path}"), "kind": "edit",
                           "input": { "path": path }, "locations": [{ "path": path }] }),
        }
    );
    assert!(matches!(&events[call + 1], AgentEvent::ApprovalRequested { tool, .. } if tool == "edit"));
    let result = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolResult { call_id, output, is_error } if call_id == "t1" => Some((output.clone(), *is_error)),
            _ => None,
        })
        .unwrap();
    assert!(!result.1);
    assert!(result.0.contains("+beta"), "diff rendered: {}", result.0);
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn always_allow_and_deny_pick_options_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.txt");
    std::fs::write(&file, "alpha\n").unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;

    let events = turn(&mut run, &mut rx, &format!("edit {}", file.display()), ApprovalDecision::AllowAlways).await;
    assert_eq!(last_message(&events), "chosen allow-always");

    let events = turn(&mut run, &mut rx, &format!("edit {}", file.display()), ApprovalDecision::Deny).await;
    assert_eq!(last_message(&events), "chosen reject-once");
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::ToolResult { output, is_error: true, .. } if output == "rejected")));
    // Allowed once, then denied: one `beta` line only.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha\nbeta\n");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn read_text_file_honours_line_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lines.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, &format!("readlines {}", file.display()), ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "two\n");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_runs_a_command_and_reports_its_output_and_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, "run", ApprovalDecision::AllowOnce).await;
    let msg = last_message(&events);
    assert!(msg.starts_with("exit 3 output "), "{msg}");
    assert!(msg.contains("out") && msg.contains("err"), "{msg}");
    // The tool result embeds the (released) terminal's output.
    let output = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolResult { call_id, output, .. } if call_id == "t2" => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(output.contains("out") && output.contains("err"), "{output}");
    run.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_kill_stops_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, "kill", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "signal SIGKILL");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn plan_appears_as_a_plan_tool_call() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, "plan", ApprovalDecision::AllowOnce).await;
    assert!(matches!(&events[0], AgentEvent::ToolCall { name, .. } if name == "Plan"));
    assert!(matches!(&events[1],
        AgentEvent::ToolResult { output, is_error: false, .. } if output == "[completed] Read\n[pending] Edit"));
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_cancels_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    run.send("slow").await.unwrap();
    assert!(matches!(next(&mut rx).await, AgentEvent::ToolCall { .. }));
    run.interrupt().await.unwrap();
    let events = collect_turn(&mut run, &mut rx, ApprovalDecision::AllowOnce).await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }));
    // The session goes on.
    let events = turn(&mut run, &mut rx, "hello", ApprovalDecision::AllowOnce).await;
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_answers_pending_permissions_with_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    run.send("slow-permission").await.unwrap();
    let approval = loop {
        if let AgentEvent::ApprovalRequested { approval_id, tool, .. } = next(&mut rx).await {
            assert_eq!(tool, "delete");
            break approval_id;
        }
    };
    run.interrupt().await.unwrap();
    let mut events = Vec::new();
    loop {
        let ev = next(&mut rx).await;
        let done = matches!(ev, AgentEvent::TurnEnded { .. });
        events.push(ev);
        if done {
            break;
        }
    }
    assert_eq!(last_message(&events), "permission cancelled");
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted }));
    assert!(run.answer(&approval, ApprovalDecision::AllowOnce).await.is_err());
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn messages_sent_mid_turn_are_queued() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    run.send("echo first").await.unwrap();
    run.send("echo second").await.unwrap();
    let a = collect_turn(&mut run, &mut rx, ApprovalDecision::AllowOnce).await;
    let b = collect_turn(&mut run, &mut rx, ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&a), "echo first");
    assert_eq!(last_message(&b), "echo second");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn instructions_without_an_argument_go_in_front_of_the_first_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path());
    s.prepend_instructions = Some("RULES".into());
    let (mut run, mut rx) = start(FakeOptions::default(), s).await.unwrap();
    next(&mut rx).await; // NativeSession
    // Only the first prompt of the process carries the instructions.
    run.send("hi").await.unwrap();
    let _ = collect_turn(&mut run, &mut rx, ApprovalDecision::AllowOnce).await;
    let events = turn(&mut run, &mut rx, "echo again", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "echo again");
    run.shutdown().await.unwrap();

    let mut s = setup(dir.path());
    s.prepend_instructions = Some("echo RULES".into());
    let (mut run, mut rx) = start(FakeOptions::default(), s).await.unwrap();
    next(&mut rx).await;
    let events = turn(&mut run, &mut rx, "hi", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "echo RULES\n\nhi");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_servers_are_passed_to_session_new() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path());
    s.mcp_servers = vec![McpServer {
        name: "ember-browser".into(),
        command: "/usr/bin/npx".into(),
        args: vec!["-y".into(), "x".into()],
        startup_timeout_secs: None,
        env: vec![],
    }];
    let (mut run, mut rx) = start(FakeOptions::default(), s).await.unwrap();
    next(&mut rx).await;
    let events = turn(&mut run, &mut rx, "mcp?", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "mcp=ember-browser");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_resumes_natively_without_re_emitting_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path());
    s.resume_native_id = Some("old-1".into());
    let (mut run, mut rx) = start(FakeOptions { load: true, ..Default::default() }, s).await.unwrap();
    // The replay (a message and a tool call) is dropped: the native id comes first.
    assert_eq!(next(&mut rx).await, AgentEvent::NativeSession { native_id: "old-1".into() });
    let events = turn(&mut run, &mut rx, "setup?", ApprovalDecision::AllowOnce).await;
    assert_eq!(
        events,
        vec![
            AgentEvent::AssistantDelta { text: "setup=load".into() },
            AgentEvent::AssistantMessage { text: "setup=load".into() },
            AgentEvent::TurnEnded { outcome: TurnOutcome::Completed },
        ]
    );
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn resume_is_preferred_over_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path());
    s.resume_native_id = Some("old-2".into());
    let (mut run, mut rx) = start(FakeOptions { load: true, resume: true, ..Default::default() }, s).await.unwrap();
    assert_eq!(next(&mut rx).await, AgentEvent::NativeSession { native_id: "old-2".into() });
    let events = turn(&mut run, &mut rx, "setup?", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "setup=resume");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_uses_session_fork_and_announces_the_new_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path());
    s.fork_from = Some(ForkFrom { native_id: "parent-1".into(), keep_turns: None });
    let (mut run, mut rx) = start(FakeOptions { fork: true, ..Default::default() }, s).await.unwrap();
    assert_eq!(next(&mut rx).await, AgentEvent::NativeSession { native_id: "parent-1-fork".into() });
    let events = turn(&mut run, &mut rx, "setup?", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "setup=fork");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_without_the_capability_fails_so_the_copy_can_take_over() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path());
    s.fork_from = Some(ForkFrom { native_id: "parent-1".into(), keep_turns: None });
    let err = start(FakeOptions::default(), s).await.err().expect("must fail");
    assert!(format!("{err:#}").contains("session/fork"), "{err:#}");
}

#[tokio::test]
async fn an_agent_that_cannot_resume_starts_a_new_session_with_a_notice() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path());
    s.resume_native_id = Some("old-3".into());
    let (mut run, mut rx) = start(FakeOptions::default(), s).await.unwrap();
    assert!(matches!(next(&mut rx).await, AgentEvent::Notice { message } if message.contains("old-3")));
    assert_eq!(next(&mut rx).await, AgentEvent::NativeSession { native_id: "fake-session-1".into() });
    let events = turn(&mut run, &mut rx, "setup?", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "setup=new");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn another_protocol_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let err = start(FakeOptions { version: 2, ..Default::default() }, setup(dir.path())).await.err().unwrap();
    assert!(format!("{err:#}").contains("protocol version"), "{err:#}");
}

#[tokio::test]
async fn unsupported_agent_requests_get_method_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, "elicit", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "elicitation error -32601");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_exit_mid_turn_fails_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, "exit", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "bye");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }));
}

// ---------------------------------------------------------------------------------------------
// Units

#[test]
fn configs_parse_presets_overrides_and_custom_agents() {
    let configs = parse_configs(
        r#"["omp", {"name": "gemini", "command": "gemini", "args": ["--experimental-acp"],
                    "env": {"A": "1"}}]"#,
        |name| (name == "omp").then(|| PathBuf::from("/opt/omp")),
    )
    .unwrap();
    assert_eq!(configs[0], AcpConfig::preset("omp", Some("/opt/omp".into())).unwrap());
    assert_eq!(configs[0].command, PathBuf::from("/opt/omp"));
    assert_eq!(configs[0].args, ["acp"]);
    assert_eq!(configs[1].name, "gemini");
    assert_eq!(configs[1].args, ["--experimental-acp"]);
    assert_eq!(configs[1].version_args, ["--version"]);
    assert_eq!(configs[1].env, [("A".to_string(), "1".to_string())]);
    // The override keeps the preset's other fields (the Gemini model template, FR-A10e).
    assert_eq!(configs[1].model_args, ["-m", "{model}"]);

    // An object named like a preset overrides only what it sets.
    let c = parse_configs(r#"[{"name": "omp", "args": ["acp", "--no-lsp"]}]"#, |_| None).unwrap();
    assert_eq!(c[0].command, PathBuf::from("omp"));
    assert_eq!(c[0].args, ["acp", "--no-lsp"]);
    assert_eq!(c[0].model_args, ["--model", "{model}"]);

    assert!(parse_configs(r#"["nope"]"#, |_| None).is_err());
    assert!(parse_configs(r#"[{"name": "x"}]"#, |_| None).is_err(), "command required");
    assert!(parse_configs(r#"["omp", "omp"]"#, |_| None).is_err(), "duplicate");
    assert!(parse_configs(r#"[{"name": "codex", "command": "c"}]"#, |_| None).is_err(), "built-in name");
    assert!(parse_configs(r#"[{"name": "antigravity", "command": "c"}]"#, |_| None).is_err(), "built-in name");
    assert!(parse_configs(r#"[{"name": "a", "command": "c", "typo": 1}]"#, |_| None).is_err());
    assert!(parse_configs("{}", |_| None).is_err());
}

#[test]
fn omp_process_args_carry_model_and_instructions() {
    let omp = AcpConfig::preset("omp", None).unwrap();
    assert_eq!(omp.process_args(&StartRequest::default()), ["acp"]);
    let req = StartRequest {
        model: Some("sonnet".into()),
        instructions: Some("use ember-a2a".into()),
        ..Default::default()
    };
    assert_eq!(
        omp.process_args(&req),
        ["acp", "--model", "sonnet", "--append-system-prompt", "use ember-a2a"]
    );
}

#[tokio::test]
async fn adapter_registers_its_kind_and_detects_a_missing_binary() {
    let mut config = AcpConfig::preset("omp", Some("/nonexistent/omp-for-ember-test".into())).unwrap();
    config.name = "omp-test-missing".into();
    let adapter = AcpAdapter::new(config).unwrap();
    assert_eq!(adapter.kind().as_str(), "omp-test-missing");
    assert_eq!(AgentKind::parse("omp-test-missing"), Some(adapter.kind()));
    let d = adapter.detect().await;
    assert!(!d.installed);
    assert_eq!(d.version, None);
}

#[test]
fn versions_are_the_last_word_of_the_first_line() {
    assert_eq!(parse_version("omp 18.5.0\n").as_deref(), Some("18.5.0"));
    assert_eq!(parse_version("\n18.5.0\n").as_deref(), Some("18.5.0"));
    assert_eq!(parse_version(""), None);
}

#[test]
fn line_slicing_is_one_based_and_keeps_newlines() {
    let t = "a\nb\nc";
    assert_eq!(slice_lines(t, None, None), t);
    assert_eq!(slice_lines(t, Some(2), None), "b\nc");
    assert_eq!(slice_lines(t, Some(1), Some(2)), "a\nb\n");
    assert_eq!(slice_lines(t, Some(9), Some(1)), "");
    assert_eq!(slice_lines(t, None, Some(1)), "a\n");
}

#[test]
fn terminal_output_truncates_from_the_front_at_a_char_boundary() {
    let mut b = OutputBuf::new(5);
    b.push("ab".as_bytes());
    assert_eq!((b.text().as_str(), b.truncated), ("ab", false));
    b.push("cdé".as_bytes()); // 6 bytes in all: `a` goes
    assert_eq!((b.text().as_str(), b.truncated), ("bcdé", true));
    let mut b = OutputBuf::new(2);
    b.push("xéy".as_bytes()); // x, 0xC3, 0xA9, y: keeps 0xA9 y; the orphan byte is skipped
    assert_eq!(b.text(), "y");
}

#[test]
fn options_are_chosen_by_kind_with_fallbacks() {
    let all = vec![
        json!({ "optionId": "a1", "kind": "allow_once" }),
        json!({ "optionId": "a2", "kind": "allow_always" }),
        json!({ "optionId": "r1", "kind": "reject_once" }),
        json!({ "optionId": "r2", "kind": "reject_always" }),
    ];
    assert_eq!(choose_option(&all, ApprovalDecision::AllowOnce).as_deref(), Some("a1"));
    assert_eq!(choose_option(&all, ApprovalDecision::AllowAlways).as_deref(), Some("a2"));
    assert_eq!(choose_option(&all, ApprovalDecision::Deny).as_deref(), Some("r1"));
    let only_always = vec![json!({ "optionId": "x", "kind": "allow_always" })];
    assert_eq!(choose_option(&only_always, ApprovalDecision::AllowOnce).as_deref(), Some("x"));
    assert_eq!(choose_option(&only_always, ApprovalDecision::Deny), None);
}

#[test]
fn stop_reasons_map_to_turn_outcomes() {
    let end = |r: Value, c| turn_end_events(&r, c).pop().unwrap();
    assert_eq!(end(json!({"stopReason": "end_turn"}), false), AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
    assert_eq!(end(json!({"stopReason": "cancelled"}), false), AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted });
    assert_eq!(end(json!({"stopReason": "end_turn"}), true), AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted });
    let refusal = turn_end_events(&json!({"stopReason": "refusal"}), false);
    assert!(matches!(&refusal[0], AgentEvent::Notice { message } if message.contains("refused")));
    assert_eq!(refusal[1], AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
}

#[test]
fn message_ids_split_messages() {
    let host = Host::new("/".into(), None);
    let mut m = Mapper::default();
    let chunk = |id: &str, t: &str| json!({ "sessionUpdate": "agent_message_chunk", "messageId": id,
                                             "content": { "type": "text", "text": t } });
    let mut out = m.on_update(&chunk("a", "one"), &host);
    out.extend(m.on_update(&chunk("b", "two"), &host));
    out.extend(m.end_turn());
    assert_eq!(
        out,
        vec![
            AgentEvent::AssistantDelta { text: "one".into() },
            AgentEvent::AssistantMessage { text: "one".into() },
            AgentEvent::AssistantDelta { text: "two".into() },
            AgentEvent::AssistantMessage { text: "two".into() },
        ]
    );
}

fn chunk(text: &str) -> String {
    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{
        "sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":text}}}})
    .to_string()
}

#[test]
fn fr_s8_drain_ends_on_the_prompt_response_or_an_unanswerable_request() {
    assert!(!drain_ends_turn(&chunk("a")));
    assert!(!drain_ends_turn("not json"));
    assert!(drain_ends_turn(r#"{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}"#));
    assert!(drain_ends_turn(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32603,"message":"x"}}"#));
    assert!(drain_ends_turn(r#"{"jsonrpc":"2.0","id":9,"method":"session/request_permission","params":{}}"#));
    assert!(drain_ends_turn(r#"{"jsonrpc":"2.0","id":9,"method":"fs/read_text_file","params":{}}"#));
}

#[test]
fn fr_s8_drained_acp_turn_becomes_deltas_a_message_usage_and_the_turn_end() {
    let lines = vec![
        chunk("tick 1 "),
        chunk("tick 2 "),
        r#"{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn","usage":{"inputTokens":3,"outputTokens":4}}}"#
            .to_string(),
    ];
    let out = translate_drained_lines(&lines);
    assert_eq!(
        out,
        vec![
            AgentEvent::AssistantDelta { text: "tick 1 ".into() },
            AgentEvent::AssistantDelta { text: "tick 2 ".into() },
            AgentEvent::AssistantMessage { text: "tick 1 tick 2 ".into() },
            AgentEvent::Usage { input_tokens: 3, output_tokens: 4 },
            AgentEvent::TurnEnded { outcome: TurnOutcome::Completed },
        ]
    );
}

#[test]
fn fr_s8_drained_acp_turn_without_an_end_flushes_the_message_and_leaves_the_turn_open() {
    let lines = vec![
        chunk("half "),
        r#"{"jsonrpc":"2.0","id":9,"method":"session/request_permission","params":{"toolCall":{}}}"#.to_string(),
        chunk("never read"),
    ];
    let out = translate_drained_lines(&lines);
    assert_eq!(
        out,
        vec![
            AgentEvent::AssistantDelta { text: "half ".into() },
            AgentEvent::AssistantMessage { text: "half ".into() },
        ]
    );
}

#[test]
fn fr_s8_drained_acp_error_response_fails_the_turn() {
    let lines = vec![r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32603,"message":"boom"}}"#.to_string()];
    let out = translate_drained_lines(&lines);
    assert_eq!(
        out,
        vec![
            AgentEvent::Error { message: "ACP session/prompt failed: boom".into() },
            AgentEvent::TurnEnded { outcome: TurnOutcome::Failed },
        ]
    );
}

#[test]
fn fr_s8_acp_adapter_drains_a_surviving_agent_and_does_not_rejoin_it() {
    let adapter = AcpAdapter::new(AcpConfig::preset("omp", None).unwrap()).unwrap();
    assert!(adapter.drains_shim());
    assert!(!adapter.rejoins_shim());
}

// ---------------------------------------------------------------------------------------------
// Gemini CLI preset (FR-A10..FR-A10f); the real CLI is not available, so the agent is the fake.

const GEMINI_AUTH_TEXT: &str =
    "sign in needed: set GEMINI_API_KEY (individual Google accounts are no longer served by the Gemini CLI, use Antigravity)";

fn gemini_setup(cwd: &Path) -> Setup {
    Setup { agent: "gemini".into(), ..setup(cwd) }
}

fn typed(events: &[AgentEvent]) -> Option<(String, String)> {
    events.iter().find_map(|e| match e {
        AgentEvent::TurnError { code, message } => Some((code.clone(), message.clone())),
        _ => None,
    })
}

#[test]
fn fr_a10_gemini_preset_fields() {
    let g = AcpConfig::preset("gemini", None).unwrap();
    assert_eq!(g.name, "gemini");
    assert_eq!(g.command, PathBuf::from("gemini"));
    assert_eq!(g.args, ["--acp"]);
    assert_eq!(g.version_args, ["--version"]);
    assert_eq!(g.model_args, ["-m", "{model}"]);
    assert!(g.instructions_args.is_empty(), "instructions go in front of the first prompt (FR-A10f)");
    assert_eq!(AcpConfig::preset("gemini", Some("/opt/g".into())).unwrap().command, PathBuf::from("/opt/g"));
}

#[test]
fn fr_a10_gemini_process_args_carry_the_model_and_no_instructions_flag() {
    let g = AcpConfig::preset("gemini", None).unwrap();
    assert_eq!(g.process_args(&StartRequest::default()), ["--acp"]);
    let req = StartRequest { model: Some("gemini-2.5-pro".into()), instructions: Some("x".into()), ..Default::default() };
    assert_eq!(g.process_args(&req), ["--acp", "-m", "gemini-2.5-pro"]);
    assert_eq!(g.process_args_with(&["--experimental-acp".to_string()], &req), ["--experimental-acp", "-m", "gemini-2.5-pro"]);
}

#[test]
fn fr_a10_gemini_is_opt_in_like_omp_and_overridable() {
    let c = parse_configs(r#"["gemini"]"#, |n| (n == "gemini").then(|| PathBuf::from("/opt/g"))).unwrap();
    assert_eq!(c, [AcpConfig::preset("gemini", Some("/opt/g".into())).unwrap()]);
    let o = parse_configs(r#"[{"name": "gemini", "args": ["--experimental-acp"], "model_args": []}]"#, |_| None).unwrap();
    assert_eq!(o[0].args, ["--experimental-acp"]);
    assert!(o[0].model_args.is_empty());
    assert_eq!(o[0].fallback_args(), None, "an overridden argument list is never retried");
    assert_eq!(c[0].fallback_args(), Some(vec!["--experimental-acp".to_string()]));
    assert_eq!(AcpConfig::preset("omp", None).unwrap().fallback_args(), None);
}

#[test]
fn fr_a10_flag_rejection_is_decided_from_the_output() {
    for rejected in [
        "Unknown argument: acp\n",
        "error: unknown option '--acp'",
        "gemini: unrecognized option `--acp'",
        "Unrecognised flag --acp",
        "Invalid argument: --acp",
        "\nUsage: gemini [options]\nUnknown arguments: acp, a\n",
    ] {
        assert!(rejects_acp_flag(rejected), "{rejected:?}");
    }
    for other in [
        "",
        "Error: GEMINI_API_KEY is not set",
        "listening on stdio",
        "ECONNREFUSED 127.0.0.1:443",
        "Authentication required",
    ] {
        assert!(!rejects_acp_flag(other), "{other:?}");
    }
}

#[test]
fn fr_a10_rpc_errors_are_classified_by_a_heuristic() {
    let e = |code: i64, m: &str| RpcError::new(code, m);
    use ErrorClass::*;
    assert_eq!(classify_rpc_error(&e(-32000, "Authentication required")), Some(AuthRequired));
    assert_eq!(classify_rpc_error(&e(-32603, "Please set an Auth method in your settings")), None);
    assert_eq!(classify_rpc_error(&e(-32603, "Failed to login. API key not valid")), Some(AuthRequired));
    assert_eq!(classify_rpc_error(&e(-32603, "request failed with status 401")), Some(AuthRequired));
    assert_eq!(classify_rpc_error(&e(-32603, "RESOURCE_EXHAUSTED: Quota exceeded")), Some(QuotaExceeded));
    assert_eq!(classify_rpc_error(&e(-32603, "[429 Too Many Requests]")), Some(QuotaExceeded));
    assert_eq!(classify_rpc_error(&e(-32603, "You have exhausted your quota")), Some(QuotaExceeded));
    assert_eq!(classify_rpc_error(&e(-32000, "API key quota exceeded")), Some(QuotaExceeded), "quota wins");
    let with_data = RpcError { data: Some("{\"status\":\"RESOURCE_EXHAUSTED\"}".into()), ..e(-32603, "Internal error") };
    assert_eq!(classify_rpc_error(&with_data), Some(QuotaExceeded), "the data is read too");
    assert_eq!(classify_rpc_error(&e(-32603, "boom")), None);
    assert_eq!(classify_rpc_error(&e(-32603, "error 4290 in parser")), None, "429 only as a whole number");
}

#[tokio::test]
async fn fr_a10b_an_authentication_error_on_a_prompt_is_the_typed_auth_required_and_the_session_lives() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = start(FakeOptions::default(), gemini_setup(dir.path())).await.unwrap();
    assert_eq!(next(&mut rx).await, AgentEvent::NativeSession { native_id: "fake-session-1".into() });
    let events = turn(&mut run, &mut rx, "auth-fail", ApprovalDecision::AllowOnce).await;
    assert_eq!(typed(&events), Some(("auth_required".into(), GEMINI_AUTH_TEXT.into())));
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }));
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
    let events = turn(&mut run, &mut rx, "echo again", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "echo again");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fr_a10b_another_agent_gets_the_generic_sign_in_text() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = new_session(FakeOptions::default(), dir.path()).await;
    let events = turn(&mut run, &mut rx, "auth-fail", ApprovalDecision::AllowOnce).await;
    let (code, message) = typed(&events).unwrap();
    assert_eq!(code, "auth_required");
    assert!(message.starts_with("sign in needed:") && !message.contains("GEMINI"), "{message}");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fr_a10b_session_new_asking_for_authentication_calls_authenticate_with_the_listed_key_method() {
    let dir = tempfile::tempdir().unwrap();
    let opts = FakeOptions { require_auth: true, key_ok: true, ..Default::default() };
    let (mut run, mut rx) = start(opts, gemini_setup(dir.path())).await.unwrap();
    assert_eq!(next(&mut rx).await, AgentEvent::NativeSession { native_id: "fake-session-1".into() });
    let events = turn(&mut run, &mut rx, "hello", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "Hi there");
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fr_a10b_without_a_key_the_session_start_does_not_crash_and_every_turn_says_auth_required() {
    let dir = tempfile::tempdir().unwrap();
    let opts = FakeOptions { require_auth: true, key_ok: false, ..Default::default() };
    let (mut run, mut rx) = start(opts, gemini_setup(dir.path())).await.unwrap();
    for _ in 0..2 {
        let events = turn(&mut run, &mut rx, "hello", ApprovalDecision::AllowOnce).await;
        assert_eq!(typed(&events), Some(("auth_required".into(), GEMINI_AUTH_TEXT.into())));
        assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }));
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::NativeSession { .. })));
    }
    // Interrupting before a session exists is harmless.
    run.interrupt().await.unwrap();
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fr_a10b_other_start_failures_still_fail_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let err = start(FakeOptions { version: 2, ..Default::default() }, gemini_setup(dir.path())).await.err().unwrap();
    assert!(format!("{err:#}").contains("protocol version"));
}

#[tokio::test]
async fn fr_a10c_a_quota_error_ends_the_turn_typed_and_the_next_turn_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = start(FakeOptions::default(), gemini_setup(dir.path())).await.unwrap();
    assert!(matches!(next(&mut rx).await, AgentEvent::NativeSession { .. }));
    let events = turn(&mut run, &mut rx, "quota", ApprovalDecision::AllowOnce).await;
    let (code, message) = typed(&events).expect("a typed error");
    assert_eq!(code, "quota_exceeded");
    assert!(message.contains("RESOURCE_EXHAUSTED"), "the agent's own message is kept: {message}");
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Failed }));
    let events = turn(&mut run, &mut rx, "echo next", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&events), "echo next");
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }));
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fr_a10c_an_unclassified_error_stays_a_plain_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = start(FakeOptions::default(), gemini_setup(dir.path())).await.unwrap();
    assert!(matches!(next(&mut rx).await, AgentEvent::NativeSession { .. }));
    let events = turn(&mut run, &mut rx, "boom", ApprovalDecision::AllowOnce).await;
    assert!(typed(&events).is_none());
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Error { message } if message.contains("boom"))));
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fr_a10d_usage_is_reported_only_when_the_agent_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut run, mut rx) = start(FakeOptions::default(), gemini_setup(dir.path())).await.unwrap();
    assert!(matches!(next(&mut rx).await, AgentEvent::NativeSession { .. }));
    let silent = turn(&mut run, &mut rx, "echo quiet", ApprovalDecision::AllowOnce).await;
    assert!(!silent.iter().any(|e| matches!(e, AgentEvent::Usage { .. })), "no invented usage: {silent:?}");
    let reported = turn(&mut run, &mut rx, "hello", ApprovalDecision::AllowOnce).await;
    assert!(reported.contains(&AgentEvent::Usage { input_tokens: 10, output_tokens: 5 }));
    run.shutdown().await.unwrap();
}

#[tokio::test]
async fn fr_a10f_instructions_go_in_front_of_the_first_prompt_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = gemini_setup(dir.path());
    s.prepend_instructions = Some("echo INSTR".into());
    let (mut run, mut rx) = start(FakeOptions::default(), s).await.unwrap();
    assert!(matches!(next(&mut rx).await, AgentEvent::NativeSession { .. }));
    let first = turn(&mut run, &mut rx, "one", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&first), "echo INSTR\n\none");
    let second = turn(&mut run, &mut rx, "echo two", ApprovalDecision::AllowOnce).await;
    assert_eq!(last_message(&second), "echo two");
    run.shutdown().await.unwrap();
}
