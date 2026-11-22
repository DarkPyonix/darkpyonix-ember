//! FR-W4 acceptance step 6 (#83): OSE install layout and discovery through
//! `GET /api/sessions/{id}/ide`.
//!
//! Uses `OseManager::with_data_root` and `ide::router_with_ose` directly so no test mutates
//! process-wide environment variables.
#![cfg(unix)]

mod common;

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::api::ide::{self, IdeConfig};
use ember_server::ose::{launcher_in, OseManager, NOT_INSTALLED};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const FAKE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn setup_router(folder: &Path, ose: Arc<OseManager>) -> (Router, String) {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), adapters);
    let session_id = sessions
        .create(NewSession {
            project: "acme".into(),
            agent: AgentKind::Scripted,
            cwd: folder.to_path_buf(),
            model: None,
            title: "t".into(),
        })
        .unwrap()
        .id;
    let app = ide::router_with_ose(sessions, Arc::new(IdeConfig::default()), ose);
    (app, session_id)
}

async fn get_ide(app: &Router, session_id: &str) -> (StatusCode, Value) {
    let uri = format!("/api/sessions/{session_id}/ide");
    let resp = app
        .clone()
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
async fn fr_w4_ide_reports_unavailable_with_install_hint_when_ose_missing() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("project");
    std::fs::create_dir(&folder).unwrap();

    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let (app, session_id) = setup_router(&folder, ose);

    let (status, body) = get_ide(&app, &session_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ember = target(&body, "ember");
    assert_eq!(ember["available"], false, "{ember}");
    assert!(ember["launch"].get("ose").is_none(), "{ember}");

    let reason = ember["reason"].as_str().expect("missing reason on unavailable ember target");
    assert_eq!(reason, NOT_INSTALLED);
    assert!(reason.contains("install-ose.sh"), "{reason}");
    assert!(reason.contains("~/.local/share"), "{reason}");
    assert!(reason.contains("EMBER_OSE_SERVER"), "{reason}");
}

#[tokio::test]
async fn fr_w4_ide_reports_unavailable_when_configured_ose_path_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("project");
    std::fs::create_dir(&folder).unwrap();

    let install_root = dir.path().join("home/.local/share/dpx-ose-1.139.1");
    let missing_launcher = launcher_in(&install_root, false);
    assert!(!missing_launcher.exists());

    let ose = Arc::new(OseManager::with_data_root(Some(missing_launcher), dir.path().join("ose-data")));
    let (app, session_id) = setup_router(&folder, ose);

    let (status, body) = get_ide(&app, &session_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ember = target(&body, "ember");
    assert_eq!(ember["available"], false, "{ember}");
    assert!(ember["launch"].get("ose").is_none(), "{ember}");

    let reason = ember["reason"].as_str().expect("missing reason on unavailable ember target");
    assert!(reason.contains("install-ose.sh"), "{reason}");
    assert!(reason.contains("EMBER_OSE_SERVER"), "{reason}");
}

#[tokio::test]
async fn fr_w4_ide_reports_available_when_ose_installed_in_install_layout() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("project");
    std::fs::create_dir(&folder).unwrap();

    // Match the install-ose.sh layout: <home>/.local/share/dpx-ose-1.139.1/bin/dpx-ose-server
    // with product.json beside bin/.
    let install_root = dir.path().join("home/.local/share/dpx-ose-1.139.1");
    let bin_dir = install_root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(
        install_root.join("product.json"),
        format!(r#"{{"commit":"{FAKE_COMMIT}","nameShort":"DarkPyonix OSE"}}"#),
    )
    .unwrap();

    let launcher = bin_dir.join("dpx-ose-server");
    assert_eq!(launcher_in(&install_root, false), launcher);
    common::write_exec_ready(
        &launcher,
        r##"#!/bin/sh
D="$(dirname "$0")"
exec python3 -c 'import socket,sys
d=sys.argv[1]; a=sys.argv[2:]
f=a[a.index("--connection-token-file")+1]
open(d+"/real-token.txt","w").write(open(f).read())
p=int(a[a.index("--port")+1]); s=socket.socket(); s.bind(("127.0.0.1",p)); s.listen()
while True: s.accept()[0].close()' "$D" "$@"
"##,
    );

    let ose = Arc::new(OseManager::with_data_root(Some(launcher), dir.path().join("ose-data")));
    let (app, session_id) = setup_router(&folder, ose);

    let (status, first) = get_ide(&app, &session_id).await;
    assert_eq!(status, StatusCode::OK, "{first}");

    let ember = target(&first, "ember");
    assert_eq!(ember["available"], true, "{ember}");
    assert!(ember.get("reason").is_none(), "{ember}");

    let ose_json = &ember["launch"]["ose"];
    let endpoint = ose_json["endpoint"].as_str().expect("missing ose.endpoint");
    let relay = ose_json["relay"].as_str().expect("missing ose.relay");
    let placeholder = ose_json["connection_token"].as_str().expect("missing ose.connection_token");
    assert!(endpoint.starts_with("http://127.0.0.1:"), "{endpoint}");
    assert!(relay.starts_with("/api/computers/local/ide/relay/"), "{relay}");
    assert_eq!(placeholder.len(), 64, "{placeholder}");
    assert_eq!(ose_json["ose_commit"], FAKE_COMMIT, "{ose_json}");

    let real_token = std::fs::read_to_string(bin_dir.join("real-token.txt")).unwrap();
    assert_eq!(real_token.len(), 64);
    assert_ne!(placeholder, real_token, "IDE JSON must expose the placeholder, never the real token");

    let (status2, second) = get_ide(&app, &session_id).await;
    assert_eq!(status2, StatusCode::OK, "{second}");
    assert_eq!(target(&second, "ember")["launch"]["ose"], *ose_json);
}
