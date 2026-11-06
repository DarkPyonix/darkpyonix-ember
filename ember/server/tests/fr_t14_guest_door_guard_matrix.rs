//! FR-T14: measures the Host and Origin guard on the WRONG-METHOD path of the guest door. The guest
//! door does not set `method_not_allowed_fallback`, so a wrong method on a registered path reaches
//! axum's default method-router fallback. Whether the guard runs there, and what the answer carries
//! (`Allow`), is printed, not asserted. One line per request goes to the REAL stdout (cargo hides the
//! output of a passing test and the harness captures `print!`, so only `writeln!(stdout())` reaches
//! the CI log):
//! `GUEST_GUARD_MATRIX|method|uri|variant|status|sorted header names (no content-length)|allow=...`
//! Asserted: only what the code makes certain and the characterization tests already pin (a foreign
//! Host or Origin on a real route with its right method is 403; HEAD with a good Host is 405).
//! Details are [provisional].

use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use ember_server::guest_door::{DoorLimits, GuestDoor, LimitConfig, ACCEPT_PATH};
use tower::ServiceExt;

const OWNER: &str = "127.0.0.1:8740";
const LISTEN: &str = "127.0.0.1:9000";
const GOOD_HOST: &str = "127.0.0.1:9000";

const METHODS: [&str; 7] = ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"];

fn door() -> GuestDoor {
    let m: HashMap<String, String> = [("EMBER_GUEST_LISTEN".to_string(), LISTEN.to_string())].into();
    GuestDoor::from_lookup(|k| m.get(k).cloned(), OWNER.parse::<SocketAddr>().unwrap())
        .map_err(|e| e.to_string())
        .unwrap()
        .expect("door on")
}

fn site() -> Router {
    Router::new()
        .route("/ok", get(|| async { "ok" }).post(|| async { "posted" }))
        .route(ACCEPT_PATH, post(|| async { "accepted" }))
        .route("/echo", post(|b: Bytes| async move { b.len().to_string() }))
}

/// Which router a row is measured on.
#[derive(Clone, Copy)]
enum Kind {
    Stub,
    Wrapped,
    Limited,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Stub => "stub",
            Kind::Wrapped => "wrap",
            Kind::Limited => "limited",
        }
    }
    /// A fresh router per request, so no limiter state is shared between rows.
    fn router(self, d: &GuestDoor) -> Router {
        match self {
            Kind::Stub => site(),
            Kind::Wrapped => d.wrap(site()),
            Kind::Limited => d.wrap_limited(site(), Arc::new(DoorLimits::new(LimitConfig::default(), Vec::new()))),
        }
    }
}

