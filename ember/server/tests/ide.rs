//! "Open IDE" launch targets: FR-L7 (`GET /api/sessions/{id}/ide`).
//!
//! The VS Code target is a launch through the server's relay like the Ember target (D55, #1064):
//! `ide_url` is still accepted in `EMBER_IDE_COMPUTERS` but no longer makes a URL.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::api::ide::{self, ComputerIde, IdeConfig};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

fn setup(cwd: &str) -> (Arc<Sessions>, String) {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let s = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), adapters);
    let id = s
        .create(NewSession {
            project: "acme".into(),
            agent: AgentKind::Scripted,
            cwd: cwd.into(),
            model: None,
            title: "t".into(),
        })
        .unwrap()
        .id;
    (s, id)
}

fn config() -> IdeConfig {
    IdeConfig::parse(
        r#"{
          "default_computer": "mini",
          "server_url": "http://mini.local:8740",
          "computers": {
            "mini":   { "local": true, "ide_url": "http://127.0.0.1:8890/", "os": "macos" },
            "studio": { "ide_url": "http://studio.local:8890", "ssh_host": "studio.local",
                        "ssh_user": "me", "os": "linux" },
            "bare":   {}
          }
        }"#,
    )
    .unwrap()
}

async fn get(s: Arc<Sessions>, cfg: IdeConfig, uri: &str) -> (StatusCode, Value) {
    let app = ide::router(s, Arc::new(cfg));
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

#[tokio::test]
async fn default_computer_targets() {
    let (s, id) = setup("/Users/me/acme app");
    let (status, v) = get(s, config(), &format!("/api/sessions/{id}/ide")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["project"], "acme");
    assert_eq!(v["folder"], "/Users/me/acme app");
    assert_eq!(v["computer"]["name"], "mini");
    assert_eq!(v["computer"]["local"], true);

    let kinds: Vec<&str> =
        v["targets"].as_array().unwrap().iter().map(|t| t["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["ember", "vscode", "gateway"]);

    // Ember: launch info only, never a URL.
    let ember = target(&v, "ember");
    assert!(ember.get("url").is_none());
    assert_eq!(ember["launch"]["session"], id.as_str());
    assert_eq!(ember["launch"]["project"], "acme");
    assert_eq!(ember["launch"]["computer"], "mini");
    assert_eq!(ember["launch"]["folder"], "/Users/me/acme app");
    assert_eq!(ember["launch"]["server_url"], "http://mini.local:8740");

    // VS Code (D55, #1064): the kind alone, never a URL, even with an `ide_url` configured. It
    // starts only when picked (`POST .../ide/vscode`), so the listing says nothing more.
    assert_eq!(*target(&v, "vscode"), serde_json::json!({ "kind": "vscode" }));

    // No SSH configured for this computer → Gateway unavailable, with a reason.
    let gw = target(&v, "gateway");
    assert_eq!(gw["available"], false);
    assert!(gw["reason"].as_str().unwrap().contains("ssh_host"));
}

#[tokio::test]
async fn remote_computer_by_query() {
    let (s, id) = setup("/home/me/acme");
    let (status, v) = get(s, config(), &format!("/api/sessions/{id}/ide?computer=studio")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["computer"]["name"], "studio");
    assert_eq!(v["computer"]["local"], false);
    // D55: no URL from `ide_url`; VS Code is listed as the kind alone, and picking it on a
    // computer with no node and not local is the pick's error.
    assert_eq!(*target(&v, "vscode"), serde_json::json!({ "kind": "vscode" }));

    let gw = target(&v, "gateway");
    let url = "jetbrains-gateway://connect#type=ssh&host=studio.local&port=22&user=me\
               &projectPath=%2Fhome%2Fme%2Facme";
    assert_eq!(gw["available"], true);
    assert_eq!(gw["url"], url);
    assert_eq!(gw["command"], serde_json::json!(["xdg-open", url]));
}

#[tokio::test]
async fn unconfigured_computer_and_host_fallback() {
    let (s, id) = setup("/p");
    let mut cfg = config();
    cfg.server_url = None;
    let (status, v) = get(s, cfg, &format!("/api/sessions/{id}/ide?computer=bare")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(*target(&v, "vscode"), serde_json::json!({ "kind": "vscode" }));
    assert_eq!(target(&v, "ember")["launch"]["server_url"], "http://ember.test:8740");
}

#[tokio::test]
async fn unknown_session_and_computer_are_404() {
    let (s, id) = setup("/p");
    let (status, _) = get(s.clone(), config(), "/api/sessions/nope/ide").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, v) = get(s, config(), &format!("/api/sessions/{id}/ide?computer=ghost")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(v["error"].as_str().unwrap().contains("ghost"));
}

async fn post(s: Arc<Sessions>, cfg: IdeConfig, uri: &str) -> (StatusCode, Value) {
    let app = ide::router(s, Arc::new(cfg));
    let resp = app
        .oneshot(Request::post(uri).header("host", "ember.test:8740").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

/// FR-W4c (1c) (D55, #1064): picking VS Code on a computer known only from
/// `EMBER_IDE_COMPUTERS` and not local is an error with a code (no OSE to relay to); an unknown
/// session or computer is 404 with a code, the same resolution as the listing.
#[tokio::test]
async fn fr_w4c_vscode_pick_errors_carry_a_code() {
    let (s, id) = setup("/home/me/acme");
    let (status, v) = post(s.clone(), config(), &format!("/api/sessions/{id}/ide/vscode?computer=studio")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], ide::VSCODE_UNAVAILABLE);
    assert_eq!(v["error"], "OSE server is only available on the local computer");

    let (status, v) = post(s.clone(), config(), "/api/sessions/nope/ide/vscode").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["code"], ide::SESSION_NOT_FOUND);
    let (status, v) = post(s, config(), &format!("/api/sessions/{id}/ide/vscode?computer=ghost")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["code"], ide::UNKNOWN_COMPUTER);
    assert!(v["error"].as_str().unwrap().contains("ghost"));
}

#[test]
fn default_config_is_one_local_computer() {
    let cfg = IdeConfig::default();
    assert_eq!(cfg.default_computer, "local");
    assert_eq!(cfg.computers["local"], ComputerIde { local: true, ..Default::default() });
}
