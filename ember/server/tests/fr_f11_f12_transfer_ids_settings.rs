//! Offer ids across restarts, stale partial files (`FR-F11`) and the received-folder setting
//! check (`FR-F12`), #773.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use ember_server::devices::Devices;
use ember_server::store::Store;
use ember_server::transfers::{self, TransferManager};
use ember_transport::{PeerId, SecretKey};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call_json(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(match body {
            Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn app_over(dir: &std::path::Path, peer: PeerId) -> axum::Router {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store).unwrap();
    devices.add(peer, "Phone").unwrap();
    transfers::router(TransferManager::new(dir, Some(devices), None))
}

async fn offer_and_accept(app: &axum::Router, peer: PeerId) -> (String, Value) {
    let (st, created) = call_json(
        app,
        "POST",
        "/api/transfers/offers",
        Some(json!({ "from": "Phone", "peer_id": peer.to_string(), "files": [{ "name": "a.txt", "size": 20 }] })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let (st, tr) = call_json(app, "POST", &format!("/api/transfers/offers/{id}/accept"), None).await;
    assert_eq!(st, StatusCode::OK, "{tr}");
    (id, tr)
}

#[tokio::test]
async fn fr_f11_offer_after_restart_does_not_adopt_stale_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let peer = SecretKey::generate().peer_id();

    let app1 = app_over(dir.path(), peer);
    let (id1, _) = offer_and_accept(&app1, peer).await;
    let data = base64::engine::general_purpose::STANDARD.encode(b"0123456789");
    let (st, tr) = call_json(
        &app1,
        "POST",
        &format!("/api/transfers/{id1}/chunks"),
        Some(json!({ "file": 0, "offset": 0, "data_base64": data })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{tr}");
    assert_eq!(tr["done"], 10);
    drop(app1);

    // Restart over the same data folder.
    let app2 = app_over(dir.path(), peer);
    let partial = dir.path().join("received/.partial");
    let leftover = std::fs::read_dir(&partial).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftover, 0, "stale partial folders are swept at start");

    let (id2, tr2) = offer_and_accept(&app2, peer).await;
    assert_ne!(id1, id2, "offer ids must not repeat across restarts");
    assert_eq!(tr2["done"], 0, "a new offer must start at 0, not adopt an old part file");
}

#[tokio::test]
async fn fr_f11_offer_ids_are_128_bit_random_hex() {
    let dir = tempfile::tempdir().unwrap();
    let peer = SecretKey::generate().peer_id();
    let app = app_over(dir.path(), peer);
    let (a, _) = offer_and_accept(&app, peer).await;
    let (b, _) = offer_and_accept(&app, peer).await;
    assert_ne!(a, b);
    for id in [&a, &b] {
        let hex = id.strip_prefix("tr-").expect("tr- prefix");
        assert_eq!(hex.len(), 32, "{id}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }
}

async fn put_dir(app: &axum::Router, dir: &str) -> StatusCode {
    call_json(app, "PUT", "/api/transfers/settings", Some(json!({ "received_dir": dir }))).await.0
}

async fn current_dir(app: &axum::Router) -> String {
    let (_, snap) = call_json(app, "GET", "/api/transfers", None).await;
    snap["settings"]["received_dir"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn fr_f12_received_dir_setting_refuses_unsafe_paths() {
    let data = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let peer = SecretKey::generate().peer_id();
    let app = app_over(data.path(), peer);
    let before = current_dir(&app).await;

    let token = data.path().join("server-token");
    std::fs::write(&token, b"secret").unwrap();
    let a_file = outside.path().join("file.txt");
    std::fs::write(&a_file, b"x").unwrap();
    let link = outside.path().join("link-to-data");
    #[cfg(unix)]
    std::os::unix::fs::symlink(data.path(), &link).unwrap();

    let mut refused: Vec<String> = vec![
        "relative/dir".into(),
        data.path().display().to_string(),
        token.display().to_string(),
        data.path().join("sub").display().to_string(),
        data.path().join("received/../sub").display().to_string(),
        data.path().parent().unwrap().display().to_string(),
        a_file.display().to_string(),
    ];
    if cfg!(unix) {
        refused.push(link.display().to_string());
    }
    for p in &refused {
        assert_eq!(put_dir(&app, p).await, StatusCode::BAD_REQUEST, "{p} must be refused");
        assert_eq!(current_dir(&app).await, before, "a refused {p} leaves the setting unchanged");
    }

    let ok_default = data.path().join("received").display().to_string();
    assert_eq!(put_dir(&app, &ok_default).await, StatusCode::OK);
    let ok_new = outside.path().join("Ember Received");
    assert_eq!(put_dir(&app, &ok_new.display().to_string()).await, StatusCode::OK);
    assert!(ok_new.is_dir(), "an accepted folder is created");
}
