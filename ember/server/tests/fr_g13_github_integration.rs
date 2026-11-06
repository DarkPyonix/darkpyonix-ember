//! FR-G13: `GET /api/integrations/github` aggregates the nodes' `POST /git/github` answers.
//! The node is a stub that answers per repository directory and counts its calls.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
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

/// Per repo directory: the status and body the stub node answers.
type Answers = Arc<Mutex<HashMap<String, (u16, Value)>>>;

struct Node {
    url: String,
    answers: Answers,
    calls: Arc<Mutex<Vec<String>>>,
}

async fn stub_node() -> Node {
    #[derive(Clone)]
    struct S { answers: Answers, calls: Arc<Mutex<Vec<String>>> }
    async fn github(State(s): State<S>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        let repo = body["repo"].as_str().unwrap_or("").to_string();
        s.calls.lock().unwrap().push(repo.clone());
        let (status, v) = s.answers.lock().unwrap().get(&repo).cloned().unwrap_or((404, json!({"error": "not_a_repo"})));
        (StatusCode::from_u16(status).unwrap(), Json(v))
    }
    let answers: Answers = Arc::new(Mutex::new(HashMap::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route("/git/github", post(github)).with_state(S { answers: answers.clone(), calls: calls.clone() });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Node { url, answers, calls }
}

struct Fixture {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    node: Node,
    computer: String,
}

async fn fixture() -> Fixture {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let computers = Computers::new(Registry::new(store), computers::default_connector());
    let node = stub_node().await;
    let c = computers.registry().insert("studio", &node.url, "tok").unwrap();
    Fixture { sessions, computers, node, computer: c.id }
}

impl Fixture {
    /// A project whose newest session lives at `cwd` on the stub node.
    fn project(&self, name: &str, cwd: &str) {
        self.sessions
            .create(NewSession { project: name.into(), agent: AgentKind::Scripted, cwd: cwd.into(), model: None, title: "t".into() })
            .unwrap();
        self.sessions.store().assign_computer(name, &self.computer).unwrap();
    }

    fn answer(&self, cwd: &str, status: u16, body: Value) {
        self.node.answers.lock().unwrap().insert(cwd.into(), (status, body));
    }

    fn app(&self, ttl: Duration) -> Router {
        ember_server::api::integrations::router_with_ttl(self.sessions.clone(), self.computers.clone(), ttl)
    }

    fn node_calls(&self) -> usize {
        self.node.calls.lock().unwrap().len()
    }
}

fn summary(name: &str, open: Option<u64>, review: Option<u64>, ci: Value) -> Value {
    json!({
        "name_with_owner": name, "default_branch": "main", "account": "octo",
        "open_prs": open, "review_requested": review, "capped": false, "ci": ci,
        "signed_out": false, "error": null
    })
}

async fn get(app: &Router) -> (StatusCode, Value) {
    let req = Request::builder().method(Method::GET).uri("/api/integrations/github").body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

const LONG: Duration = Duration::from_secs(600);

#[tokio::test]
async fn fr_g13_two_projects_in_one_repository_count_it_once() {
    let f = fixture().await;
    f.project("alpha", "/w/a");
    f.project("beta", "/w/b");
    f.project("gamma", "/w/a"); // same directory as alpha: one node call
    f.answer("/w/a", 200, summary("o/r", Some(3), Some(1), json!(["success"])));
    f.answer("/w/b", 200, summary("o/r", Some(3), Some(1), json!(["success"])));
    let (status, body) = get(&f.app(LONG)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.node_calls(), 2, "one call per distinct directory");
    assert_eq!(body["state"], "on");
    assert_eq!(body["reason"], Value::Null);
    assert_eq!(body["account"], "octo");
    assert_eq!(body["repos"].as_array().unwrap().len(), 1);
    assert_eq!(body["repos"][0]["name_with_owner"], "o/r");
    assert_eq!(body["totals"]["repos"], 1);
    assert_eq!(body["totals"]["open_prs"], 3);
    assert_eq!(body["totals"]["review_requested"], 1);
    assert!(body["checked_at"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn fr_g13_signed_out_gives_warn() {
    let f = fixture().await;
    f.project("alpha", "/w/a");
    let mut v = summary("o/r", None, None, Value::Null);
    v["signed_out"] = json!(true);
    v["account"] = Value::Null;
    v["error"] = json!("GitHub에 로그인되어 있지 않습니다.");
    f.answer("/w/a", 200, v);
    let (_, body) = get(&f.app(LONG)).await;
    assert_eq!(body["state"], "warn");
    assert!(body["reason"].as_str().unwrap().contains("gh auth login"));
    assert_eq!(body["account"], Value::Null);
    assert_eq!(body["totals"]["open_prs"], 0);
    assert_eq!(body["totals"]["ci_pass_percent"], Value::Null);
}

#[tokio::test]
async fn fr_g13_no_projects_gives_off() {
    let f = fixture().await;
    let (status, body) = get(&f.app(LONG)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "off");
    assert!(body["reason"].is_string());
    assert_eq!(body["repos"], json!([]));
    assert_eq!(body["totals"]["repos"], 0);
    assert_eq!(f.node_calls(), 0);
}

#[tokio::test]
async fn fr_g13_gh_missing_everywhere_gives_off() {
    let f = fixture().await;
    f.project("alpha", "/w/a");
    f.answer("/w/a", 409, json!({"code": "conflict", "error": "gh 명령을 찾을 수 없습니다. 설치되어 있는지 확인하세요."}));
    let (_, body) = get(&f.app(LONG)).await;
    assert_eq!(body["state"], "off");
    assert!(body["reason"].as_str().unwrap().contains("설치"));
}

#[tokio::test]
async fn fr_g13_ci_percent_and_null_numbers() {
    let f = fixture().await;
    f.project("alpha", "/w/a");
    f.project("beta", "/w/b");
    f.answer("/w/a", 200, summary("o/a", Some(2), Some(1), json!(["success", "success", "failure"])));
    // A failed call: null numbers are left out of the sums; cancelled and pending do not count.
    f.answer("/w/b", 200, summary("o/b", None, None, json!(["success", "cancelled", "pending"])));
    let (_, body) = get(&f.app(LONG)).await;
    assert_eq!(body["repos"].as_array().unwrap().len(), 2);
    assert_eq!(body["totals"]["repos"], 2);
    assert_eq!(body["totals"]["open_prs"], 2);
    assert_eq!(body["totals"]["review_requested"], 1);
    // 3 successes of 4 decided runs.
    assert_eq!(body["totals"]["ci_pass_percent"], 75);
    let b = body["repos"].as_array().unwrap().iter().find(|r| r["name_with_owner"] == "o/b").unwrap();
    assert_eq!(b["open_prs"], Value::Null);
}

#[tokio::test]
async fn fr_g13_second_call_within_ttl_is_cached_and_expiry_asks_again() {
    let f = fixture().await;
    f.project("alpha", "/w/a");
    f.answer("/w/a", 200, summary("o/r", Some(1), Some(0), json!(["success"])));
    let app = f.app(Duration::from_millis(300));
    let (_, first) = get(&app).await;
    let (_, second) = get(&app).await;
    assert_eq!(f.node_calls(), 1, "served from the cache");
    assert_eq!(first["checked_at"], second["checked_at"]);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let _ = get(&app).await;
    assert_eq!(f.node_calls(), 2, "asked again after the cache expired");
}

#[tokio::test]
async fn fr_g13_concurrent_callers_share_one_refresh() {
    let f = fixture().await;
    f.project("alpha", "/w/a");
    f.answer("/w/a", 200, summary("o/r", Some(1), Some(0), json!(["success"])));
    let app = f.app(LONG);
    let (a, b, c) = tokio::join!(get(&app), get(&app), get(&app));
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b.1, c.1);
    assert_eq!(f.node_calls(), 1);
}
