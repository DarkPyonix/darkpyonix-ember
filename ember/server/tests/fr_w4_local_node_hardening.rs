//! FR-W4 (#474): follow-ups of the local node review. The token is 32 random bytes, the
//! WebSocket upgrade needs the Bearer token whatever the Origin, an attach in flight outlives the
//! dropped `Computers` (the listener closes, accepted connections end on their own), and an
//! attach aborted half way leaves the session usable.
//! Written test first; not compiled by the author (sub-agents do not build).
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use ember_node::client::TermAttachment;
use ember_node::proto::*;
use ember_server::computers::local_node::new_token;
use ember_server::computers::{self, Computers, Registry};
use ember_server::store::Store;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn computers() -> Arc<Computers> {
    Computers::new(Registry::new(Arc::new(Store::open_in_memory().unwrap())), computers::default_connector())
}

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), f).await.expect("timed out")
}

fn create_req() -> TermCreateRequest {
    let home = std::path::PathBuf::from(std::env::var("HOME").expect("HOME is set"));
    let mut env = std::collections::BTreeMap::new();
    env.insert("PS1".to_string(), "$ ".to_string());
    TermCreateRequest {
        program: Some(Program::Argv(vec!["/bin/sh".into()])),
        cwd: home.clone(),
        env,
        env_clear: false,
        size: Some(PtySize { rows: 24, cols: 80 }),
        origin: TermOrigin::IdeEmber,
        project: Some(home.display().to_string()),
        title: None,
        key: None,
        tags: Default::default(),
    }
}

fn hello() -> TermHello {
    TermHello { device: "t".into(), kind: Some("test".into()), pid: None, size: None, active: true, read_only: false, snapshot: true }
}

/// Type `line` every 3 s until `needle` shows up (a starting shell can drop early input).
async fn type_until(a: &mut TermAttachment, line: &str, needle: &str) {
    let mut out = String::new();
    loop {
        a.send(TermInput::Input { data: line.as_bytes().to_vec() }).await.unwrap();
        let round = tokio::time::Instant::now() + Duration::from_secs(3);
        while !out.contains(needle) {
            match tokio::time::timeout_at(round, a.recv()).await {
                Err(_) => break,
                Ok(Ok(Some(TermEvent::Output { data }))) | Ok(Ok(Some(TermEvent::Snapshot { data, .. }))) => {
                    out.push_str(&String::from_utf8_lossy(&data))
                }
                Ok(Ok(Some(_))) => {}
                Ok(other) => panic!("attachment ended waiting for {needle:?}: {other:?}; output {out:?}"),
            }
        }
        if out.contains(needle) {
            return;
        }
    }
}

#[test]
fn fr_w4_local_node_token_is_32_random_bytes() {
    let a = new_token().unwrap();
    let b = new_token().unwrap();
    assert_eq!(a.len(), 64, "{a}");
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{a}");
    assert_ne!(a, b);
}

#[tokio::test]
async fn fr_w4_local_node_websocket_upgrade_needs_the_token_whatever_the_origin() {
    within(async {
        let c = computers();
        let client = c.local_node_client().unwrap();
        let made = client.term_create(&create_req()).await.unwrap();
        let addr = c.local_node_addr().unwrap();
        let url = format!("ws://{addr}/terms/{}/attach", made.term.id);

        let status = |r: Result<_, tokio_tungstenite::tungstenite::Error>| match r {
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => resp.status().as_u16(),
            Err(e) => panic!("unexpected error {e:?}"),
            Ok(_) => panic!("the upgrade must be refused"),
        };
        // No token.
        assert_eq!(status(tokio_tungstenite::connect_async(url.as_str()).await.map(drop)), 401);
        // A foreign Origin (a web page) without the token: a browser cannot add Authorization to
        // a WebSocket upgrade, so the token alone is the gate.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert("Origin", "https://evil.example".parse().unwrap());
        assert_eq!(status(tokio_tungstenite::connect_async(req).await.map(drop)), 401);
        // A foreign Origin with a wrong token.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert("Origin", "https://evil.example".parse().unwrap());
        req.headers_mut().insert("Authorization", "Bearer local".parse().unwrap());
        assert_eq!(status(tokio_tungstenite::connect_async(req).await.map(drop)), 401);

        client.term_kill(&made.term.id, None).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn fr_w4_local_node_attach_in_flight_outlives_dropped_computers() {
    within(async {
        let c = computers();
        let client = c.local_node_client().unwrap();
        let made = client.term_create(&create_req()).await.unwrap();
        let mut a = client.term_attach(&made.term.id, &hello()).await.unwrap();
        let addr = c.local_node_addr().unwrap();
        drop(c);
        // The listener closes...
        let mut closed = false;
        for _ in 0..200 {
            if tokio::net::TcpStream::connect(addr).await.is_err() {
                closed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(closed, "the loopback port is closed after Computers is dropped");
        // ...but the accepted attach keeps working until it ends (documented in SPEC).
        type_until(&mut a, "echo inflight-$((40+2))\n", "inflight-42").await;
        a.send(TermInput::Kill { signal: None }).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn fr_w4_local_node_attach_aborted_in_flight_leaves_the_session_usable() {
    within(async {
        let c = computers();
        let client = c.local_node_client().unwrap();
        let made = client.term_create(&create_req()).await.unwrap();
        // Abort attaches at several points of the handshake.
        for us in [0u64, 100, 1000, 5000] {
            let _ = tokio::time::timeout(Duration::from_micros(us), client.term_attach(&made.term.id, &hello())).await;
        }
        let info = client.term(&made.term.id).await.unwrap();
        assert_eq!(info.state, TermState::Running);
        let mut a = client.term_attach(&made.term.id, &hello()).await.unwrap();
        type_until(&mut a, "echo after-$((40+2))\n", "after-42").await;
        client.term_kill(&made.term.id, None).await.unwrap();
    })
    .await;
}
