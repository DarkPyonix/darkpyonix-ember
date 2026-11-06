//! FR-T14: the ORDER of the guest door's layers, pinned before the mixed items move into
//! `door/` (step C0). `GuestDoor::build` stacks, innermost first: the body limit, the Host/Origin
//! guard, the request timeout, the limit layer (only for `wrap_limited`), the response headers.
//! So a request meets: response headers (outermost), limits, timeout, guard, body limit, route.
//!
//! What `fr_t14_guest_door_characterization.rs` already pins and is NOT repeated here:
//! a bad Host from an address with no per-address budget left is 429, not 403 (its
//! `fr_t14_char_security_headers_on_every_kind_of_answer`), a good Host with no budget is 429
//! (`fr_t14_guest_limits.rs`), a good Host with an empty global bucket or no in-flight permit is 503.
//!
//! Every expected value was read from `GuestDoor::build`, `limit_layer`, `guard_layer` and
//! `timeout_layer` in the code, and NONE of these tests has been run when this file was written
//! (nothing was compiled). A behaviour that looks odd is pinned as it is and marked
//! `CHARACTERISES CURRENT BEHAVIOUR (not a requirement)`. Details are [provisional].
//!
//! Not driven: the timeout layer with an injected short timeout. `build` fixes it to
//! `REQUEST_TIMEOUT` (10 s) and is private, `with_timeout` can only wrap the whole door from
//! outside, and tokio's `test-util` (paused time) is not enabled in `Cargo.toml` and no dependency
//! may be added. The one timeout test below therefore waits the real `REQUEST_TIMEOUT`, bounded
//! by an outer 15 s timeout, and uses no sleep (the handler never answers).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use ember_server::guest_door::{DoorLimits, GuestDoor, LimitConfig, ACCEPT_PATH, MAX_BODY_BYTES};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "127.0.0.1:8740";
const LISTEN: &str = "127.0.0.1:9000";
const HOST: &[u8] = b"127.0.0.1:9000";
const FOREIGN: &[u8] = b"evil.example";

fn door() -> GuestDoor {
    let m: HashMap<String, String> = [("EMBER_GUEST_LISTEN".to_string(), LISTEN.to_string())].into_iter().collect();
    GuestDoor::from_lookup(|k| m.get(k).cloned(), OWNER.parse::<SocketAddr>().unwrap())
        .map_err(|e| e.to_string())
        .unwrap()
        .expect("door on")
}

/// A small router in place of the guest router: the door's layers are what is under test.
fn site() -> Router {
    Router::new()
        .route("/ok", get(|| async { "ok" }).post(|| async { "posted" }))
        .route(ACCEPT_PATH, post(|| async { "accepted" }))
        .route("/echo", post(|b: Bytes| async move { b.len().to_string() }))
        .route("/hang", get(|| async { std::future::pending::<&'static str>().await }))
}

fn limited(cfg: LimitConfig) -> Router {
    door().wrap_limited(site(), Arc::new(DoorLimits::new(cfg, Vec::new())))
}

type Reply = (StatusCode, HeaderMap, Vec<u8>);

async fn call(app: &Router, method: &str, uri: &str, headers: &[(&str, &[u8])], peer: Option<&str>) -> Reply {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, HeaderValue::from_bytes(v).unwrap());
    }
    let mut req = req.body(Body::empty()).unwrap();
    if let Some(p) = peer {
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(p.parse::<IpAddr>().unwrap(), 40000)));
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = res.into_parts();
    (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
}

fn json_of(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|e| panic!("not json ({e}): {:?}", String::from_utf8_lossy(body)))
}

fn hv(h: &HeaderMap, name: &str) -> String {
    h.get(name).unwrap_or_else(|| panic!("header {name} missing")).to_str().unwrap().to_string()
}

fn big() -> String {
    (MAX_BODY_BYTES + 1).to_string()
}

// ---- limits run before the guard ----

