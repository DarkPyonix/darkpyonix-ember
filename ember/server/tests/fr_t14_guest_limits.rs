//! FR-T14: the guest door's limits (invite PR 5b). The numbers are a leader decision, adjustable.
//! Limiter logic is driven with injected `Instant`s (`check_at`); the layer is driven in process
//! with `oneshot` (a `ConnectInfo` put in the request, as `serve_with` does); the header-read
//! timeout and the real peer address use a real listener on 127.0.0.1:0.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::channels::guest::guest_router;
use ember_server::guest_door::{
    addr_key, client_ip, AddrLimiter, DoorLimits, GuestDoor, LimitConfig, LimitKind, TokenBucket, ACCEPT_PATH,
    ACCEPT_PER_HOUR, ACCEPT_PER_MINUTE, GLOBAL_BURST, GLOBAL_PER_SECOND, HEADER_READ_TIMEOUT, MAX_CONNECTIONS, MAX_IN_FLIGHT,
    MAX_TRACKED_ADDRS, OTHER_PER_MINUTE,
};
use ember_server::session::Sessions;
use ember_server::store::Store;
use tower::ServiceExt;

const OWNER: &str = "127.0.0.1:8740";
const LOCAL_HOST: &str = "127.0.0.1:9000";

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn door_with(vars: &[(&str, &str)]) -> Result<Option<GuestDoor>, String> {
    let m: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    GuestDoor::from_lookup(|k| m.get(k).cloned(), OWNER.parse::<SocketAddr>().unwrap()).map_err(|e| e.to_string())
}

fn door(vars: &[(&str, &str)]) -> GuestDoor {
    let mut v = vec![("EMBER_GUEST_LISTEN", "127.0.0.1:9000")];
    v.extend_from_slice(vars);
    door_with(&v).unwrap().expect("door on")
}

fn guest() -> Router {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    guest_router(store, sessions)
}

fn limited(d: &GuestDoor, cfg: LimitConfig, router: Router) -> Router {
    d.wrap_limited(router, Arc::new(DoorLimits::new(cfg, Vec::new())))
}

fn limited_with_trusted(d: &GuestDoor, cfg: LimitConfig, trusted: Vec<IpAddr>, router: Router) -> Router {
    d.wrap_limited(router, Arc::new(DoorLimits::new(cfg, trusted)))
}

/// One request from `peer`, with extra headers. Accept requests carry a JSON body.
async fn from(app: &Router, peer: &str, method: &str, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, axum::http::HeaderMap) {
    let mut req = Request::builder().method(method).uri(uri).header("host", LOCAL_HOST);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let body = if uri == ACCEPT_PATH {
        req = req.header("content-type", "application/json");
        Body::from(r#"{"token":"dpi_nope","name":"x"}"#)
    } else {
        Body::empty()
    };
    let mut req = req.body(body).unwrap();
    req.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip(peer), 40000)));
    let res = app.clone().oneshot(req).await.unwrap();
    (res.status(), res.headers().clone())
}

#[test]
fn fr_t14_limit_numbers_are_the_leaders_decision() {
    assert_eq!((ACCEPT_PER_MINUTE, ACCEPT_PER_HOUR, OTHER_PER_MINUTE), (5, 20, 120));
    assert_eq!((GLOBAL_PER_SECOND, GLOBAL_BURST), (50.0, 100.0));
    assert_eq!((MAX_IN_FLIGHT, MAX_TRACKED_ADDRS, MAX_CONNECTIONS), (64, 10_000, 256));
    assert_eq!(HEADER_READ_TIMEOUT, Duration::from_secs(10));
}

// ---- AddrLimiter, injected clock ----

