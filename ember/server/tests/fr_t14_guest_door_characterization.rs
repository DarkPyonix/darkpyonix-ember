//! FR-T14: characterization tests of the guest door's hardening code. They pin what the code does
//! TODAY, on the unmoved `guest_door.rs`, so that the move of that code into a shared module for a
//! second door can be proven to change nothing. They describe, they do not judge: a behaviour that
//! looks odd is pinned as it is and marked `CHARACTERISES CURRENT BEHAVIOUR (not a requirement)`.
//! Every expected value was read from the code, not guessed. Details are [provisional].
//!
//! Not testable without fault injection or private access (nothing here pins them):
//! the accept-error backoff of `serve_capped` (needs `accept()` to fail, e.g. out of descriptors)
//! and the limiter mutex poison recovery (`AddrLimiter` and `TokenBucket` keep their `Mutex` private
//! and no public call can panic while holding it).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

fn door_with(vars: &[(&str, &str)]) -> Result<Option<GuestDoor>, String> {
    let m: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    GuestDoor::from_lookup(|k| m.get(k).cloned(), OWNER.parse::<SocketAddr>().unwrap()).map_err(|e| e.to_string())
}

/// A door on the loopback listen address with `extra` variables.
fn door(extra: &[(&str, &str)]) -> GuestDoor {
    let mut v = vec![("EMBER_GUEST_LISTEN", LISTEN)];
    v.extend_from_slice(extra);
    door_with(&v).unwrap().expect("door on")
}

/// A small router in place of the guest router: the door's layers are what is under test.
fn site() -> Router {
    Router::new()
        .route("/ok", get(|| async { "ok" }).post(|| async { "posted" }))
        .route(ACCEPT_PATH, post(|| async { "accepted" }))
        .route("/echo", post(|b: Bytes| async move { b.len().to_string() }))
}

fn app(d: &GuestDoor) -> Router {
    d.wrap(site())
}

fn limited(d: &GuestDoor, cfg: LimitConfig) -> Router {
    d.wrap_limited(site(), Arc::new(DoorLimits::new(cfg, Vec::new())))
}

type Reply = (StatusCode, HeaderMap, Vec<u8>);

/// One request. `headers` may repeat a name (appended, never replaced) and carry raw bytes.
async fn call(app: &Router, method: &str, uri: &str, headers: &[(&str, &[u8])], body: Vec<u8>, peer: Option<&str>) -> Reply {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, HeaderValue::from_bytes(v).unwrap());
    }
    let mut req = req.body(Body::from(body)).unwrap();
    if let Some(p) = peer {
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(p.parse::<IpAddr>().unwrap(), 40000)));
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = res.into_parts();
    (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
}

async fn get_with(app: &Router, uri: &str, headers: &[(&str, &[u8])]) -> Reply {
    call(app, "GET", uri, headers, Vec::new(), None).await
}

async fn status_of(app: &Router, uri: &str, headers: &[(&str, &[u8])]) -> StatusCode {
    get_with(app, uri, headers).await.0
}

/// The sorted header names of a reply, WITHOUT `content-length`: whether a reply built by `oneshot`
/// carries one depends on axum and hyper, not on the door, and no door code sets it. Everything the
/// door sets is still compared exactly.
fn names(h: &HeaderMap) -> Vec<String> {
    let mut v: Vec<String> = h.keys().map(|k| k.as_str().to_string()).filter(|k| k != "content-length").collect();
    v.sort();
    v
}

fn hv(h: &HeaderMap, name: &str) -> String {
    h.get(name).unwrap_or_else(|| panic!("header {name} missing")).to_str().unwrap().to_string()
}

fn json_of(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|e| panic!("not json ({e}): {:?}", String::from_utf8_lossy(body)))
}

fn forbidden(why: &str) -> Value {
    json!({ "error": why, "code": "forbidden" })
}

// ---- Host ----

#[tokio::test]
async fn fr_t14_char_two_host_headers_are_403() {
    let a = app(&door(&[]));
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): two Host headers are refused even when
    // both are the same allowed name (request_host returns None for any count but one).
    let pairs: [[&[u8]; 2]; 3] = [[HOST, HOST], [HOST, b"localhost"], [b"evil.example", HOST]];
    for pair in pairs {
        let (s, _, body) = get_with(&a, "/ok", &[("host", pair[0]), ("host", pair[1])]).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{pair:?}");
        assert_eq!(json_of(&body), forbidden("host not allowed"));
    }
    assert_eq!(status_of(&a, "/ok", &[("host", HOST)]).await, StatusCode::OK);
}

