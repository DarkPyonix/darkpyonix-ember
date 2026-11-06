//! FR-C2e, FR-C2f: the enrol door's guards and limits (step C2, all [provisional]), and its route
//! table (step C3a: exactly `POST /enrol/redeem` and `GET /enrol/status/{enrol_id}`, 404 for the
//! rest). The tests of the layers drive them over an EMPTY router (`Router::new()`), so what they
//! count and refuse does not depend on the handlers; the routes themselves are tested in
//! `fr_c2_enrol_redeem.rs`.
//!
//! NONE of these tests has been run (nothing was compiled when this file was written). Every
//! expected value was read from `ember/server/src/enrol_door.rs` and the shared `door/` code.
//! Modelled on `fr_t14_guest_door*.rs` and `fr_t14_guest_limits.rs`: the guards are driven in
//! process with `oneshot` (a `ConnectInfo` put in the request, as the serve loop does), the accept
//! loop over `127.0.0.1:0` sockets. No fixed sleeps: bounded polling only. No tracing capture here
//! (a capture test lives alone in its own file, #549).
//!
//! Not written, and why:
//! - `fr_c2_slow_request_is_408`: the request timeout is the fixed `ENROL_REQUEST_TIMEOUT` (10 s)
//!   and cannot be injected, tokio's `test-util` is not enabled and no dependency may be added.
//!   The shared timeout layer is pinned (with a real 10 s wait) by `fr_t14_guest_door_order.rs`.
//! - `fr_c2_enrol_door_is_its_own_task_so_its_failure_never_stops_the_owner`: that is wiring in
//!   `main.rs` (a `tokio::spawn` whose error is only logged, on the stop watch of the guest task).
//!   It is not testable without running the binary; it is covered by review of `main.rs`.
//! - A log test for the startup lines: the lines are in `main.rs`, not in the library.
//! - The body limit for a body with no `Content-Length` is observed in `fr_c2_enrol_redeem.rs`
//!   (the redeem handler reads the body; here only the declared-size guard is observable).
//!
//! C3a changed `fr_c2_enrol_door_serves_exactly_the_route_table` and the inventory test for the
//! two routes and the `Arc<Enrol>` argument. Not run: nothing was compiled.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::get;
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::auth::{self, ServerAuth};
use ember_server::computers::enrol::Enrol;
use ember_server::computers::{self, Computers, Registry};
use ember_server::enrol_door::{
    DoorLimits, EnrolDoor, LimitConfig, ENROL_ACCEPT_PER_HOUR, ENROL_ACCEPT_PER_MINUTE, ENROL_ALLOWED_HOSTS_VAR,
    ENROL_ALLOW_REMOTE_VAR, ENROL_GLOBAL_BURST, ENROL_GLOBAL_PER_SECOND, ENROL_HEADER_READ_TIMEOUT, ENROL_LISTEN_VAR,
    ENROL_MAX_BODY_BYTES, ENROL_MAX_CONNECTIONS, ENROL_MAX_IN_FLIGHT, ENROL_MAX_TRACKED_ADDRS, ENROL_OTHER_PER_MINUTE,
    ENROL_PUBLIC_URL_VAR, ENROL_REDEEM_PATH, ENROL_REQUEST_TIMEOUT, ENROL_TRUSTED_PROXIES_VAR,
};
use ember_server::guest_door::{GuestDoor, ACCEPT_PATH};
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "127.0.0.1:8740";
const GUEST: &str = "127.0.0.1:9100";
const LISTEN: &str = "127.0.0.1:9300";
const HOST: &str = "127.0.0.1:9300";

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

fn lookup(vars: &[(&str, &str)], guest: Option<&str>) -> Result<Option<EnrolDoor>, String> {
    let m: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    EnrolDoor::from_lookup(|k| m.get(k).cloned(), sa(OWNER), guest.map(sa)).map_err(|e| e.to_string())
}

/// A loopback door on `LISTEN` with extra variables, the guest door off.
fn door(extra: &[(&str, &str)]) -> EnrolDoor {
    let mut v = vec![(ENROL_LISTEN_VAR, LISTEN)];
    v.extend_from_slice(extra);
    lookup(&v, None).unwrap().expect("door on")
}

/// A door service over an in-memory store, for tests that need a router and nothing else.
fn test_enrol() -> Arc<Enrol> {
    Enrol::new(Arc::new(Store::open_in_memory().unwrap()))
}

type Reply = (StatusCode, HeaderMap, Vec<u8>);

async fn call(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)], peer: Option<&str>) -> Reply {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::empty()).unwrap();
    if let Some(p) = peer {
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(p.parse::<IpAddr>().unwrap(), 40000)));
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = res.into_parts();
    (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
}

/// A request with the right Host, from `peer`.
async fn at(app: &Router, peer: &str, method: &str, uri: &str, extra: &[(&str, &str)]) -> Reply {
    let mut h = vec![("host", HOST)];
    h.extend_from_slice(extra);
    call(app, method, uri, &h, Some(peer)).await
}

fn unlimited(d: &EnrolDoor, routes: Router) -> Router {
    d.wrap(routes)
}

/// The enrol numbers with a global bucket that never runs out, so a test sees the per-address
/// limits alone. Trusted proxies are given by the test.
fn cfg_big_bucket() -> LimitConfig {
    LimitConfig {
        accept_per_minute: ENROL_ACCEPT_PER_MINUTE,
        accept_per_hour: ENROL_ACCEPT_PER_HOUR,
        other_per_minute: ENROL_OTHER_PER_MINUTE,
        global_per_second: 1_000_000.0,
        global_burst: 1_000_000.0,
        max_in_flight: ENROL_MAX_IN_FLIGHT,
        max_addrs: ENROL_MAX_TRACKED_ADDRS,
    }
}

fn with_cfg(d: &EnrolDoor, cfg: LimitConfig, routes: Router) -> Router {
    d.wrap_limited(routes, Arc::new(DoorLimits::with_strict_path(cfg, Vec::new(), ENROL_REDEEM_PATH)))
}

