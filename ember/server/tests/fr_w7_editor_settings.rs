//! SPEC FR-W7, FR-W7a, FR-W7b (`INTENT.md` D56, #1121): the editor settings record in the
//! server's settings, inherited and overridden per computer, applied to each node through its
//! DarkPyonix manager, and the default IDE in the IDE listing.
//!
//! The nodes are real in-process `ember-node`s on loopback ports. Each has its own fake manager:
//! an axum server with `GET /api/editor` (the `ose` runtime installed and the default, so the CLI
//! is never run) and `PUT /api/editor/{runtime}/presets`, which records the body and answers 422
//! `extension_failed` for the extension `fail.ext`. The routes run behind the owner layer
//! (`owner::require`, enforce mode) with a live key, as on the local listener. Not covered: a real
//! manager, the CLI install (the node tests cover it with a fake CLI), a node over the transport,
//! and the app.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use ember_node::api::{self as node_api, Node};
use ember_node::config::NodeConfig;
use ember_node::editor_manager::{EditorRelays, ManagerSource};
use ember_node::editor_settings::NO_MANAGER;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::api::ide::{self, IdeConfig};
use ember_server::computers::{self, Computers, Registry};
use ember_server::editor_settings::{self as es, EditorSettingsService};
use ember_server::owner::{self, Claimant, Owner, Presence};
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const NODE_TOKEN: &str = "node-token";
const MANAGER_TOKEN: &str = "manager-master-token";
const OWNER_NAME: &str = "Brew's MacBook";
const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Default)]
struct FakeManager {
    /// `(runtime, body)` of every presets request.
    presets: Arc<Mutex<Vec<(String, Value)>>>,
}

impl FakeManager {
    fn themes(&self) -> Vec<String> {
        self.presets.lock().unwrap().iter().filter_map(|(_, b)| b["theme"].as_str().map(String::from)).collect()
    }

    fn clear(&self) {
        self.presets.lock().unwrap().clear();
    }
}

async fn start_fake_manager() -> (String, FakeManager) {
    use axum::extract::{Path as AxPath, State};
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::{get, put};
    fn bearer_ok(h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {MANAGER_TOKEN}"))
    }
    let m = FakeManager::default();
    let app = Router::new()
        .route(
            "/api/editor",
            get(|h: HeaderMap| async move {
                if !bearer_ok(&h) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                axum::Json(json!({ "runtimes": [{ "runtime": "ose", "version": "1.139.1", "commit": "abc", "state": "running", "path": "/ide/ose/" }],
                    "default_runtime": "ose", "autostart": true }))
                .into_response()
            }),
        )
        .route(
            "/api/editor/{runtime}/presets",
            put(|State(m): State<FakeManager>, AxPath(runtime): AxPath<String>, h: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                if !bearer_ok(&h) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                m.presets.lock().unwrap().push((runtime, body.clone()));
                let fails = body["extensions"].as_array().is_some_and(|a| a.iter().any(|e| e["id"] == "fail.ext"));
                if fails {
                    return (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({ "error": { "code": "extension_failed",
                        "message": "fail.ext was not installed", "data": { "id": "fail.ext", "output": "Extension 'fail.ext' not found." } } })))
                        .into_response();
                }
                axum::Json(body).into_response()
            }),
        )
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, m)
}

struct NodeFx {
    url: String,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

async fn start_node(source: ManagerSource) -> NodeFx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let node = Node::with_editor(NodeConfig::new(NODE_TOKEN, vec![root.clone()]), Arc::new(EditorRelays::new(source))).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(node_api::serve(listener, node));
    NodeFx { url, root, _dir: dir }
}

/// A node with its own fake manager.
async fn node_with_manager() -> (NodeFx, FakeManager) {
    let (url, m) = start_fake_manager().await;
    (start_node(ManagerSource::fixed(url, MANAGER_TOKEN)).await, m)
}

struct ServerFx {
    store: Arc<Store>,
    computers: Arc<Computers>,
    owner: Arc<Owner>,
    key: String,
    app: Router,
}