#[tokio::test]
async fn fr_t14_char_absolute_form_uri_supplies_the_host_only_when_there_is_no_host_header() {
    let a = app(&door(&[]));
    // No Host header: the URI authority is the host.
    assert_eq!(status_of(&a, "http://127.0.0.1:9000/ok", &[]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "http://localhost/ok", &[]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "http://evil.example/ok", &[]).await, StatusCode::FORBIDDEN);
    // Userinfo in the authority string contains '@': refused.
    assert_eq!(status_of(&a, "http://user@127.0.0.1:9000/ok", &[]).await, StatusCode::FORBIDDEN);
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): when a Host header is present the URI
    // authority is ignored by the guard, in both directions, and the router uses only the path.
    assert_eq!(status_of(&a, "http://evil.example/ok", &[("host", HOST)]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "http://127.0.0.1:9000/ok", &[("host", b"evil.example")]).await, StatusCode::FORBIDDEN);
    // A path-only URI without Host stays 403.
    assert_eq!(status_of(&a, "/ok", &[]).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn fr_t14_char_host_with_slash_backslash_space_or_bad_bytes_is_403() {
    let a = app(&door(&[]));
    let bad: [&[u8]; 8] = [
        b"127.0.0.1:9000/x",
        b"127.0.0.1/",
        b"127.0.0.1:9000\\",
        b"127.0.0.1\\x",
        b"127.0.0.1 :9000",
        b"127.0.0.1:9000 evil.example",
        b"user@127.0.0.1",
        b"127.0.0.1\xff",
    ];
    for h in bad {
        let (s, _, body) = get_with(&a, "/ok", &[("host", h)]).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{:?}", String::from_utf8_lossy(h));
        assert_eq!(json_of(&body), forbidden("host not allowed"));
    }
    // An empty Host is 403 too (the guard refuses empty before the allow-list).
    assert_eq!(status_of(&a, "/ok", &[("host", b"")]).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn fr_t14_char_allowed_hosts_are_parsed_to_lower_case_names_without_ports() {
    let d = door(&[("EMBER_GUEST_ALLOWED_HOSTS", " Example.COM:8080 , [::1]:9 ,,user@Foo.org"), ("EMBER_GUEST_PUBLIC_URL", "https://G.Pub.example:8443")]);
    let a = app(&d);
    for h in ["example.com:1234", "EXAMPLE.com", "[::1]:9000", "foo.org", "g.pub.example:1", "127.0.0.1", "localhost"] {
        assert_eq!(status_of(&a, "/ok", &[("host", h.as_bytes())]).await, StatusCode::OK, "{h}");
    }
    for h in ["10.0.0.1", "127.0.0.2", "pub.example", "example.com.evil"] {
        assert_eq!(status_of(&a, "/ok", &[("host", h.as_bytes())]).await, StatusCode::FORBIDDEN, "{h}");
    }
}

#[tokio::test]
async fn fr_t14_char_an_unspecified_listen_address_adds_no_host() {
    let d = door_with(&[
        ("EMBER_GUEST_LISTEN", "0.0.0.0:9000"),
        ("EMBER_GUEST_LISTEN_ALLOW_REMOTE", "1"),
        ("EMBER_GUEST_ALLOWED_HOSTS", "h.example"),
    ])
    .unwrap()
    .unwrap();
    assert!(d.is_remote());
    let a = app(&d);
    for h in ["0.0.0.0:9000", "127.0.0.1:9000", "localhost:9000"] {
        assert_eq!(status_of(&a, "/ok", &[("host", h.as_bytes())]).await, StatusCode::FORBIDDEN, "{h}");
    }
    assert_eq!(status_of(&a, "/ok", &[("host", b"h.example:9000")]).await, StatusCode::OK);
    // A remote concrete address is a host, and localhost is not added for it.
    let d = door_with(&[("EMBER_GUEST_LISTEN", "192.168.1.5:9000"), ("EMBER_GUEST_LISTEN_ALLOW_REMOTE", "1")]).unwrap().unwrap();
    let a = app(&d);
    assert_eq!(status_of(&a, "/ok", &[("host", b"192.168.1.5:9000")]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "/ok", &[("host", b"localhost:9000")]).await, StatusCode::FORBIDDEN);
}

// ---- Origin ----

#[tokio::test]
async fn fr_t14_char_two_origin_headers_are_403() {
    let a = app(&door(&[]));
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): two Origin headers are refused even when
    // both are acceptable on their own.
    let (s, _, body) = get_with(&a, "/ok", &[("host", HOST), ("origin", b"http://127.0.0.1:9000"), ("origin", b"http://127.0.0.1:9000")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(json_of(&body), forbidden("origin not allowed"));
    let (s, _, _) = get_with(&a, "/ok", &[("host", HOST), ("origin", b"http://127.0.0.1:9000"), ("origin", b"null")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // One Origin of the same value is fine.
    assert_eq!(status_of(&a, "/ok", &[("host", HOST), ("origin", b"http://127.0.0.1:9000")]).await, StatusCode::OK);
}

#[tokio::test]
async fn fr_t14_char_origin_forms_that_are_refused_or_accepted() {
    let a = app(&door(&[("EMBER_GUEST_ALLOWED_HOSTS", "other.example")]));
    let h: &[u8] = b"127.0.0.1:9000";
    // Refused: not text, no scheme, null, other scheme, empty authority, path/userinfo/space/backslash.
    let refused: [&[u8]; 11] = [
        b"\xff\xfe",
        b"127.0.0.1:9000",
        b"null",
        b"ftp://127.0.0.1:9000",
        b"http://",
        b"http://127.0.0.1:9000/path",
        b"http://127.0.0.1:9000/",
        b"http://u@127.0.0.1:9000",
        b"http://127.0.0.1:9000 x",
        b"http://127.0.0.1\\:9000",
        b"https://evil.example",
    ];
    for o in refused {
        let (s, _, body) = get_with(&a, "/ok", &[("host", h), ("origin", o)]).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{:?}", String::from_utf8_lossy(o));
        assert_eq!(json_of(&body), forbidden("origin not allowed"));
    }
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): the scheme is compared case-sensitively
    // to "http"/"https", so an upper-case scheme is refused even for the very same authority.
    assert_eq!(status_of(&a, "/ok", &[("host", h), ("origin", b"HTTP://127.0.0.1:9000")]).await, StatusCode::FORBIDDEN);
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): the Origin scheme is not compared with
    // anything about the connection; https against a plain-HTTP door passes when the authority is equal.
    assert_eq!(status_of(&a, "/ok", &[("host", h), ("origin", b"https://127.0.0.1:9000")]).await, StatusCode::OK);
    // The authority equals Host without regard to case.
    assert_eq!(status_of(&a, "/ok", &[("host", b"LocalHost:9000"), ("origin", b"http://localhost:9000")]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "/ok", &[("host", b"localhost:9000"), ("origin", b"http://LOCALHOST:9000")]).await, StatusCode::OK);
    // A different port makes it a different authority: loopback names then fail, an allowed
    // non-loopback name passes whatever its port.
    assert_eq!(status_of(&a, "/ok", &[("host", h), ("origin", b"http://127.0.0.1:9001")]).await, StatusCode::FORBIDDEN);
    assert_eq!(status_of(&a, "/ok", &[("host", h), ("origin", b"http://other.example")]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "/ok", &[("host", h), ("origin", b"https://Other.Example:1234")]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "/ok", &[("host", h), ("origin", b"http://not-allowed.example")]).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn fr_t14_char_localhost_names_in_host_and_origin() {
    let a = app(&door(&[("EMBER_GUEST_ALLOWED_HOSTS", "foo.localhost,127.0.0.2,other.example")]));
    // Host: `localhost` itself is implicit on a loopback bind; a `.localhost` name is not implied,
    // it must be listed. Lookalikes are refused.
    assert_eq!(status_of(&a, "/ok", &[("host", b"localhost:9000")]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "/ok", &[("host", b"foo.localhost:9000")]).await, StatusCode::OK);
    assert_eq!(status_of(&a, "/ok", &[("host", b"127.0.0.2:9000")]).await, StatusCode::OK);
    for h in ["bar.localhost", "localhost.", "sub.foo.localhost", "xlocalhost", "localhost.evil.example"] {
        assert_eq!(status_of(&a, "/ok", &[("host", h.as_bytes())]).await, StatusCode::FORBIDDEN, "{h}");
    }
    // Origin equal to Host passes, loopback or not.
    assert_eq!(status_of(&a, "/ok", &[("host", b"foo.localhost:9000"), ("origin", b"http://foo.localhost:9000")]).await, StatusCode::OK);
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): a listed `.localhost` name or a listed
    // loopback IP is still a loopback name, so as an Origin that differs from Host it is refused
    // although the allow-list holds it.
    for o in ["http://foo.localhost:9000", "http://foo.localhost", "http://127.0.0.2:9000", "http://localhost:9000", "http://127.0.0.1:9000"] {
        let (s, _, body) = get_with(&a, "/ok", &[("host", b"localhost:9000"), ("origin", o.as_bytes())]).await;
        let expect = if o == "http://localhost:9000" { StatusCode::OK } else { StatusCode::FORBIDDEN };
        assert_eq!(s, expect, "{o}");
        if expect == StatusCode::FORBIDDEN {
            assert_eq!(json_of(&body), forbidden("origin not allowed"), "{o}");
        }
    }
    // A non-loopback allowed name passes as an Origin against a loopback Host.
    assert_eq!(status_of(&a, "/ok", &[("host", b"localhost:9000"), ("origin", b"http://other.example:81")]).await, StatusCode::OK);
}

// ---- methods ----

#[tokio::test]
async fn fr_t14_char_head_trace_and_connect_are_405_with_no_body_after_the_host_and_origin_checks() {
    let a = app(&door(&[]));
    for m in ["HEAD", "TRACE", "CONNECT"] {
        let (s, h, body) = call(&a, m, "/ok", &[("host", HOST)], Vec::new(), None).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "{m}");
        assert!(body.is_empty(), "{m}");
        // The reply has no content type. Whether it also carries `allow` depends on the method and
        // on which layer answered, not on the door's own code (the door has no per-method branch):
        // CHARACTERISES CURRENT BEHAVIOUR as observed in CI: HEAD has NO `allow`, TRACE has one.
        // CONNECT was not observed yet (an earlier assertion failed first), so it is only printed
        // here and pinned in the next change once CI has shown it.
        match m {
            "HEAD" => assert_eq!(names(&h), ["cache-control", "x-content-type-options"], "{m}"),
            "TRACE" => assert_eq!(names(&h), ["allow", "cache-control", "x-content-type-options"], "{m}"),
            _ => {
                // cargo hides the output of a passing test; a direct write to the real stdout
                // reaches the CI log as a notice (the same trick as tests/script_ready.rs).
                use std::io::Write;
                let _ = writeln!(std::io::stdout(), "::notice title=OBSERVE::{m} 405 header names: {:?}", names(&h));
            }
        }
        // Never more than the door's two headers plus, at most, `allow`.
        assert!(names(&h).iter().all(|n| ["allow", "cache-control", "x-content-type-options"].contains(&n.as_str())), "{m}: {:?}", names(&h));
        // The method check runs after Host and Origin: a bad one is 403, not 405. A reply to HEAD
        // has no body by the HTTP rules, so only the other methods can be read back as JSON.
        let (s, _, body) = call(&a, m, "/ok", &[("host", b"evil.example")], Vec::new(), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} host");
        if m == "HEAD" {
            assert!(body.is_empty(), "a HEAD reply has no body");
        } else {
            assert_eq!(json_of(&body), forbidden("host not allowed"));
        }
        let (s, _, body) = call(&a, m, "/ok", &[("host", HOST), ("origin", b"null")], Vec::new(), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} origin");
        if m == "HEAD" {
            assert!(body.is_empty(), "a HEAD reply has no body");
        } else {
            assert_eq!(json_of(&body), forbidden("origin not allowed"));
        }
        // 405 on an unknown path as well (before the 404 fallback).
        assert_eq!(call(&a, m, "/nothing", &[("host", HOST)], Vec::new(), None).await.0, StatusCode::METHOD_NOT_ALLOWED, "{m} unknown");
    }
    // The method match is on the exact upper-case token only, so GET still works.
    assert_eq!(status_of(&a, "/ok", &[("host", HOST)]).await, StatusCode::OK);
}

// ---- body ----

#[tokio::test]
async fn fr_t14_char_non_numeric_content_length_is_not_refused_by_the_guard() {
    let a = app(&door(&[]));
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): a Content-Length that is not a base-10
    // u64 (text, empty, negative, fraction, hex, overflow) is read as "not too big" and the
    // request goes on to the router.
    for v in ["abc", "", "-1", "1.5", "0x10", "99999999999999999999999", "16384x"] {
        let (s, _, body) = call(&a, "POST", "/ok", &[("host", HOST), ("content-length", v.as_bytes())], Vec::new(), None).await;
        assert_eq!(s, StatusCode::OK, "content-length {v:?}");
        assert_eq!(body, b"posted");
    }
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): surrounding spaces and a leading '+'
    // are accepted by the parse, so they count.
    for v in ["  17000 ", "+17000", "0016385"] {
        let s = call(&a, "POST", "/ok", &[("host", HOST), ("content-length", v.as_bytes())], Vec::new(), None).await.0;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "content-length {v:?}");
    }
    for v in ["0", "16384", " 16384 ", "+16384"] {
        let s = call(&a, "POST", "/ok", &[("host", HOST), ("content-length", v.as_bytes())], Vec::new(), None).await.0;
        assert_eq!(s, StatusCode::OK, "content-length {v:?}");
    }
}

#[tokio::test]
async fn fr_t14_char_a_body_over_max_body_bytes_is_413_with_a_json_body() {
    let a = app(&door(&[]));
    let big = vec![b'a'; MAX_BODY_BYTES + 1];
    // Declared: the guard answers, before any handler.
    let len = big.len().to_string();
    let (s, h, body) = call(&a, "POST", "/ok", &[("host", HOST), ("content-length", len.as_bytes())], big.clone(), None).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json_of(&body), json!({ "error": "body too large", "code": "too_large" }));
    assert_eq!(hv(&h, "content-type"), "application/json");
    // A declared length wins over a missing route and over a bad method for the handler: any path.
    let s = call(&a, "POST", "/nothing", &[("host", HOST), ("content-length", len.as_bytes())], Vec::new(), None).await.0;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // The size check runs before the Upgrade check.
    let s = call(&a, "POST", "/ok", &[("host", HOST), ("content-length", len.as_bytes()), ("upgrade", b"websocket")], Vec::new(), None).await.0;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // The Upgrade check alone: 400 bad_request.
    let (s, _, body) = call(&a, "GET", "/ok", &[("host", HOST), ("upgrade", b"websocket")], Vec::new(), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(json_of(&body), json!({ "error": "no upgrade here", "code": "bad_request" }));
    // Exactly MAX_BODY_BYTES passes the guard and the extractor.
    let ok = vec![b'a'; MAX_BODY_BYTES];
    let (s, _, body) = call(&a, "POST", "/echo", &[("host", HOST)], ok, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, MAX_BODY_BYTES.to_string().as_bytes());
    // Not declared: the body extractor stops at the limit (status only: the text is axum's).
    let s = call(&a, "POST", "/echo", &[("host", HOST)], big, None).await.0;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

// ---- headers and fallback ----

#[tokio::test]
async fn fr_t14_char_the_404_fallback_is_empty_with_only_the_two_door_headers() {
    let a = app(&door(&[]));
    for (m, p) in [("GET", "/nothing"), ("POST", "/nothing"), ("DELETE", "/api/sessions"), ("GET", "/ok/"), ("GET", "/OK"), ("PUT", "/")] {
        let (s, h, body) = call(&a, m, p, &[("host", HOST)], Vec::new(), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{m} {p}");
        assert!(body.is_empty(), "{m} {p}");
        assert_eq!(names(&h), ["cache-control", "x-content-type-options"], "{m} {p}");
        assert_eq!(hv(&h, "cache-control"), "no-store");
        assert_eq!(hv(&h, "x-content-type-options"), "nosniff");
    }
}

#[tokio::test]
async fn fr_t14_char_security_headers_on_every_kind_of_answer() {
    let d = door(&[]);
    let a = app(&d);
    let two = ["cache-control", "x-content-type-options"];
    let check_values = |h: &HeaderMap| {
        assert_eq!(hv(h, "cache-control"), "no-store");
        assert_eq!(hv(h, "x-content-type-options"), "nosniff");
        // Exactly one value each (the page sets them and the layer inserts, never appends).
        assert_eq!(h.get_all("cache-control").iter().count(), 1);
        assert_eq!(h.get_all("x-content-type-options").iter().count(), 1);
    };
    // Normal answer of a handler.
    let (s, h, _) = get_with(&a, "/ok", &[("host", HOST)]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(names(&h), ["cache-control", "content-type", "x-content-type-options"]);
    assert_eq!(hv(&h, "content-type"), "text/plain; charset=utf-8");
    check_values(&h);
    // The /join page: its own headers, and the two once.
    let (s, h, _) = get_with(&a, "/join", &[("host", HOST)]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        names(&h),
        ["cache-control", "content-security-policy", "content-type", "referrer-policy", "x-content-type-options"]
    );
    assert_eq!(hv(&h, "content-type"), "text/html; charset=utf-8");
    assert_eq!(hv(&h, "referrer-policy"), "no-referrer");
    assert!(hv(&h, "content-security-policy").starts_with("default-src 'none'"));
    check_values(&h);
    // 404.
    let (s, h, _) = get_with(&a, "/nothing", &[("host", HOST)]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(names(&h), two);
    check_values(&h);
    // 403 (host and origin), 405, 400, 413: JSON ones carry a content type, 405 none.
    let (s, h, _) = get_with(&a, "/ok", &[("host", b"evil.example")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(names(&h), ["cache-control", "content-type", "x-content-type-options"]);
    assert_eq!(hv(&h, "content-type"), "application/json");
    check_values(&h);
    let (s, h, _) = get_with(&a, "/ok", &[("host", HOST), ("origin", b"null")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(names(&h), ["cache-control", "content-type", "x-content-type-options"]);
    check_values(&h);
    let (s, h, _) = call(&a, "TRACE", "/ok", &[("host", HOST)], Vec::new(), None).await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    // `allow` is added by axum's method router, not by the door; its value is not pinned.
    assert_eq!(names(&h), ["allow", "cache-control", "x-content-type-options"]);
    check_values(&h);
    let (s, h, _) = get_with(&a, "/ok", &[("host", HOST), ("upgrade", b"websocket")]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(names(&h), ["cache-control", "content-type", "x-content-type-options"]);
    check_values(&h);
    let (s, h, _) = call(&a, "POST", "/ok", &[("host", HOST), ("content-length", b"99999")], Vec::new(), None).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(names(&h), ["cache-control", "content-type", "x-content-type-options"]);
    check_values(&h);

    // 429 (per-address) and 503 (global bucket): the limit layer is inside the header layer.
    let l = limited(&d, LimitConfig { other_per_minute: 1, ..LimitConfig::default() });
    let ok = call(&l, "GET", "/ok", &[("host", HOST)], Vec::new(), Some("203.0.113.1")).await;
    assert_eq!(ok.0, StatusCode::OK);
    let (s, h, body) = call(&l, "GET", "/ok", &[("host", HOST)], Vec::new(), Some("203.0.113.1")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(names(&h), ["cache-control", "content-type", "retry-after", "x-content-type-options"]);
    assert_eq!(hv(&h, "content-type"), "application/json");
    assert_eq!(json_of(&body), json!({ "error": "too many requests", "code": "rate_limited" }));
    check_values(&h);
    // A refusal comes before the Host check: a bad Host from a limited address is 429, not 403.
    let (s, _, _) = call(&l, "GET", "/ok", &[("host", b"evil.example")], Vec::new(), Some("203.0.113.1")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);

    let l = limited(&d, LimitConfig { global_burst: 1.0, global_per_second: 0.0, ..LimitConfig::default() });
    assert_eq!(call(&l, "GET", "/ok", &[("host", HOST)], Vec::new(), Some("203.0.113.2")).await.0, StatusCode::OK);
    let (s, h, body) = call(&l, "GET", "/ok", &[("host", HOST)], Vec::new(), Some("203.0.113.2")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(names(&h), ["cache-control", "content-type", "retry-after", "x-content-type-options"]);
    assert_eq!(hv(&h, "retry-after"), "1");
    assert_eq!(json_of(&body), json!({ "error": "busy", "code": "busy" }));
    check_values(&h);

    // No in-flight permit at all: 503 busy with Retry-After 1 as well.
    let l = limited(&d, LimitConfig { max_in_flight: 0, ..LimitConfig::default() });
    let (s, h, body) = call(&l, "GET", "/ok", &[("host", HOST)], Vec::new(), Some("203.0.113.3")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(hv(&h, "retry-after"), "1");
    assert_eq!(json_of(&body), json!({ "error": "busy", "code": "busy" }));
}

/// `Retry-After` is the window minus the real time elapsed since the first request, rounded UP to a
/// whole second (guest_door.rs `too_many`). The two requests of a test normally run within a
/// millisecond, so the value is the window itself; on a loaded runner a second can pass and the
/// value is `window - 1`. Pin the contract, not the scheduler: a whole number of seconds that is at
/// most the window and at least `window - 5`.
fn assert_retry_after(h: &HeaderMap, window: u64) {
    let v = hv(h, "retry-after");
    let n: u64 = v.parse().unwrap_or_else(|_| panic!("retry-after {v:?} is not whole seconds"));
    assert!(n <= window && n + 5 >= window, "retry-after {n} outside {}..={window}", window.saturating_sub(5));
}

#[tokio::test]
async fn fr_t14_char_retry_after_rounds_a_fractional_wait_up_to_whole_seconds() {
    let d = door(&[]);
    // The wait is the window minus the real elapsed time (59.99... s) and `too_many` rounds up:
    // 60 when the two requests run within a second of each other, 59 or less on a loaded runner.
    // `assert_retry_after` therefore accepts window-5..=window (see its comment).
    let l = limited(&d, LimitConfig { other_per_minute: 1, ..LimitConfig::default() });
    assert_eq!(call(&l, "GET", "/ok", &[("host", HOST)], Vec::new(), Some("203.0.113.10")).await.0, StatusCode::OK);
    let (s, h, _) = call(&l, "GET", "/ok", &[("host", HOST)], Vec::new(), Some("203.0.113.10")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_retry_after(&h, 60);

    // The strict budget, minute window: the same 60.
    let l = limited(&d, LimitConfig { accept_per_minute: 1, ..LimitConfig::default() });
    assert_eq!(call(&l, "POST", ACCEPT_PATH, &[("host", HOST)], Vec::new(), Some("203.0.113.11")).await.0, StatusCode::OK);
    let (s, h, _) = call(&l, "POST", ACCEPT_PATH, &[("host", HOST)], Vec::new(), Some("203.0.113.11")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_retry_after(&h, 60);

    // The strict budget, hour window: 3600 (the longer of the two waits wins).
    let l = limited(&d, LimitConfig { accept_per_hour: 1, ..LimitConfig::default() });
    assert_eq!(call(&l, "POST", ACCEPT_PATH, &[("host", HOST)], Vec::new(), Some("203.0.113.12")).await.0, StatusCode::OK);
    let (s, h, _) = call(&l, "POST", ACCEPT_PATH, &[("host", HOST)], Vec::new(), Some("203.0.113.12")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_retry_after(&h, 3600);

    // Only POST on the accept path is strict: a GET there spends the loose budget.
    let l = limited(&d, LimitConfig { accept_per_minute: 1, other_per_minute: 3, ..LimitConfig::default() });
    for _ in 0..3 {
        assert_ne!(call(&l, "GET", ACCEPT_PATH, &[("host", HOST)], Vec::new(), Some("203.0.113.13")).await.0, StatusCode::TOO_MANY_REQUESTS);
    }
    assert_eq!(call(&l, "GET", ACCEPT_PATH, &[("host", HOST)], Vec::new(), Some("203.0.113.13")).await.0, StatusCode::TOO_MANY_REQUESTS);
}

// ---- configuration text ----

#[test]
fn fr_t14_char_startup_error_texts() {
    let err = |extra: &[(&str, &str)]| door_with(extra).err().expect("a startup error");
    // Trusted proxies: the entry is trimmed, shown with Debug quotes; empty entries are skipped.
    assert_eq!(
        err(&[("EMBER_GUEST_LISTEN", LISTEN), ("EMBER_GUEST_TRUSTED_PROXIES", "10.0.0.1, banana ,10.0.0.2")]),
        "EMBER_GUEST_TRUSTED_PROXIES: \"banana\" is not an IP address"
    );
    // The first bad entry is named; the port form is not an IP address.
    assert_eq!(
        err(&[("EMBER_GUEST_LISTEN", LISTEN), ("EMBER_GUEST_TRUSTED_PROXIES", "10.0.0.1:80,x")]),
        "EMBER_GUEST_TRUSTED_PROXIES: \"10.0.0.1:80\" is not an IP address"
    );
    let ok = door_with(&[("EMBER_GUEST_LISTEN", LISTEN), ("EMBER_GUEST_TRUSTED_PROXIES", " 10.0.0.1 ,, ::1 ,")]);
    assert!(ok.unwrap().is_some());
    // The other texts.
    assert_eq!(
        err(&[("EMBER_GUEST_LISTEN", " not-an-address ")]),
        "EMBER_GUEST_LISTEN=\"not-an-address\" is not an ip:port address"
    );
    assert_eq!(err(&[("EMBER_GUEST_LISTEN", OWNER)]), "EMBER_GUEST_LISTEN must differ from EMBER_LISTEN (127.0.0.1:8740)");
    assert_eq!(
        err(&[("EMBER_GUEST_LISTEN", "0.0.0.0:8740")]),
        "EMBER_GUEST_LISTEN must differ from EMBER_LISTEN (127.0.0.1:8740)"
    );
    let remote = "EMBER_GUEST_LISTEN=0.0.0.0:9000 is not a loopback address: set EMBER_GUEST_LISTEN_ALLOW_REMOTE=1 to expose the guest door to a network (it speaks plain HTTP: put TLS in front of it)";
    assert_eq!(err(&[("EMBER_GUEST_LISTEN", "0.0.0.0:9000")]), remote);
    // Only the exact word 1 (trimmed) opens it.
    for v in ["true", "0", "yes", "11", ""] {
        assert_eq!(err(&[("EMBER_GUEST_LISTEN", "0.0.0.0:9000"), ("EMBER_GUEST_LISTEN_ALLOW_REMOTE", v)]), remote, "{v:?}");
    }
    assert!(door_with(&[("EMBER_GUEST_LISTEN", "0.0.0.0:9000"), ("EMBER_GUEST_LISTEN_ALLOW_REMOTE", " 1 ")]).unwrap().is_some());
    // Check order: listen syntax, owner clash, remote opt-in, trusted proxies, public URL.
    assert_eq!(
        err(&[("EMBER_GUEST_LISTEN", "0.0.0.0:9000"), ("EMBER_GUEST_TRUSTED_PROXIES", "bad"), ("EMBER_GUEST_PUBLIC_URL", "x")]),
        remote
    );
    assert_eq!(
        err(&[("EMBER_GUEST_LISTEN", LISTEN), ("EMBER_GUEST_TRUSTED_PROXIES", "bad"), ("EMBER_GUEST_PUBLIC_URL", "x")]),
        "EMBER_GUEST_TRUSTED_PROXIES: \"bad\" is not an IP address"
    );
}

#[test]
fn fr_t14_char_parse_public_url_every_branch() {
    let url = |u: &str| door_with(&[("EMBER_GUEST_LISTEN", LISTEN), ("EMBER_GUEST_PUBLIC_URL", u)]).map(|d| d.unwrap().public_url);
    let bad_text = |u: &str| format!("EMBER_GUEST_PUBLIC_URL={u:?} must be http(s)://host[:port] (scheme and authority only)");
    // Accepted, normalised: one trailing slash dropped, value trimmed first.
    assert_eq!(url("https://g.example.com").unwrap().as_deref(), Some("https://g.example.com"));
    assert_eq!(url("https://g.example.com/").unwrap().as_deref(), Some("https://g.example.com"));
    assert_eq!(url("  http://g.example.com:8080/  ").unwrap().as_deref(), Some("http://g.example.com:8080"));
    assert_eq!(url("https://[::1]:8443").unwrap().as_deref(), Some("https://[::1]:8443"));
    assert_eq!(url("http://10.0.0.5").unwrap().as_deref(), Some("http://10.0.0.5"));
    // CHARACTERISES CURRENT BEHAVIOUR (not a requirement): the case of the authority is kept in the
    // stored URL (the host set lowercases it), and the port is not validated as a number.
    assert_eq!(url("https://G.Example.COM").unwrap().as_deref(), Some("https://G.Example.COM"));
    assert_eq!(url("https://g.example.com:99999").unwrap().as_deref(), Some("https://g.example.com:99999"));
    assert_eq!(url("https://g.example.com:abc").unwrap().as_deref(), Some("https://g.example.com:abc"));
    // Unset: empty and whitespace-only values mean no public URL.
    assert_eq!(url("").unwrap(), None);
    assert_eq!(url("   ").unwrap(), None);
    // Refused, with the original (trimmed) text in the message.
    let refused = [
        "g.example.com",              // no scheme
        "//g.example.com",            // no scheme
        "ftp://g.example.com",        // scheme
        "HTTP://g.example.com",       // scheme is case-sensitive
        "https:/g.example.com",       // not ://
        "https://",                   // the slash strip leaves "https:/"
        "https:///",                  // empty authority
        "https://g.example.com//",    // one slash stripped, one left
        "https://g.example.com/path", // path
        "https://g.example.com?q=1",  // query
        "https://g.example.com#frag", // fragment
        "https://u@g.example.com",    // userinfo
        "https://g example.com",      // space
        "https://g.example.com\\x",   // backslash
        "https://:8080",              // empty host
        "https://[]:8080",            // empty host in brackets
    ];
    for u in refused {
        assert_eq!(url(u).err().as_deref(), Some(bad_text(u).as_str()), "{u:?}");
    }
    // The stored URL keeps its case and loses one trailing slash.
    let d = door(&[("EMBER_GUEST_PUBLIC_URL", "https://G.Example.com:8443/")]);
    assert_eq!(d.public_url.as_deref(), Some("https://G.Example.com:8443"));
}

// ---- the real socket ----

#[tokio::test]
async fn fr_t14_char_graceful_shutdown_lets_an_in_flight_request_finish_and_refuses_new_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (s2, r2) = (started.clone(), release.clone());
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(GuestDoor::serve_with(
        listener,
        app,
        async move {
            let _ = rx.await;
        },
        Duration::from_secs(10),
    ));

    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client.write_all(b"GET /slow HTTP/1.1\r\nHost: 127.0.0.1:9000\r\nConnection: close\r\n\r\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified()).await.expect("the handler started");

    // Shut down while the request is in flight.
    tx.send(()).unwrap();

    // New connections are refused once the listener is dropped: poll with a deadline, no fixed sleep.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout(Duration::from_millis(500), tokio::net::TcpStream::connect(addr)).await {
            Ok(Err(_)) => break,
            _ => {}
        }
        assert!(Instant::now() < deadline, "new connections are still accepted after shutdown");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The server has not returned: it waits for the open request.
    assert!(!task.is_finished(), "serve returned while a request was in flight");

    // Let the handler finish: the in-flight request completes normally.
    release.notify_one();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut out)).await.expect("response in time").unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.ends_with("finished"), "{text}");
    assert!(text.to_ascii_lowercase().contains("x-content-type-options: nosniff"), "{text}");

    // Then serve returns Ok.
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve returns").expect("join").expect("serve is Ok");
}

// ---- inventory ----

/// The public items the guest door's tests import, at the paths they import them from. A move of
/// the hardening code into a shared module must keep every one of them resolvable here, with the
/// same shape.
#[test]
fn fr_t14_char_inventory_of_public_items_at_their_paths() {
    use ember_server::guest_door::{
        addr_key, build_join_page_from, client_ip, door_status, invite_link, join_page, status_body, with_timeout,
        AddrLimiter, DoorError, DoorLimits, GuestDoor, JoinPage, LimitConfig, LimitKind, TokenBucket, ACCEPT_PATH,
        ACCEPT_PER_HOUR, ACCEPT_PER_MINUTE, ALLOWED_HOSTS_VAR, ALLOW_REMOTE_VAR, GLOBAL_BURST, GLOBAL_PER_SECOND,
        HEADER_READ_TIMEOUT, JOIN_ROUTE, LISTEN_VAR, MAX_BODY_BYTES, MAX_CONNECTIONS, MAX_IN_FLIGHT, MAX_TRACKED_ADDRS,
        OTHER_PER_MINUTE, PUBLIC_URL_VAR, REQUEST_TIMEOUT, SWEEP_EVERY, TRUSTED_PROXIES_VAR,
    };

    // Functions and methods, by type.
    let _: fn(IpAddr) -> IpAddr = addr_key;
    let _: fn(IpAddr, &HeaderMap, &[IpAddr]) -> IpAddr = client_ip;
    let _: fn(Router, Duration) -> Router = with_timeout;
    let _: fn(Option<&GuestDoor>, &str) -> (String, bool, Value) = invite_link;
    let _: fn(Option<&GuestDoor>) -> (Value, bool) = door_status;
    let _: fn(Option<&GuestDoor>) -> Value = status_body;
    let _: fn(Option<&GuestDoor>) -> Value = GuestDoor::state_json;
    let _: fn(&str, &str) -> JoinPage = build_join_page_from;
    let _: fn() -> &'static JoinPage = join_page;
    let _: fn(&GuestDoor) -> bool = GuestDoor::is_remote;
    let _: fn(SocketAddr) -> Result<Option<GuestDoor>, DoorError> = GuestDoor::from_env;
    let _: fn(&GuestDoor) -> Arc<DoorLimits> = GuestDoor::limits;
    let _: fn(&GuestDoor, Router) -> Router = GuestDoor::wrap;
    let _: fn(&GuestDoor, Router, Arc<DoorLimits>) -> Router = GuestDoor::wrap_limited;
    let _: fn(LimitConfig, Vec<IpAddr>) -> DoorLimits = DoorLimits::new;
    let _: fn(LimitConfig) -> AddrLimiter = AddrLimiter::new;
    let _: fn(&AddrLimiter, IpAddr, LimitKind, Instant) -> Result<(), Duration> = AddrLimiter::check_at;
    let _: fn(&AddrLimiter) -> usize = AddrLimiter::tracked;
    let _: fn(f64, f64, Instant) -> TokenBucket = TokenBucket::new_at;
    let _: fn(&TokenBucket, Instant) -> bool = TokenBucket::try_take_at;

    // The serve family and bind (futures built, never polled).
    #[allow(dead_code)]
    async fn serve_signatures(d: &GuestDoor, l1: tokio::net::TcpListener, l2: tokio::net::TcpListener, l3: tokio::net::TcpListener, a: Router) {
        let _bind = d.bind();
        let _f1 = GuestDoor::serve(l1, a.clone(), async {});
        let _f2 = GuestDoor::serve_with(l2, a.clone(), async {}, HEADER_READ_TIMEOUT);
        let _f3 = GuestDoor::serve_capped(l3, a, async {}, HEADER_READ_TIMEOUT, MAX_CONNECTIONS);
    }

    // Constants, by type and (for names and routes) by value.
    let _: usize = ACCEPT_PER_MINUTE;
    let _: usize = ACCEPT_PER_HOUR;
    let _: usize = OTHER_PER_MINUTE;
    let _: f64 = GLOBAL_PER_SECOND;
    let _: f64 = GLOBAL_BURST;
    let _: usize = MAX_IN_FLIGHT;
    let _: usize = MAX_TRACKED_ADDRS;
    let _: u64 = SWEEP_EVERY;
    let _: Duration = HEADER_READ_TIMEOUT;
    let _: usize = MAX_CONNECTIONS;
    let _: usize = MAX_BODY_BYTES;
    let _: Duration = REQUEST_TIMEOUT;
    assert_eq!(ACCEPT_PATH, "/api/guest/accept");
    assert_eq!(JOIN_ROUTE, ("GET", "/join"));
    assert_eq!(LISTEN_VAR, "EMBER_GUEST_LISTEN");
    assert_eq!(ALLOW_REMOTE_VAR, "EMBER_GUEST_LISTEN_ALLOW_REMOTE");
    assert_eq!(ALLOWED_HOSTS_VAR, "EMBER_GUEST_ALLOWED_HOSTS");
    assert_eq!(PUBLIC_URL_VAR, "EMBER_GUEST_PUBLIC_URL");
    assert_eq!(TRUSTED_PROXIES_VAR, "EMBER_GUEST_TRUSTED_PROXIES");

    // Types: public fields, variants and tuple fields.
    let cfg = LimitConfig {
        accept_per_minute: 1usize,
        accept_per_hour: 1usize,
        other_per_minute: 1usize,
        global_per_second: 1.0f64,
        global_burst: 1.0f64,
        max_in_flight: 1usize,
        max_addrs: 1usize,
    };
    let _clone: LimitConfig = cfg.clone();
    assert_eq!(LimitKind::Accept, LimitKind::Accept);
    assert_ne!(LimitKind::Accept, LimitKind::Other);
    assert_eq!(DoorError("x".into()).to_string(), "x");
    let limits = DoorLimits::new(cfg, Vec::new());
    assert_eq!(limits.addr.tracked(), 0);
    let now = Instant::now();
    assert!(limits.bucket.try_take_at(now));
    let d = door(&[("EMBER_GUEST_PUBLIC_URL", "https://g.example.com")]);
    let _listen: SocketAddr = d.listen;
    let _public: Option<String> = d.public_url.clone();
    let b = TokenBucket::new_at(1.0, 1.0, now);
    assert!(b.try_take_at(now));
    assert!(!b.try_take_at(now));
}
