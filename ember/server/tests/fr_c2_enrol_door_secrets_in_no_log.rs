//! FR-C2g: the code, the poll secret and the node token are in no tracing output of a full
//! enrolment through the enrol door: mint, redeem, poll (waiting), confirm, poll (joined), poll
//! (delivered).
//!
//! NOT RUN: nothing was compiled when this file was written.
//!
//! This test lives in its own file on purpose, and the `Capture` writer is copied from
//! `fr_c2_enrol_capture.rs` on purpose (issue #549). Every integration test file is its own
//! process, and the tracing callsite interest cache is process-wide: in a shared binary another
//! test could call the same handlers with no subscriber and cache the callsite as "never". Here
//! exactly one test runs in the process and the capture subscriber is the global default before
//! the first event. Keep this file to ONE test. The order of the checks is the lesson of #549: the
//! absence of the secrets is asserted FIRST (the property that matters), the status answers and the
//! expected lines after.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::computers::enrol::{Enrol, NodeInfo, RegisterError, Registrar};
use ember_server::computers::{self};
use ember_server::enrol_door::{EnrolDoor, ENROL_LISTEN_VAR};
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const BASE: &str = "http://192.168.0.5:8740";
const HOST: &str = "127.0.0.1:9300";
const PEER: &str = "203.0.113.50";

/// A registry that lives in a Vec.
#[derive(Default)]
struct Fake {
    registered: Mutex<Vec<(String, String)>>,
}

impl Registrar for Fake {
    fn name_taken(&self, name: &str) -> bool {
        self.registered.lock().unwrap().iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }
    fn register(&self, name: &str, token: &str, _i: &NodeInfo, _o: &str) -> Result<String, RegisterError> {
        if self.name_taken(name) {
            return Err(RegisterError::NameTaken);
        }
        let mut r = self.registered.lock().unwrap();
        r.push((name.to_string(), token.to_string()));
        Ok(format!("computer-{}", r.len()))
    }
    fn remove(&self, _id: &str) {}
}

/// A tracing writer that collects everything written.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