fn server() -> ServerFx {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let computers = Computers::new(Registry::new(store.clone()), computers::default_connector());
    // The local computer has no manager here, so nothing comes from this machine.
    let service = Arc::new(EditorSettingsService::new(
        store.clone(),
        computers.clone(),
        Arc::new(EditorRelays::new(ManagerSource::none())),
    ));
    // As in main: `Computers::register` applies at once (`ServerFx::register` inserts into the
    // registry directly, so the other tests start from a computer that was never applied).
    service.apply_on_registration();
    let owner = Owner::new(Presence::Enforce);
    let key = owner.grant(owner.next_claim(), Claimant { pid: 1, exe: "ember-app".into(), name: OWNER_NAME.into() });
    let app = es::router(service)
        .merge(ide::router_with_computers(sessions, Arc::new(IdeConfig::default()), computers.clone()));
    let app = owner::require(app, owner.clone());
    ServerFx { store, computers, owner, key, app }
}

async fn call(app: &Router, method: Method, uri: &str, body: Option<&Value>, key: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header("host", "ember.test:8740");
    if let Some(k) = key {
        req = req.header(owner::OWNER_HEADER, k);
    }
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

impl ServerFx {
    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        call(&self.app, Method::GET, uri, None, None).await
    }

    async fn put(&self, uri: &str, body: &Value) -> (StatusCode, Value) {
        call(&self.app, Method::PUT, uri, Some(body), Some(&self.key)).await
    }

    fn register(&self, name: &str, url: &str) -> String {
        self.computers.registry().insert(name, url, NODE_TOKEN).unwrap().id
    }
}

async fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "{what} after {WAIT:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn defaults() -> Value {
    json!({
        "editor_server": "code_ose", "script": null, "vscode_license_accepted": false,
        "vscode_license_accepted_by": null, "vscode_license_accepted_at": null,
        "extensions": [], "theme": "DarkPyonix Dark", "default_ide": "ember", "settings": {}
    })
}

/// FR-W7: a fresh server has the defaults of the row, and the local computer has no overrides.
#[tokio::test]
async fn fr_w7_defaults_hold_on_a_fresh_server() {
    let s = server();
    let (status, v) = s.get("/api/editor/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, defaults());
    let (status, v) = s.get("/api/computers/local/editor/settings").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["overrides"], json!({}));
    assert_eq!(v["effective"], defaults());
    assert_eq!(v["applied"], Value::Null);
}

/// FR-W7: the record round-trips through the API (and the store).
#[tokio::test]
async fn fr_w7_record_round_trips() {
    let s = server();
    let record = json!({
        "editor_server": "manual_script",
        "script": "#!/bin/sh\nexec code serve-web \"$@\"\n",
        "vscode_license_accepted": false,
        "vscode_license_accepted_by": null,
        "vscode_license_accepted_at": null,
        "extensions": [{ "id": "redhat.vscode-yaml", "version": "1.2.3" }, { "id": "ms-python.python" }],
        "theme": "Solarized Light",
        "default_ide": "gateway",
        "settings": { "editor.fontSize": 15, "files.autoSave": "afterDelay" }
    });
    let (status, saved) = s.put("/api/editor/settings", &record).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved, record);
    let (_, v) = s.get("/api/editor/settings").await;
    assert_eq!(v, record);
    assert_eq!(serde_json::to_value(es::load(&s.store).unwrap()).unwrap(), record);
    // The local computer follows it.
    let (_, v) = s.get("/api/computers/local/editor/settings").await;
    assert_eq!(v["effective"], record);
}

