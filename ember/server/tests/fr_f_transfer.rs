//! Integration tests for server-side file transfer API (`FR-F1`..`FR-F10`, `#487`, `#527`).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use ember_server::devices::Devices;
use ember_server::store::Store;
use ember_server::transfers::{self, TransferManager};
use ember_transport::SecretKey;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

async fn call_json(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
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
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

fn hex_sha256(data: &[u8]) -> String {
    data.iter()
        .fold(Sha256::new(), |mut h, b| {
            h.update([*b]);
            h
        })
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn fr_f3_f5_f8_f9_offer_accept_chunk_resume_collision_and_mode_0644() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store).unwrap();
    let peer = SecretKey::generate().peer_id();
    devices.add(peer, "BREW iPhone").unwrap();

    let mgr = TransferManager::new(dir.path(), Some(devices), None);
    let app = transfers::router(mgr);

    // 1. Offer a file with path traversal and executable name (`../.run.sh`).
    let payload = b"#!/bin/sh\necho hello\n";
    let sha = hex_sha256(payload);
    let (st, created) = call_json(
        &app,
        "POST",
        "/api/transfers/offers",
        Some(json!({
            "from": "BREW iPhone",
            "peer_id": peer.to_string(),
            "files": [{ "name": "../.run.sh", "size": payload.len(), "sha256": sha }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(created["status"], "offered");
    let offer_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(
        created["offer"]["cleaned_names"],
        json!(["run.sh"]),
        "prompt and offer must expose only cleaned names"
    );
    assert!(
        !dir.path().join("received/run.sh").exists(),
        "nothing written before accept"
    );

    // 2. Accept the offer.
    let (st, accepted) = call_json(
        &app,
        "POST",
        &format!("/api/transfers/offers/{offer_id}/accept"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(accepted["state"], "sending");

    // 3. Stream first 10 bytes, verify resume offset is 10, pause/resume, then stream remaining bytes.
    let b64_part1 = base64::engine::general_purpose::STANDARD.encode(&payload[..10]);
    let (st, tr) = call_json(
        &app,
        "POST",
        &format!("/api/transfers/{offer_id}/chunks"),
        Some(json!({ "file": 0, "offset": 0, "data_base64": b64_part1 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(tr["done"], 10);

    let (st, off) = call_json(
        &app,
        "GET",
        &format!("/api/transfers/{offer_id}/resume-offset/0"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(off["offset"], 10);

    // Pause blocks further chunks until resumed (FR-F4).
    let (st, _) = call_json(&app, "POST", &format!("/api/transfers/{offer_id}/pause"), None).await;
    assert_eq!(st, StatusCode::OK);
    let b64_part2 = base64::engine::general_purpose::STANDARD.encode(&payload[10..]);
    let (st, _) = call_json(
        &app,
        "POST",
        &format!("/api/transfers/{offer_id}/chunks"),
        Some(json!({ "file": 0, "offset": 10, "data_base64": b64_part2.clone() })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    let (st, _) = call_json(&app, "POST", &format!("/api/transfers/{offer_id}/resume"), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_json(
        &app,
        "POST",
        &format!("/api/transfers/{offer_id}/chunks"),
        Some(json!({
            "file": 0,
            "offset": 10,
            "data_base64": b64_part2,
            "sha256": hex_sha256(&payload[10..])
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 4. Complete transfer: file saved in `received/run.sh` with mode 0644 (exec bit stripped, FR-F9).
    let (st, done) = call_json(
        &app,
        "POST",
        &format!("/api/transfers/{offer_id}/complete"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(done["stored"][0]["name"], "run.sh");
    let saved_path = dir.path().join("received/run.sh");
    assert_eq!(std::fs::read(&saved_path).unwrap(), payload);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&saved_path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o644);
    }

    // 5. Receiving the same filename a second time never overwrites: saves as `run (1).sh` (FR-F8).
    let (_, created2) = call_json(
        &app,
        "POST",
        "/api/transfers/offers",
        Some(json!({
            "from": "BREW iPhone",
            "peer_id": peer.to_string(),
            "files": [{ "name": "run.sh", "size": 4 }]
        })),
    )
    .await;
    let id2 = created2["id"].as_str().unwrap();
    call_json(&app, "POST", &format!("/api/transfers/offers/{id2}/accept"), None).await;
    call_json(
        &app,
        "POST",
        &format!("/api/transfers/{id2}/chunks"),
        Some(json!({
            "file": 0,
            "offset": 0,
            "data_base64": base64::engine::general_purpose::STANDARD.encode(b"data")
        })),
    )
    .await;
    let (st, done2) = call_json(&app, "POST", &format!("/api/transfers/{id2}/complete"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(done2["stored"][0]["name"], "run (1).sh");
    assert_eq!(std::fs::read(&saved_path).unwrap(), payload, "first file untouched");
    assert_eq!(std::fs::read(dir.path().join("received/run (1).sh")).unwrap(), b"data");

    // 6. Clearing history removes history entries without deleting received files on disk (FR-F8).
    let (_, snap_before) = call_json(&app, "GET", "/api/transfers", None).await;
    assert_eq!(snap_before["history"].as_array().unwrap().len(), 2);
    let (st, _) = call_json(&app, "DELETE", "/api/transfers/history", None).await;
    assert_eq!(st, StatusCode::OK);
    let (_, snap_after) = call_json(&app, "GET", "/api/transfers", None).await;
    assert!(snap_after["history"].as_array().unwrap().is_empty());
    assert!(saved_path.exists());
    assert!(dir.path().join("received/run (1).sh").exists());
}

#[tokio::test]
async fn fr_f4_f9_f10_limits_unregistered_peer_receive_off_and_cancel_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store).unwrap();
    let registered_peer = SecretKey::generate().peer_id();
    let stranger_peer = SecretKey::generate().peer_id();
    devices.add(registered_peer, "MacBook").unwrap();

    let mgr = TransferManager::new(dir.path(), Some(devices), None);
    let app = transfers::router(mgr);

    // 1. Default limit is 4 GiB; an offer above 4 GiB is refused with 413 Payload Too Large (FR-F10).
    let (_, snap) = call_json(&app, "GET", "/api/transfers", None).await;
    assert_eq!(snap["settings"]["max_bytes"], 4u64 * 1024 * 1024 * 1024);

    let (st, err) = call_json(
        &app,
        "POST",
        "/api/transfers/offers",
        Some(json!({
            "from": "MacBook",
            "peer_id": registered_peer.to_string(),
            "files": [
                { "name": "part1.bin", "size": 3u64 * 1024 * 1024 * 1024 },
                { "name": "part2.bin", "size": 2u64 * 1024 * 1024 * 1024 }
            ]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(err["error"].as_str().unwrap().contains("TooLarge"));

    // 2. Unregistered peer is refused with 403 Forbidden (FR-F9, FR-F10).
    let (st, err) = call_json(
        &app,
        "POST",
        "/api/transfers/offers",
        Some(json!({
            "from": "Stranger",
            "peer_id": stranger_peer.to_string(),
            "files": [{ "name": "a.txt", "size": 5 }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(err["error"].as_str().unwrap().contains("NotRegistered"));

    // 3. Cancel removes partial files and records Cancelled in history (FR-F4).
    let (_, created) = call_json(
        &app,
        "POST",
        "/api/transfers/offers",
        Some(json!({
            "from": "MacBook",
            "peer_id": registered_peer.to_string(),
            "files": [{ "name": "big.bin", "size": 100 }]
        })),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    call_json(&app, "POST", &format!("/api/transfers/offers/{id}/accept"), None).await;
    call_json(
        &app,
        "POST",
        &format!("/api/transfers/{id}/chunks"),
        Some(json!({
            "file": 0,
            "offset": 0,
            "data_base64": base64::engine::general_purpose::STANDARD.encode([7u8; 40])
        })),
    )
    .await;
    assert!(dir.path().join(format!("received/.partial/{id}/0.part")).exists());
    let (st, cancelled) = call_json(&app, "POST", &format!("/api/transfers/{id}/cancel"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cancelled["result"], "cancelled");
    assert!(!dir.path().join(format!("received/.partial/{id}")).exists());

    // 4. Setting receive_mode to "off" refuses offers with 403 Forbidden (FR-F9).
    let (st, _) = call_json(
        &app,
        "PUT",
        "/api/transfers/settings",
        Some(json!({ "receive_mode": "off" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, err) = call_json(
        &app,
        "POST",
        "/api/transfers/offers",
        Some(json!({
            "from": "MacBook",
            "peer_id": registered_peer.to_string(),
            "files": [{ "name": "a.txt", "size": 5 }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(err["error"].as_str().unwrap().contains("ReceiveOff"));
}
