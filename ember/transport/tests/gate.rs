//! FR-N3 admission and revocation, and the dialer's connection cache, on the fake transport.
#![cfg(feature = "http")]

use std::time::Duration;

use axum::routing::get;
use axum::Router;
use ember_transport::http::{http1_handshake, HttpListener};
use ember_transport::mem::MemNetwork;
use ember_transport::{CloseReason, Dialer, PeerAddr, PeerGate, Transport};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;

const SERVICE: &str = "gate-test/1";

fn serve(server: &Transport, gate: PeerGate) {
    let listener = HttpListener::with_gate(server.listen(SERVICE).unwrap(), gate);
    let app = Router::new().route("/ping", get(|| async { "pong" }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
}

async fn ping(dialer: &Dialer, server: &Transport) -> Result<String, String> {
    let addr = PeerAddr::new(server.peer_id());
    let stream = dialer.open_bi(&addr, SERVICE).await.map_err(|e| e.to_string())?;
    let mut http = http1_handshake::<Full<Bytes>>(stream).await.map_err(|e| e.to_string())?;
    let req = hyper::Request::get("http://peer/ping").body(Full::default()).unwrap();
    let fut = async {
        let resp = http.send_request(req).await.map_err(|e| e.to_string())?;
        let body = resp.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
        Ok(String::from_utf8_lossy(&body).into_owned())
    };
    tokio::time::timeout(Duration::from_secs(5), fut).await.map_err(|_| "timed out".to_string())?
}

#[tokio::test]
async fn unknown_peer_is_refused_and_revocation_closes_connections() {
    let net = MemNetwork::new();
    let (server, device, stranger) = (net.transport(), net.transport(), net.transport());
    let gate = PeerGate::allow_list([device.peer_id()]);
    serve(&server, gate.clone());

    let d = Dialer::new(device.clone());
    assert_eq!(ping(&d, &server).await.unwrap(), "pong");
    // Several requests share one cached connection.
    assert_eq!(ping(&d, &server).await.unwrap(), "pong");
    assert_eq!(gate.live_connections(&device.peer_id()), 1);

    // A peer that is not on the list is closed at accept.
    let s = Dialer::new(stranger.clone());
    assert!(ping(&s, &server).await.is_err());
    assert_eq!(gate.live_connections(&stranger.peer_id()), 0);

    // Revoke: the live connection is closed with the revoked code, and new ones are refused.
    let conn = d.connection(&PeerAddr::new(server.peer_id()), SERVICE).await.unwrap();
    assert_eq!(gate.revoke(&device.peer_id()), 1);
    let reason = tokio::time::timeout(Duration::from_secs(1), conn.closed()).await.unwrap();
    assert!(
        matches!(reason, CloseReason::Remote { code: ember_transport::gate::CLOSE_REVOKED, .. }),
        "{reason:?}"
    );
    assert!(ping(&d, &server).await.is_err());

    // Allowing it again restores access (the dialer re-dials).
    gate.allow(device.peer_id());
    assert_eq!(ping(&d, &server).await.unwrap(), "pong");
}

#[tokio::test]
async fn set_allowed_drops_peers_no_longer_listed() {
    let net = MemNetwork::new();
    let (server, a, b) = (net.transport(), net.transport(), net.transport());
    let gate = PeerGate::allow_list([a.peer_id(), b.peer_id()]);
    serve(&server, gate.clone());
    let (da, db) = (Dialer::new(a.clone()), Dialer::new(b.clone()));
    assert!(ping(&da, &server).await.is_ok());
    assert!(ping(&db, &server).await.is_ok());
    assert_eq!(gate.set_allowed([a.peer_id()]), 1);
    assert!(ping(&da, &server).await.is_ok());
    assert!(ping(&db, &server).await.is_err());
}

#[tokio::test]
async fn open_gate_admits_everyone_until_revoked() {
    let net = MemNetwork::new();
    let (server, a) = (net.transport(), net.transport());
    let gate = PeerGate::open();
    serve(&server, gate.clone());
    let da = Dialer::new(a.clone());
    assert!(ping(&da, &server).await.is_ok());
    gate.revoke(&a.peer_id());
    assert!(!gate.is_allowed(&a.peer_id()));
    assert!(ping(&da, &server).await.is_err());
}