async fn eventually<F, Fut>(timeout: Duration, what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if cond().await {
            return;
        }
        if Instant::now() >= deadline {
            panic!("condition not met within {timeout:?}: {what}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn raw(addr: SocketAddr, request: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

fn json_of(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|e| panic!("not json ({e}): {:?}", String::from_utf8_lossy(body)))
}

// ---- configuration ----

#[test]
fn fr_c2_enrol_door_is_off_without_the_variable() {
    assert!(lookup(&[], None).unwrap().is_none());
    assert!(lookup(&[(ENROL_LISTEN_VAR, "")], None).unwrap().is_none());
    assert!(lookup(&[(ENROL_LISTEN_VAR, "   ")], None).unwrap().is_none());
    // Settings of a door that is off do not turn it on, and the guest variables are not read.
    assert!(lookup(&[(ENROL_PUBLIC_URL_VAR, "https://e.example.com")], None).unwrap().is_none());
    assert!(lookup(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9100")], Some(GUEST)).unwrap().is_none());
    // A guest door that is on makes no difference to an enrol door that is off.
    assert!(lookup(&[], Some(GUEST)).unwrap().is_none());
}

#[test]
fn fr_c2_bad_enrol_listen_is_a_startup_error() {
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, "not-an-address")], None).unwrap_err(),
        r#"EMBER_ENROL_LISTEN="not-an-address" is not an ip:port address"#
    );
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, "9300")], None).unwrap_err(),
        r#"EMBER_ENROL_LISTEN="9300" is not an ip:port address"#
    );
    // The value is trimmed before it is parsed and quoted.
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, " localhost:9300 ")], None).unwrap_err(),
        r#"EMBER_ENROL_LISTEN="localhost:9300" is not an ip:port address"#
    );
}

#[test]
fn fr_c2_enrol_listen_equal_to_owner_or_guest_listen_is_a_startup_error() {
    // The owner's address.
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, OWNER)], None).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_LISTEN (127.0.0.1:8740)"
    );
    // The guest door's address, when that door is on.
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, GUEST)], Some(GUEST)).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_GUEST_LISTEN (127.0.0.1:9100)"
    );
    // The guest door is off: its would-be address is free.
    assert!(lookup(&[(ENROL_LISTEN_VAR, GUEST)], None).unwrap().is_some());
    // Both clash: the order of `others` decides, the owner is first (issue #570, hole 4).
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, OWNER)], Some("0.0.0.0:8740")).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_LISTEN (127.0.0.1:8740)"
    );
    // The guest door sitting on the owner's address is no excuse either: still the owner first.
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, OWNER)], Some(OWNER)).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_LISTEN (127.0.0.1:8740)"
    );
    // Clashing only with the guest door while the owner is elsewhere: the guest is named.
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, "127.0.0.1:9100")], Some("0.0.0.0:9100")).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_GUEST_LISTEN (0.0.0.0:9100)"
    );
}

#[test]
fn fr_c2_enrol_listen_clashes_on_an_unspecified_address_with_the_same_port() {
    // An unspecified address on either side takes the port on every interface. The clash check runs
    // before the allow-remote check, so the clash is what is reported.
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, "0.0.0.0:8740")], None).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_LISTEN (127.0.0.1:8740)"
    );
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, "[::]:8740")], None).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_LISTEN (127.0.0.1:8740)"
    );
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, "127.0.0.1:8740")], None).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_LISTEN (127.0.0.1:8740)"
    );
    // The owner on an unspecified address and the enrol door on a loopback address, same port.
    let m: HashMap<String, String> = [(ENROL_LISTEN_VAR.to_string(), "127.0.0.1:8740".to_string())].into_iter().collect();
    let e = EnrolDoor::from_lookup(|k| m.get(k).cloned(), sa("0.0.0.0:8740"), None).unwrap_err().to_string();
    assert_eq!(e, "EMBER_ENROL_LISTEN must differ from EMBER_LISTEN (0.0.0.0:8740)");
    // The guest door on an unspecified address, same port.
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, "127.0.0.1:9100")], Some("[::]:9100")).unwrap_err(),
        "EMBER_ENROL_LISTEN must differ from EMBER_GUEST_LISTEN ([::]:9100)"
    );
    // Another port does not clash.
    assert!(lookup(&[(ENROL_LISTEN_VAR, "127.0.0.1:8741")], Some(GUEST)).unwrap().is_some());
}

fn remote_text(addr: &str) -> String {
    format!(
        "EMBER_ENROL_LISTEN={addr} is not a loopback address: set EMBER_ENROL_LISTEN_ALLOW_REMOTE=1 to expose the \
         enrol door to a network (it speaks plain HTTP: put TLS in front of it)"
    )
}

#[test]
fn fr_c2_enrol_door_refuses_non_loopback_without_allow_remote_and_binds_with_it() {
    for addr in ["0.0.0.0:9300", "[::]:9300", "192.168.1.5:9300", "10.0.0.7:9300", "[::ffff:127.0.0.1]:9300"] {
        for allow in [None, Some("0"), Some(""), Some("true")] {
            let mut v = vec![(ENROL_LISTEN_VAR, addr)];
            if let Some(a) = allow {
                v.push((ENROL_ALLOW_REMOTE_VAR, a));
            }
            assert_eq!(lookup(&v, None).unwrap_err(), remote_text(addr), "{addr} allow={allow:?}");
        }
        let d = lookup(&[(ENROL_LISTEN_VAR, addr), (ENROL_ALLOW_REMOTE_VAR, "1")], None).unwrap().expect("door on");
        assert!(d.is_remote(), "{addr}");
    }
    // The guest door's switch does not open the enrol door.
    let e = lookup(&[(ENROL_LISTEN_VAR, "192.168.1.5:9300"), ("EMBER_GUEST_LISTEN_ALLOW_REMOTE", "1")], None).unwrap_err();
    assert_eq!(e, remote_text("192.168.1.5:9300"));
    // Loopback addresses need no switch.
    for a in ["127.0.0.1:9300", "127.0.0.2:9300", "[::1]:9300"] {
        let d = lookup(&[(ENROL_LISTEN_VAR, a)], None).unwrap().expect("door on");
        assert!(!d.is_remote(), "{a}");
    }
}

