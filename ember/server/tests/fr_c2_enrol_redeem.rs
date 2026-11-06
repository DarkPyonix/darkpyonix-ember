//! FR-C2b, FR-C2d, FR-C2e, FR-C2f, FR-C2g (step C3a, all [provisional]): the two routes of the
//! enrol door, `POST /enrol/redeem` and `GET /enrol/status/{enrol_id}`.
//!
//! NONE of these tests has been run (nothing was compiled when this file was written). Every
//! expected value was read from `ember/server/src/enrol_door.rs` and
//! `ember/server/src/computers/enrol.rs`. The door is driven in process with `oneshot` through the
//! real `EnrolDoor::app` / `wrap_limited` (so the limit layer, the timeout, the guard and the body
//! limit are in the path), with the `ConnectInfo` put in the request as the serve loop does. A fake
//! `Registrar` stands in for the registry, over a real in-memory `Store`. No tracing capture here:
//! that test is alone in `fr_c2_enrol_door_secrets_in_no_log.rs` (issue #549).
//!
//! The poll limiter is per router (it lives in the routes' state). A test that polls one
//! enrolment several times builds a fresh door router over the SAME `Arc<Enrol>` for each poll
//! (`Fx::app`), so the 2 s limit does not get in its way and nothing sleeps; the tests of the limit
//! itself use one router, or `PollLimiter` with an injected clock. The handler verifies the poll
//! secret before it asks the limiter, so only a poll with a valid secret can take a slot.
//!
//! Not written, and why:
//! - A 408 on the redeem route: the request timeout is the fixed `ENROL_REQUEST_TIMEOUT` (10 s)
//!   and cannot be injected; no dependency may be added for a paused clock.
//! - That a wrong-shape code is not looked up in SQLite: nothing public observes a lookup. What is
//!   pinned is that it is the same refusal and the same failure books (see
//!   `fr_c2_a_malformed_code_is_the_same_403_and_is_recorded_as_a_failure`); the skip itself is in
//!   the unit test in `computers/enrol.rs` and in review.
//! - Timing equality between the poll refusals: no stable way to assert it in a test.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use ember_server::computers::enrol::{
    Enrol, NodeInfo, RegisterError, Registrar, BAD_NODE_INFO_MESSAGE, CONFIRM_WINDOW_MS, GLOBAL_MAX_FAILURES,
    MAX_FAILURES, RATE_LIMITED_MESSAGE, REFUSED_MESSAGE,
};
use ember_server::computers::{self};
use ember_server::enrol_door::{
    DoorLimits, EnrolDoor, LimitConfig, PollLimiter, ENROL_ACCEPT_PER_HOUR, ENROL_ACCEPT_PER_MINUTE,
    ENROL_LISTEN_VAR, ENROL_MAX_BODY_BYTES, ENROL_MAX_IN_FLIGHT, ENROL_MAX_TRACKED_ADDRS, ENROL_POLL_ENTRY_TTL,
    ENROL_POLL_INTERVAL, ENROL_POLL_MAX_ENTRIES, ENROL_REDEEM_PATH,
};
use ember_server::store::{now_ms, Store};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "127.0.0.1:8740";
const BASE: &str = "http://192.168.0.5:8740";
const LISTEN: &str = "127.0.0.1:9300";
const HOST: &str = "127.0.0.1:9300";
const PEER: &str = "203.0.113.50";

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

/// A registry that lives in a Vec.
#[derive(Default)]
struct Fake {
    registered: Mutex<Vec<(String, String)>>,
}

impl Fake {
    fn count(&self) -> usize {
        self.registered.lock().unwrap().len()
    }
    fn token(&self, i: usize) -> String {
        self.registered.lock().unwrap()[i].1.clone()
    }
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

fn door(extra: &[(&str, &str)]) -> EnrolDoor {
    let mut m: HashMap<String, String> = extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    m.insert(ENROL_LISTEN_VAR.into(), LISTEN.into());
    EnrolDoor::from_lookup(|k| m.get(k).cloned(), sa(OWNER), None).unwrap().expect("door on")
}

/// Limits so large that a test sees the handlers and the core's limiters, not the door's.
fn big_cfg() -> LimitConfig {
    LimitConfig {
        accept_per_minute: 100_000,
        accept_per_hour: 100_000,
        other_per_minute: 100_000,
        global_per_second: 1_000_000.0,
        global_burst: 1_000_000.0,
        max_in_flight: ENROL_MAX_IN_FLIGHT,
        max_addrs: ENROL_MAX_TRACKED_ADDRS,
    }
}

struct Fx {
    d: EnrolDoor,
    enrol: Arc<Enrol>,
    /// The owner's router (mint, status, confirm, deny), over the SAME `Arc<Enrol>` as the door.
    owner: Router,
    fake: Arc<Fake>,
}

fn fx_with(vars: &[(&str, &str)]) -> Fx {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let enrol = Enrol::new(store);
    let fake = Arc::new(Fake::default());
    let owner = computers::enrol::router(enrol.clone(), BASE.into(), fake.clone());
    Fx { d: door(vars), enrol, owner, fake }
}

fn fx() -> Fx {
    fx_with(&[])
}

impl Fx {
    /// The router `main.rs` serves, real limits, a fresh poll limiter.
    fn app(&self) -> Router {
        self.d.app(self.enrol.clone())
    }

    /// The same routes with the door's per-address and global limits out of the way, a fresh poll limiter.
    fn open_app(&self) -> Router {
        self.limited(big_cfg())
    }

    fn limited(&self, cfg: LimitConfig) -> Router {
        self.d.wrap_limited(
            self.d.routes_for(self.enrol.clone()),
            Arc::new(DoorLimits::with_strict_path(cfg, Vec::new(), ENROL_REDEEM_PATH)),
        )
    }
}

type Reply = (StatusCode, HeaderMap, Vec<u8>);

async fn send(app: &Router, peer: &str, method: &str, uri: &str, headers: &[(&str, &str)], body: Vec<u8>) -> Reply {
    let mut req = Request::builder().method(method).uri(uri).header("host", HOST);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::from(body)).unwrap();
    req.extensions_mut().insert(ConnectInfo(SocketAddr::new(peer.parse::<IpAddr>().unwrap(), 40000)));
    let res = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = res.into_parts();
    (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
}

fn json_of(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|e| panic!("not json ({e}): {:?}", String::from_utf8_lossy(body)))
}

