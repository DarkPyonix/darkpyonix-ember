//! FR-G4: `/api/projects/{name}/git/<op>` relays to the project's computer node, with `repo`
//! added, and returns the node's status and body unchanged. The node is a stub.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::computers::{self, Computers, Registry};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

type Seen = Arc<Mutex<Vec<(String, Option<String>, Value)>>>;

/// A node that records each `/git/<op>` call (op, bearer token, body) and answers per op.
async fn stub_node() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    async fn git(
        State(seen): State<Seen>,
        Path(op): Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(String::from);
        seen.lock().unwrap().push((op.clone(), auth, body.clone()));
        match op.as_str() {
            "status" if body["repo"] == "/not/a/repo" => {
                (StatusCode::NOT_FOUND, Json(json!({ "code": "not_found", "error": "not_a_repo" })))
            }
            "commit" => (StatusCode::CONFLICT, Json(json!({ "code": "conflict", "error": "nothing to commit" }))),
            "stage" => (StatusCode::OK, Json(Value::Null)),
            _ => (StatusCode::OK, Json(json!({ "op": op, "echo": body }))),
        }
    }
    let app = Router::new().route("/git/{op}", post(git)).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

struct Fixture {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    app: Router,
}

fn fixture() -> Fixture {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let app = ember_server::api::router(sessions.clone())
        .merge(ember_server::api::git::router(sessions.clone(), computers.clone()));
    Fixture { sessions, computers, app }
}

fn new_session(s: &Sessions, project: &str, cwd: &str) -> String {
    s.create(NewSession {
        project: project.into(),
        agent: AgentKind::Scripted,
        cwd: cwd.into(),
        model: None,
        title: "t".into(),
    })
    .unwrap()
    .id
}

async fn call(app: &Router, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(Method::POST).uri(uri);
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, value)
}

/// A project `alpha` whose session lives at `cwd` on the stub node.
async fn on_node(cwd: &str) -> (Fixture, Seen) {
    let f = fixture();
    let (url, seen) = stub_node().await;
    new_session(&f.sessions, "alpha", cwd);
    let c = f.computers.registry().insert("studio", &url, "tok").unwrap();
    f.sessions.store().assign_computer("alpha", &c.id).unwrap();
    (f, seen)
}

#[tokio::test]
async fn fr_g4_every_op_is_forwarded_with_repo_and_the_nodes_token() {
    let (f, seen) = on_node("/work/alpha").await;
    for op in computers_ops() {
        let (st, out) = call(&f.app, &format!("/api/projects/alpha/git/{op}"), Some(json!({ "x": 1 }))).await;
        assert!(st == StatusCode::OK || st == StatusCode::CONFLICT, "{op}: {st} {out}");
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), computers_ops().len());
    for (op, auth, body) in seen.iter() {
        assert_eq!(auth.as_deref(), Some("Bearer tok"), "{op}");
        assert_eq!(body["repo"], "/work/alpha", "{op}");
        assert_eq!(body["x"], 1, "{op}: the body is forwarded");
    }
}

fn computers_ops() -> Vec<&'static str> {
    ember_server::api::git::OPS.to_vec()
}

#[test]
fn fr_g4_the_contract_ops_are_all_routed() {
    let mut ops = computers_ops();
    ops.sort();
    let mut want =
        vec!["status", "diff", "stage", "unstage", "apply", "commit", "checkout", "branch", "fetch", "pull", "push", "conflicts"];
    want.sort();
    assert_eq!(ops, want);
}

#[tokio::test]
async fn fr_g4_node_status_and_body_are_relayed_unchanged() {
    let (f, _) = on_node("/not/a/repo").await;
    let (st, out) = call(&f.app, "/api/projects/alpha/git/status", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(out["error"], "not_a_repo");
    let (st, out) = call(&f.app, "/api/projects/alpha/git/commit", Some(json!({ "summary": "s", "amend": false }))).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(out["error"], "nothing to commit");
}

#[tokio::test]
async fn fr_g4_a_client_cannot_choose_the_repo() {
    let (f, seen) = on_node("/work/alpha").await;
    call(&f.app, "/api/projects/alpha/git/diff", Some(json!({ "repo": "/etc", "staged": false }))).await;
    assert_eq!(seen.lock().unwrap()[0].2["repo"], "/work/alpha");
}

#[tokio::test]
async fn fr_g4_the_session_query_picks_the_repo_and_an_empty_body_is_allowed() {
    let (f, seen) = on_node("/work/alpha").await;
    let other = new_session(&f.sessions, "alpha", "/work/alpha-wt");
    let (st, _) = call(&f.app, &format!("/api/projects/alpha/git/conflicts?session={other}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(seen.lock().unwrap()[0].2["repo"], "/work/alpha-wt");
}

#[tokio::test]
async fn fr_g4_unknown_project_op_or_missing_session_is_404_and_a_dead_node_is_502() {
    let f = fixture();
    let (st, _) = call(&f.app, "/api/projects/nope/git/status", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (f, _) = on_node("/work/alpha").await;
    let (st, _) = call(&f.app, "/api/projects/alpha/git/rebase", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let f2 = fixture();
    f2.sessions.store().ensure_project("empty").unwrap();
    let (st, _) = call(&f2.app, "/api/projects/empty/git/status", Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let f3 = fixture();
    new_session(&f3.sessions, "beta", "/work/beta");
    let c = f3.computers.registry().insert("down", "http://127.0.0.1:1", "t").unwrap();
    f3.sessions.store().assign_computer("beta", &c.id).unwrap();
    let (st, _) = call(&f3.app, "/api/projects/beta/git/status", Some(json!({}))).await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
}

/// The in-process local node serves `/git/status` with no remote computer. The node is rooted at
/// `$HOME`, so the test points `HOME` at a scratch directory first (this is the only test in the
/// binary that reads `HOME`, and the node is built on first use).
#[tokio::test]
async fn fr_g4_the_local_node_serves_status_without_a_remote_computer() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch").join(format!("git-relay-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let base = base.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let git = |args: &[&str]| {
        let o = std::process::Command::new("git").args(args).current_dir(&repo).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
    // Set before the local node is built.
    std::env::set_var("HOME", &base);

    let f = fixture();
    new_session(&f.sessions, "local", repo.to_str().unwrap());
    let (st, out) = call(&f.app, "/api/projects/local/git/status", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let files = out["files"].as_array().expect("files array");
    assert!(files.iter().any(|x| x["path"] == "a.txt"), "{out}");

    let (st, out) = call(&f.app, "/api/projects/local/git/conflicts", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let _ = std::fs::remove_dir_all(&base);
}