#[test]
fn fr_t14_accept_is_limited_to_5_a_minute_and_20_an_hour() {
    let l = AddrLimiter::new(LimitConfig::default());
    let t0 = Instant::now();
    let a = ip("203.0.113.9");
    for i in 0..5 {
        assert!(l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(i)).is_ok(), "{i}");
    }
    let wait = l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(5)).unwrap_err();
    assert!(wait >= Duration::from_secs(50) && wait <= Duration::from_secs(60), "{wait:?}");
    // A minute after the first, one slot is free again.
    assert!(l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(61)).is_ok());
    // Hourly: spend 20 spread so the minute limit never trips, the 21st within the hour is refused.
    let l = AddrLimiter::new(LimitConfig::default());
    for i in 0..20u64 {
        assert!(l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(i * 70)).is_ok(), "{i}");
    }
    let wait = l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(20 * 70)).unwrap_err();
    assert!(wait > Duration::from_secs(60), "{wait:?}");
    // After the first entry is an hour old the next passes.
    assert!(l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(3600 + 1)).is_ok());
}

#[test]
fn fr_t14_other_requests_are_limited_to_120_a_minute() {
    let l = AddrLimiter::new(LimitConfig::default());
    let t0 = Instant::now();
    let a = ip("203.0.113.9");
    for i in 0..120 {
        assert!(l.check_at(a, LimitKind::Other, t0 + Duration::from_millis(i)).is_ok(), "{i}");
    }
    assert!(l.check_at(a, LimitKind::Other, t0 + Duration::from_secs(1)).is_err());
    assert!(l.check_at(a, LimitKind::Other, t0 + Duration::from_secs(61)).is_ok());
    // The accept budget is separate.
    assert!(l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(1)).is_ok());
}

#[test]
fn fr_t14_a_limited_address_does_not_limit_another() {
    let l = AddrLimiter::new(LimitConfig::default());
    let t0 = Instant::now();
    for _ in 0..5 {
        l.check_at(ip("203.0.113.9"), LimitKind::Accept, t0).unwrap();
    }
    assert!(l.check_at(ip("203.0.113.9"), LimitKind::Accept, t0).is_err());
    assert!(l.check_at(ip("203.0.113.10"), LimitKind::Accept, t0).is_ok());
}

#[test]
fn fr_t14_ipv6_addresses_in_one_slash_64_share_a_bucket() {
    let l = AddrLimiter::new(LimitConfig::default());
    let t0 = Instant::now();
    for i in 0..5 {
        let a = ip(&format!("2001:db8:1:2::{:x}", i + 1));
        l.check_at(a, LimitKind::Accept, t0).unwrap();
    }
    assert!(l.check_at(ip("2001:db8:1:2:ffff:ffff:ffff:ffff"), LimitKind::Accept, t0).is_err());
    // Another /64 is another bucket.
    assert!(l.check_at(ip("2001:db8:1:3::1"), LimitKind::Accept, t0).is_ok());
    assert_eq!(addr_key(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd")), ip("2001:db8:1:2::"));
    // IPv4 stays exact, and an IPv4-mapped IPv6 address is that IPv4 address.
    assert_eq!(addr_key(ip("203.0.113.9")), ip("203.0.113.9"));
    assert_eq!(addr_key(ip("::ffff:203.0.113.9")), ip("203.0.113.9"));
}

#[test]
fn fr_t14_limiter_memory_is_bounded_and_evicts_the_oldest() {
    let cfg = LimitConfig { max_addrs: 100, ..LimitConfig::default() };
    let l = AddrLimiter::new(cfg);
    let t0 = Instant::now();
    for i in 0..20_000u32 {
        let a = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + i));
        l.check_at(a, LimitKind::Other, t0 + Duration::from_micros(i as u64)).unwrap();
        assert!(l.tracked() <= 100, "{i}: {}", l.tracked());
    }
    assert_eq!(l.tracked(), 100);
    // The first address was evicted: its history is gone (it may spend a fresh accept budget).
    let first = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000u32));
    for _ in 0..5 {
        l.check_at(first, LimitKind::Accept, t0 + Duration::from_secs(1)).unwrap();
    }
}