fn text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).to_string()
}

/// Header names and values, sorted, so two answers compare as a whole.
fn pairs(h: &HeaderMap) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> =
        h.iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string())).collect();
    v.sort();
    v
}

fn node_info() -> Value {
    json!({
        "hostname": "ibrew-laptop", "os": "macos", "arch": "aarch64", "node_version": "0.1.0",
        "url": "http://192.168.0.9:8741"
    })
}

async fn redeem_raw(app: &Router, peer: &str, ctype: Option<&str>, body: &str) -> Reply {
    let mut h: Vec<(&str, &str)> = Vec::new();
    if let Some(c) = ctype {
        h.push(("content-type", c));
    }
    send(app, peer, "POST", "/enrol/redeem", &h, body.as_bytes().to_vec()).await
}

async fn redeem(app: &Router, peer: &str, code: &str) -> Reply {
    redeem_raw(app, peer, Some("application/json"), &json!({ "code": code, "node_public_info": node_info() }).to_string())
        .await
}

async fn poll(app: &Router, id: &str, secret: Option<&str>) -> Reply {
    poll_at(app, PEER, &format!("/enrol/status/{id}"), secret).await
}

async fn poll_at(app: &Router, peer: &str, uri: &str, secret: Option<&str>) -> Reply {
    let bearer = secret.map(|s| format!("Bearer {s}"));
    let mut h: Vec<(&str, &str)> = Vec::new();
    if let Some(b) = &bearer {
        h.push(("authorization", b.as_str()));
    }
    send(app, peer, "GET", uri, &h, Vec::new()).await
}

/// A call on the owner router (no auth wrapper: the token layer is not under test here).
async fn owner(f: &Fx, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path).header("host", OWNER);
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let r = f.owner.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let st = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn mint(f: &Fx, name: &str) -> (String, String) {
    let (st, b) = owner(f, "POST", "/api/computers/enrol-codes", Some(json!({ "name": name }))).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    (b["id"].as_str().unwrap().to_string(), b["code"].as_str().unwrap().to_string())
}

struct Opened {
    id: String,
    code: String,
    secret: String,
}

/// Mint through the owner router and redeem through the door.
async fn open(f: &Fx, name: &str, peer: &str) -> Opened {
    let (id, code) = mint(f, name).await;
    let (st, _, body) = redeem(&f.open_app(), peer, &code).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let b = json_of(&body);
    assert_eq!(b["enrol_id"], id);
    Opened { id, code, secret: b["poll_secret"].as_str().unwrap().to_string() }
}

fn api(id: &str, tail: &str) -> String {
    format!("/api/computers/enrol-codes/{id}{tail}")
}

fn forbidden_body() -> Value {
    json!({ "error": REFUSED_MESSAGE, "code": "forbidden" })
}

fn not_found_body() -> Value {
    json!({ "error": "not found", "code": "not_found" })
}

/// A code of the right shape that no mint made.
fn unknown_code(c: char) -> String {
    format!("dpe_{}", c.to_string().repeat(64))
}

// ---- redeem: the answers ----

#[tokio::test]
async fn fr_c2_redeem_happy_path_returns_a_handle_and_never_a_credential() {
    let f = fx();
    let (id, code) = mint(&f, "laptop").await;
    let before = now_ms();
    let (st, h, body) = redeem(&f.app(), PEER, &code).await;
    let after = now_ms();
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let b = json_of(&body);
    let mut keys: Vec<&str> = b.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["enrol_id", "expires_ms", "poll_secret"], "exactly the handle");
    assert_eq!(b["enrol_id"], id);
    assert!(b["poll_secret"].as_str().unwrap().starts_with("dps_"));
    let exp = b["expires_ms"].as_i64().unwrap();
    assert!(exp >= before + CONFIRM_WINDOW_MS && exp <= after + CONFIRM_WINDOW_MS, "{exp}");
    assert_eq!(h["cache-control"], "no-store");
    assert!(h["content-type"].to_str().unwrap().starts_with("application/json"));
    let t = text(&body);
    assert!(!t.contains("dpn_") && !t.contains("node_token") && !t.contains("credential"), "{t}");
    // Nothing was registered and the owner sees the enrolment waiting for the confirm.
    assert_eq!(f.fake.count(), 0);
    let (st, s) = owner(&f, "GET", &api(&id, ""), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s["status"], "awaiting_confirmation");
    // The code is used: a second redeem is the refusal.
    let (st, _, body) = redeem(&f.open_app(), "203.0.113.51", &code).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(json_of(&body), forbidden_body());
}

#[tokio::test]
async fn fr_c2_redeem_wrong_unknown_used_expired_and_cancelled_codes_are_the_same_403() {
    let f = fx();
    let app = f.open_app();
    let used = open(&f, "used", "203.0.113.1").await.code;
    let cancelled_id = {
        let (id, code) = mint(&f, "cancelled").await;
        f.enrol.cancel(&id).unwrap();
        code
    };
    // Minted in the past: its ten minutes are over.
    let expired = f.enrol.mint("expired", now_ms() - 700_000, None, BASE).unwrap().code.expose().to_string();
    let cases: Vec<(&str, String)> = vec![
        ("used", used),
        ("unknown", unknown_code('b')),
        ("expired", expired),
        ("cancelled", cancelled_id),
        ("wrong text", "dpe_wrong".into()),
        ("empty", String::new()),
    ];
    let mut seen: Vec<(&str, Reply)> = Vec::new();
    for (i, (what, code)) in cases.into_iter().enumerate() {
        // One address each, so no failure budget of the core is shared.
        seen.push((what, redeem(&app, &format!("198.51.100.{}", i + 1), &code).await));
    }
    let (first_what, first) = &seen[0];
    assert_eq!(first.0, StatusCode::FORBIDDEN, "{first_what}");
    assert_eq!(json_of(&first.2), forbidden_body());
    for (what, r) in &seen {
        assert_eq!(r.0, first.0, "{what}: status");
        assert_eq!(r.2, first.2, "{what}: body");
        assert_eq!(pairs(&r.1), pairs(&first.1), "{what}: header names and values");
    }
}

