//! "Open IDE" computers come from the computers registry and the session's current computer
//! (FR-L7, FR-W4; the issue is #454). `EMBER_IDE_COMPUTERS` stays as an override. These ids are
//! the closest SPEC rows for the IDE launch: FR-W5i does not exist, so none was invented.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::api::ide::{self, IdeConfig};
use ember_server::computers::{default_connector, Computers, Registry};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Fx {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    session: String,
    node_id: String,
}

/// A session, a registry with one node named "studio", and the registry's local computer.
fn fx() -> Fx {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(store.clone(), adapters);
    let session = sessions
        .create(NewSession {
            project: "acme".into(),
            agent: AgentKind::Scripted,
            cwd: "/home/me/acme".into(),
            model: None,
            title: "t".into(),
        })
        .unwrap()
        .id;
    let registry = Registry::new(store);
    let node = registry.insert("studio", "http://studio.local:8741", "tok").unwrap();
    let computers = Computers::new(registry, default_connector());
    Fx { sessions, computers, session, node_id: node.id }
}

fn cfg(json: &str) -> IdeConfig {
    IdeConfig::parse(json).unwrap()
}

async fn get_with(f: &Fx, cfg: IdeConfig, registry: bool, query: &str) -> (StatusCode, Value) {
    let app = if registry {
        ide::router_with_computers(f.sessions.clone(), Arc::new(cfg), f.computers.clone())
    } else {
        ide::router(f.sessions.clone(), Arc::new(cfg))
    };
    let uri = format!("/api/sessions/{}/ide{query}", f.session);
    let resp = app
        .oneshot(Request::get(uri).header("host", "ember.test:8740").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

fn target<'a>(v: &'a Value, kind: &str) -> &'a Value {
    v["targets"].as_array().unwrap().iter().find(|t| t["kind"] == kind).unwrap()
}

const NO_ENV: &str = r#"{"default_computer":"local","computers":{"local":{"local":true}}}"#;

#[tokio::test]
async fn fr_l7_fr_w4_registry_computer_is_a_target_with_the_registry_id() {
    let f = fx();
    let (status, v) = get_with(&f, cfg(NO_ENV), true, &format!("?computer={}", f.node_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["computer"]["name"], f.node_id.as_str());
    assert_eq!(v["computer"]["computer_name"], "studio");
    assert_eq!(v["computer"]["local"], false);
    let ember = target(&v, "ember");
    assert_eq!(ember["launch"]["computer"], f.node_id.as_str());
    assert_eq!(ember["launch"]["computer_name"], "studio");
}

#[tokio::test]
async fn fr_l7_fr_w4_session_current_computer_is_the_default() {
    let f = fx();
    // Never switched: the config default (local).
    let (_, v) = get_with(&f, cfg(NO_ENV), true, "").await;
    assert_eq!(v["computer"]["name"], "local");
    assert_eq!(v["computer"]["local"], true);
    // Switched: the session's computer wins over the config default.
    f.computers.registry().set_session_computer(&f.session, &f.node_id, None, None).unwrap();
    let (_, v) = get_with(&f, cfg(NO_ENV), true, "").await;
    assert_eq!(v["computer"]["name"], f.node_id.as_str());
    assert_eq!(target(&v, "ember")["launch"]["computer"], f.node_id.as_str());
}

#[tokio::test]
async fn fr_l7_fr_w4_env_override_merges_onto_a_registry_computer_by_name() {
    let f = fx();
    let c = cfg(
        r#"{"default_computer":"local","computers":{"local":{"local":true},
            "studio":{"ide_url":"http://studio.local:8890","ssh_host":"studio.local","os":"linux"}}}"#,
    );
    let (status, v) = get_with(&f, c, true, "?computer=studio").await;
    assert_eq!(status, StatusCode::OK);
    // The id is the registry id, the IDE details are the env entry's.
    assert_eq!(v["computer"]["name"], f.node_id.as_str());
    assert_eq!(target(&v, "vscode")["url"], "http://studio.local:8890/?folder=/home/me/acme");
    assert_eq!(target(&v, "gateway")["available"], true);
    assert_eq!(target(&v, "ember")["launch"]["computer"], f.node_id.as_str());
}

#[tokio::test]
async fn fr_l7_fr_w4_env_name_without_a_registry_match_keeps_working() {
    let f = fx();
    let c = cfg(
        r#"{"default_computer":"local","computers":{"local":{"local":true},
            "lab":{"ide_url":"http://lab:8890"}}}"#,
    );
    let (status, v) = get_with(&f, c, true, "?computer=lab").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["computer"]["name"], "lab");
    assert_eq!(target(&v, "vscode")["url"], "http://lab:8890/?folder=/home/me/acme");
}

#[tokio::test]
async fn fr_l7_fr_w4_query_by_id_name_and_env_name_resolve_the_same() {
    let f = fx();
    let c = r#"{"default_computer":"local","computers":{"local":{"local":true},
        "studio":{"ide_url":"http://studio.local:8890"}}}"#;
    let mut seen = Vec::new();
    for q in [f.node_id.clone(), "studio".to_string()] {
        let (status, v) = get_with(&f, cfg(c), true, &format!("?computer={q}")).await;
        assert_eq!(status, StatusCode::OK, "{q}");
        seen.push(v);
    }
    assert_eq!(seen[0], seen[1]);
    // An env name that differs from the registry name reaches the same computer through its alias.
    let aliased = r#"{"default_computer":"local","computers":{"local":{"local":true}}}"#;
    let (_, by_id) = get_with(&f, cfg(aliased), true, &format!("?computer={}", f.node_id)).await;
    let (_, by_name) = get_with(&f, cfg(aliased), true, "?computer=studio").await;
    assert_eq!(by_id, by_name);
}

#[tokio::test]
async fn fr_l7_fr_w4_unknown_computer_is_404() {
    let f = fx();
    let (status, v) = get_with(&f, cfg(NO_ENV), true, "?computer=ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(v["error"].as_str().unwrap().contains("ghost"));
}

#[tokio::test]
async fn fr_l7_fr_w4_node_computer_ember_target_stays_unavailable() {
    let f = fx();
    let (_, v) = get_with(&f, cfg(NO_ENV), true, &format!("?computer={}", f.node_id)).await;
    let ember = target(&v, "ember");
    assert_eq!(ember["available"], false);
    assert_eq!(ember["reason"], "OSE server is only available on the local computer");
    assert!(ember["launch"].get("ose").is_none());
}

#[tokio::test]
async fn fr_l7_fr_w4_env_only_configuration_yields_the_old_json() {
    let f = fx();
    let c = r#"{"default_computer":"mini","server_url":"http://mini.local:8740","computers":{
        "mini":{"local":true,"ide_url":"http://127.0.0.1:8890/","os":"macos"}}}"#;
    let (status, v) = get_with(&f, cfg(c), false, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["computer"], json!({"name": "mini", "local": true}));
    let launch = &target(&v, "ember")["launch"];
    assert_eq!(launch.get("computer_name"), None);
    assert_eq!(launch["computer"], "mini");
    let mut keys: std::collections::BTreeSet<String> = launch.as_object().unwrap().keys().cloned().collect();
    keys.remove("ose"); // present only when an OSE binary happens to be installed
    assert_eq!(
        keys,
        ["computer", "folder", "project", "server_url", "session"]
            .iter().map(|s| s.to_string()).collect()
    );
}
