//! FR-L20 and FR-S8 (#311): the per-harness memory limit is stored on the server.
//!
//! `PUT /api/agents/{agent}/settings` and `GET /api/agents/settings`, the startup load, and the
//! effect on the start request of agents started afterwards. Written test first; not compiled or
//! run by the author.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::auth::{self, ServerAuth};
use ember_server::events::AgentEvent;
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;

const LISTEN: &str = "127.0.0.1:8740";
const RANGE_MESSAGE: &str = "mem_limit_mb must be 0 (clear) or between 256 and 262144 MiB";

/// A Codex adapter that is the scripted agent underneath, so a session of kind Codex can start.
struct FakeCodex;

#[async_trait]
impl AgentAdapter for FakeCodex {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    async fn detect(&self) -> Detected {
        Detected { kind: AgentKind::Codex, installed: true, version: Some("test".into()) }
    }

    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        ScriptedAdapter.start(req, events).await
    }
}

fn sessions_on(store: Arc<Store>) -> Arc<Sessions> {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(FakeCodex), Arc::new(ScriptedAdapter)];
    Sessions::new(store, adapters)
}

fn sessions() -> Arc<Sessions> {
    sessions_on(Arc::new(Store::open_in_memory().unwrap()))
}

async fn call(app: &Router, method: Method, path: &str, body: Option<&str>) -> (StatusCode, Value) {
    call_with(app, method, path, body, &[]).await
}

