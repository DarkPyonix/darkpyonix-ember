//! FR-C2l (#539): with `EMBER_ENROL_AUTO_CLOSE=1` the enrol door is closed while no enrolment
//! needs it, the owner's mint opens it (and says so in its answer), and it closes again when the
//! last code is used and delivered, denied, cancelled or expired.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::computers::enrol::{self, DoorUrl, Enrol, NodeInfo, MAX_TTL};
use ember_server::computers::{self, Computers, Registry};
use ember_server::enrol_door::{DoorGate, EnrolDoor, ENROL_AUTO_CLOSE_VAR, ENROL_LISTEN_VAR};
use ember_server::store::{now_ms, Store};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "127.0.0.1:8740";

fn door(auto: Option<&str>) -> EnrolDoor {
    // Port 0: every open binds a fresh port, so a closed door is told apart by a refused connect.
    let mut m: HashMap<String, String> = HashMap::new();
    m.insert(ENROL_LISTEN_VAR.into(), "127.0.0.1:0".into());
    if let Some(v) = auto {
        m.insert(ENROL_AUTO_CLOSE_VAR.into(), v.into());
    }
    EnrolDoor::from_lookup(|k| m.get(k).cloned(), OWNER.parse().unwrap(), None).unwrap().expect("door on")
}

struct Fixture {
    enrol: Arc<Enrol>,
    gate: Arc<DoorGate>,
    owner: Router,
}

fn fixture() -> Fixture {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let comps = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let enrol = Enrol::new(store);
    let gate = DoorGate::new(door(Some("1")), enrol.clone());
    let owner = enrol::router_with_gate(
        enrol.clone(),
        DoorUrl::Url("http://127.0.0.1:9300".into()),
        Arc::new(enrol::ComputersRegistrar::new(comps)),
        Some(gate.clone()),
    );
    Fixture { enrol, gate, owner }
}

async fn owner_call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header("host", OWNER);
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn mint(f: &Fixture) -> Value {
    let (st, b) = owner_call(&f.owner, "POST", "/api/computers/enrol-codes", Some(json!({"name": "laptop"}))).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b
}

async fn connects(addr: SocketAddr) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

#[test]
fn fr_c2l_auto_close_is_opt_in_and_only_for_1() {
    assert!(!door(None).auto_close);
    assert!(!door(Some("0")).auto_close);
    assert!(!door(Some("yes")).auto_close);
    assert!(door(Some("1")).auto_close);
}

#[tokio::test]
async fn fr_c2l_door_is_closed_without_a_pending_code_and_opens_at_mint() {
    let f = fixture();
    assert!(!f.gate.is_open().await, "closed before any mint");
    assert!(!f.gate.close_if_idle(now_ms()).await);
    let b = mint(&f).await;
    assert_eq!(b["door"], "open", "the mint answer shows the door state");
    let addr = f.gate.local_addr().await.expect("open after mint");
    assert!(connects(addr).await, "the door accepts connections");
    // A second mint while open keeps the same socket.
    mint(&f).await;
    assert_eq!(f.gate.local_addr().await, Some(addr));
    // With codes pending the check keeps it open.
    assert!(f.gate.close_if_idle(now_ms()).await);
    f.gate.close().await;
}

#[tokio::test]
async fn fr_c2l_door_closes_after_the_last_code_expires() {
    let f = fixture();
    mint(&f).await;
    let addr = f.gate.local_addr().await.unwrap();
    // Just before the code lifetime ends the door stays open; after it, it closes.
    assert!(f.gate.close_if_idle(now_ms() + 1_000).await);
    assert!(!f.gate.close_if_idle(now_ms() + MAX_TTL.as_millis() as i64 + 1_000).await);
    assert!(!f.gate.is_open().await);
    assert!(!connects(addr).await, "the socket is released");
}

#[tokio::test]
async fn fr_c2l_door_closes_after_the_last_code_is_denied_or_cancelled() {
    let f = fixture();
    let a = mint(&f).await;
    let b = mint(&f).await;
    // Cancel one: the other still needs the door.
    let id_a = a["id"].as_str().unwrap();
    let (st, _) = owner_call(&f.owner, "DELETE", &format!("/api/computers/enrol-codes/{id_a}"), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(f.gate.close_if_idle(now_ms()).await, "one code is still pending");
    // Redeem the other, then deny it: nothing is left.
    let code_b = b["code"].as_str().unwrap();
    let id_b = b["id"].as_str().unwrap();
    f.enrol.redeem(code_b, &NodeInfo::minimal("laptop"), now_ms(), "127.0.0.1").unwrap();
    assert!(f.gate.close_if_idle(now_ms()).await, "awaiting confirmation still needs the door");
    let (st, _) = owner_call(&f.owner, "POST", &format!("/api/computers/enrol-codes/{id_b}/deny"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!f.gate.close_if_idle(now_ms()).await);
    // A new mint opens it again.
    assert_eq!(mint(&f).await["door"], "open");
    assert!(f.gate.is_open().await);
    f.gate.close().await;
}

#[tokio::test]
async fn fr_c2l_the_watch_closes_the_door_after_a_deny_nudge() {
    let f = fixture();
    let watch = tokio::spawn(f.gate.clone().watch(Duration::from_secs(3600), std::future::pending()));
    let b = mint(&f).await;
    f.enrol.redeem(b["code"].as_str().unwrap(), &NodeInfo::minimal("laptop"), now_ms(), "127.0.0.1").unwrap();
    let id = b["id"].as_str().unwrap();
    owner_call(&f.owner, "POST", &format!("/api/computers/enrol-codes/{id}/deny"), None).await;
    // The deny nudged the watch; the hour-long interval is not what closes it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while f.gate.is_open().await {
        assert!(tokio::time::Instant::now() < deadline, "the watch did not close the door");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    watch.abort();
}

#[tokio::test]
async fn fr_c2l_without_a_gate_the_mint_says_door_on() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let comps = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let owner = enrol::router(
        Enrol::new(store),
        DoorUrl::Url("http://127.0.0.1:9300".into()),
        Arc::new(enrol::ComputersRegistrar::new(comps)),
    );
    let (st, b) = owner_call(&owner, "POST", "/api/computers/enrol-codes", Some(json!({"name": "laptop"}))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(b["door"], "on");
}
