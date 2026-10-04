//! End-to-end session flow against the scripted agent: FR-S1–S3, FR-A5, PR-1.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus, TurnOutcome};
use ember_server::session::{NewSession, Push, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn sessions_with(store: Arc<Store>) -> Arc<Sessions> {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    Sessions::new(store, adapters)
}

fn new_session(s: &Sessions) -> String {
    s.create(NewSession {
        project: "acme".into(),
        agent: AgentKind::Scripted,
        cwd: std::env::temp_dir(),
        model: None,
        title: "t".into(),
    })
    .unwrap()
    .id
}

/// Wait until the stored session reaches `want`.
async fn wait_status(s: &Sessions, id: &str, want: SessionStatus) {
    for _ in 0..200 {
        if s.store().session(id).unwrap().unwrap().status == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("session {id} never reached {want:?}");
}

#[tokio::test]
async fn turn_completes_with_approval_and_is_stored_in_order() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    let mut push = s.subscribe();

    s.send(&id, "hello").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    s.answer(&id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
    wait_status(&s, &id, SessionStatus::Finished).await;

    let kinds: Vec<_> = s
        .store()
        .events_after(&id, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert!(matches!(kinds[0], AgentEvent::NativeSession { .. }));
    assert_eq!(kinds[1], AgentEvent::UserMessage { text: "hello".into() });
    assert!(matches!(kinds[2], AgentEvent::ToolCall { .. }));
    assert!(matches!(kinds[3], AgentEvent::ApprovalRequested { .. }));
    assert!(matches!(kinds[4], AgentEvent::ApprovalResolved { decision: ApprovalDecision::AllowOnce, .. }));
    assert_eq!(kinds.last().unwrap(), &AgentEvent::TurnEnded { outcome: TurnOutcome::Completed });
    assert!(kinds.contains(&AgentEvent::AssistantMessage { text: "echo: hello".into() }));

    // Every stored event was also pushed, with consecutive sequence numbers.
    let mut seqs = Vec::new();
    while let Ok(p) = push.try_recv() {
        if let Push::Event { event, .. } = p {
            seqs.push(event.seq);
        }
    }
    assert_eq!(seqs, (1..=kinds.len() as i64).collect::<Vec<_>>());
}

#[tokio::test]
async fn turn_completes_with_no_client_attached_and_late_reader_sees_all() {
    // FR-S3: nobody subscribes to push; a late reader replays from the store.
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "unattended").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    s.answer(&id, "approval-1", ApprovalDecision::AllowAlways).await.unwrap();
    wait_status(&s, &id, SessionStatus::Finished).await;
    let all = s.store().events_after(&id, 0).unwrap();
    assert!(all.iter().any(|e| e.event == AgentEvent::AssistantMessage { text: "echo: unattended".into() }));
}

#[tokio::test]
async fn interrupt_ends_turn_as_interrupted() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let id = new_session(&s);
    s.send(&id, "slow task").await.unwrap();
    wait_status(&s, &id, SessionStatus::Running).await;
    s.interrupt(&id).await.unwrap();
    wait_status(&s, &id, SessionStatus::Finished).await;
    let last = s.store().events_after(&id, 0).unwrap().pop().unwrap();
    assert_eq!(last.event, AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted });
}

#[tokio::test]
async fn restart_resumes_with_the_stored_native_id() {
    // FR-S2: after a "restart" (new Sessions over the same file), the next message resumes the
    // agent with the native id recorded before, and the history is intact.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ember.db");
    let (id, native) = {
        let s = sessions_with(Arc::new(Store::open(&path).unwrap()));
        let id = new_session(&s);
        s.send(&id, "first").await.unwrap();
        wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
        s.answer(&id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
        wait_status(&s, &id, SessionStatus::Finished).await;
        let native = s.store().session(&id).unwrap().unwrap().native_id.unwrap();
        s.release(&id).await.unwrap();
        (id, native)
    };
    let store = Arc::new(Store::open(&path).unwrap());
    store.reset_live_statuses().unwrap();
    let s = sessions_with(store);
    let before = s.store().session(&id).unwrap().unwrap().last_seq;
    s.send(&id, "second").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    let rec = s.store().session(&id).unwrap().unwrap();
    assert_eq!(rec.native_id.as_deref(), Some(native.as_str()));
    // The resumed agent's repeated NativeSession was not stored again.
    let new_events = s.store().events_after(&id, before).unwrap();
    assert!(!new_events.iter().any(|e| matches!(e.event, AgentEvent::NativeSession { .. })));
}

#[tokio::test]
async fn http_create_send_and_read_events() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let app = ember_server::api::router(s.clone());

    let body = serde_json::json!({ "project": "acme", "agent": "scripted", "cwd": "/tmp" });
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let rec: serde_json::Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let id = rec["id"].as_str().unwrap().to_string();

    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/api/sessions/{id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;

    let res = app
        .clone()
        .oneshot(Request::get(format!("/api/sessions/{id}/events?after=1")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let events: Vec<serde_json::Value> =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(events[0]["seq"], 2);
    assert_eq!(events[0]["event"]["kind"], "user_message");

    let res = app
        .oneshot(Request::get("/api/sessions/nope/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn idle_agents_are_released_unless_leased_and_resume_on_next_message() {
    // FR-S6.
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let a = new_session(&s);
    let b = new_session(&s);
    for id in [&a, &b] {
        s.send(id, "x").await.unwrap();
        wait_status(&s, id, SessionStatus::WaitingForApproval).await;
    }
    // Mid-turn sessions are never reaped.
    assert!(s.reap_idle(Duration::ZERO).await.is_empty());
    for id in [&a, &b] {
        s.answer(id, "approval-1", ApprovalDecision::AllowOnce).await.unwrap();
        wait_status(&s, id, SessionStatus::Finished).await;
    }
    s.lease(&b, Duration::from_secs(60)).unwrap();
    assert_eq!(s.reap_idle(Duration::ZERO).await, vec![a.clone()]);
    assert!(!s.is_live(&a).await);
    assert!(s.is_live(&b).await);
    // A released session comes back on the next message.
    s.send(&a, "again").await.unwrap();
    wait_status(&s, &a, SessionStatus::WaitingForApproval).await;
    assert!(s.is_live(&a).await);
}

#[tokio::test]
async fn start_hooks_run_for_each_agent_start() {
    let s = sessions_with(Arc::new(Store::open_in_memory().unwrap()));
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    s.add_start_hook(Arc::new(move |rec| {
        log.lock().unwrap().push(rec.id.clone());
        vec![("EMBER_SESSION_ID".into(), rec.id.clone())]
    }));
    let id = new_session(&s);
    s.send(&id, "x").await.unwrap();
    wait_status(&s, &id, SessionStatus::WaitingForApproval).await;
    assert_eq!(*seen.lock().unwrap(), vec![id]);
}