#[tokio::test]
async fn fr_c2_a_malformed_code_is_the_same_403_and_is_recorded_as_a_failure() {
    let f = fx();
    let app = f.open_app();
    let real_unknown = redeem(&app, "203.0.113.200", &unknown_code('c')).await;
    assert_eq!(real_unknown.0, StatusCode::FORBIDDEN);
    let upper = format!("dpe_{}", "A".repeat(64));
    let short = format!("dpe_{}", "a".repeat(63));
    let long = format!("dpe_{}", "a".repeat(65));
    let nonhex = format!("dpe_{}", "g".repeat(64));
    let bad_codes: Vec<String> = vec![
        String::new(),
        "dpe_".into(),
        "dpe_wrong".into(),
        "x".into(),
        upper,
        short,
        long,
        nonhex,
        format!("dps_{}", "a".repeat(64)),
        "dpe_\u{1F600}".into(),
        format!("dpe_{}", "a".repeat(64)) + "\n",
    ];
    assert_eq!(bad_codes.len(), MAX_FAILURES + 1);
    // Ten malformed codes from one address: each is the identical refusal of a real unknown code.
    for (i, c) in bad_codes.iter().take(MAX_FAILURES).enumerate() {
        let r = redeem(&app, "192.0.2.10", c).await;
        assert_eq!(r.0, real_unknown.0, "{i}");
        assert_eq!(r.2, real_unknown.2, "{i}");
        // The same headers (the body differs in no byte, so `content-length` matches too).
        assert_eq!(pairs(&r.1), pairs(&real_unknown.1), "{i}");
    }
    // The eleventh is rate limited by the core, as after ten real failures.
    let eleventh = redeem(&app, "192.0.2.10", &bad_codes[MAX_FAILURES]).await;
    assert_eq!(eleventh.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_of(&eleventh.2), json!({ "error": RATE_LIMITED_MESSAGE, "code": "rate_limited" }));
    assert_eq!(eleventh.1["retry-after"], "60");
    // Real failures from another address end the same way, with the same answer.
    for _ in 0..MAX_FAILURES {
        assert_eq!(redeem(&app, "192.0.2.11", &unknown_code('d')).await.0, StatusCode::FORBIDDEN);
    }
    let real_eleventh = redeem(&app, "192.0.2.11", &unknown_code('d')).await;
    assert_eq!(real_eleventh.0, eleventh.0);
    assert_eq!(real_eleventh.2, eleventh.2);
    assert_eq!(pairs(&real_eleventh.1), pairs(&eleventh.1));
    // The global counter moved too: malformed codes from 60 addresses fill it (the two addresses
    // above spent 20 of the 60), after which a valid code from a fresh address is rate limited
    // and is NOT burnt.
    let g = fx();
    let app = g.open_app();
    for i in 0..GLOBAL_MAX_FAILURES {
        let r = redeem(&app, &format!("10.1.{}.{}", i / 200, i % 200 + 1), "dpe_wrong").await;
        assert_eq!(r.0, StatusCode::FORBIDDEN, "{i}");
    }
    let (id, code) = mint(&g, "late").await;
    let r = redeem(&app, "10.9.9.9", &code).await;
    assert_eq!(r.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_of(&r.2), json!({ "error": RATE_LIMITED_MESSAGE, "code": "rate_limited" }));
    let (_, s) = owner(&g, "GET", &api(&id, ""), None).await;
    assert_eq!(s["status"], "waiting", "the code was not burnt");
}

