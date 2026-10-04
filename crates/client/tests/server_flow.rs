//! The client against a real ember-server router in-process, with the scripted agent.
//! PR-1 (push, gap recovery, version check), FR-L1 (cache), FR-L2 (status), FR-S6 (lease).

use std::sync::Arc;
use std::time::Duration;

use ember_client::state::LauncherStatus;
use ember_client::transcript::{ApprovalState, Transcript, TranscriptItem};
use ember_client::wire::{self, ApprovalDecision, NewSession};
use ember_client::{Client, ClientConfig, ConnectionState};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::SessionStatus;
use ember_server::session::Sessions;
use ember_server::store::Store;

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn start_server() -> (Arc<Sessions>, String) {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let s = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), adapters);
    let url = serve(ember_server::api::router(s.clone())).await;
    (s, url)
}

fn config(url: &str, cache: &std::path::Path) -> ClientConfig {
    let mut c = ClientConfig::new(url);
    c.cache_path = Some(cache.to_path_buf());
    c.lease_interval = Duration::from_millis(50);
    c.backoff_min = Duration::from_millis(20);
    c.backoff_max = Duration::from_millis(200);
    c.resync_settle = Duration::from_millis(50);
    c.cache_debounce = Duration::from_millis(10);
    c
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn wait_server(s: &Sessions, id: &str, want: SessionStatus) {
    until(&format!("server {want:?}"), || s.store().session(id).unwrap().unwrap().status == want).await;
}

/// The server's stored events, reduced by the client's own reducer from scratch.
fn reference(s: &Sessions, id: &str) -> Transcript {
    let mut t = Transcript::new();
    for e in s.store().events_after(id, 0).unwrap() {
        let e: wire::StoredEvent = serde_json::from_value(serde_json::to_value(e).unwrap()).unwrap();
        t.apply(&e);
    }
    t
}

fn client_status(c: &Client, id: &str) -> Option<wire::SessionStatus> {
    c.read(|st| st.session(id).map(|v| v.record.status))
}

fn wire_status(s: SessionStatus) -> wire::SessionStatus {
    serde_json::from_value(serde_json::to_value(s).unwrap()).unwrap()
}

/// Client state for `id` equals the server's: same status, same last seq, and a transcript equal
/// to one reduced from the server's stored events (so nothing lost, nothing applied twice).
async fn wait_in_sync(c: &Client, s: &Sessions, id: &str, with_transcript: bool) {
    until("client in sync with server", || {
        let rec = s.store().session(id).unwrap().unwrap();
        c.read(|st| {
            let Some(v) = st.session(id) else { return false };
            let same_rec = v.record.status == wire_status(rec.status) && v.record.last_seq == rec.last_seq;
            let same_t = !with_transcript || st.transcript(id) == Some(&reference(s, id));
            same_rec && same_t
        })
    })
    .await;
}

fn pending_approval(c: &Client, id: &str) -> Option<String> {
    c.read(|st| {
        st.transcript(id)?.pending_approvals().next().and_then(|i| match i {
            TranscriptItem::Approval { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
    })
}

#[tokio::test]
async fn turn_with_approval_survives_push_restarts_without_loss_or_duplicates() {
    let (s, url) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("client-cache.json");
    let c = Client::new(config(&url, &cache)).await.unwrap();
    c.start();
    until("connected", || c.read(|st| *st.connection() == ConnectionState::Connected)).await;

    let a = c
        .create_session(&NewSession {
            project: "acme".into(),
            agent: "scripted".into(),
            cwd: std::env::temp_dir().to_string_lossy().into(),
            model: None,
            title: Some("A".into()),
        })
        .await
        .unwrap()
        .id;
    // A second session created elsewhere: learned from push only, never opened.
    let b = s
        .create(ember_server::session::NewSession {
            project: "acme".into(),
            agent: AgentKind::Scripted,
            cwd: std::env::temp_dir(),
            model: None,
            title: "B".into(),
        })
        .unwrap()
        .id;
    until("B pushed", || client_status(&c, &b).is_some()).await;

    c.open_session(&a);
    c.send_message(&a, "hello").await.unwrap();
    until("approval shown", || pending_approval(&c, &a).is_some()).await;
    assert_eq!(c.read(|st| st.session(&a).unwrap().status), LauncherStatus::WaitingForApproval);

    // Kill push mid-turn; the turn goes on without us (answered over HTTP, which still works).
    c.pause_push();
    until("offline", || c.read(|st| *st.connection() == ConnectionState::Offline)).await;
    let approval = pending_approval(&c, &a).unwrap();
    c.answer(&a, &approval, ApprovalDecision::AllowOnce).await.unwrap();
    wait_server(&s, &a, SessionStatus::Finished).await;
    s.send(&b, "elsewhere").await.unwrap();
    wait_server(&s, &b, SessionStatus::WaitingForApproval).await;
    s.answer(&b, "approval-1", ember_server::events::ApprovalDecision::AllowAlways).await.unwrap();
    wait_server(&s, &b, SessionStatus::Finished).await;
    // Nothing arrived while disconnected.
    assert_eq!(client_status(&c, &a), Some(wire::SessionStatus::WaitingForApproval));

    c.resume_push();
    wait_in_sync(&c, &s, &a, true).await;
    wait_in_sync(&c, &s, &b, false).await;

    // Second turn, with the push socket dropped between the approval request and its answer.
    c.send_message(&a, "again").await.unwrap();
    until("second approval", || pending_approval(&c, &a).as_deref() == Some("approval-2")).await;
    c.reconnect_push();
    c.answer(&a, "approval-2", ApprovalDecision::Deny).await.unwrap();
    wait_server(&s, &a, SessionStatus::Finished).await;
    wait_in_sync(&c, &s, &a, true).await;

    // Shape of the reduced transcript.
    let t = c.read(|st| st.transcript(&a).unwrap().clone());
    let users = t.items.iter().filter(|i| matches!(i, TranscriptItem::User { .. })).count();
    assert_eq!(users, 2);
    assert!(t.items.iter().any(|i| matches!(i,
        TranscriptItem::Assistant { text, streaming: false, .. } if text == "echo: hello")));
    let approvals: Vec<_> = t
        .items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Approval { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(
        approvals,
        vec![ApprovalState::Resolved(ApprovalDecision::AllowOnce), ApprovalState::Resolved(ApprovalDecision::Deny)]
    );
    assert_eq!(t.usage.output_tokens, 10);
    assert_eq!(t.last_seq, s.store().session(&a).unwrap().unwrap().last_seq);

    // FR-L2: the open session is read; the one finished elsewhere is unread.
    assert_eq!(c.read(|st| st.session(&a).unwrap().status), LauncherStatus::Finished);
    assert_eq!(c.read(|st| st.session(&b).unwrap().status), LauncherStatus::FinishedUnread);

    // FR-S6: the open session is leased, so only B is released.
    assert_eq!(s.reap_idle(Duration::ZERO).await, vec![b.clone()]);
    c.close_session(&a);

    // FR-L1: a fresh client renders from the cache before any network request.
    c.flush_cache().await.unwrap();
    c.stop();
    let cold = Client::new(config("http://127.0.0.1:9", &cache)).await.unwrap();
    let projects = cold.read(|st| st.projects());
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "acme");
    assert_eq!(projects[0].sessions.len(), 2);
    assert_eq!(cold.read(|st| st.session(&a).unwrap().status), LauncherStatus::Finished);
    assert_eq!(cold.read(|st| st.session(&b).unwrap().status), LauncherStatus::FinishedUnread);
}

#[tokio::test]
async fn push_retries_with_backoff_until_server_comes_up() {
    // No server at first: the client keeps retrying with backoff.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let dir = tempfile::tempdir().unwrap();
    let c = Client::new(config(&format!("http://{addr}"), &dir.path().join("c.json"))).await.unwrap();
    c.start();
    until("reconnecting", || {
        c.read(|st| matches!(st.connection(), ConnectionState::Reconnecting { attempt, .. } if *attempt >= 2))
    })
    .await;

    // The server comes up on that address: the client connects and loads the sessions.
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let s = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), adapters);
    let id = s
        .create(ember_server::session::NewSession {
            project: "p".into(),
            agent: AgentKind::Scripted,
            cwd: std::env::temp_dir(),
            model: None,
            title: "t".into(),
        })
        .unwrap()
        .id;
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let app = ember_server::api::router(s.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    until("connected", || c.read(|st| *st.connection() == ConnectionState::Connected)).await;
    until("session listed", || client_status(&c, &id).is_some()).await;
    c.stop();
}

#[tokio::test]
async fn version_mismatch_is_reported_not_dropped() {
    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::routing::get;

    // Health says v2: refused before connecting.
    let app = axum::Router::new()
        .route("/api/v1/health", get(|| async { axum::Json(serde_json::json!({"ok": true, "push_version": 2})) }));
    let url = serve(app).await;
    let c = Client::new(ClientConfig::new(url)).await.unwrap();
    c.start();
    until("incompatible (health)", || {
        c.read(|st| *st.connection() == ConnectionState::Incompatible { server: 2, client: 1 })
    })
    .await;
    c.stop();

    // Health says v1 but a push message says v3: detected on the message.
    let app = axum::Router::new()
        .route("/api/v1/health", get(|| async { axum::Json(serde_json::json!({"ok": true, "push_version": 1})) }))
        .route("/api/v1/sessions", get(|| async { axum::Json(serde_json::json!([])) }))
        .route(
            "/api/v1/push",
            get(|ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut socket| async move {
                    let _ = socket.send(Message::Text(r#"{"type":"event","v":3}"#.into())).await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                })
            }),
        );
    let url = serve(app).await;
    let c = Client::new(ClientConfig::new(url)).await.unwrap();
    c.start();
    until("incompatible (push)", || {
        c.read(|st| *st.connection() == ConnectionState::Incompatible { server: 3, client: 1 })
    })
    .await;
    c.stop();
}

/// FR-L4, FR-L9, FR-S4: a second client sees pins, renames, archives and computer assignments
/// made by the first without reloading, and server search finds messages it never loaded.
#[tokio::test]
async fn metadata_and_assignment_reach_a_second_client_by_push() {
    let (s, url) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let one = Client::new(config(&url, &dir.path().join("one.json"))).await.unwrap();
    let two = Client::new(config(&url, &dir.path().join("two.json"))).await.unwrap();
    one.start();
    two.start();
    for c in [&one, &two] {
        until("connected", || c.read(|st| *st.connection() == ConnectionState::Connected)).await;
    }

    let id = one
        .create_session(&NewSession {
            project: "acme".into(),
            agent: "scripted".into(),
            cwd: std::env::temp_dir().to_string_lossy().into(),
            model: None,
            title: Some("A".into()),
        })
        .await
        .unwrap()
        .id;
    until("session on two", || two.read(|st| st.session(&id).is_some())).await;
    until("project on two", || two.read(|st| st.project("acme").is_some())).await;

    one.patch_session(&id, &wire::SessionPatch { title: Some("Deploy".into()), pinned: Some(true), archived: None })
        .await
        .unwrap();
    until("rename and pin on two", || {
        two.read(|st| st.session(&id).is_some_and(|v| v.record.title == "Deploy" && v.record.pinned))
    })
    .await;

    one.assign_computer("acme", "local").await.unwrap();
    until("assignment on two", || two.read(|st| st.project_computers("acme") == ["local".to_string()])).await;
    one.unassign_computer("acme", "local").await.unwrap();
    until("unassignment on two", || two.read(|st| st.project_computers("acme").is_empty())).await;

    s.record_event(&id, &ember_server::events::AgentEvent::UserMessage { text: "배포 스크립트가 실패했어요".into() })
        .unwrap();
    let hits = two.search("스크립트", None).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].session_id.as_str(), hits[0].seq), (id.as_str(), 1));
    assert_eq!(hits[0].plain_snippet(), "배포 스크립트가 실패했어요");

    let export = two.api().export_session(&id).await.unwrap();
    assert_eq!(export["session"]["title"], "Deploy");
    let detail = two.api().session(&id).await.unwrap();
    assert!(!detail.can_fork);
    assert_eq!(two.api().fork_session(&id, None).await.unwrap_err().status(), Some(501));
}