async fn owner(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path).header("host", "127.0.0.1:8740");
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let r = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let st = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn on_door(app: &Router, method: &str, uri: &str, headers: &[(&str, String)], body: Option<String>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header("host", HOST);
    for (k, v) in headers {
        req = req.header(*k, v.as_str());
    }
    let mut req = req.body(Body::from(body.unwrap_or_default())).unwrap();
    req.extensions_mut().insert(ConnectInfo(SocketAddr::new(PEER.parse().unwrap(), 40000)));
    let r = app.clone().oneshot(req).await.unwrap();
    let st = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn fr_c2_the_secrets_are_in_no_log_of_a_full_enrolment_through_the_door() {
    let cap = Capture::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(cap.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // Global default, once, before any event: the callsite interest is computed with it present.
    tracing::subscriber::set_global_default(sub).expect("no other subscriber in this process");

    let store = Arc::new(Store::open_in_memory().unwrap());
    let enrol = Enrol::new(store);
    let fake = Arc::new(Fake::default());
    let owner_app = computers::enrol::router(enrol.clone(), ember_server::computers::enrol::DoorUrl::Url(BASE.into()), fake.clone());
    let vars = [(ENROL_LISTEN_VAR.to_string(), "127.0.0.1:9300".to_string())];
    let d = EnrolDoor::from_lookup(
        |k| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()),
        "127.0.0.1:8740".parse().unwrap(),
        None,
    )
    .unwrap()
    .expect("door on");
    let json_h = || [("content-type", "application/json".to_string())];

    // 1. Mint (admin side).
    let (mint_st, minted) = owner(&owner_app, "POST", "/api/computers/enrol-codes", Some(json!({ "name": "laptop" }))).await;
    let id = minted["id"].as_str().unwrap_or_default().to_string();
    let code = minted["code"].as_str().unwrap_or_default().to_string();

    // 2. Redeem on the door, and a refused redeem with a wrong code (its warn line must not carry the code either).
    let info = json!({ "hostname": "ibrew-laptop", "os": "macos", "arch": "aarch64", "node_version": "0.1.0" });
    let body = json!({ "code": code, "node_public_info": info }).to_string();
    let (redeem_st, redeemed) = on_door(&d.app(enrol.clone()), "POST", "/enrol/redeem", &json_h(), Some(body)).await;
    let secret = redeemed["poll_secret"].as_str().unwrap_or_default().to_string();
    let wrong = format!("dpe_{}", "0".repeat(64));
    let body = json!({ "code": wrong, "node_public_info": info }).to_string();
    let (refused_st, _) = on_door(&d.app(enrol.clone()), "POST", "/enrol/redeem", &json_h(), Some(body)).await;

    // 3. Poll before the confirm. A fresh router per poll gives a fresh 2 s limiter (no sleeping).
    let bearer = [("authorization", format!("Bearer {secret}"))];
    let poll_uri = format!("/enrol/status/{id}");
    let (waiting_st, waiting) = on_door(&d.app(enrol.clone()), "GET", &poll_uri, &bearer, None).await;
    // A poll with a wrong secret (its refusal must not log the secret either).
    let wrong_bearer = [("authorization", "Bearer dps_wrongwrongwrong".to_string())];
    let (wrong_st, _) = on_door(&d.app(enrol.clone()), "GET", &poll_uri, &wrong_bearer, None).await;

    // An invented id with a newline and an ANSI escape (percent-encoded in the path, decoded by the
    // door): whatever the door logs about it must not carry either raw.
    let evil_uri = "/enrol/status/x%0AINJECTEDLINE%1B%5B31mred";
    let (evil_st, _) = on_door(&d.app(enrol.clone()), "GET", evil_uri, &bearer, None).await;
    let (evil_wrong_st, _) = on_door(&d.app(enrol.clone()), "GET", evil_uri, &wrong_bearer, None).await;

    // 4. The owner confirms; 5. the poll delivers; 6. the next poll says delivered.
    let (confirm_st, _) = owner(&owner_app, "POST", &format!("/api/computers/enrol-codes/{id}/confirm"), Some(json!({}))).await;
    let (joined_st, joined) = on_door(&d.app(enrol.clone()), "GET", &poll_uri, &bearer, None).await;
    let (delivered_st, delivered) = on_door(&d.app(enrol.clone()), "GET", &poll_uri, &bearer, None).await;
    let token = fake.registered.lock().unwrap().first().map(|r| r.1.clone()).unwrap_or_default();

    let out = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    let thread = std::thread::current();
    let context = format!("thread={:?}, runtime=current_thread, captured output:\n{out}", thread.name());

    // A. Secret absence, FIRST and independent of the status checks: all failures are collected.
    // The part after the prefix is compared, so the prefix alone cannot hide a leak.
    let mut failures: Vec<String> = Vec::new();
    let secrets = [("code", &code), ("poll secret", &secret), ("node token", &token), ("wrong code", &wrong)];
    for (what, s) in secrets {
        let body = s.split_once('_').map_or(s.as_str(), |(_, b)| b);
        if body.is_empty() {
            failures.push(format!("the {what} is empty: the flow did not produce it"));
        } else if out.contains(body) {
            failures.push(format!("the {what} reached the log"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}; {context}");
    assert!(!out.contains("wrongwrongwrong"), "the wrong poll secret reached the log; {context}");
    // Attacker-chosen text does not reach the log raw: no escape byte, and the invented id's
    // newline did not start a line of its own.
    assert!(!out.contains('\u{1b}'), "an escape byte reached the log; {context}");
    assert!(!out.contains("x\nINJECTEDLINE"), "a newline-injected id reached the log; {context}");
    assert!(!out.lines().any(|l| l.starts_with("INJECTEDLINE")), "an injected line reached the log; {context}");

    // B. The flow itself went as intended, so a missing line below is a logging problem.
    assert_eq!(mint_st, StatusCode::CREATED);
    assert_eq!(redeem_st, StatusCode::OK, "{redeemed}");
    assert_eq!(refused_st, StatusCode::FORBIDDEN);
    assert_eq!((waiting_st, &waiting["status"]), (StatusCode::OK, &json!("waiting")));
    assert_eq!(wrong_st, StatusCode::NOT_FOUND);
    assert_eq!((evil_st, evil_wrong_st), (StatusCode::NOT_FOUND, StatusCode::NOT_FOUND));
    assert_eq!(confirm_st, StatusCode::OK);
    assert_eq!((joined_st, &joined["status"]), (StatusCode::OK, &json!("joined")));
    assert_eq!(joined["credential"]["node_token"], token.as_str());
    assert_eq!((delivered_st, &delivered["status"]), (StatusCode::OK, &json!("delivered")));
    assert!(delivered.get("credential").is_none());

    // C. The log names the enrolment and has the expected lines.
    assert!(out.contains(&id), "the enrol id is not in the captured output; {context}");
    for line in ["enrol code minted", "enrol code redeemed", "enrol confirmed", "enrol credential delivered"] {
        assert!(out.contains(line), "the line {line:?} is missing; {context}");
    }
}
