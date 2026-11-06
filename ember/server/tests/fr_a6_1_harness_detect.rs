//! FR-A6.1: per-computer harness detection that never invents a version. The adapters take the
//! CLI path in their constructor, so the tests point them at fake shell scripts in a temp dir.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::codex::CodexAdapter;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::auth::{self, ServerAuth};
use ember_server::computers::{default_connector, Computers, Registry};
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

mod common;

const DETECT: &str = "/api/computers/local/harnesses/detect";
const LISTEN: &str = "127.0.0.1:8740";

/// Replace `path` with an executable script that can be executed when this returns (see
/// `common::write_exec_ready`).
fn script(path: &Path, body: &str) {
    common::write_exec_ready(path, &format!("#!/bin/sh\n{body}\n"));
}

struct Fx {
    app: Router,
    computers: Arc<Computers>,
    claude: PathBuf,
    _dir: tempfile::TempDir,
}

/// Claude Code at `<dir>/claude` (not created), Codex at a path that does not exist.
fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![
        Arc::new(ScriptedAdapter),
        Arc::new(ClaudeCodeAdapter::new(&claude)),
        Arc::new(CodexAdapter::new(dir.path().join("no-such-codex"))),
    ];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), default_connector());
    let app = ember_server::api::router(sessions.clone())
        .merge(ember_server::api::harnesses::router(sessions, computers.clone()));
    Fx { app, computers, claude, _dir: dir }
}

async fn call(app: &Router, method: Method, path: &str) -> (StatusCode, Value) {
    let r = Request::builder().method(method).uri(path).header("host", LISTEN).body(Body::empty()).unwrap();
    let r = app.clone().oneshot(r).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn row<'a>(v: &'a Value, harness: &str) -> &'a Value {
    v.as_array().unwrap().iter().find(|r| r["harness"] == harness).unwrap_or_else(|| panic!("{harness} in {v}"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

#[tokio::test]
async fn fr_a6_1_local_detect_reruns_the_probe_and_reports_version_path_and_time() {
    let f = fx();
    script(&f.claude, "echo '9.9.9 (Claude Code)'");
    let (st, v) = call(&f.app, Method::POST, DETECT).await;
    assert_eq!(st, StatusCode::OK);
    let r = row(&v, "claude-code");
    assert_eq!(r["installed"], true);
    assert_eq!(r["version"], "9.9.9");
    assert_eq!(r["path"], f.claude.to_str().unwrap());
    let at = r["detected_at"].as_i64().unwrap();
    assert!((now_ms() - at).abs() < 60_000, "detected_at {at}");
    // The scripted test adapter is not a harness.
    assert!(v.as_array().unwrap().iter().all(|r| r["harness"] != "scripted"));
}

#[tokio::test]
async fn fr_a6_1_absent_cli_is_not_installed_with_no_version_and_no_path() {
    let f = fx();
    let (st, v) = call(&f.app, Method::POST, DETECT).await;
    assert_eq!(st, StatusCode::OK);
    for h in ["claude-code", "codex"] {
        let r = row(&v, h);
        assert_eq!(r["installed"], false, "{h}");
        assert!(r["version"].is_null(), "{h}: {r}");
        assert!(r["path"].is_null(), "{h}: {r}");
        assert!(r["detected_at"].is_i64(), "{h}");
    }
    assert!(!v.to_string().contains("1.0.0"));
}

#[tokio::test]
async fn fr_a6_1_unparsable_version_output_is_installed_with_a_null_version() {
    let f = fx();
    script(&f.claude, "echo 'garbage in, garbage out'");
    let (_, v) = call(&f.app, Method::POST, DETECT).await;
    let r = row(&v, "claude-code");
    assert_eq!(r["installed"], true);
    assert!(r["version"].is_null(), "{r}");
    assert_eq!(r["path"], f.claude.to_str().unwrap());
}

#[tokio::test]
async fn fr_a6_1_second_call_sees_a_replaced_cli_no_cache() {
    let f = fx();
    script(&f.claude, "echo '1.2.3 (Claude Code)'");
    let (_, v) = call(&f.app, Method::POST, DETECT).await;
    assert_eq!(row(&v, "claude-code")["version"], "1.2.3");
    script(&f.claude, "echo '4.5.6 (Claude Code)'");
    let (_, v) = call(&f.app, Method::POST, DETECT).await;
    assert_eq!(row(&v, "claude-code")["version"], "4.5.6");
    std::fs::remove_file(&f.claude).unwrap();
    let (_, v) = call(&f.app, Method::POST, DETECT).await;
    let r = row(&v, "claude-code");
    assert_eq!(r["installed"], false);
    assert!(r["version"].is_null() && r["path"].is_null());
}

#[tokio::test]
async fn fr_a6_1_unknown_computer_is_404_and_a_registered_one_is_501() {
    let f = fx();
    let (st, body) = call(&f.app, Method::POST, "/api/computers/nope/harnesses/detect").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string());
    let c = f.computers.registry().insert("studio", "http://studio.local:8741", "tok").unwrap();
    let (st, body) = call(&f.app, Method::POST, &format!("/api/computers/{}/harnesses/detect", c.id)).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["code"], "not_implemented");
    assert!(body["error"].as_str().unwrap().contains("not implemented"));
}

#[tokio::test]
async fn fr_a6_1_get_agents_keeps_its_shape_and_gains_path_and_detected_at() {
    let f = fx();
    script(&f.claude, "echo '9.9.9 (Claude Code)'");
    let (st, v) = call(&f.app, Method::GET, "/api/agents").await;
    assert_eq!(st, StatusCode::OK);
    let r = v.as_array().unwrap().iter().find(|r| r["kind"] == "claude-code").unwrap();
    assert_eq!(r["installed"], true);
    assert_eq!(r["version"], "9.9.9");
    assert_eq!(r["path"], f.claude.to_str().unwrap());
    assert!(r["detected_at"].is_i64());
    let c = v.as_array().unwrap().iter().find(|r| r["kind"] == "codex").unwrap();
    assert_eq!(c["installed"], false);
    assert!(c["version"].is_null() && c["path"].is_null());
}

#[tokio::test]
async fn fr_a6_1_the_route_is_behind_the_token() {
    let f = fx();
    let dir = tempfile::tempdir().unwrap();
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let app = auth::protect(f.app.clone(), ServerAuth::new(token.clone(), LISTEN.parse().unwrap(), []));
    let (st, _) = call(&app, Method::POST, DETECT).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let r = Request::post(DETECT)
        .header("host", LISTEN)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(r).await.unwrap().status(), StatusCode::OK);
}
