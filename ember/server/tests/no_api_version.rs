//! INTENT D15 [user]: no REST path carries a version segment, and the old `/api/v1` prefix is
//! gone with no alias (issues #63 and #85). Mirrors `darkpyonix-core`'s
//! `nfr_v1_no_version_segment_in_any_rest_path`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::session::Sessions;
use ember_server::store::Store;
use tower::ServiceExt;

/// True when `path` has a segment `v<digits>` (the regex `/v[0-9]+(/|$)`).
fn has_version_segment(path: &str) -> bool {
    path.split('/').skip(1).any(|seg| {
        seg.len() > 1 && seg.starts_with('v') && seg[1..].bytes().all(|b| b.is_ascii_digit())
    })
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// axum cannot list its routes, so every router is registered with a string literal that
/// starts with `"/api`; read them straight out of the sources, so a new route is covered the
/// day it is written.
fn registered_paths() -> Vec<String> {
    let mut files = Vec::new();
    rust_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut paths = Vec::new();
    for f in files {
        let text = fs::read_to_string(&f).unwrap();
        for part in text.split("\"/api").skip(1) {
            let rest: String = part.chars().take_while(|c| !matches!(c, '"' | ' ' | '`' | '\n')).collect();
            paths.push(format!("/api{rest}"));
        }
    }
    paths
}

fn app() -> Router {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), adapters);
    let a2a = A2a::new(
        sessions.clone(),
        A2aStore::open_in_memory().unwrap(),
        A2aConfig::new("http://127.0.0.1:1"),
    );
    a2a.install();
    ember_server::api::router(sessions).merge(ember_server::a2a::api::router(a2a))
}

async fn status(app: &Router, path: &str) -> StatusCode {
    app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap().status()
}

#[test]
fn nfr_v1_no_version_segment_in_any_rest_path() {
    assert!(has_version_segment("/api/v1/sessions"));
    assert!(has_version_segment("/api/v12"));
    assert!(!has_version_segment("/api/sessions/{id}/events"));
    assert!(!has_version_segment("/api/vault"));

    let paths = registered_paths();
    assert!(paths.len() > 50, "found only {} route literals; the scan is broken", paths.len());
    assert!(paths.iter().any(|p| p == "/api/sessions"), "{paths:?}");
    let bad: Vec<_> = paths.iter().filter(|p| has_version_segment(p)).collect();
    assert!(bad.is_empty(), "versioned REST paths: {bad:?}");
}

#[tokio::test]
async fn nfr_v1_old_prefix_is_gone_without_an_alias() {
    let app = app();
    assert_eq!(status(&app, "/api/sessions").await, StatusCode::OK);
    assert_eq!(status(&app, "/api/health").await, StatusCode::OK);
    assert_eq!(status(&app, "/api/v1/sessions").await, StatusCode::NOT_FOUND);
    assert_eq!(status(&app, "/api/v1/health").await, StatusCode::NOT_FOUND);
    // A2A: unversioned path is routed (401/400 without a runtime token, never 404), v1 is not.
    assert_ne!(status(&app, "/api/a2a/targets").await, StatusCode::NOT_FOUND);
    assert_eq!(status(&app, "/api/v1/a2a/targets").await, StatusCode::NOT_FOUND);
}