#[test]
fn fr_c2_an_invalid_trusted_proxy_or_public_url_is_a_startup_error() {
    assert_eq!(
        lookup(&[(ENROL_LISTEN_VAR, LISTEN), (ENROL_TRUSTED_PROXIES_VAR, "10.0.0.2, nope")], None).unwrap_err(),
        r#"EMBER_ENROL_TRUSTED_PROXIES: "nope" is not an IP address"#
    );
    let e = lookup(&[(ENROL_LISTEN_VAR, LISTEN), (ENROL_PUBLIC_URL_VAR, "https://e.example.com/x")], None).unwrap_err();
    assert!(e.starts_with("EMBER_ENROL_PUBLIC_URL="), "{e}");
    let d = door(&[(ENROL_PUBLIC_URL_VAR, "https://e.example.com/")]);
    assert_eq!(d.public_url.as_deref(), Some("https://e.example.com"));
}

#[tokio::test]
async fn fr_c2_enrol_door_does_not_read_the_guest_variables_and_the_guest_door_does_not_read_its() {
    // Guest variables set, enrol ones not: a bad guest trusted-proxy entry is not an enrol error.
    let d = door(&[("EMBER_GUEST_TRUSTED_PROXIES", "not-an-ip"), ("EMBER_GUEST_ALLOWED_HOSTS", "evil.example")]);
    let a = unlimited(&d, Router::new());
    // The guest allowed host is not accepted by the enrol door ...
    assert_eq!(call(&a, "GET", "/", &[("host", "evil.example")], None).await.0, StatusCode::FORBIDDEN);
    // ... and the enrol one is.
    let d = door(&[(ENROL_ALLOWED_HOSTS_VAR, "enrol.example")]);
    let a = unlimited(&d, Router::new());
    assert_eq!(call(&a, "GET", "/", &[("host", "enrol.example")], None).await.0, StatusCode::NOT_FOUND);
    // The public URL's host is accepted too.
    let d = door(&[(ENROL_PUBLIC_URL_VAR, "https://pub.example.com")]);
    let a = unlimited(&d, Router::new());
    assert_eq!(call(&a, "GET", "/", &[("host", "pub.example.com")], None).await.0, StatusCode::NOT_FOUND);
    // The guest door does not read the enrol hosts variable either.
    let m: HashMap<String, String> = [
        ("EMBER_GUEST_LISTEN", "127.0.0.1:9000"),
        (ENROL_ALLOWED_HOSTS_VAR, "enrol.example"),
        (ENROL_TRUSTED_PROXIES_VAR, "not-an-ip"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let g = GuestDoor::from_lookup(|k| m.get(k).cloned(), sa(OWNER)).unwrap().expect("guest door on");
    let ga = g.wrap(Router::new());
    assert_eq!(call(&ga, "GET", "/join", &[("host", "enrol.example")], None).await.0, StatusCode::FORBIDDEN);
}

// ---- the route table is empty ----

/// C3a: the table is exactly `POST /enrol/redeem` and `GET /enrol/status/{enrol_id}`. Everything
/// else is 404 with an empty body. Every path OUTSIDE the two registered paths carries exactly the
/// two door headers whatever the method, so those cannot be told apart. A wrong METHOD on a
/// registered path is also a 404 with an empty body, but it also carries `allow`: the router's 405
/// body is replaced by `method_not_allowed_fallback`, which keeps the 405's headers (measured in
/// CI on #584; the author had written 'unverified' for this very behaviour). The two registered
/// paths are public (SPEC FR-C2e, the node calls them), so `allow` leaks nothing new; the
/// supervisor accepted this exception on 2026-11-06. The two routes themselves answer something
/// else (415 for a redeem with no content type, 404 with a JSON body for a poll), which
/// `fr_c2_enrol_redeem.rs` pins in detail.
///
/// THE MATRIX BELOW IS MEASURED, NOT READ: every (method, path) pair prints its header names as a
/// `ROUTE_MATRIX|` line straight to the real stdout (cargo hides the output of a passing test and
/// the harness captures print!, so the line reaches the CI log only this way). The assertion for a
/// wrong method on a registered path is deliberately loose (the two door headers, with or without
/// `allow`) until the matrix has been read from CI and pinned.
#[tokio::test]
async fn fr_c2_enrol_door_serves_exactly_the_route_table() {
    let a = unlimited(&door(&[]), EnrolDoor::routes(test_enrol()));
    let want: BTreeSet<&str> = ["cache-control", "x-content-type-options"].into_iter().collect();
    let paths = [
        "/",
        "/enrol",
        "/enrol/",
        "/enrol/redeem",
        "/enrol/redeem/",
        "/enrol/status",
        "/enrol/status/x",
        "/enrol/status/x/y",
        "/api/health",
        "/join",
        "/..%2fenrol/redeem",
    ];
    for m in ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"] {
        for p in paths {
            let is_route = (m == "POST" && p == "/enrol/redeem") || (m == "GET" && p == "/enrol/status/x");
            let (s, h, body) = call(&a, m, p, &[("host", HOST)], None).await;
            if is_route {
                // Reaches the handler: no content type is 415 for the redeem; an unknown id is the
                // JSON 404 for the poll. (No ConnectInfo on this unlimited router: the redeem
                // refuses the content type first.)
                assert!(!body.is_empty(), "{m} {p}");
                continue;
            }
            assert_eq!(s, StatusCode::NOT_FOUND, "{m} {p}");
            assert!(body.is_empty(), "{m} {p}: {:?}", String::from_utf8_lossy(&body));
            // `content-length` is added by whoever serializes the response; it is not a door header.
            let names: BTreeSet<String> =
                h.keys().map(|k| k.as_str().to_string()).filter(|k| k != "content-length").collect();
            let names: BTreeSet<&str> = names.iter().map(String::as_str).collect();
            {
                // Straight to the real stdout so that the line survives in the CI log of a passing test.
                use std::io::Write;
                let allow = h.get("allow").and_then(|v| v.to_str().ok()).unwrap_or("-");
                let _ = writeln!(std::io::stdout(), "ROUTE_MATRIX|{m}|{p}|{names:?}|allow={allow}");
            }
            let registered = p == "/enrol/redeem" || p == "/enrol/status/x";
            if registered {
                // A wrong method on a registered path: the two door headers, with or without `allow`.
                let without_allow: BTreeSet<&str> = names.iter().copied().filter(|n| *n != "allow").collect();
                assert_eq!(without_allow, want, "{m} {p}: {names:?}");
            } else {
                assert_eq!(names, want, "{m} {p}");
            }
            assert_eq!(h["x-content-type-options"], "nosniff");
            assert_eq!(h["cache-control"], "no-store");
        }
    }
}

#[tokio::test]
async fn fr_c2_every_owner_route_is_404_on_the_enrol_door() {
    let a = unlimited(&door(&[]), EnrolDoor::routes(test_enrol()));
    let owner_paths = [
        "/api/health",
        "/api/computers",
        "/api/computers/enrol-codes",
        "/api/computers/enrol-codes/x",
        "/api/computers/enrol-codes/x/confirm",
        "/api/computers/enrol-codes/x/deny",
        "/api/sessions",
        "/api/guest-door",
        "/api/guest/accept",
        "/api/channels",
        "/api/devices",
        "/api/a2a/anything",
        "/join",
    ];
    for m in ["GET", "POST", "DELETE"] {
        for p in owner_paths {
            let (s, _, body) = call(&a, m, p, &[("host", HOST)], None).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{m} {p}");
            assert!(body.is_empty(), "{m} {p}");
        }
    }
}

/// Built from real owner parts: `api::router`, `computers::api::router` and the enrol router
/// (mint, status, confirm, deny), the way `main.rs` merges them, but NOT the whole set of
/// `main.rs` (the rest needs a browser manager, a scheduler and so on, none of which defines a
/// `/enrol/...` route). C2 has no door routes at all, so what is pinned is that no
/// `/enrol/redeem` or `/enrol/status/{id}` exists on the owner listener, bare or under
/// `auth::protect` with the right token.
#[tokio::test]
async fn fr_c2_the_enrol_routes_are_404_on_the_owner_listener() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let comps = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let enrol = computers::enrol::Enrol::new(store.clone());
    let owner = ember_server::api::router(sessions.clone())
        .merge(computers::api::router(comps.clone(), sessions.clone()))
        .merge(computers::enrol::router(
            enrol,
            "http://127.0.0.1:8740".into(),
            Arc::new(computers::enrol::ComputersRegistrar::new(comps)),
        ));
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let guarded = auth::protect(owner.clone(), ServerAuth::new(token.clone(), sa(OWNER), []));
    let bearer = format!("Bearer {token}");
    for (m, p) in [("POST", "/enrol/redeem"), ("GET", "/enrol/status/x"), ("GET", "/enrol/redeem"), ("POST", "/enrol/status/x")] {
        let bare = call(&owner, m, p, &[("host", OWNER)], None).await;
        assert_eq!(bare.0, StatusCode::NOT_FOUND, "bare {m} {p}");
        let with_token = call(&guarded, m, p, &[("host", OWNER), ("authorization", bearer.as_str())], None).await;
        assert_eq!(with_token.0, StatusCode::NOT_FOUND, "protected {m} {p}");
    }
    // The harness is the real thing: a known owner route answers.
    let health = call(&guarded, "GET", "/api/health", &[("host", OWNER)], None).await;
    assert_eq!(health.0, StatusCode::OK);
}