#[test]
fn fr_t14_eviction_takes_the_oldest_touched_not_the_newest() {
    let l = AddrLimiter::new(LimitConfig { max_addrs: 2, ..LimitConfig::default() });
    let t0 = Instant::now();
    let (a, b, c) = (ip("198.51.100.1"), ip("198.51.100.2"), ip("198.51.100.3"));
    for _ in 0..5 {
        l.check_at(a, LimitKind::Accept, t0).unwrap();
    }
    l.check_at(b, LimitKind::Other, t0 + Duration::from_secs(1)).unwrap();
    // a is touched again (refused), so b is the oldest-touched when c arrives.
    assert!(l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(2)).is_err());
    l.check_at(c, LimitKind::Other, t0 + Duration::from_secs(3)).unwrap();
    assert!(l.check_at(a, LimitKind::Accept, t0 + Duration::from_secs(4)).is_err(), "a was kept");
}

#[test]
fn fr_t14_limiter_forgets_idle_addresses() {
    let l = AddrLimiter::new(LimitConfig::default());
    let t0 = Instant::now();
    for i in 0..200u32 {
        l.check_at(IpAddr::from(std::net::Ipv4Addr::from(0xc000_0200 + i)), LimitKind::Other, t0).unwrap();
    }
    assert_eq!(l.tracked(), 200);
    // An hour later the sweep (every 256th call) drops every idle address.
    let later = t0 + Duration::from_secs(3601);
    for _ in 0..256 {
        let _ = l.check_at(ip("192.0.2.1"), LimitKind::Other, later);
    }
    assert_eq!(l.tracked(), 1);
}

#[test]
fn fr_t14_the_token_bucket_refills_and_is_capped() {
    let t0 = Instant::now();
    let b = TokenBucket::new_at(50.0, 100.0, t0);
    for i in 0..100 {
        assert!(b.try_take_at(t0), "{i}");
    }
    assert!(!b.try_take_at(t0));
    // 50 a second: 25 ms is one and a quarter tokens.
    assert!(b.try_take_at(t0 + Duration::from_millis(25)));
    assert!(!b.try_take_at(t0 + Duration::from_millis(25)));
    // A long idle refills to the burst, not beyond.
    let later = t0 + Duration::from_secs(3600);
    let mut n = 0;
    while b.try_take_at(later) {
        n += 1;
    }
    assert_eq!(n, 100);
}

// ---- client address and X-Forwarded-For ----

#[test]
fn fr_t14_forwarded_for_is_ignored_unless_the_peer_is_a_trusted_proxy() {
    let mut h = axum::http::HeaderMap::new();
    h.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
    let proxy = ip("10.0.0.2");
    // Untrusted peer: the header is ignored entirely.
    assert_eq!(client_ip(ip("203.0.113.9"), &h, &[]), ip("203.0.113.9"));
    assert_eq!(client_ip(ip("203.0.113.9"), &h, &[proxy]), ip("203.0.113.9"));
    // Trusted peer: the entry is believed.
    assert_eq!(client_ip(proxy, &h, &[proxy]), ip("1.2.3.4"));
    // Right-most untrusted entry; a trusted hop is skipped, a forged left entry is not used.
    let mut h = axum::http::HeaderMap::new();
    h.insert("x-forwarded-for", "9.9.9.9, 1.2.3.4, 10.0.0.3".parse().unwrap());
    assert_eq!(client_ip(proxy, &h, &[proxy, ip("10.0.0.3")]), ip("1.2.3.4"));
    // Several header lines count as one chain, in order.
    let mut h = axum::http::HeaderMap::new();
    h.append("x-forwarded-for", "9.9.9.9".parse().unwrap());
    h.append("x-forwarded-for", "1.2.3.4".parse().unwrap());
    assert_eq!(client_ip(proxy, &h, &[proxy]), ip("1.2.3.4"));
    // Garbage in the chain, or no header, or only trusted entries: the peer.
    let mut h = axum::http::HeaderMap::new();
    h.insert("x-forwarded-for", "1.2.3.4, not-an-ip".parse().unwrap());
    assert_eq!(client_ip(proxy, &h, &[proxy]), proxy);
    assert_eq!(client_ip(proxy, &axum::http::HeaderMap::new(), &[proxy]), proxy);
    let mut h = axum::http::HeaderMap::new();
    h.insert("x-forwarded-for", "10.0.0.2".parse().unwrap());
    assert_eq!(client_ip(proxy, &h, &[proxy]), proxy);
}