async fn call_with(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path).header("host", LISTEN);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = match body {
        Some(t) => b.header("content-type", "application/json").body(Body::from(t.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn put(app: &Router, agent: &str, body: &str) -> (StatusCode, Value) {
    call(app, Method::PUT, &format!("/api/agents/{agent}/settings"), Some(body)).await
}

async fn get_settings(app: &Router) -> Value {
    let (st, body) = call(app, Method::GET, "/api/agents/settings", None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn fr_l20_put_then_get() {
    let s = sessions();
    let app = ember_server::api::router(s.clone());
    assert_eq!(get_settings(&app).await, json!({ "settings": {} }), "nothing stored, nothing listed");
    let (st, body) = put(&app, "codex", r#"{"mem_limit_mb": 2048}"#).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "agent": "codex", "mem_limit_mb": 2048, "applies": "next_session" }));
    assert_eq!(get_settings(&app).await, json!({ "settings": { "codex": { "mem_limit_mb": 2048 } } }));
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), Some(2048), "the running server sees it at once");
    let (st, _) = put(&app, "claude-code", r#"{"mem_limit_mb": 1024}"#).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        get_settings(&app).await,
        json!({ "settings": { "codex": { "mem_limit_mb": 2048 }, "claude-code": { "mem_limit_mb": 1024 } } })
    );
}

#[tokio::test]
async fn fr_l20_null_and_zero_clear() {
    for clear in [r#"{"mem_limit_mb": null}"#, r#"{"mem_limit_mb": 0}"#] {
        let s = sessions();
        let app = ember_server::api::router(s.clone());
        put(&app, "codex", r#"{"mem_limit_mb": 512}"#).await;
        let (st, body) = put(&app, "codex", clear).await;
        assert_eq!(st, StatusCode::OK, "{clear}: {body}");
        assert_eq!(body, json!({ "agent": "codex", "mem_limit_mb": null, "applies": "next_session" }));
        assert_eq!(get_settings(&app).await, json!({ "settings": {} }), "{clear}");
        assert_eq!(s.mem_limit_mb(AgentKind::Codex), None, "{clear}");
        // Clearing what is not set is fine.
        let (st, _) = put(&app, "codex", clear).await;
        assert_eq!(st, StatusCode::OK, "{clear}");
    }
}

#[tokio::test]
async fn fr_l20_range_edges() {
    let app = ember_server::api::router(sessions());
    for ok in [256u64, 262_144] {
        let (st, body) = put(&app, "codex", &format!(r#"{{"mem_limit_mb": {ok}}}"#)).await;
        assert_eq!(st, StatusCode::OK, "{ok}: {body}");
        assert_eq!(body["mem_limit_mb"], json!(ok));
    }
    put(&app, "codex", r#"{"mem_limit_mb": 1024}"#).await;
    for bad in ["1", "255", "262145", "18446744073709551615", "-1", "1.5"] {
        let (st, body) = put(&app, "codex", &format!(r#"{{"mem_limit_mb": {bad}}}"#)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert_eq!(body["error"], json!(RANGE_MESSAGE), "{bad}");
    }
    assert_eq!(
        get_settings(&app).await,
        json!({ "settings": { "codex": { "mem_limit_mb": 1024 } } }),
        "a refused value changes nothing"
    );
}

#[tokio::test]
async fn fr_l20_unknown_field_and_missing_key_are_422() {
    let s = sessions();
    let app = ember_server::api::router(s.clone());
    for body in [
        r#"{"mem_limit_mb": 1024, "run_mode": "x"}"#,
        r#"{"run_mode": "x"}"#,
        r#"{}"#,
        r#"[]"#,
        r#""1024""#,
        r#"null"#,
    ] {
        let (st, b) = put(&app, "codex", body).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {b}");
    }
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), None, "nothing was applied");
    assert_eq!(get_settings(&app).await, json!({ "settings": {} }));
}

#[tokio::test]
async fn fr_l20_wrong_type_is_400() {
    let app = ember_server::api::router(sessions());
    for bad in [r#""2048""#, "true", "[]", "{}"] {
        let (st, body) = put(&app, "codex", &format!(r#"{{"mem_limit_mb": {bad}}}"#)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert_eq!(body["error"], json!(RANGE_MESSAGE), "{bad}");
    }
}

#[tokio::test]
async fn fr_l20_unknown_agent_and_scripted_are_404() {
    let s = sessions();
    let app = ember_server::api::router(s.clone());
    for agent in ["no-such-agent", "scripted", "claude", ""] {
        let path = format!("/api/agents/{agent}/settings");
        let (st, _) = call(&app, Method::PUT, &path, Some(r#"{"mem_limit_mb": 1024}"#)).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{agent:?}");
    }
    assert_eq!(s.mem_limit_mb(AgentKind::Scripted), None);
    assert_eq!(get_settings(&app).await, json!({ "settings": {} }));
}

/// Both routes sit behind the token layer, like `/api/agents` (FR-S9).
#[tokio::test]
async fn fr_s8_settings_routes_need_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let app = auth::protect(
        ember_server::api::router(sessions()),
        ServerAuth::new(token.clone(), LISTEN.parse().unwrap(), []),
    );
    let put_path = "/api/agents/codex/settings";
    let (st, body) = call(&app, Method::GET, "/api/agents/settings", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(body["error"].is_string(), "{body}");
    let (st, _) = call(&app, Method::PUT, put_path, Some(r#"{"mem_limit_mb": 1024}"#)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call_with(&app, Method::PUT, put_path, Some(r#"{"mem_limit_mb": 1024}"#), &[("authorization", "Bearer nope")]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // Refused without the token means nothing was stored: with the token the list is empty.
    let bearer = format!("Bearer {token}");
    let auth_h = [("authorization", bearer.as_str())];
    let (st, body) = call_with(&app, Method::GET, "/api/agents/settings", None, &auth_h).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, json!({ "settings": {} }));
    let (st, _) = call_with(&app, Method::PUT, put_path, Some(r#"{"mem_limit_mb": 1024}"#), &auth_h).await;
    assert_eq!(st, StatusCode::OK);
}

#[test]
fn fr_l20_load_settings_applies_a_stored_value() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.set_harness_setting("codex", "mem_limit_mb", "2048").unwrap();
    let s = sessions_on(store);
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), None, "nothing is loaded before load_settings");
    s.load_settings();
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), Some(2048));
    assert_eq!(s.mem_limit_mb(AgentKind::ClaudeCode), None);
}

#[test]
fn fr_l20_load_settings_ignores_bad_rows() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.set_harness_setting("codex", "mem_limit_mb", "2048").unwrap();
    store.set_harness_setting("claude-code", "mem_limit_mb", "not a number").unwrap();
    store.set_harness_setting("antigravity", "mem_limit_mb", "255").unwrap();
    store.set_harness_setting("scripted", "mem_limit_mb", "262145").unwrap();
    store.set_harness_setting("removed-acp-agent", "mem_limit_mb", "2048").unwrap();
    store.set_harness_setting("codex", "other_key", "9999999").unwrap();
    let s = sessions_on(store);
    s.load_settings();
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), Some(2048));
    assert_eq!(s.mem_limit_mb(AgentKind::ClaudeCode), None, "unparsable");
    assert_eq!(s.mem_limit_mb(AgentKind::Antigravity), None, "below the range");
    assert_eq!(s.mem_limit_mb(AgentKind::Scripted), None, "above the range");
}

#[test]
fn fr_l20_load_settings_takes_the_range_edges() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.set_harness_setting("codex", "mem_limit_mb", "256").unwrap();
    store.set_harness_setting("claude-code", "mem_limit_mb", "262144").unwrap();
    let s = sessions_on(store);
    s.load_settings();
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), Some(256));
    assert_eq!(s.mem_limit_mb(AgentKind::ClaudeCode), Some(262_144));
}

#[test]
fn fr_l20_set_mem_limit_persisted_writes_store_and_memory() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let s = sessions_on(store.clone());
    s.set_mem_limit_persisted(AgentKind::Codex, Some(4096)).unwrap();
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), Some(4096));
    assert_eq!(store.harness_setting("codex", "mem_limit_mb").unwrap().as_deref(), Some("4096"));
    s.set_mem_limit_persisted(AgentKind::Codex, Some(0)).unwrap();
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), None);
    assert_eq!(store.harness_setting("codex", "mem_limit_mb").unwrap(), None);
    s.set_mem_limit_persisted(AgentKind::Codex, Some(512)).unwrap();
    s.set_mem_limit_persisted(AgentKind::Codex, None).unwrap();
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), None);
    assert_eq!(store.harness_setting("codex", "mem_limit_mb").unwrap(), None);
}