/// The unit test with the same name in `src/auth.rs` pins `token_exempt` itself (it is private).
/// Here the whole owner path: through `auth::protect` the mint, status, confirm and deny routes are
/// 401 without the token, and so is anything under `/enrol/` (401, or 404 if the layer does not
/// reach the fallback; never a 2xx).
#[tokio::test]
async fn fr_c2_redeem_is_not_token_exempt() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let comps = Computers::new(Registry::new(store.clone()), computers::default_connector());
    let owner = computers::enrol::router(
        computers::enrol::Enrol::new(store),
        "http://127.0.0.1:8740".into(),
        Arc::new(computers::enrol::ComputersRegistrar::new(comps)),
    );
    let token = auth::load_or_create_token(dir.path()).unwrap();
    let guarded = auth::protect(owner, ServerAuth::new(token, sa(OWNER), []));
    for (m, p) in [
        ("POST", "/api/computers/enrol-codes"),
        ("GET", "/api/computers/enrol-codes/x"),
        ("DELETE", "/api/computers/enrol-codes/x"),
        ("POST", "/api/computers/enrol-codes/x/confirm"),
        ("POST", "/api/computers/enrol-codes/x/deny"),
    ] {
        let (s, _, _) = call(&guarded, m, p, &[("host", OWNER)], None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {p}");
    }
    for (m, p) in [("POST", "/enrol/redeem"), ("GET", "/enrol/status/x")] {
        let (s, _, _) = call(&guarded, m, p, &[("host", OWNER)], None).await;
        assert!(s == StatusCode::UNAUTHORIZED || s == StatusCode::NOT_FOUND, "{m} {p}: {s}");
    }
}

// ---- the guard ----