/// `limit_layer`: the per-address check passes for a fresh address, `bucket.try_take_at` fails,
/// the answer is `busy()` (503), and the guard never runs, so the foreign Host is not 403.
#[tokio::test]
async fn fr_t14_order_an_empty_global_bucket_is_503_even_for_a_foreign_host() {
    let a = limited(LimitConfig { global_burst: 1.0, global_per_second: 0.0, ..LimitConfig::default() });
    assert_eq!(call(&a, "GET", "/ok", &[("host", HOST)], Some("203.0.113.1")).await.0, StatusCode::OK);
    let (s, h, body) = call(&a, "GET", "/ok", &[("host", FOREIGN)], Some("203.0.113.2")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(hv(&h, "retry-after"), "1");
    assert_eq!(json_of(&body), json!({ "error": "busy", "code": "busy" }));
}

/// `limit_layer`: `try_acquire_owned` on a semaphore of 0 permits fails, `busy()`, no guard.
#[tokio::test]
async fn fr_t14_order_no_in_flight_permit_is_503_even_for_a_foreign_host() {
    let a = limited(LimitConfig { max_in_flight: 0, ..LimitConfig::default() });
    let (s, _, body) = call(&a, "GET", "/ok", &[("host", FOREIGN)], Some("203.0.113.3")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_of(&body), json!({ "error": "busy", "code": "busy" }));
}

/// `limit_layer` answers 500 `no peer address` before anything else, so a foreign Host without a
/// peer address is 500, not 403. `response_headers` is the outermost layer, so the 500 carries
/// both door headers.
#[tokio::test]
async fn fr_t14_order_no_peer_address_is_500_with_the_door_headers_even_for_a_foreign_host() {
    let a = limited(LimitConfig::default());
    let (s, h, body) = call(&a, "GET", "/ok", &[("host", FOREIGN)], None).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(json_of(&body), json!({ "error": "no peer address", "code": "internal" }));
    assert_eq!(hv(&h, "x-content-type-options"), "nosniff");
    assert_eq!(hv(&h, "cache-control"), "no-store");
}

/// An over-limit `Content-Length` from an address with no budget left: the limit layer answers
/// 429 before the guard's size check (which would be 413).
#[tokio::test]
async fn fr_t14_order_an_oversized_declared_body_from_a_limited_address_is_429_not_413() {
    let a = limited(LimitConfig { other_per_minute: 1, ..LimitConfig::default() });
    assert_eq!(call(&a, "GET", "/ok", &[("host", HOST)], Some("203.0.113.4")).await.0, StatusCode::OK);
    let len = big();
    let (s, _, _) = call(&a, "POST", "/ok", &[("host", HOST), ("content-length", len.as_bytes())], Some("203.0.113.4")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    // The same request from a fresh address reaches the guard: 413.
    let (s, _, _) = call(&a, "POST", "/ok", &[("host", HOST), ("content-length", len.as_bytes())], Some("203.0.113.5")).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

/// CHARACTERISES CURRENT BEHAVIOUR (not a requirement): the limit layer is outside the guard, so a
/// request the guard refuses (403) has already spent the per-address budget and the global token.
#[tokio::test]
async fn fr_t14_order_a_request_the_guard_refuses_has_already_spent_the_budgets() {
    // Per address: other_per_minute 2 are spent by two foreign-Host requests (both 403), the third
    // request, with a good Host, is 429.
    let a = limited(LimitConfig { other_per_minute: 2, ..LimitConfig::default() });
    for i in 0..2 {
        assert_eq!(call(&a, "GET", "/ok", &[("host", FOREIGN)], Some("203.0.113.6")).await.0, StatusCode::FORBIDDEN, "{i}");
    }
    assert_eq!(call(&a, "GET", "/ok", &[("host", HOST)], Some("203.0.113.6")).await.0, StatusCode::TOO_MANY_REQUESTS);
    // Global: a burst of 2 and no refill are spent by two foreign-Host requests from two fresh
    // addresses (both 403), the third request from a third address is 503.
    let a = limited(LimitConfig { global_burst: 2.0, global_per_second: 0.0, ..LimitConfig::default() });
    assert_eq!(call(&a, "GET", "/ok", &[("host", FOREIGN)], Some("203.0.113.7")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&a, "GET", "/ok", &[("host", FOREIGN)], Some("203.0.113.8")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&a, "GET", "/ok", &[("host", HOST)], Some("203.0.113.9")).await.0, StatusCode::SERVICE_UNAVAILABLE);
}

// ---- the guard runs before the body limit ----

/// `guard_layer` checks, in this order: Host, Origin, method (HEAD, TRACE, CONNECT), declared
/// size, Upgrade. A request that fails an earlier check and also the size check gets the earlier
/// answer.
#[tokio::test]
async fn fr_t14_order_the_guard_checks_host_and_origin_before_the_declared_size() {
    let a = door().wrap(site());
    let len = big();
    let (s, _, body) = call(&a, "POST", "/ok", &[("host", FOREIGN), ("content-length", len.as_bytes())], None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(json_of(&body), json!({ "error": "host not allowed", "code": "forbidden" }));
    let (s, _, body) =
        call(&a, "POST", "/ok", &[("host", HOST), ("origin", b"null"), ("content-length", len.as_bytes())], None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(json_of(&body), json!({ "error": "origin not allowed", "code": "forbidden" }));
}

/// CHARACTERISES CURRENT BEHAVIOUR (not a requirement): the method check is before the size
/// check, so a HEAD or TRACE with an over-limit `Content-Length` is 405, not 413.
#[tokio::test]
async fn fr_t14_order_the_method_check_is_before_the_declared_size() {
    let a = door().wrap(site());
    let len = big();
    for m in ["HEAD", "TRACE"] {
        let s = call(&a, m, "/ok", &[("host", HOST), ("content-length", len.as_bytes())], None).await.0;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "{m}");
    }
}

// ---- the timeout wraps the guard and the route ----

/// A route that never answers is cut by `timeout_layer` after `REQUEST_TIMEOUT` (10 s, real time):
/// 408 with the JSON `{"error":"request timed out","code":"timeout"}`. The response-headers layer
/// is outside the timeout, so the 408 has both door headers. The limit layer is outside the
/// timeout too, so the in-flight permit is free again afterwards (`max_in_flight` 1: the next
/// request is served, not 503). No sleep in the test; the outer 15 s timeout only bounds a failure.
#[tokio::test]
async fn fr_t14_order_a_route_that_never_answers_gets_the_408_json_with_the_door_headers() {
    let a = limited(LimitConfig { max_in_flight: 1, ..LimitConfig::default() });
    let (s, h, body) = tokio::time::timeout(
        Duration::from_secs(15),
        call(&a, "GET", "/hang", &[("host", HOST)], Some("203.0.113.10")),
    )
    .await
    .expect("the request timeout cut the route within 15 s");
    assert_eq!(s, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(json_of(&body), json!({ "error": "request timed out", "code": "timeout" }));
    assert_eq!(hv(&h, "x-content-type-options"), "nosniff");
    assert_eq!(hv(&h, "cache-control"), "no-store");
    // The permit was released when the 408 left the limit layer.
    assert_eq!(call(&a, "GET", "/ok", &[("host", HOST)], Some("203.0.113.10")).await.0, StatusCode::OK);
}