/// Exactly ONE Host header is set here; the request carries no other Host.
async fn send(router: Router, method: &str, uri: &str, host: &str, origin: Option<&str>, peer: &str) -> (StatusCode, Vec<String>, String) {
    let mut req = Request::builder().method(method).uri(uri).header("host", HeaderValue::from_str(host).unwrap());
    if let Some(o) = origin {
        req = req.header("origin", HeaderValue::from_str(o).unwrap());
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut().insert(ConnectInfo(SocketAddr::new(peer.parse::<IpAddr>().unwrap(), 40000)));
    let res = router.oneshot(req).await.unwrap();
    let (parts, _) = res.into_parts();
    let mut names: Vec<String> = parts.headers.keys().map(|k| k.as_str().to_string()).filter(|k| k != "content-length").collect();
    names.sort();
    let allow = parts.headers.get("allow").map(|v| v.to_str().unwrap_or("?").to_string()).unwrap_or_else(|| "-".to_string());
    (parts.status, names, allow)
}

async fn measure(kind: Kind, out: &mut Vec<(String, String, &'static str, StatusCode)>) {
    let d = door();
    let paths = ["/ok", ACCEPT_PATH, "/echo", "/nothing"];
    let variants: [(&'static str, &str, Option<&str>); 3] = [
        ("good-host", GOOD_HOST, None),
        ("foreign-host", "evil.example", None),
        ("foreign-origin", GOOD_HOST, Some("http://evil.example")),
    ];
    let mut n: u32 = 0;
    for method in METHODS {
        for uri in paths {
            for (variant, host, origin) in variants {
                n += 1;
                // A distinct peer per request: the per-address limits never decide a row.
                let peer = format!("10.1.{}.{}", n / 250, n % 250 + 1);
                let (status, names, allow) = send(kind.router(&d), method, uri, host, origin, &peer).await;
                let line = format!(
                    "GUEST_GUARD_MATRIX|{}|{method}|{uri}|{variant}|{}|{}|allow={allow}",
                    kind.label(),
                    status.as_u16(),
                    names.join(",")
                );
                writeln!(std::io::stdout(), "{line}").unwrap();
                out.push((method.to_string(), uri.to_string(), variant, status));
            }
        }
    }
}

fn status_of(rows: &[(String, String, &'static str, StatusCode)], method: &str, uri: &str, variant: &str) -> StatusCode {
    rows.iter().find(|r| r.0 == method && r.1 == uri && r.2 == variant).unwrap_or_else(|| panic!("row {method} {uri} {variant}")).3
}

/// Prints the stub's rows (no door, nothing to assert but the stub itself) as the baseline.
#[tokio::test]
async fn fr_t14_guard_matrix_stub_baseline_is_printed() {
    let mut rows = Vec::new();
    measure(Kind::Stub, &mut rows).await;
    assert_eq!(status_of(&rows, "GET", "/ok", "good-host"), StatusCode::OK);
    assert_eq!(status_of(&rows, "POST", ACCEPT_PATH, "good-host"), StatusCode::OK);
}

#[tokio::test]
async fn fr_t14_guard_matrix_wrapped_door() {
    let mut rows = Vec::new();
    measure(Kind::Wrapped, &mut rows).await;
    certain(&rows);
}

#[tokio::test]
async fn fr_t14_guard_matrix_limited_door() {
    let mut rows = Vec::new();
    measure(Kind::Limited, &mut rows).await;
    certain(&rows);
}

/// Pinned to the CI measurement of #594 (head 12ad88d, 84 rows per kind, wrapped and limited
/// identical): the guard covers the wrong-method path of the guest door. A foreign Host or a foreign
/// Origin is a 403 for EVERY method on EVERY path, registered or not. With a good Host a wrong method
/// on a registered path is axum's default 405 (the guest door sets no `method_not_allowed_fallback`,
/// unlike the enrol door, whose wrong method is a 404), HEAD is the guard's 405 on every path, and an
/// unregistered path is 404. `Allow` is printed, not asserted: axum adds it after the fallback.
fn certain(rows: &[(String, String, &'static str, StatusCode)]) {
    for (method, uri) in [("POST", ACCEPT_PATH), ("GET", "/ok"), ("POST", "/ok"), ("POST", "/echo")] {
        assert_eq!(status_of(rows, method, uri, "good-host"), StatusCode::OK, "{method} {uri} good host");
        assert_eq!(status_of(rows, method, uri, "foreign-host"), StatusCode::FORBIDDEN, "{method} {uri} foreign host");
        assert_eq!(status_of(rows, method, uri, "foreign-origin"), StatusCode::FORBIDDEN, "{method} {uri} foreign origin");
    }
    for uri in ["/ok", ACCEPT_PATH, "/echo", "/nothing"] {
        assert_eq!(status_of(rows, "HEAD", uri, "good-host"), StatusCode::METHOD_NOT_ALLOWED, "HEAD {uri}");
    }
    // The measured wrong-method rows.
    let registered: [(&str, &[&str]); 3] = [("/ok", &["GET", "POST"]), (ACCEPT_PATH, &["POST"]), ("/echo", &["POST"])];
    for method in ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"] {
        for uri in ["/ok", ACCEPT_PATH, "/echo", "/nothing"] {
            assert_eq!(status_of(rows, method, uri, "foreign-host"), StatusCode::FORBIDDEN, "{method} {uri} foreign host");
            assert_eq!(status_of(rows, method, uri, "foreign-origin"), StatusCode::FORBIDDEN, "{method} {uri} foreign origin");
            let want = match registered.iter().find(|(u, _)| *u == uri) {
                _ if method == "HEAD" => StatusCode::METHOD_NOT_ALLOWED,
                Some((_, ok)) if ok.contains(&method) => StatusCode::OK,
                Some(_) => StatusCode::METHOD_NOT_ALLOWED,
                None => StatusCode::NOT_FOUND,
            };
            assert_eq!(status_of(rows, method, uri, "good-host"), want, "{method} {uri} good host");
        }
    }
}