#[tokio::test]
async fn fr_c2_foreign_host_and_origin_are_refused() {
    let a = unlimited(&door(&[]), Router::new());
    for host in ["evil.example.com", "10.0.0.7:9300", "127.0.0.2:9300", "user@127.0.0.1", ""] {
        let (s, _, body) = call(&a, "POST", "/enrol/redeem", &[("host", host)], None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "host {host:?}");
        assert_eq!(json_of(&body), json!({ "error": "host not allowed", "code": "forbidden" }));
    }
    assert_eq!(call(&a, "GET", "/", &[], None).await.0, StatusCode::FORBIDDEN, "no Host");
    // Two Host headers.
    let (s, _, _) = call(&a, "GET", "/", &[("host", HOST), ("host", "evil.example.com")], None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "two Hosts");
    // Origin: a foreign site, `null` and a loopback Origin different from Host are refused.
    for origin in ["https://evil.example.com", "null", "http://127.0.0.1:9999", "http://localhost:9300"] {
        let (s, _, body) = call(&a, "POST", "/enrol/redeem", &[("host", HOST), ("origin", origin)], None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "origin {origin:?}");
        assert_eq!(json_of(&body), json!({ "error": "origin not allowed", "code": "forbidden" }));
    }
    // An absent Origin is accepted (the node CLI sends none), and so is one equal to Host.
    assert_eq!(call(&a, "POST", "/enrol/redeem", &[("host", HOST)], None).await.0, StatusCode::NOT_FOUND);
    let own = format!("http://{HOST}");
    assert_eq!(call(&a, "POST", "/enrol/redeem", &[("host", HOST), ("origin", own.as_str())], None).await.0, StatusCode::NOT_FOUND);
    // Even a refusal carries the door headers.
    let (_, h, _) = call(&a, "GET", "/", &[("host", "evil.example.com")], None).await;
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["cache-control"], "no-store");
}

#[tokio::test]
async fn fr_c2_trace_connect_head_and_upgrade_are_refused() {
    let a = unlimited(&door(&[]), Router::new());
    for m in ["HEAD", "TRACE", "CONNECT"] {
        let (s, _, _) = call(&a, m, "/enrol/redeem", &[("host", HOST)], None).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "{m}");
    }
    let (s, _, body) = call(&a, "GET", "/enrol/status/x", &[("host", HOST), ("upgrade", "websocket"), ("connection", "upgrade")], None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(json_of(&body), json!({ "error": "no upgrade here", "code": "bad_request" }));
}

#[tokio::test]
async fn fr_c2_oversized_body_is_413() {
    let a = unlimited(&door(&[]), Router::new());
    let ok = ENROL_MAX_BODY_BYTES.to_string();
    let big = (ENROL_MAX_BODY_BYTES + 1).to_string();
    for p in ["/", "/enrol/redeem", "/enrol/status/x", "/anything/else"] {
        // Exactly the limit passes the guard (and meets the empty route table: 404).
        let (s, _, _) = call(&a, "POST", p, &[("host", HOST), ("content-length", ok.as_str())], None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{p} at the limit");
        let (s, _, body) = call(&a, "POST", p, &[("host", HOST), ("content-length", big.as_str())], None).await;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{p} over the limit");
        assert_eq!(json_of(&body), json!({ "error": "body too large", "code": "too_large" }));
    }
}

// ---- limits ----

/// `limits()` is the enrol door's: the redeem path is the STRICT class (10 a minute), not the
/// loose one, and the guest door's accept path is not strict here. This is the weakness found in
/// the review of #569: the guest door's constructor would have put `/enrol/redeem` in the loose
/// class. Driven with the door's own `limits()` (global bucket: burst 10), so at most 10 requests
/// go through before the bucket is empty; the 11th is answered by the per-address check first
/// (429), which runs before the bucket.
#[tokio::test]
async fn fr_c2_the_enrol_door_uses_with_strict_path() {
    // The strict class: 10 redeems pass, the 11th is 429 with Retry-After.
    let d = door(&[]);
    let a = d.wrap_limited(Router::new(), d.limits());
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        assert_eq!(at(&a, "203.0.113.1", "POST", ENROL_REDEEM_PATH, &[]).await.0, StatusCode::NOT_FOUND, "{i}");
    }
    let (s, h, body) = at(&a, "203.0.113.1", "POST", ENROL_REDEEM_PATH, &[]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(h.contains_key("retry-after"));
    assert_eq!(json_of(&body), json!({ "error": "too many requests", "code": "rate_limited" }));
    // A GET on the same path is the loose class, not strict: not 429 (404, or 503 when the bucket
    // is empty). It would be 429 only if the address were out of loose budget (60).
    let (s, _, _) = at(&a, "203.0.113.1", "GET", ENROL_REDEEM_PATH, &[]).await;
    assert_ne!(s, StatusCode::TOO_MANY_REQUESTS);
    // The guest door's accept path is NOT strict on this door: ten POSTs are all 404 (the guest
    // budget is 5 a minute, so a 429 among them would mean the guest path was wired in).
    let d = door(&[]);
    let a = d.wrap_limited(Router::new(), d.limits());
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        assert_eq!(at(&a, "203.0.113.2", "POST", ACCEPT_PATH, &[]).await.0, StatusCode::NOT_FOUND, "{i}");
    }
}