#[test]
fn fr_l20_set_mem_limit_persisted_refuses_out_of_range() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let s = sessions_on(store.clone());
    s.set_mem_limit_persisted(AgentKind::Codex, Some(1024)).unwrap();
    for bad in [1u64, 255, 262_145, u64::MAX] {
        let e = s.set_mem_limit_persisted(AgentKind::Codex, Some(bad)).unwrap_err();
        assert!(e.to_string().contains("between 256 and 262144"), "{bad}: {e}");
    }
    assert_eq!(s.mem_limit_mb(AgentKind::Codex), Some(1024), "memory unchanged");
    assert_eq!(
        store.harness_setting("codex", "mem_limit_mb").unwrap().as_deref(),
        Some("1024"),
        "store unchanged"
    );
}

/// A session started after the PUT carries the stored limit on its start request; a session that
/// is already live keeps the one it started with (`applies: "next_session"`).
#[tokio::test]
async fn fr_s8_new_session_gets_the_stored_limit_and_a_live_one_keeps_its_own() {
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path().to_str().unwrap().to_string();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let s = sessions_on(store.clone());
    let starts: Arc<std::sync::Mutex<Vec<(String, Option<u64>)>>> = Arc::default();
    {
        let starts = starts.clone();
        s.add_start_config_hook(Arc::new(move |rec, req| {
            starts.lock().unwrap().push((rec.id.clone(), req.mem_limit_mb));
            Ok(())
        }));
    }
    let app = ember_server::api::router(s.clone());

    let (st, _) = put(&app, "codex", r#"{"mem_limit_mb": 2048}"#).await;
    assert_eq!(st, StatusCode::OK);
    let a = store.create_session("p", AgentKind::Codex, &cwd, None, "a").unwrap().id;
    s.send(&a, "hi").await.unwrap();
    assert_eq!(*starts.lock().unwrap(), vec![(a.clone(), Some(2048))]);

    let (st, _) = put(&app, "codex", r#"{"mem_limit_mb": 4096}"#).await;
    assert_eq!(st, StatusCode::OK);
    s.send(&a, "again").await.unwrap();
    assert_eq!(
        *starts.lock().unwrap(),
        vec![(a.clone(), Some(2048))],
        "the live session is not started again and keeps its limit"
    );

    let b = store.create_session("p", AgentKind::Codex, &cwd, None, "b").unwrap().id;
    s.send(&b, "hi").await.unwrap();
    assert_eq!(
        *starts.lock().unwrap(),
        vec![(a, Some(2048)), (b, Some(4096))],
        "a session started after the change uses the new value"
    );
}

/// The value survives a restart: a new `Sessions` on the same database file plus `load_settings`.
#[tokio::test]
async fn fr_s8_limit_survives_a_rebuilt_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ember.db");
    {
        let s = sessions_on(Arc::new(Store::open(&path).unwrap()));
        let app = ember_server::api::router(s);
        let (st, _) = put(&app, "antigravity", r#"{"mem_limit_mb": 3000}"#).await;
        assert_eq!(st, StatusCode::OK);
    }
    let s = sessions_on(Arc::new(Store::open(&path).unwrap()));
    assert_eq!(s.mem_limit_mb(AgentKind::Antigravity), None, "memory only until the load");
    s.load_settings();
    assert_eq!(s.mem_limit_mb(AgentKind::Antigravity), Some(3000));
    let app = ember_server::api::router(s);
    assert_eq!(get_settings(&app).await, json!({ "settings": { "antigravity": { "mem_limit_mb": 3000 } } }));
}
