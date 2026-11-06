//! The door-agnostic parts of the limits: the address key, the global token bucket, the client
//! address behind trusted proxies and the 429 and 503 answers. Moved without change from
//! `guest_door.rs`; the tests in `ember/server/tests/fr_t14_guest_limits.rs` pin them.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// IPv4-mapped IPv6 addresses compare as their IPv4 address.
pub(crate) fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// The limiter key: IPv4 exact, IPv6 reduced to its /64 (one host cannot rotate through its range).
pub fn addr_key(ip: IpAddr) -> IpAddr {
    match normalize_ip(ip) {
        IpAddr::V6(v6) => {
            let mut o = v6.octets();
            for b in &mut o[8..] {
                *b = 0;
            }
            IpAddr::V6(Ipv6Addr::from(o))
        }
        v4 => v4,
    }
}

/// A token bucket for the whole door.
pub struct TokenBucket {
    per_second: f64,
    burst: f64,
    state: Mutex<(f64, Instant)>,
}

impl TokenBucket {
    /// Starts full at `now`.
    pub fn new_at(per_second: f64, burst: f64, now: Instant) -> Self {
        Self { per_second, burst, state: Mutex::new((burst, now)) }
    }

    pub fn try_take_at(&self, now: Instant) -> bool {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let elapsed = now.saturating_duration_since(st.1).as_secs_f64();
        st.0 = (st.0 + elapsed * self.per_second).min(self.burst);
        if now > st.1 {
            st.1 = now;
        }
        if st.0 >= 1.0 {
            st.0 -= 1.0;
            true
        } else {
            false
        }
    }
}

/// One `X-Forwarded-For` entry as an address: whitespace trimmed, an IPv4 `:port`, `[v6]:port`
/// and `[v6]` accepted, the port dropped, so one address is one key.
fn parse_forwarded(entry: &str) -> Option<IpAddr> {
    let e = entry.trim();
    if let Ok(ip) = e.parse::<IpAddr>() {
        return Some(normalize_ip(ip));
    }
    if let Ok(sa) = e.parse::<SocketAddr>() {
        return Some(normalize_ip(sa.ip()));
    }
    let inner = e.strip_prefix('[')?.strip_suffix(']')?;
    inner.parse::<IpAddr>().ok().map(normalize_ip)
}

/// The address a request counts against. The TCP `peer` unless it is a trusted proxy; then the
/// chain of `X-Forwarded-For` is walked from the right: trusted hops are skipped and the first
/// entry that is not a trusted proxy is the client. Everything left of it is ignored (a client
/// can forge only what it sends, which the proxy puts on the left). If that entry does not
/// parse, or every entry is a trusted hop, the key is `peer`.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[IpAddr]) -> IpAddr {
    let peer = normalize_ip(peer);
    if !trusted.contains(&peer) {
        return peer;
    }
    // Entries in arrival order across all header lines; a line that is not text is one bad entry.
    let mut chain: Vec<Option<&str>> = Vec::new();
    for v in headers.get_all("x-forwarded-for").iter() {
        match v.to_str() {
            Ok(v) => chain.extend(v.split(',').map(Some)),
            Err(_) => chain.push(None),
        }
    }
    for entry in chain.into_iter().rev() {
        match entry.and_then(parse_forwarded) {
            Some(ip) if trusted.contains(&ip) => continue,
            Some(ip) => return ip,
            None => return peer,
        }
    }
    peer
}

pub(crate) fn too_many(wait: Duration) -> Response {
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let mut r = (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": "too many requests", "code": "rate_limited" })))
        .into_response();
    r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
    r
}

pub(crate) fn busy() -> Response {
    let mut r = (StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({ "error": "busy", "code": "busy" }))).into_response();
    r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    r
}