#[tokio::test]
async fn fr_c2_per_address_limit_and_another_address_unaffected() {
    let d = door(&[]);
    let a = with_cfg(&d, cfg_big_bucket(), Router::new());
    // Strict: 10 a minute, counted whatever the answer (all 404 here).
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        assert_eq!(at(&a, "203.0.113.1", "POST", ENROL_REDEEM_PATH, &[]).await.0, StatusCode::NOT_FOUND, "{i}");
    }
    assert_eq!(at(&a, "203.0.113.1", "POST", ENROL_REDEEM_PATH, &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
    // Another address is unaffected, and so is the same address on the loose class.
    assert_eq!(at(&a, "203.0.113.2", "POST", ENROL_REDEEM_PATH, &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(at(&a, "203.0.113.1", "GET", "/x", &[]).await.0, StatusCode::NOT_FOUND);
    // Loose: 60 a minute on the address (the GET above was the first), the 61st is 429.
    for i in 1..ENROL_OTHER_PER_MINUTE {
        assert_eq!(at(&a, "203.0.113.3", "GET", "/x", &[]).await.0, StatusCode::NOT_FOUND, "{i}");
    }
    assert_eq!(at(&a, "203.0.113.3", "GET", "/x", &[]).await.0, StatusCode::NOT_FOUND, "the 60th");
    assert_eq!(at(&a, "203.0.113.3", "GET", "/x", &[]).await.0, StatusCode::TOO_MANY_REQUESTS, "the 61st");
    assert_eq!(at(&a, "203.0.113.4", "GET", "/x", &[]).await.0, StatusCode::NOT_FOUND);
    // IPv6 addresses of one /64 share a budget.
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        let peer = format!("2001:db8::{:x}", i + 1);
        assert_eq!(at(&a, &peer, "POST", ENROL_REDEEM_PATH, &[]).await.0, StatusCode::NOT_FOUND, "{peer}");
    }
    assert_eq!(at(&a, "2001:db8::ffff", "POST", ENROL_REDEEM_PATH, &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn fr_c2_an_empty_global_bucket_or_no_in_flight_permit_is_503_after_the_address_check_passes() {
    let d = door(&[]);
    // The bucket: burst 2, no refill. The third request, from a fresh address, is 503 with Retry-After: 1.
    let cfg = LimitConfig { global_per_second: 0.0, global_burst: 2.0, ..cfg_big_bucket() };
    let a = with_cfg(&d, cfg, Router::new());
    assert_eq!(at(&a, "203.0.113.1", "GET", "/x", &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(at(&a, "203.0.113.2", "GET", "/x", &[]).await.0, StatusCode::NOT_FOUND);
    let (s, h, body) = at(&a, "203.0.113.3", "GET", "/x", &[]).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(h["retry-after"], "1");
    assert_eq!(json_of(&body), json!({ "error": "busy", "code": "busy" }));

    // A request refused per address does not spend the bucket: 60 loose requests of one address
    // are served, 20 more are 429, and the bucket (burst 70, no refill) still has 10 tokens for
    // ten other addresses; the eleventh is 503.
    let cfg = LimitConfig { global_per_second: 0.0, global_burst: 70.0, ..cfg_big_bucket() };
    let a = with_cfg(&d, cfg, Router::new());
    for i in 0..ENROL_OTHER_PER_MINUTE {
        assert_eq!(at(&a, "203.0.113.9", "GET", "/x", &[]).await.0, StatusCode::NOT_FOUND, "{i}");
    }
    for _ in 0..20 {
        assert_eq!(at(&a, "203.0.113.9", "GET", "/x", &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
    }
    for i in 0..10 {
        assert_eq!(at(&a, &format!("203.0.113.{}", 100 + i), "GET", "/x", &[]).await.0, StatusCode::NOT_FOUND, "{i}");
    }
    assert_eq!(at(&a, "203.0.113.200", "GET", "/x", &[]).await.0, StatusCode::SERVICE_UNAVAILABLE);

    // The in-flight cap (8): eight requests held in the handler, the ninth is 503 at once.
    let started = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (st, g) = (started.clone(), gate.clone());
    let slow = Router::new().route(
        "/slow",
        get(move || {
            let (st, g) = (st.clone(), g.clone());
            async move {
                st.fetch_add(1, Ordering::SeqCst);
                let _ = g.acquire().await;
                "done"
            }
        }),
    );
    let a = with_cfg(&d, cfg_big_bucket(), slow);
    let held: Vec<_> = (0..ENROL_MAX_IN_FLIGHT)
        .map(|i| {
            let a = a.clone();
            tokio::spawn(async move { at(&a, &format!("198.51.100.{}", i + 1), "GET", "/slow", &[]).await.0 })
        })
        .collect();
    eventually(Duration::from_secs(5), "eight requests are in the handler", || {
        let started = started.clone();
        async move { started.load(Ordering::SeqCst) == ENROL_MAX_IN_FLIGHT }
    })
    .await;
    let ninth = tokio::time::timeout(Duration::from_secs(2), at(&a, "198.51.100.99", "GET", "/slow", &[]))
        .await
        .expect("the ninth request is not left waiting");
    assert_eq!(ninth.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ninth.1["retry-after"], "1");
    gate.add_permits(100);
    for h in held {
        assert_eq!(h.await.unwrap(), StatusCode::OK);
    }
}

#[tokio::test]
async fn fr_c2_a_limited_enrol_door_without_a_peer_address_refuses() {
    let d = door(&[]);
    let a = d.app(test_enrol());
    let (s, h, body) = call(&a, "GET", "/", &[("host", HOST)], None).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(json_of(&body), json!({ "error": "no peer address", "code": "internal" }));
    assert_eq!(h["x-content-type-options"], "nosniff");
}

/// `X-Forwarded-For` is read only from a peer in `EMBER_ENROL_TRUSTED_PROXIES`, and that variable
/// is the enrol door's own: the guest door's has no effect here, and the enrol one none on the
/// guest door. Driven with each door's own `limits()` (so the global bucket of the enrol door has
/// 10 tokens): a scenario sends at most 11 strict requests. After the bucket is empty a request
/// that PASSED the per-address check is 503 (not 429), which is what "a new key" looks like then.
#[tokio::test]
async fn fr_c2_trusted_proxy_xff_is_read_only_from_a_trusted_peer() {
    let proxy = "10.0.0.2";
    let redeem = |a: &Router, xff: String| {
        let a = a.clone();
        async move { at(&a, proxy, "POST", ENROL_REDEEM_PATH, &[("x-forwarded-for", xff.as_str())]).await.0 }
    };

    // 1. Trusted proxy: the forwarded address is the key.
    let d = door(&[(ENROL_TRUSTED_PROXIES_VAR, proxy)]);
    let a = d.wrap_limited(Router::new(), d.limits());
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        assert_eq!(redeem(&a, "198.51.100.7".into()).await, StatusCode::NOT_FOUND, "{i}");
    }
    assert_eq!(redeem(&a, "198.51.100.7".into()).await, StatusCode::TOO_MANY_REQUESTS, "same client, 11th");
    let other = redeem(&a, "198.51.100.8".into()).await;
    assert_ne!(other, StatusCode::TOO_MANY_REQUESTS, "another forwarded client has its own budget");

    // 2. The same peer with NO trusted proxies: the header is ignored, the key is the peer.
    let d = door(&[]);
    let a = d.wrap_limited(Router::new(), d.limits());
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        assert_eq!(redeem(&a, format!("198.51.100.{i}")).await, StatusCode::NOT_FOUND, "{i}");
    }
    assert_eq!(redeem(&a, "198.51.100.200".into()).await, StatusCode::TOO_MANY_REQUESTS, "a rotating header cannot dodge");

    // 3. The GUEST variable has no effect on the enrol door: still keyed on the peer.
    let d = door(&[("EMBER_GUEST_TRUSTED_PROXIES", proxy)]);
    let a = d.wrap_limited(Router::new(), d.limits());
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        assert_eq!(redeem(&a, format!("198.51.100.{i}")).await, StatusCode::NOT_FOUND, "{i}");
    }
    assert_eq!(redeem(&a, "198.51.100.201".into()).await, StatusCode::TOO_MANY_REQUESTS);

    // 4. An untrusted peer behind a trusted-proxy list cannot forge a key either.
    let d = door(&[(ENROL_TRUSTED_PROXIES_VAR, proxy)]);
    let a = d.wrap_limited(Router::new(), d.limits());
    for i in 0..ENROL_ACCEPT_PER_MINUTE {
        let xff = format!("198.51.100.{i}");
        assert_eq!(at(&a, "203.0.113.9", "POST", ENROL_REDEEM_PATH, &[("x-forwarded-for", xff.as_str())]).await.0, StatusCode::NOT_FOUND, "{i}");
    }
    let (s, _, _) = at(&a, "203.0.113.9", "POST", ENROL_REDEEM_PATH, &[("x-forwarded-for", "198.51.100.250")]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);

    // 5. The ENROL variable has no effect on the guest door: its strict class (5 a minute) is
    // keyed on the peer even though the enrol list names the peer.
    let m: HashMap<String, String> =
        [("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), (ENROL_TRUSTED_PROXIES_VAR, proxy)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    let g = GuestDoor::from_lookup(|k| m.get(k).cloned(), sa(OWNER)).unwrap().expect("guest door on");
    let ga = g.wrap_limited(Router::new(), g.limits());
    for i in 0..5 {
        let xff = format!("198.51.100.{i}");
        let (s, _, _) = call(&ga, "POST", ACCEPT_PATH, &[("host", "127.0.0.1:9000"), ("x-forwarded-for", xff.as_str())], Some(proxy)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "guest {i}");
    }
    let (s, _, _) = call(&ga, "POST", ACCEPT_PATH, &[("host", "127.0.0.1:9000"), ("x-forwarded-for", "198.51.100.250")], Some(proxy)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "the guest door ignores the enrol list");
}

#[test]
fn fr_c2_the_numbers_are_the_spec() {
    assert_eq!(ENROL_REDEEM_PATH, "/enrol/redeem");
    assert_eq!(ENROL_ACCEPT_PER_MINUTE, 10);
    assert_eq!(ENROL_ACCEPT_PER_HOUR, 30);
    assert_eq!(ENROL_OTHER_PER_MINUTE, 60);
    assert_eq!(ENROL_GLOBAL_PER_SECOND, 5.0);
    assert_eq!(ENROL_GLOBAL_BURST, 10.0);
    assert_eq!(ENROL_MAX_IN_FLIGHT, 8);
    assert_eq!(ENROL_MAX_TRACKED_ADDRS, 10_000);
    assert_eq!(ENROL_MAX_CONNECTIONS, 32);
    assert_eq!(ENROL_MAX_BODY_BYTES, 4096);
    assert_eq!(ENROL_REQUEST_TIMEOUT, Duration::from_secs(10));
    assert_eq!(ENROL_HEADER_READ_TIMEOUT, Duration::from_secs(10));
    // Not the guest door's numbers.
    assert_ne!(ENROL_REDEEM_PATH, ACCEPT_PATH);
    assert_ne!(ENROL_MAX_CONNECTIONS, ember_server::guest_door::MAX_CONNECTIONS);
}

// ---- the real socket ----

fn app_of(d: &EnrolDoor) -> Router {
    d.app(test_enrol())
}

async fn real_door(timeout: Duration, app: Router) -> (SocketAddr, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = d_bind().await;
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(EnrolDoor::serve_with(
        listener,
        app,
        async move {
            let _ = rx.await;
        },
        timeout,
    ));
    (addr, tx, task)
}

async fn d_bind() -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap()
}

#[tokio::test]
async fn fr_c2_the_real_door_answers_404_with_the_door_headers() {
    let d = door(&[]);
    let (addr, stop, task) = real_door(ENROL_HEADER_READ_TIMEOUT, app_of(&d)).await;
    let r = raw(addr, "POST /enrol/nothing HTTP/1.1\r\nHost: 127.0.0.1:9300\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    assert!(r.starts_with("HTTP/1.1 404"), "{r}");
    let lower = r.to_ascii_lowercase();
    assert!(lower.contains("x-content-type-options: nosniff"), "{r}");
    assert!(lower.contains("cache-control: no-store"), "{r}");
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve returns on shutdown").unwrap().unwrap();
}

#[tokio::test]
async fn fr_c2_connection_cap() {
    use tokio::io::AsyncWriteExt;
    // The real serve path: `EnrolDoor::serve_with` uses ENROL_MAX_CONNECTIONS (32). The door has no
    // limits layer here (`wrap`), so 32 requests are not cut short by the global bucket.
    let d = door(&[]);
    let listener = d_bind().await;
    let addr = listener.local_addr().unwrap();
    let app = unlimited(&d, Router::new());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(EnrolDoor::serve_with(
        listener,
        app,
        async move {
            let _ = rx.await;
        },
        Duration::from_secs(30),
    ));
    let req = "GET / HTTP/1.1\r\nHost: 127.0.0.1:9300\r\n\r\n";
    let mut open = Vec::new();
    for n in 0..ENROL_MAX_CONNECTIONS {
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(req.as_bytes()).await.unwrap();
        // The connection counts once the door has answered on it (keep-alive keeps it open).
        let got = std::cell::RefCell::new(Vec::<u8>::new());
        {
            let c: &tokio::net::TcpStream = &c;
            eventually(Duration::from_secs(5), &format!("connection {} is served", n + 1), || {
                let got = &got;
                async move {
                    let mut chunk = [0u8; 2048];
                    // try_read never blocks: WouldBlock means the answer has not arrived yet.
                    if let Ok(k) = c.try_read(&mut chunk) {
                        got.borrow_mut().extend_from_slice(&chunk[..k]);
                    }
                    String::from_utf8_lossy(&got.borrow()).contains("\r\n\r\n")
                }
            })
            .await;
        }
        assert!(String::from_utf8_lossy(&got.borrow()).starts_with("HTTP/1.1 404"));
        open.push(c);
    }
    // The 33rd is closed by the door at accept (it does not wait): EOF or a reset, no response.
    use tokio::io::AsyncReadExt;
    let mut c33 = tokio::net::TcpStream::connect(addr).await.unwrap();
    let _ = c33.write_all(req.as_bytes()).await;
    let mut buf = Vec::new();
    let r = tokio::time::timeout(Duration::from_secs(5), c33.read_to_end(&mut buf)).await;
    assert!(r.is_ok(), "the 33rd connection was left open");
    assert!(buf.is_empty(), "the 33rd connection got an answer: {:?}", String::from_utf8_lossy(&buf));
    // One of the 32 closes; a new connection is served once the door frees the slot.
    drop(open.pop());
    eventually(Duration::from_secs(5), "a new connection is served after one closed", || async {
        raw(addr, "GET / HTTP/1.1\r\nHost: 127.0.0.1:9300\r\nConnection: close\r\n\r\n").await.starts_with("HTTP/1.1 404")
    })
    .await;
    drop(open);
    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

#[tokio::test]
async fn fr_c2_graceful_shutdown_finishes_an_in_flight_request() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (s2, r2) = (started.clone(), release.clone());
    // The door's own table is empty in C2, so a test route stands in to hold a request open.
    let slow = Router::new().route(
        "/slow",
        get(move || {
            let (s, r) = (s2.clone(), r2.clone());
            async move {
                s.notify_one();
                r.notified().await;
                "finished"
            }
        }),
    );
    let d = door(&[]);
    let app = d.wrap_limited(slow, d.limits());
    let (addr, stop, task) = real_door(Duration::from_secs(10), app).await;
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client.write_all(b"GET /slow HTTP/1.1\r\nHost: 127.0.0.1:9300\r\nConnection: close\r\n\r\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified()).await.expect("the handler started");
    stop.send(()).unwrap();
    // New connections are refused once the listener is dropped: bounded polling, no fixed sleep.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(Err(_)) = tokio::time::timeout(Duration::from_millis(500), tokio::net::TcpStream::connect(addr)).await {
            break;
        }
        assert!(Instant::now() < deadline, "new connections are still accepted after shutdown");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!task.is_finished(), "serve returned while a request was in flight");
    release.notify_one();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut out)).await.expect("response in time").unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.ends_with("finished"), "{text}");
    assert!(text.to_ascii_lowercase().contains("x-content-type-options: nosniff"), "{text}");
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve returns").expect("join").expect("serve is Ok");
}

/// The public items other code (main.rs, C3) reaches, at their paths and with their shapes.
#[test]
fn fr_c2_enrol_door_inventory_of_public_items() {
    let _: fn(SocketAddr, Option<SocketAddr>) -> Result<Option<EnrolDoor>, ember_server::enrol_door::DoorError> =
        EnrolDoor::from_env;
    let _: fn(Arc<Enrol>) -> Router = EnrolDoor::routes;
    let _: fn(&EnrolDoor, Arc<Enrol>) -> Router = EnrolDoor::routes_for;
    let _: fn(&EnrolDoor, Router) -> Router = EnrolDoor::wrap;
    let _: fn(&EnrolDoor, Router, Arc<DoorLimits>) -> Router = EnrolDoor::wrap_limited;
    let _: fn(&EnrolDoor, Arc<Enrol>) -> Router = EnrolDoor::app;
    let _: fn() -> ember_server::enrol_door::PollLimiter = ember_server::enrol_door::PollLimiter::new;
    let _: fn(&ember_server::enrol_door::PollLimiter, &str, Instant) -> Result<(), Duration> =
        ember_server::enrol_door::PollLimiter::check_at;
    let _: fn(&EnrolDoor) -> Arc<DoorLimits> = EnrolDoor::limits;
    let _: fn(&EnrolDoor) -> bool = EnrolDoor::is_remote;
    assert_eq!(ENROL_LISTEN_VAR, "EMBER_ENROL_LISTEN");
    assert_eq!(ENROL_ALLOW_REMOTE_VAR, "EMBER_ENROL_LISTEN_ALLOW_REMOTE");
    assert_eq!(ENROL_TRUSTED_PROXIES_VAR, "EMBER_ENROL_TRUSTED_PROXIES");
    assert_eq!(ENROL_ALLOWED_HOSTS_VAR, "EMBER_ENROL_ALLOWED_HOSTS");
    assert_eq!(ENROL_PUBLIC_URL_VAR, "EMBER_ENROL_PUBLIC_URL");
}