#[test]
fn fr_t14_an_invalid_trusted_proxy_is_a_startup_error() {
    let e = door_with(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), ("EMBER_GUEST_TRUSTED_PROXIES", "10.0.0.1, nonsense")]).unwrap_err();
    assert!(e.contains("EMBER_GUEST_TRUSTED_PROXIES") && e.contains("nonsense"), "{e}");
    assert!(door_with(&[("EMBER_GUEST_LISTEN", "127.0.0.1:9000"), ("EMBER_GUEST_TRUSTED_PROXIES", "10.0.0.1, ::1,")]).unwrap().is_some());
}

// ---- the layer ----

#[tokio::test]
async fn fr_t14_the_6th_accept_in_a_minute_is_429_even_when_all_failed() {
    let d = door(&[]);
    let app = limited(&d, LimitConfig::default(), guest());
    for i in 0..5 {
        let (s, _) = from(&app, "203.0.113.9", "POST", ACCEPT_PATH, &[]).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "attempt {i} fails with the same 404 and still counts");
    }
    let (s, h) = from(&app, "203.0.113.9", "POST", ACCEPT_PATH, &[]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = h.get("retry-after").expect("Retry-After").to_str().unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff", "limit answers carry the door headers");
    // Another address is untouched; a GET from the limited one is not the accept budget.
    assert_eq!(from(&app, "203.0.113.10", "POST", ACCEPT_PATH, &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(from(&app, "203.0.113.9", "GET", "/join", &[]).await.0, StatusCode::OK);
}

#[tokio::test]
async fn fr_t14_other_requests_are_429_after_120_a_minute() {
    let d = door(&[]);
    let app = limited(&d, LimitConfig { global_burst: 1000.0, ..LimitConfig::default() }, guest());
    for i in 0..120 {
        assert_eq!(from(&app, "203.0.113.9", "GET", "/join", &[]).await.0, StatusCode::OK, "{i}");
    }
    assert_eq!(from(&app, "203.0.113.9", "GET", "/join", &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn fr_t14_ipv6_in_one_slash_64_shares_the_accept_budget_on_the_door() {
    let d = door(&[]);
    let app = limited(&d, LimitConfig::default(), guest());
    for i in 1..=5 {
        let peer = format!("2001:db8:5:6::{i:x}");
        assert_eq!(from(&app, &peer, "POST", ACCEPT_PATH, &[]).await.0, StatusCode::NOT_FOUND);
    }
    assert_eq!(from(&app, "2001:db8:5:6:1:2:3:4", "POST", ACCEPT_PATH, &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn fr_t14_a_spoofed_forwarded_for_from_an_untrusted_peer_cannot_dodge_the_limit() {
    let d = door(&[]);
    let app = limited(&d, LimitConfig::default(), guest());
    for i in 0..5 {
        let spoof = format!("198.51.100.{i}");
        assert_eq!(from(&app, "203.0.113.9", "POST", ACCEPT_PATH, &[("x-forwarded-for", &spoof)]).await.0, StatusCode::NOT_FOUND);
    }
    let (s, _) = from(&app, "203.0.113.9", "POST", ACCEPT_PATH, &[("x-forwarded-for", "198.51.100.77")]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "a fresh forged address did not change the key");
    // Nor can it frame someone else: the forged victim's own budget is untouched.
    assert_eq!(from(&app, "198.51.100.1", "POST", ACCEPT_PATH, &[]).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fr_t14_a_trusted_proxy_gets_the_forwarded_client_as_the_key() {
    let d = door(&[]);
    let app = limited_with_trusted(&d, LimitConfig::default(), vec![ip("10.0.0.2")], guest());
    for _ in 0..5 {
        assert_eq!(from(&app, "10.0.0.2", "POST", ACCEPT_PATH, &[("x-forwarded-for", "1.1.1.1")]).await.0, StatusCode::NOT_FOUND);
    }
    assert_eq!(from(&app, "10.0.0.2", "POST", ACCEPT_PATH, &[("x-forwarded-for", "1.1.1.1")]).await.0, StatusCode::TOO_MANY_REQUESTS);
    // A different client behind the same proxy has its own budget; a left-side forgery does not help the first.
    assert_eq!(from(&app, "10.0.0.2", "POST", ACCEPT_PATH, &[("x-forwarded-for", "2.2.2.2")]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        from(&app, "10.0.0.2", "POST", ACCEPT_PATH, &[("x-forwarded-for", "7.7.7.7, 1.1.1.1")]).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn fr_t14_the_empty_global_bucket_answers_503() {
    let d = door(&[]);
    let cfg = LimitConfig { global_per_second: 0.001, global_burst: 3.0, ..LimitConfig::default() };
    let app = limited(&d, cfg, guest());
    for i in 0..3 {
        assert_eq!(from(&app, &format!("203.0.113.{i}"), "GET", "/join", &[]).await.0, StatusCode::OK);
    }
    // Any address, any path: the door is busy.
    let (s, _) = from(&app, "203.0.113.99", "GET", "/join", &[]).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn fr_t14_more_than_the_concurrency_limit_in_flight_is_503() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (st, rel) = (started.clone(), release.clone());
    let slow = Router::new().route(
        "/api/guest/slow",
        axum::routing::get(move || {
            let (st, rel) = (st.clone(), rel.clone());
            async move {
                st.notify_one();
                rel.notified().await;
                "done"
            }
        }),
    );
    let d = door(&[]);
    let cfg = LimitConfig { max_in_flight: 1, ..LimitConfig::default() };
    let app = limited(&d, cfg, slow);
    let first = {
        let app = app.clone();
        tokio::spawn(async move { from(&app, "203.0.113.1", "GET", "/api/guest/slow", &[]).await.0 })
    };
    tokio::time::timeout(Duration::from_secs(5), started.notified()).await.expect("first request reached the handler");
    // Not queued: answered at once.
    let second = tokio::time::timeout(Duration::from_secs(2), from(&app, "203.0.113.2", "GET", "/api/guest/slow", &[]))
        .await
        .expect("the second request is not left waiting");
    assert_eq!(second.0, StatusCode::SERVICE_UNAVAILABLE);
    release.notify_one();
    assert_eq!(first.await.unwrap(), StatusCode::OK);
    // The slot is free again.
    release.notify_one();
    assert_eq!(from(&app, "203.0.113.3", "GET", "/api/guest/slow", &[]).await.0, StatusCode::OK);
}

#[tokio::test]
async fn fr_t14_a_limited_door_without_a_peer_address_refuses() {
    let d = door(&[]);
    let app = limited(&d, LimitConfig::default(), guest());
    let req = Request::builder().uri("/join").header("host", LOCAL_HOST).body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

// ---- the real socket ----

async fn real_door(timeout: Duration, cfg: LimitConfig) -> (SocketAddr, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let d = door(&[]);
    let app = limited(&d, cfg, guest());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(GuestDoor::serve_with(
        listener,
        app,
        async move {
            let _ = rx.await;
        },
        timeout,
    ));
    (addr, tx, task)
}

async fn raw(addr: SocketAddr, request: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn fr_t14_the_real_door_serves_with_the_peer_address_and_limits_by_it() {
    let (addr, stop, task) = real_door(Duration::from_secs(10), LimitConfig::default()).await;
    let req = "GET /join HTTP/1.1\r\nHost: 127.0.0.1:9000\r\nConnection: close\r\n\r\n";
    let r = raw(addr, req).await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(r.to_ascii_lowercase().contains("nosniff"), "{r}");
    let acc = "POST /api/guest/accept HTTP/1.1\r\nHost: 127.0.0.1:9000\r\nContent-Type: application/json\r\nContent-Length: 31\r\nConnection: close\r\n\r\n{\"token\":\"dpi_nope\",\"name\":\"x\"}";
    let mut last = String::new();
    for _ in 0..6 {
        last = raw(addr, acc).await;
    }
    assert!(last.starts_with("HTTP/1.1 429"), "{last}");
    assert!(last.to_ascii_lowercase().contains("retry-after"), "{last}");
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("serve returns on shutdown").unwrap().unwrap();
}

#[tokio::test]
async fn fr_t14_a_partial_header_is_disconnected_by_the_header_read_timeout() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // A short test-only timeout; the door's own is HEADER_READ_TIMEOUT (10 s, asserted above).
    let (addr, stop, task) = real_door(Duration::from_secs(1), LimitConfig::default()).await;
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /join HTTP/1.1\r\nHost: 127.0.0.1:9000\r\nX-Slow: a").await.unwrap();
    let t0 = Instant::now();
    let mut buf = Vec::new();
    // The peer closes (EOF or reset) well inside 11 s; a hung connection would time this out.
    let r = tokio::time::timeout(Duration::from_secs(11), s.read_to_end(&mut buf)).await;
    assert!(r.is_ok(), "the connection was still open after 11 s");
    assert!(t0.elapsed() < Duration::from_secs(11), "{:?}", t0.elapsed());
    assert!(t0.elapsed() >= Duration::from_millis(500), "closed too early to be the timeout: {:?}", t0.elapsed());
    // The door keeps serving others.
    let ok = raw(addr, "GET /join HTTP/1.1\r\nHost: 127.0.0.1:9000\r\nConnection: close\r\n\r\n").await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    let _ = stop.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

// ---- review of #425: order of checks, XFF from the right, connection cap ----

fn xff(peer: &str, value: &[&str], trusted: &[&str]) -> IpAddr {
    let mut h = axum::http::HeaderMap::new();
    for v in value {
        h.append("x-forwarded-for", v.parse().unwrap());
    }
    let t: Vec<IpAddr> = trusted.iter().map(|x| ip(x)).collect();
    client_ip(ip(peer), &h, &t)
}

#[test]
fn fr_t14_forwarded_for_is_walked_from_the_right_and_stops_at_the_first_untrusted_entry() {
    let t = ["10.0.0.2"];
    // A forged or unparsable entry to the left changes nothing.
    assert_eq!(xff("10.0.0.2", &["x, 203.0.113.9"], &t), ip("203.0.113.9"));
    assert_eq!(xff("10.0.0.2", &["1.2.3.4, 203.0.113.9"], &t), ip("203.0.113.9"));
    assert_eq!(xff("10.0.0.2", &["garbage, 1.2.3.4, 203.0.113.9"], &t), ip("203.0.113.9"));
    // An unparsable first untrusted entry: the peer.
    assert_eq!(xff("10.0.0.2", &["x"], &t), ip("10.0.0.2"));
    assert_eq!(xff("10.0.0.2", &["203.0.113.9, x"], &t), ip("10.0.0.2"));
    assert_eq!(xff("10.0.0.2", &["203.0.113.9,"], &t), ip("10.0.0.2"));
    // Two trusted hops: the trusted one on the right is skipped.
    assert_eq!(xff("10.0.0.3", &["203.0.113.9, 10.0.0.2"], &["10.0.0.2", "10.0.0.3"]), ip("203.0.113.9"));
    // A second header line is to the right of the first.
    assert_eq!(xff("10.0.0.2", &["1.2.3.4", "203.0.113.9"], &t), ip("203.0.113.9"));
    assert_eq!(xff("10.0.0.2", &["203.0.113.9", "x"], &t), ip("10.0.0.2"));
    // Only trusted entries, or none: the peer. An untrusted peer: its own address, header ignored.
    assert_eq!(xff("10.0.0.2", &["10.0.0.2"], &t), ip("10.0.0.2"));
    assert_eq!(xff("10.0.0.2", &[], &t), ip("10.0.0.2"));
    assert_eq!(xff("198.51.100.5", &["203.0.113.9"], &t), ip("198.51.100.5"));
}

#[test]
fn fr_t14_forwarded_for_ports_and_brackets_normalise_to_the_ip() {
    let t = ["10.0.0.2"];
    for v in ["203.0.113.9", " 203.0.113.9 ", "203.0.113.9:51234", "::ffff:203.0.113.9"] {
        assert_eq!(xff("10.0.0.2", &[v], &t), ip("203.0.113.9"), "{v}");
    }
    for v in ["2001:db8::7", "[2001:db8::7]", "[2001:db8::7]:443", "  [2001:db8::7]:8080"] {
        assert_eq!(xff("10.0.0.2", &[v], &t), ip("2001:db8::7"), "{v}");
    }
    // A trusted proxy written with a port is still that proxy.
    assert_eq!(xff("10.0.0.2", &["203.0.113.9, 10.0.0.2:4000"], &t), ip("203.0.113.9"));
}

#[tokio::test]
async fn fr_t14_garbage_from_one_client_behind_a_proxy_cannot_move_another_guests_key() {
    let d = door(&[]);
    let app = limited_with_trusted(&d, LimitConfig::default(), vec![ip("10.0.0.2")], guest());
    // The attacker sends garbage on the left of what the proxy appends; his own address is keyed.
    for i in 0..5 {
        let v = format!("garbage{i}, 6.6.6.6");
        assert_eq!(from(&app, "10.0.0.2", "POST", ACCEPT_PATH, &[("x-forwarded-for", &v)]).await.0, StatusCode::NOT_FOUND);
    }
    assert_eq!(from(&app, "10.0.0.2", "POST", ACCEPT_PATH, &[("x-forwarded-for", "zzz, 6.6.6.6")]).await.0, StatusCode::TOO_MANY_REQUESTS);
    // Another guest behind the same proxy is untouched, also when the attacker puts that address on the left.
    assert_eq!(from(&app, "10.0.0.2", "POST", ACCEPT_PATH, &[("x-forwarded-for", "7.7.7.7")]).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fr_t14_a_request_refused_per_address_does_not_spend_the_global_budget() {
    let d = door(&[]);
    // 130 tokens, no refill: one address may use 120 (its limit), then floods 100 more.
    let cfg = LimitConfig { global_per_second: 0.0001, global_burst: 130.0, ..LimitConfig::default() };
    let app = limited(&d, cfg, guest());
    for i in 0..120 {
        assert_eq!(from(&app, "203.0.113.9", "GET", "/join", &[]).await.0, StatusCode::OK, "{i}");
    }
    for _ in 0..100 {
        assert_eq!(from(&app, "203.0.113.9", "GET", "/join", &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
    }
    // The flood cost no global token: 10 are left for everybody else, and nobody sees 503.
    for i in 0..10 {
        assert_eq!(from(&app, &format!("203.0.113.{}", 100 + i), "GET", "/join", &[]).await.0, StatusCode::OK, "{i}");
    }
    assert_eq!(from(&app, "203.0.113.9", "GET", "/join", &[]).await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn fr_t14_connections_over_the_cap_are_closed_and_a_slot_frees_when_one_closes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let d = door(&[]);
    let app = limited(&d, LimitConfig::default(), guest());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(GuestDoor::serve_capped(
        listener,
        app,
        async move {
            let _ = rx.await;
        },
        Duration::from_secs(30),
        2,
    ));
    // Two idle connections fill the cap.
    let c1 = tokio::net::TcpStream::connect(addr).await.unwrap();
    let c2 = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The third is closed by the door: it reads EOF (or a reset) at once, with no response.
    let mut c3 = tokio::net::TcpStream::connect(addr).await.unwrap();
    let _ = c3.write_all(b"GET /join HTTP/1.1\r\nHost: 127.0.0.1:9000\r\n\r\n").await;
    let mut buf = Vec::new();
    let r = tokio::time::timeout(Duration::from_secs(5), c3.read_to_end(&mut buf)).await;
    assert!(r.is_ok(), "the extra connection was left open");
    assert!(buf.is_empty(), "the extra connection got an answer: {:?}", String::from_utf8_lossy(&buf));
    // One of the first two closes; a new connection is then served.
    drop(c1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ok = raw(addr, "GET /join HTTP/1.1\r\nHost: 127.0.0.1:9000\r\nConnection: close\r\n\r\n").await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    drop(c2);
    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}