#[tokio::test]
async fn fr_c2_redeem_needs_application_json() {
    let f = fx();
    let app = f.open_app();
    let want = json!({ "error": "unsupported media type", "code": "unsupported" });
    // A body that is not even JSON: 415 before any parsing.
    for (i, c) in [
        None,
        Some("text/plain"),
        Some("text/json"),
        Some("application/x-www-form-urlencoded"),
        Some("application/jsonx"),
        Some("application/json-patch+json"),
        Some("multipart/form-data; boundary=x"),
        Some(""),
    ]
    .into_iter()
    .enumerate()
    {
        let r = redeem_raw(&app, &format!("198.51.100.{}", i + 1), c, "this is {{ not json").await;
        assert_eq!(r.0, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{c:?}");
        assert_eq!(json_of(&r.2), want, "{c:?}");
    }
    // Two Content-Type headers are not one media type.
    let r = send(
        &app,
        "198.51.100.40",
        "POST",
        "/enrol/redeem",
        &[("content-type", "application/json"), ("content-type", "application/json")],
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(r.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    // The exact media type, in any case and with parameters, is accepted (the code is merely unknown).
    let body = json!({ "code": unknown_code('e'), "node_public_info": node_info() }).to_string();
    for (i, c) in ["application/json", "application/json; charset=utf-8", "Application/JSON", "application/json;charset=UTF-8"]
        .into_iter()
        .enumerate()
    {
        let r = redeem_raw(&app, &format!("198.51.100.{}", 50 + i), Some(c), &body).await;
        assert_eq!(r.0, StatusCode::FORBIDDEN, "{c}");
    }
    // A refused content type never looked at the code: a valid one is not burnt.
    let (id, code) = mint(&f, "ct").await;
    let valid = json!({ "code": code, "node_public_info": node_info() }).to_string();
    let r = redeem_raw(&app, "198.51.100.90", Some("text/plain"), &valid).await;
    assert_eq!(r.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (_, s) = owner(&f, "GET", &api(&id, ""), None).await;
    assert_eq!(s["status"], "waiting");
    assert_eq!(redeem_raw(&app, "198.51.100.90", Some("application/json"), &valid).await.0, StatusCode::OK);
}

#[tokio::test]
async fn fr_c2_redeem_bad_json_and_unknown_fields_are_400_with_a_fixed_text() {
    let f = fx();
    let app = f.open_app();
    let m = "ZZ-MARKER-9137";
    let ok_info = r#"{"hostname":"h"}"#;
    let bodies: Vec<String> = vec![
        format!(r#"{{"code":"{m}""#),
        format!(r#"["{m}"]"#),
        format!(r#""{m}""#),
        format!(r#"{{"node_public_info":{ok_info},"{m}":1}}"#),
        format!(r#"{{"node_public_info":{ok_info}}}"#),
        format!(r#"{{"code":"{m}"}}"#),
        format!(r#"{{"code":"{m}","node_public_info":{ok_info},"extra":"{m}"}}"#),
        format!(r#"{{"code":"{m}","node_public_info":{{"hostname":"h","{m}":1}}}}"#),
        format!(r#"{{"code":5,"node_public_info":{ok_info},"note":"{m}"}}"#),
        format!(r#"{{"code":"{m}","node_public_info":"{m}"}}"#),
        String::new(),
        "null".into(),
        format!("{m}{m}"),
    ];
    let want = json!({ "error": "bad request", "code": "bad_request" });
    for (i, b) in bodies.iter().enumerate() {
        let r = redeem_raw(&app, &format!("198.51.100.{}", i + 1), Some("application/json"), b).await;
        assert_eq!(r.0, StatusCode::BAD_REQUEST, "{b}");
        assert_eq!(json_of(&r.2), want, "{b}");
        assert!(!text(&r.2).contains(m), "the answer echoes the input: {}", text(&r.2));
        assert!(!pairs(&r.1).iter().any(|(_, v)| v.contains(m)), "a header echoes the input");
    }
    // Nothing above was a failure of the books: no unknown-field body spent a failure budget.
    let (id, code) = mint(&f, "after-bad-json").await;
    assert_eq!(redeem(&app, "198.51.100.1", &code).await.0, StatusCode::OK, "{id}");
}

#[tokio::test]
async fn fr_c2_redeem_oversized_body_is_413_with_or_without_content_length() {
    let f = fx();
    let app = f.open_app();
    let big = " ".repeat(ENROL_MAX_BODY_BYTES + 1);
    // Declared size over the limit: the guard.
    let len = big.len().to_string();
    let r = send(
        &app,
        PEER,
        "POST",
        "/enrol/redeem",
        &[("content-type", "application/json"), ("content-length", len.as_str())],
        big.clone().into_bytes(),
    )
    .await;
    assert_eq!(r.0, StatusCode::PAYLOAD_TOO_LARGE);
    // No declared size: the handler's own read stops at the same limit.
    let r = redeem_raw(&app, PEER, Some("application/json"), &big).await;
    assert_eq!(r.0, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json_of(&r.2), json!({ "error": "body too large", "code": "too_large" }));
}

#[tokio::test]
async fn fr_c2_redeem_bad_node_info_is_400_and_does_not_burn_the_code() {
    let f = fx();
    let app = f.open_app();
    let (id, code) = mint(&f, "bad-info").await;
    let want = json!({ "error": BAD_NODE_INFO_MESSAGE, "code": "bad_request" });
    let bad = [
        json!({ "hostname": "" }),
        json!({ "hostname": "bad\nhost" }),
        json!({ "hostname": "h".repeat(65) }),
        json!({ "hostname": "h", "url": "javascript:alert(1)" }),
    ];
    // More attempts than the failure budget: bad node info spends nothing.
    for i in 0..(MAX_FAILURES + 5) {
        let body = json!({ "code": code, "node_public_info": bad[i % bad.len()] }).to_string();
        let r = redeem_raw(&app, PEER, Some("application/json"), &body).await;
        assert_eq!(r.0, StatusCode::BAD_REQUEST, "{i}: {}", text(&r.2));
        assert_eq!(json_of(&r.2), want, "{i}");
    }
    // It depends on the node info only: an unknown code with bad info says the same.
    let body = json!({ "code": unknown_code('f'), "node_public_info": bad[0] }).to_string();
    let r = redeem_raw(&app, PEER, Some("application/json"), &body).await;
    assert_eq!(json_of(&r.2), want);
    // The code was not burnt: a valid redeem from the same address still works.
    let (_, s) = owner(&f, "GET", &api(&id, ""), None).await;
    assert_eq!(s["status"], "waiting");
    let (st, _, body) = redeem(&app, PEER, &code).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
}

// ---- redeem: the observed address ----

#[tokio::test]
async fn fr_c2_the_observed_address_is_the_client_ip_not_the_slash_64() {
    let f = fx();
    // IPv6: the owner sees the whole address, not the /64 the limiter keys on.
    let v6 = "2001:db8:1:2:3:4:5:6";
    let o = open(&f, "v6", v6).await;
    let (st, s) = owner(&f, "GET", &api(&o.id, ""), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s["observed_addr"], v6);
    assert_ne!(s["observed_addr"], "2001:db8:1:2::");
    // IPv4: the address without the port.
    let o = open(&f, "v4", "203.0.113.9").await;
    let (_, s) = owner(&f, "GET", &api(&o.id, ""), None).await;
    assert_eq!(s["observed_addr"], "203.0.113.9");
}

#[tokio::test]
async fn fr_c2_redeem_through_a_trusted_proxy_uses_the_forwarded_address() {
    let proxy = "10.0.0.2";
    let f = fx_with(&[("EMBER_ENROL_TRUSTED_PROXIES", proxy)]);
    // The served router, as main.rs builds it.
    let (id, code) = mint(&f, "behind-proxy").await;
    let body = json!({ "code": code, "node_public_info": node_info() }).to_string();
    let r = send(
        &f.app(),
        proxy,
        "POST",
        "/enrol/redeem",
        &[("content-type", "application/json"), ("x-forwarded-for", "198.51.100.7")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(r.0, StatusCode::OK, "{}", text(&r.2));
    let (_, s) = owner(&f, "GET", &api(&id, ""), None).await;
    assert_eq!(s["observed_addr"], "198.51.100.7");
    // An untrusted peer cannot forge the address with the same header.
    let (id, code) = mint(&f, "forged").await;
    let body = json!({ "code": code, "node_public_info": node_info() }).to_string();
    let r = send(
        &f.app(),
        "203.0.113.9",
        "POST",
        "/enrol/redeem",
        &[("content-type", "application/json"), ("x-forwarded-for", "198.51.100.7")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(r.0, StatusCode::OK);
    let (_, s) = owner(&f, "GET", &api(&id, ""), None).await;
    assert_eq!(s["observed_addr"], "203.0.113.9");
    // A door with no trusted proxy ignores the header from any peer, the proxy's address included.
    let g = fx();
    let (id, code) = mint(&g, "no-proxies").await;
    let body = json!({ "code": code, "node_public_info": node_info() }).to_string();
    let r = send(
        &g.app(),
        proxy,
        "POST",
        "/enrol/redeem",
        &[("content-type", "application/json"), ("x-forwarded-for", "198.51.100.7")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(r.0, StatusCode::OK);
    let (_, s) = owner(&g, "GET", &api(&id, ""), None).await;
    assert_eq!(s["observed_addr"], proxy);
}

// ---- poll: the answers ----

#[tokio::test]
async fn fr_c2_poll_before_confirm_never_carries_a_credential() {
    let f = fx();
    let o = open(&f, "waiting", PEER).await;
    let (st, h, body) = poll(&f.open_app(), &o.id, Some(&o.secret)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "status": "waiting" }));
    assert_eq!(h["cache-control"], "no-store");
    let t = text(&body);
    assert!(!t.contains("credential") && !t.contains("node_token") && !t.contains("dpn_"), "{t}");
    assert_eq!(f.fake.count(), 0);
    // A code that is only minted has no poll secret at all: nobody can poll it.
    let (id, _) = mint(&f, "never-redeemed").await;
    let (st, _, body) = poll(&f.open_app(), &id, Some(&o.secret)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(json_of(&body), not_found_body());
}

#[tokio::test]
async fn fr_c2_poll_after_confirm_delivers_the_credential_exactly_once_then_delivered() {
    let f = fx();
    let o = open(&f, "laptop", PEER).await;
    let (st, c) = owner(&f, "POST", &api(&o.id, "/confirm"), Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let token = f.fake.token(0);
    // First poll: the credential, exactly name, computer id and token (no server peer id yet: C3b).
    let (st, h, body) = poll(&f.open_app(), &o.id, Some(&o.secret)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h["cache-control"], "no-store");
    let b = json_of(&body);
    assert_eq!(b["status"], "joined");
    assert_eq!(b["credential"], json!({ "node_token": token, "name": "laptop", "computer_id": "computer-1" }));
    assert_eq!(b.as_object().unwrap().len(), 2);
    assert!(token.starts_with("dpn_"));
    // Second and third poll: delivered, no credential, and the token is nowhere in the answer.
    for _ in 0..2 {
        let (st, _, body) = poll(&f.open_app(), &o.id, Some(&o.secret)).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(json_of(&body), json!({ "status": "delivered" }));
        assert!(!text(&body).contains(token.trim_start_matches("dpn_")));
    }
    let (_, s) = owner(&f, "GET", &api(&o.id, ""), None).await;
    assert_eq!(s["status"], "joined");
    assert_eq!(f.fake.count(), 1);
}

#[tokio::test]
async fn fr_c2_poll_with_a_wrong_secret_a_missing_header_a_non_bearer_header_and_an_unknown_id_are_the_same_404() {
    let f = fx();
    let o = open(&f, "laptop", PEER).await;
    let wrong = format!("dps_{}", "0".repeat(96));
    let unknown_id = "5b0f2c2e-9a53-4a53-8f0e-2f9d2c1d0a11";
    let long_id = "x".repeat(100);
    // (id, headers) for every refusal; each is sent to its own router so the poll limit is not met.
    let id = o.id.as_str();
    let bearer_ok = format!("Bearer {}", o.secret);
    let basic = format!("Basic {}", o.secret);
    let raw_secret = o.secret.clone();
    let bearer_wrong = format!("Bearer {wrong}");
    let too_long = format!("Bearer dps_{}", "x".repeat(300));
    let cases: Vec<(&str, String, Vec<(&str, &str)>)> = vec![
        ("wrong secret", id.into(), vec![("authorization", bearer_wrong.as_str())]),
        ("no header", id.into(), vec![]),
        ("basic scheme", id.into(), vec![("authorization", basic.as_str())]),
        ("no scheme", id.into(), vec![("authorization", raw_secret.as_str())]),
        ("bearer without secret", id.into(), vec![("authorization", "Bearer")]),
        ("bearer with empty secret", id.into(), vec![("authorization", "Bearer ")]),
        ("two headers", id.into(), vec![("authorization", bearer_ok.as_str()), ("authorization", bearer_ok.as_str())]),
        ("unknown id, right secret", unknown_id.into(), vec![("authorization", bearer_ok.as_str())]),
        ("unknown id, no header", unknown_id.into(), vec![]),
        ("over-long id", long_id, vec![("authorization", bearer_ok.as_str())]),
        ("over-long secret", id.into(), vec![("authorization", too_long.as_str())]),
    ];
    let mut seen: Vec<(&str, Reply)> = Vec::new();
    for (what, uri_id, headers) in &cases {
        let app = f.open_app();
        seen.push((what, send(&app, PEER, "GET", &format!("/enrol/status/{uri_id}"), headers, Vec::new()).await));
    }
    let first = &seen[0].1;
    assert_eq!(first.0, StatusCode::NOT_FOUND);
    assert_eq!(json_of(&first.2), not_found_body());
    for (what, r) in &seen {
        assert_eq!(r.0, first.0, "{what}: status");
        assert_eq!(r.2, first.2, "{what}: body");
        assert_eq!(pairs(&r.1), pairs(&first.1), "{what}: headers");
    }
    // The control: the right secret on the same enrolment is not refused.
    let (st, _, body) = poll(&f.open_app(), id, Some(&o.secret)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "status": "waiting" }));
}

#[tokio::test]
async fn fr_c2_a_secret_in_the_query_string_is_not_honoured() {
    let f = fx();
    let o = open(&f, "laptop", PEER).await;
    let missing = poll(&f.open_app(), &o.id, None).await;
    assert_eq!(missing.0, StatusCode::NOT_FOUND);
    // A secret only in the query, under any name: the same refusal as no secret at all.
    for q in [
        format!("poll_secret={}", o.secret),
        format!("secret={}", o.secret),
        format!("token={}", o.secret),
        format!("access_token={}", o.secret),
        format!("authorization=Bearer%20{}", o.secret),
    ] {
        let r = poll_at(&f.open_app(), PEER, &format!("/enrol/status/{}?{q}", o.id), None).await;
        assert_eq!(r.0, missing.0, "{q}");
        assert_eq!(r.2, missing.2, "{q}");
        assert_eq!(pairs(&r.1), pairs(&missing.1), "{q}");
    }
    // The header wins and the query is ignored, both ways.
    let r = poll_at(&f.open_app(), PEER, &format!("/enrol/status/{}?poll_secret=dps_wrong", o.id), Some(&o.secret)).await;
    assert_eq!(r.0, StatusCode::OK);
    assert_eq!(json_of(&r.2), json!({ "status": "waiting" }));
    let wrong = format!("dps_{}", "1".repeat(96));
    let r = poll_at(&f.open_app(), PEER, &format!("/enrol/status/{}?poll_secret={}", o.id, o.secret), Some(&wrong)).await;
    assert_eq!(r.0, StatusCode::NOT_FOUND);
    assert_eq!(r.2, missing.2);
}

#[tokio::test]
async fn fr_c2_poll_after_deny_and_after_expiry_say_so_without_a_credential() {
    let f = fx();
    // Denied by the owner.
    let o = open(&f, "denied", PEER).await;
    let (st, _) = owner(&f, "POST", &api(&o.id, "/deny"), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _, body) = poll(&f.open_app(), &o.id, Some(&o.secret)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "status": "denied" }));
    // Expired: minted and redeemed (through the core) in the past, so the confirm deadline has
    // passed when the door looks at it with the real clock.
    let past = now_ms() - 700_000;
    let minted = f.enrol.mint("old", past, None, BASE).unwrap();
    let r = f.enrol.redeem(minted.code.expose(), &NodeInfo::minimal("old-host"), past + 1, "192.0.2.77").unwrap();
    let (st, _, body) = poll(&f.open_app(), &minted.id, Some(r.poll_secret.expose())).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body), json!({ "status": "expired" }));
    let t = text(&body);
    assert!(!t.contains("credential") && !t.contains("node_token"), "{t}");
    assert_eq!(f.fake.count(), 0);
}

// ---- poll: the limit ----

#[test]
fn fr_c2_poll_is_limited_to_one_per_two_seconds_per_enrol_id() {
    assert_eq!(ENROL_POLL_INTERVAL, Duration::from_secs(2));
    let l = PollLimiter::new();
    let t0 = Instant::now();
    assert_eq!(l.check_at("a", t0), Ok(()));
    // Inside the interval: refused, with the time left. A refused poll is not recorded.
    assert_eq!(l.check_at("a", t0 + Duration::from_millis(500)), Err(Duration::from_millis(1500)));
    assert_eq!(l.check_at("a", t0 + Duration::from_millis(1999)), Err(Duration::from_millis(1)));
    assert_eq!(l.check_at("a", t0), Err(Duration::from_secs(2)), "the same instant is inside the interval");
    // Exactly at the interval it passes, and the new interval starts there.
    assert_eq!(l.check_at("a", t0 + Duration::from_secs(2)), Ok(()));
    assert!(l.check_at("a", t0 + Duration::from_millis(3999)).is_err());
    assert_eq!(l.check_at("a", t0 + Duration::from_secs(4)), Ok(()));
    // Another enrolment has its own interval.
    assert_eq!(l.check_at("b", t0 + Duration::from_millis(100)), Ok(()));
    assert_eq!(l.len(), 2);
}

#[tokio::test]
async fn fr_c2_the_second_immediate_poll_on_the_door_is_429_with_retry_after() {
    let f = fx();
    let o = open(&f, "laptop", PEER).await;
    // One router for both polls, so they share the limiter. Real clock, no sleeping: the two
    // polls are well inside two seconds of each other.
    let app = f.open_app();
    let first = poll(&app, &o.id, Some(&o.secret)).await;
    assert_eq!(first.0, StatusCode::OK);
    let second = poll(&app, &o.id, Some(&o.secret)).await;
    assert_eq!(second.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_of(&second.2), json!({ "error": "too many requests", "code": "rate_limited" }));
    let wait: u64 = second.1["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=2).contains(&wait), "{wait}");
    // The limit is per enrolment: another enrolment on the same router is answered.
    let o2 = open(&f, "second", PEER).await;
    assert_eq!(poll(&app, &o2.id, Some(&o2.secret)).await.0, StatusCode::OK);
    // An unknown id is never limited (it takes no slot): the same 404 every time, never a 429.
    for _ in 0..3 {
        let unknown = poll(&app, "5b0f2c2e-9a53-4a53-8f0e-2f9d2c1d0a11", Some(&o.secret)).await;
        assert_eq!(unknown.0, StatusCode::NOT_FOUND);
    }
    // A wrong secret on the limited enrolment is the 404, not the 429: it is refused before the limiter.
    let wrong = format!("dps_{}", "0".repeat(96));
    assert_eq!(poll(&app, &o.id, Some(&wrong)).await.0, StatusCode::NOT_FOUND);
    // The refused poll did not touch the enrolment: the secret still works on a fresh router.
    assert_eq!(poll(&f.open_app(), &o.id, Some(&o.secret)).await.0, StatusCode::OK);
}

#[test]
fn fr_c2_the_poll_limiter_is_bounded_and_fails_closed() {
    assert_eq!(ENROL_POLL_MAX_ENTRIES, 1024);
    assert_eq!(ENROL_POLL_ENTRY_TTL, Duration::from_secs(60));
    let l = PollLimiter::new();
    let t0 = Instant::now();
    for i in 0..ENROL_POLL_MAX_ENTRIES {
        assert_eq!(l.check_at(&format!("id{i}"), t0 + Duration::from_millis(i as u64)), Ok(()), "{i}");
    }
    assert_eq!(l.len(), ENROL_POLL_MAX_ENTRIES);
    // The 1025th id is refused, and the table did not grow. The wait is at least one second.
    let wait = l.check_at("extra", t0 + Duration::from_secs(10)).unwrap_err();
    assert!(wait >= Duration::from_secs(1) && wait <= ENROL_POLL_ENTRY_TTL, "{wait:?}");
    assert_eq!(l.len(), ENROL_POLL_MAX_ENTRIES);
    // Still refused while every entry is younger than the TTL.
    assert!(l.check_at("extra", t0 + Duration::from_secs(59)).is_err());
    // An id already in the table is not new and is admitted when its interval is over.
    assert_eq!(l.check_at("id5", t0 + Duration::from_secs(12)), Ok(()));
    assert_eq!(l.len(), ENROL_POLL_MAX_ENTRIES);
    // At 60 s the first entry (inserted at t0) is as old as the TTL and is pruned on the insert, which then fits.
    assert_eq!(l.check_at("extra", t0 + Duration::from_secs(60)), Ok(()));
    assert_eq!(l.len(), ENROL_POLL_MAX_ENTRIES);
    // Full again: the next new id is refused.
    assert!(l.check_at("extra2", t0 + Duration::from_secs(60)).is_err());
    // Once the rest have aged past the TTL they are pruned and the table admits again.
    assert_eq!(l.check_at("extra2", t0 + Duration::from_millis(61_100)), Ok(()));
    assert!(l.len() < ENROL_POLL_MAX_ENTRIES, "{}", l.len());
    assert!(l.len() <= 3, "only the recent entries are left: {}", l.len());
}

#[tokio::test]
async fn fr_c2_wrong_secret_polls_never_take_a_poll_limiter_slot() {
    let f = fx();
    let o = open(&f, "laptop", PEER).await;
    // One router for everything, so one limiter. 1500 invented ids (more than the 1024 slots),
    // alternately with a wrong secret and with none: every one is the same 404 and none is a 429.
    let app = f.open_app();
    let wrong = format!("dps_{}", "0".repeat(96));
    for i in 0..1500 {
        let id = format!("invented-{i}");
        let secret = if i % 2 == 0 { Some(wrong.as_str()) } else { None };
        let r = poll(&app, &id, secret).await;
        assert_eq!(r.0, StatusCode::NOT_FOUND, "{i}");
        assert_eq!(json_of(&r.2), not_found_body(), "{i}");
    }
    // A wrong secret on the real id takes no slot either.
    assert_eq!(poll(&app, &o.id, Some(&wrong)).await.0, StatusCode::NOT_FOUND);
    // The legitimate node's first poll is admitted (the table is not full of invented ids; with
    // the old order it would be a 429), and only now does the real id take its slot.
    let first = poll(&app, &o.id, Some(&o.secret)).await;
    assert_eq!(first.0, StatusCode::OK, "{}", text(&first.2));
    assert_eq!(json_of(&first.2), json!({ "status": "waiting" }));
    assert_eq!(poll(&app, &o.id, Some(&o.secret)).await.0, StatusCode::TOO_MANY_REQUESTS);
    // The limiter's len() is not reachable from the door (it lives in the private routes state),
    // so the property is asserted by behaviour: see above.
}

#[tokio::test]
async fn fr_c2_a_rate_limited_poll_does_not_consume_the_credential() {
    let f = fx();
    let o = open(&f, "laptop", PEER).await;
    let app = f.open_app();
    // An admitted poll before the confirm takes the slot.
    assert_eq!(poll(&app, &o.id, Some(&o.secret)).await.0, StatusCode::OK);
    let (st, c) = owner(&f, "POST", &api(&o.id, "/confirm"), Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    // Inside the interval, on the same router: 429, with Retry-After and no credential.
    let limited = poll(&app, &o.id, Some(&o.secret)).await;
    assert_eq!(limited.0, StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.1.contains_key("retry-after"));
    assert!(!text(&limited.2).contains("node_token"), "{}", text(&limited.2));
    let again = poll(&app, &o.id, Some(&o.secret)).await;
    assert_eq!(again.0, StatusCode::TOO_MANY_REQUESTS);
    // The credential was not taken: the owner still sees awaiting_confirmation, not joined.
    let (_, s) = owner(&f, "GET", &api(&o.id, ""), None).await;
    assert_eq!(s["status"], "awaiting_confirmation", "{s}");
    // The clock cannot be injected into the handler, so "after 2 s" is a second router (a fresh
    // limiter): the next admitted poll delivers the credential, once.
    let token = f.fake.token(0);
    let (st, _, body) = poll(&f.open_app(), &o.id, Some(&o.secret)).await;
    assert_eq!(st, StatusCode::OK);
    let b = json_of(&body);
    assert_eq!(b["status"], "joined", "{b}");
    assert_eq!(b["credential"]["node_token"], token.as_str());
    assert_eq!(f.fake.count(), 1);
}

// ---- the route table and the classes of the limiter ----

#[tokio::test]
async fn fr_c2_the_trailing_slash_redeem_is_404_and_not_the_strict_class() {
    let f = fx();
    let (id, code) = mint(&f, "slash").await;
    let body = json!({ "code": code, "node_public_info": node_info() }).to_string();
    // A tight strict budget (2) and a loose one (5): the trailing-slash path must run on the loose one.
    let cfg = LimitConfig { accept_per_minute: 2, accept_per_hour: 100, other_per_minute: 5, ..big_cfg() };
    let app = f.limited(cfg);
    for i in 0..5 {
        let r = send(&app, PEER, "POST", "/enrol/redeem/", &[("content-type", "application/json")], body.clone().into_bytes()).await;
        assert_eq!(r.0, StatusCode::NOT_FOUND, "{i}");
        assert!(r.2.is_empty(), "the plain 404, no JSON");
    }
    // The 6th is over the loose budget (the strict one would have refused at the 3rd).
    let r = send(&app, PEER, "POST", "/enrol/redeem/", &[("content-type", "application/json")], body.clone().into_bytes()).await;
    assert_eq!(r.0, StatusCode::TOO_MANY_REQUESTS);
    // The handler was never reached: the code is untouched and no failure was recorded for the address.
    let (_, s) = owner(&f, "GET", &api(&id, ""), None).await;
    assert_eq!(s["status"], "waiting");
    // Other lookalikes are 404 too and never reach the handler.
    let app = f.limited(big_cfg());
    for (m, p) in [
        ("POST", "/enrol/redeem/"),
        ("POST", "/ENROL/redeem"),
        ("POST", "/Enrol/Redeem"),
        ("POST", "//enrol/redeem"),
        ("POST", "/enrol//redeem"),
        ("POST", "/enrol/redeem%2f"),
        ("POST", "/enrol/redeem/x"),
        ("GET", "/enrol/redeem"),
        ("PUT", "/enrol/redeem"),
        ("POST", "/enrol/status/x"),
        ("GET", "/enrol/status"),
        ("GET", "/enrol/status/"),
        ("GET", "/enrol/status/x/"),
    ] {
        let r = send(&app, "203.0.113.77", m, p, &[("content-type", "application/json")], body.clone().into_bytes()).await;
        // `/enrol/status/` with an empty id may match the route with an empty segment (axum does
        // not match an empty segment), so every case here is the plain 404.
        assert_eq!(r.0, StatusCode::NOT_FOUND, "{m} {p}");
        assert!(r.2.is_empty(), "{m} {p}: {}", text(&r.2));
    }
    let (_, s) = owner(&f, "GET", &api(&id, ""), None).await;
    assert_eq!(s["status"], "waiting", "no lookalike redeemed the code");
    // The real path with the same body redeems it.
    assert_eq!(redeem(&app, "203.0.113.77", &code).await.0, StatusCode::OK);
}

#[tokio::test]
async fn fr_c2_redeem_with_a_query_string_is_still_the_strict_class() {
    let f = fx();
    // Strict budget 3, loose budget large: the query must not move the request to the loose class.
    let cfg = LimitConfig { accept_per_minute: 3, accept_per_hour: 100, other_per_minute: 1000, ..big_cfg() };
    let app = f.limited(cfg);
    let body = json!({ "code": unknown_code('a'), "node_public_info": node_info() }).to_string();
    let h = [("content-type", "application/json")];
    // Two with a query, one without: all reach the handler (403, the code is unknown) ...
    for uri in ["/enrol/redeem?x=1", "/enrol/redeem", "/enrol/redeem?code=ignored&x=2"] {
        let r = send(&app, PEER, "POST", uri, &h, body.clone().into_bytes()).await;
        assert_eq!(r.0, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(json_of(&r.2), forbidden_body(), "{uri}");
    }
    // ... and the next one, with a query, is the strict class's 429 (the door's own text).
    let r = send(&app, PEER, "POST", "/enrol/redeem?x=3", &h, body.clone().into_bytes()).await;
    assert_eq!(r.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_of(&r.2), json!({ "error": "too many requests", "code": "rate_limited" }));
    assert!(r.1.contains_key("retry-after"));
    // The query never carries the code: a valid code only in the query is a bad request, not a redeem.
    let g = fx();
    let (id, code) = mint(&g, "query-code").await;
    let r = send(&g.open_app(), PEER, "POST", &format!("/enrol/redeem?code={code}"), &h, b"{}".to_vec()).await;
    assert_eq!(r.0, StatusCode::BAD_REQUEST);
    let (_, s) = owner(&g, "GET", &api(&id, ""), None).await;
    assert_eq!(s["status"], "waiting");
}

#[tokio::test]
async fn fr_c2_the_door_and_the_owner_routes_share_one_enrol() {
    let f = fx();
    // Mint through the owner router.
    let (id, code) = mint(&f, "shared").await;
    // Redeem on the door, a router built from the same Arc.
    let (st, _, body) = redeem(&f.open_app(), PEER, &code).await;
    assert_eq!(st, StatusCode::OK, "{}", text(&body));
    let secret = json_of(&body)["poll_secret"].as_str().unwrap().to_string();
    // The owner sees the redeem the door made: the node's own description and the address.
    let (_, s) = owner(&f, "GET", &api(&id, ""), None).await;
    assert_eq!(s["status"], "awaiting_confirmation");
    assert_eq!(s["node_info"]["hostname"], "ibrew-laptop");
    assert_eq!(s["observed_addr"], PEER);
    // The owner confirms; the door's poll, on a router made after, delivers the token the confirm minted.
    let (st, c) = owner(&f, "POST", &api(&id, "/confirm"), Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let (st, _, body) = poll(&f.open_app(), &id, Some(&secret)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(json_of(&body)["credential"]["node_token"], f.fake.token(0));
    // The failure books are shared as well: failures made on the door are the owner instance's.
    let g = fx();
    for _ in 0..MAX_FAILURES {
        let _ = redeem(&g.open_app(), "192.0.2.50", &unknown_code('9')).await;
    }
    let (_, valid) = mint(&g, "after-failures").await;
    assert!(matches!(
        g.enrol.redeem(&valid, &NodeInfo::minimal("h"), now_ms(), "192.0.2.50"),
        Err(ember_server::computers::enrol::Refused::RateLimited)
    ));
}

#[tokio::test]
async fn fr_c2_the_eleventh_redeem_in_a_minute_from_one_address_is_429() {
    let f = fx();
    // The real limits of the served door: strict 10 a minute, global burst 10.
    let app = f.app();
    assert_eq!(ENROL_ACCEPT_PER_MINUTE, 10);
    assert!(ENROL_ACCEPT_PER_HOUR >= ENROL_ACCEPT_PER_MINUTE);
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        let r = redeem(&app, PEER, &unknown_code('a')).await;
        assert_eq!(r.0, StatusCode::FORBIDDEN, "{i}");
    }
    // The per-address check comes first, so it is the door's 429 (not the core's text, and not a 503).
    let r = redeem(&app, PEER, &unknown_code('a')).await;
    assert_eq!(r.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_of(&r.2), json!({ "error": "too many requests", "code": "rate_limited" }));
    assert!(r.1.contains_key("retry-after"));
}