/// FR-W7 (FR-S12): without owner presence the record cannot be changed (403 `owner_required`)
/// and stays as it was; reading it needs no owner key. A computer's overrides are the same.
#[tokio::test]
async fn fr_w7_non_owner_cannot_change_the_record() {
    let s = server();
    let mut changed = defaults();
    changed["theme"] = json!("Hacked");
    for key in [None, Some("eok_0000"), Some("not-a-key")] {
        let (status, v) = call(&s.app, Method::PUT, "/api/editor/settings", Some(&changed), key).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{key:?}: {v}");
        assert_eq!(v["code"], "owner_required");
    }
    let id = s.register("box", "http://127.0.0.1:1");
    let (status, v) =
        call(&s.app, Method::PUT, &format!("/api/computers/{id}/editor/settings"), Some(&json!({ "theme": "Hacked" })), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (status, v) = s.get("/api/editor/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, defaults());
    assert_eq!(es::overrides(&s.store, &id).unwrap(), es::EditorOverrides::default());

    // A voided key is refused too.
    let claim = s.owner.next_claim();
    let key = s.owner.grant(claim, Claimant { pid: 2, exe: "x".into(), name: "x".into() });
    s.owner.void(claim);
    let (status, _) = call(&s.app, Method::PUT, "/api/editor/settings", Some(&changed), Some(&key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// FR-W7: a bad value is 400 and nothing is saved: an unknown enum value, a bad extension id or
/// version, `manual_script` without its script, `vscode` without the licence, an empty theme.
#[tokio::test]
async fn fr_w7_bad_values_are_400() {
    let s = server();
    let with = |k: &str, v: Value| {
        let mut r = defaults();
        r[k] = v;
        r
    };
    let mut no_script = defaults();
    no_script["editor_server"] = json!("manual_script");
    let mut no_licence = defaults();
    no_licence["editor_server"] = json!("vscode");
    for bad in [
        with("editor_server", json!("emacs")),
        with("default_ide", json!("cursor")),
        with("extensions", json!([{ "id": "../../etc/passwd" }])),
        with("extensions", json!([{ "id": "a.b", "version": "--pre" }])),
        with("extensions", json!("redhat.vscode-yaml")),
        with("theme", json!("")),
        with("settings", json!([1])),
        no_script,
        no_licence,
        json!([1, 2, 3]),
    ] {
        let (status, v) = s.put("/api/editor/settings", &bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {v}");
        assert!(v["error"].is_string(), "{v}");
    }
    assert_eq!(s.get("/api/editor/settings").await.1, defaults());

    let id = s.register("box", "http://127.0.0.1:1");
    for bad in [json!({ "editor_server": "vscode" }), json!({ "extensions": [{ "id": "nodot" }] }), json!({ "theme": "" })] {
        let (status, v) = s.put(&format!("/api/computers/{id}/editor/settings"), &bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
}

/// FR-W7 (leader, 2026-11-25): the server records who accepted the VS Code Server licence and
/// when, as it knows the caller (the owner key's claimant); it keeps them while the licence stays
/// accepted, clears them when it is withdrawn, and ignores values a client sends. A node's
/// override of the field is the same.
#[tokio::test]
async fn fr_w7_vscode_licence_records_who_accepted() {
    let s = server();
    let mut r = defaults();
    r["editor_server"] = json!("vscode");
    r["vscode_license_accepted"] = json!(true);
    r["vscode_license_accepted_by"] = json!("mallory");
    r["vscode_license_accepted_at"] = json!(1);
    let before = ember_server::store::now_ms();
    let (status, saved) = s.put("/api/editor/settings", &r).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["vscode_license_accepted_by"], OWNER_NAME);
    let at = saved["vscode_license_accepted_at"].as_i64().unwrap();
    assert!(at >= before, "{at} < {before}");
    assert_eq!(s.get("/api/editor/settings").await.1["vscode_license_accepted_by"], OWNER_NAME);

    // Saved again by another owner while it stays accepted: the first acceptance stays.
    let other = s.owner.grant(s.owner.next_claim(), Claimant { pid: 3, exe: "cli".into(), name: "ember-server owner".into() });
    r["theme"] = json!("Other Theme");
    r["vscode_license_accepted_by"] = json!(null);
    let (status, saved) = call(&s.app, Method::PUT, "/api/editor/settings", Some(&r), Some(&other)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["vscode_license_accepted_by"], OWNER_NAME);
    assert_eq!(saved["vscode_license_accepted_at"].as_i64(), Some(at));

    // Withdrawn: both cleared, whatever the client sends.
    let mut off = defaults();
    off["vscode_license_accepted_by"] = json!("mallory");
    off["vscode_license_accepted_at"] = json!(5);
    let (_, saved) = s.put("/api/editor/settings", &off).await;
    assert_eq!(saved["vscode_license_accepted"], false);
    assert_eq!(saved["vscode_license_accepted_by"], Value::Null);
    assert_eq!(saved["vscode_license_accepted_at"], Value::Null);

    // A node's override: set by the server, effective from the override.
    let id = s.register("box", "http://127.0.0.1:1");
    let uri = format!("/api/computers/{id}/editor/settings");
    let (status, v) = s
        .put(&uri, &json!({ "editor_server": "vscode", "vscode_license_accepted": true, "vscode_license_accepted_by": "mallory" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["overrides"]["vscode_license_accepted_by"], OWNER_NAME);
    assert!(v["overrides"]["vscode_license_accepted_at"].as_i64().unwrap() >= before);
    assert_eq!(v["effective"]["vscode_license_accepted_by"], OWNER_NAME);
    // The override dropped: the bookkeeping goes with it, and the effective record is the server's.
    let (status, v) = s.put(&uri, &json!({ "vscode_license_accepted": null, "vscode_license_accepted_by": "mallory" })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["overrides"], json!({}));
    assert_eq!(v["effective"]["vscode_license_accepted_by"], Value::Null);
}

/// FR-W7a: change a server field, and a node that inherits it applies it while a node that
/// overrides it does not; a field the overriding node inherits reaches it too.
#[tokio::test]
async fn fr_w7a_server_change_reaches_inheriting_nodes_only() {
    let (a, ma) = node_with_manager().await;
    let (b, mb) = node_with_manager().await;
    let s = server();
    let ida = s.register("a", &a.url);
    let idb = s.register("b", &b.url);

    // B overrides the theme: its PUT applies at once.
    let (status, v) = s.put(&format!("/api/computers/{idb}/editor/settings"), &json!({ "theme": "B Theme" })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(mb.themes(), ["B Theme"]);
    assert_eq!(v["applied"]["fields"]["theme"]["applied"], true, "{v}");
    assert!(ma.themes().is_empty(), "A was applied by B's override");

    // The server's theme changes: A applies it, B keeps its own and is not applied.
    let mut r = defaults();
    r["theme"] = json!("New Theme");
    let (status, _) = s.put("/api/editor/settings", &r).await;
    assert_eq!(status, StatusCode::OK);
    wait_for("A applied the new theme", || ma.themes() == ["New Theme"]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mb.themes(), ["B Theme"], "B overrides the theme and was applied anyway");
    let (_, v) = s.get(&format!("/api/computers/{ida}/editor/settings")).await;
    assert_eq!(v["effective"]["theme"], "New Theme");
    assert_eq!(v["reachable"], true);
    wait_for("A's report is kept", || es::applied(&s.store, &ida).unwrap().is_some()).await;

    // A field B inherits (the extensions): both apply it, B with its own theme.
    ma.clear();
    mb.clear();
    r["extensions"] = json!([{ "id": "redhat.vscode-yaml" }]);
    let (status, _) = s.put("/api/editor/settings", &r).await;
    assert_eq!(status, StatusCode::OK);
    wait_for("both applied the extensions", || ma.themes().len() == 1 && mb.themes().len() == 1).await;
    assert_eq!(mb.themes(), ["B Theme"]);
    let sent = mb.presets.lock().unwrap()[0].1.clone();
    assert_eq!(sent["extensions"], json!([{ "id": "redhat.vscode-yaml" }]));

    // Only the default IDE changes: it is Ember's alone, so no node is applied.
    ma.clear();
    mb.clear();
    r["default_ide"] = json!("vscode");
    let (status, _) = s.put("/api/editor/settings", &r).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(ma.themes().is_empty() && mb.themes().is_empty(), "a default_ide change was sent to a node");
}

/// FR-W7a: an override on one node changes only that node; dropping it (`null`) makes the node
/// inherit again.
#[tokio::test]
async fn fr_w7a_override_on_one_node_changes_only_that_node() {
    let (a, ma) = node_with_manager().await;
    let (b, mb) = node_with_manager().await;
    let s = server();
    let ida = s.register("a", &a.url);
    let idb = s.register("b", &b.url);

    let uri_a = format!("/api/computers/{ida}/editor/settings");
    let (status, v) = s.put(&uri_a, &json!({ "theme": "Only A", "settings": { "editor.tabSize": 2 } })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["overrides"], json!({ "theme": "Only A", "settings": { "editor.tabSize": 2 } }));
    assert_eq!(v["effective"]["theme"], "Only A");
    assert_eq!(ma.themes(), ["Only A"]);
    assert_eq!(ma.presets.lock().unwrap()[0].1["settings"], json!({ "editor.tabSize": 2 }));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(mb.themes().is_empty(), "B changed with A's override");
    let (_, v) = s.get(&format!("/api/computers/{idb}/editor/settings")).await;
    assert_eq!(v["overrides"], json!({}));
    assert_eq!(v["effective"]["theme"], "DarkPyonix Dark");
    assert_eq!(s.get("/api/editor/settings").await.1, defaults(), "the server record changed");

    // Inherit again.
    ma.clear();
    let (status, v) = s.put(&uri_a, &json!({ "theme": null, "settings": null })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["overrides"], json!({}));
    assert_eq!(ma.themes(), ["DarkPyonix Dark"]);
    assert!(mb.themes().is_empty());
}

/// FR-W7a: a failed apply shows on the computer's settings with the reason: an extension the
/// manager cannot install, a node with no manager, and a node that does not answer (which is also
/// `reachable: false`).
#[tokio::test]
async fn fr_w7a_failed_apply_shows_with_the_reason() {
    let (a, ma) = node_with_manager().await;
    let bare = start_node(ManagerSource::none()).await;
    let s = server();
    let ida = s.register("a", &a.url);
    let idbare = s.register("bare", &bare.url);
    let idgone = s.register("gone", "http://127.0.0.1:1");

    let uri = format!("/api/computers/{ida}/editor/settings");
    let (status, v) = s.put(&uri, &json!({ "extensions": [{ "id": "fail.ext" }] })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let fields = &v["applied"]["fields"];
    assert_eq!(fields["extensions"]["applied"], false, "{v}");
    let reason = fields["extensions"]["reason"].as_str().unwrap();
    assert!(reason.contains("fail.ext was not installed") && reason.contains("not found"), "{reason}");
    assert_eq!(fields["theme"]["applied"], true, "{v}");
    assert!(!ma.presets.lock().unwrap().is_empty());
    // The GET shows the same, from the node and from the store.
    let (_, v) = s.get(&uri).await;
    assert_eq!(v["applied"]["fields"]["extensions"]["reason"].as_str(), Some(reason));
    assert_eq!(v["reachable"], true);
    let kept = es::applied(&s.store, &ida).unwrap().unwrap();
    assert!(!kept.fields.extensions.applied);

    let (status, v) = s.put(&format!("/api/computers/{idbare}/editor/settings"), &json!({ "theme": "T" })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    for f in ["editor_server", "extensions", "theme", "settings"] {
        assert_eq!(v["applied"]["fields"][f]["applied"], false, "{v}");
        assert_eq!(v["applied"]["fields"][f]["reason"], NO_MANAGER, "{v}");
    }

    let (status, v) = s.put(&format!("/api/computers/{idgone}/editor/settings"), &json!({ "theme": "T" })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["reachable"], false, "{v}");
    assert_eq!(v["applied"]["fields"]["theme"]["applied"], false, "{v}");
    assert!(v["applied"]["fields"]["theme"]["reason"].as_str().unwrap().contains("노드에 연결할 수 없습니다"), "{v}");
    // The kept report stays on GET while the node does not answer.
    let (_, again) = s.get(&format!("/api/computers/{idgone}/editor/settings")).await;
    assert_eq!(again["applied"], v["applied"]);
    assert_eq!(again["reachable"], false);
}

/// FR-W7a: `local` has no overrides (PUT is 400), an unknown computer is 404, and removing a
/// computer removes its overrides and report.
#[tokio::test]
async fn fr_w7a_local_has_no_overrides_and_removal_drops_the_row() {
    let s = server();
    let (status, v) = s.put("/api/computers/local/editor/settings", &json!({ "theme": "T" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, _) = s.get("/api/computers/nope/editor/settings").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = s.put("/api/computers/nope/editor/settings", &json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let id = s.register("box", "http://127.0.0.1:1");
    let (status, _) = s.put(&format!("/api/computers/{id}/editor/settings"), &json!({ "default_ide": "gateway" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(es::applied(&s.store, &id).unwrap().is_some());
    assert_eq!(es::effective_default_ide(&s.store, &id), es::DefaultIde::Gateway);
    s.computers.remove(&id).unwrap();
    assert_eq!(es::overrides(&s.store, &id).unwrap(), es::EditorOverrides::default());
    assert!(es::applied(&s.store, &id).unwrap().is_none());
}

/// FR-W7b (server part): the IDE listing carries the computer's effective `default_ide`: the
/// node's override, else the server's (the local computer and any other node follow the server).
#[tokio::test]
async fn fr_w7b_listing_default_ide_is_the_node_override_else_the_server() {
    let (a, _ma) = node_with_manager().await;
    let (b, _mb) = node_with_manager().await;
    let s = server();
    let ida = s.register("a", &a.url);
    let idb = s.register("b", &b.url);
    let sid = s.store.create_session("proj", AgentKind::Scripted, &a.root.to_string_lossy(), None, "t").unwrap().id;

    // A fresh server: every computer defaults to the Ember editor.
    let (status, v) = s.get(&format!("/api/sessions/{sid}/ide?computer={ida}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["default_ide"], "ember");

    let mut r = defaults();
    r["default_ide"] = json!("vscode");
    assert_eq!(s.put("/api/editor/settings", &r).await.0, StatusCode::OK);
    let (status, _) = s.put(&format!("/api/computers/{ida}/editor/settings"), &json!({ "default_ide": "ember" })).await;
    assert_eq!(status, StatusCode::OK);

    let listing = |c: String| {
        let s = &s;
        let sid = sid.clone();
        async move {
            let (status, v) = s.get(&format!("/api/sessions/{sid}/ide?computer={c}")).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            v
        }
    };
    let va = listing(ida.clone()).await;
    assert_eq!(va["default_ide"], "ember", "{va}");
    assert_eq!(va["computer"]["name"], ida.as_str());
    assert_eq!(listing(idb.clone()).await["default_ide"], "vscode");
    assert_eq!(listing("local".into()).await["default_ide"], "vscode");
    // A listing by the computer's name resolves to the same computer.
    assert_eq!(listing("a".into()).await["default_ide"], "ember");
}

/// FR-W7a: a node gets its settings when it is enrolled. Registering it (the path the API and the
/// enrol registrar share) applies the effective record in the background, with no settings save:
/// the node's own `GET /editor/settings` then shows the server's effective record and a report,
/// and the server keeps the report.
#[tokio::test]
async fn fr_w7a_an_enrolled_node_gets_the_effective_settings() {
    let (a, ma) = node_with_manager().await;
    let s = server();
    // A server record saved before the node exists (its own apply reaches no node).
    let mut r = defaults();
    r["theme"] = json!("Enrol Theme");
    r["extensions"] = json!([{ "id": "redhat.vscode-yaml" }]);
    assert_eq!(s.put("/api/editor/settings", &r).await.0, StatusCode::OK);
    assert!(ma.themes().is_empty());

    let c = s.computers.register("fresh", &a.url, NODE_TOKEN).unwrap();
    wait_for("the enrolled node applied its settings", || ma.themes() == ["Enrol Theme"]).await;
    assert_eq!(ma.presets.lock().unwrap()[0].1["extensions"], json!([{ "id": "redhat.vscode-yaml" }]));

    let node = ember_node::client::NodeClient::new(&a.url, NODE_TOKEN).unwrap();
    let deadline = Instant::now() + WAIT;
    let state = loop {
        let state = node.editor_settings().await.unwrap();
        if state.applied.is_some() {
            break state;
        }
        assert!(Instant::now() < deadline, "the node kept no report");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let expected = es::node_settings(&es::effective_for(&s.store, &c.id).unwrap());
    assert_eq!(state.settings.as_ref(), Some(&expected));
    assert_eq!(expected.theme.as_deref(), Some("Enrol Theme"));
    let report = state.applied.unwrap();
    assert!(report.all_applied(), "{report:?}");

    wait_for("the server kept the report", || es::applied(&s.store, &c.id).unwrap().is_some()).await;
    let (_, v) = s.get(&format!("/api/computers/{}/editor/settings", c.id)).await;
    assert_eq!(v["effective"]["theme"], "Enrol Theme");
    assert_eq!(v["applied"]["fields"]["extensions"]["applied"], true, "{v}");
    assert_eq!(v["reachable"], true);
}
