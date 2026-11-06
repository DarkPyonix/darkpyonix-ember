//! The door-agnostic parts of the limits: the address key, the global token bucket, the client
//! address behind trusted proxies and the 429 and 503 answers. Moved without change from
//! `guest_door.rs`; the tests in `ember/server/tests/fr_t14_guest_limits.rs` pin them.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::connect_info::ConnectInfo;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::guest_door::{
    ACCEPT_PATH, ACCEPT_PER_HOUR, ACCEPT_PER_MINUTE, GLOBAL_BURST, GLOBAL_PER_SECOND, MAX_IN_FLIGHT, MAX_TRACKED_ADDRS,
    OTHER_PER_MINUTE, SWEEP_EVERY,
};

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

// ---- limits (PR 5b) -------------------------------------------------------------------------

/// The numbers of the limits. `Default` is the leader's decision (2026-11-06, adjustable).
#[derive(Clone, Debug)]
pub struct LimitConfig {
    pub accept_per_minute: usize,
    pub accept_per_hour: usize,
    pub other_per_minute: usize,
    pub global_per_second: f64,
    pub global_burst: f64,
    pub max_in_flight: usize,
    pub max_addrs: usize,
}

impl Default for LimitConfig {
    fn default() -> Self {
        Self {
            accept_per_minute: ACCEPT_PER_MINUTE,
            accept_per_hour: ACCEPT_PER_HOUR,
            other_per_minute: OTHER_PER_MINUTE,
            global_per_second: GLOBAL_PER_SECOND,
            global_burst: GLOBAL_BURST,
            max_in_flight: MAX_IN_FLIGHT,
            max_addrs: MAX_TRACKED_ADDRS,
        }
    }
}

/// Which budget a request spends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// `POST /api/guest/accept`, counted whatever the answer. `Accept` means "the strict class":
    /// the request is `POST` to the door's `strict_path` (`ACCEPT_PATH` for the guest door).
    Accept,
    /// Everything else.
    Other,
}

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);

struct Entry {
    accept: VecDeque<Instant>,
    other: VecDeque<Instant>,
    touched: Instant,
}

impl Entry {
    fn new(now: Instant) -> Self {
        Self { accept: VecDeque::new(), other: VecDeque::new(), touched: now }
    }

    fn prune(&mut self, now: Instant) {
        while self.accept.front().is_some_and(|t| now.saturating_duration_since(*t) >= HOUR) {
            self.accept.pop_front();
        }
        while self.other.front().is_some_and(|t| now.saturating_duration_since(*t) >= MINUTE) {
            self.other.pop_front();
        }
    }
}

struct LimiterState {
    map: HashMap<IpAddr, Entry>,
    calls: u64,
}

/// Sliding-window limits per remote address. Timestamps older than the window are dropped when
/// an address is touched and by a sweep every [`SWEEP_EVERY`] calls; at most `max_addrs`
/// addresses are tracked, the oldest-touched is evicted for a new one, so memory is bounded.
pub struct AddrLimiter {
    cfg: LimitConfig,
    state: Mutex<LimiterState>,
}

impl AddrLimiter {
    pub fn new(mut cfg: LimitConfig) -> Self {
        // A zero would make the index arithmetic below meaningless; the least allowed is one.
        cfg.accept_per_minute = cfg.accept_per_minute.max(1);
        cfg.accept_per_hour = cfg.accept_per_hour.max(1);
        cfg.other_per_minute = cfg.other_per_minute.max(1);
        Self { cfg, state: Mutex::new(LimiterState { map: HashMap::new(), calls: 0 }) }
    }

    /// Spends one request of `kind` for `ip` at `now`. `Err(wait)` is how long until the request
    /// would pass; a refused request is not recorded.
    pub fn check_at(&self, ip: IpAddr, kind: LimitKind, now: Instant) -> Result<(), Duration> {
        let key = addr_key(ip);
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.calls += 1;
        if st.calls % SWEEP_EVERY == 0 {
            st.map.retain(|_, e| {
                e.prune(now);
                !(e.accept.is_empty() && e.other.is_empty())
            });
        }
        if !st.map.contains_key(&key) && st.map.len() >= self.cfg.max_addrs.max(1) {
            if let Some(oldest) = st.map.iter().min_by_key(|(_, e)| e.touched).map(|(k, _)| *k) {
                st.map.remove(&oldest);
            }
        }
        let e = st.map.entry(key).or_insert_with(|| Entry::new(now));
        e.touched = now;
        e.prune(now);
        match kind {
            LimitKind::Accept => {
                let mut wait = Duration::ZERO;
                if e.accept.len() >= self.cfg.accept_per_hour {
                    let i = e.accept.len() - self.cfg.accept_per_hour;
                    wait = wait.max(HOUR.saturating_sub(now.saturating_duration_since(e.accept[i])));
                }
                let in_minute = e.accept.iter().rev().take_while(|t| now.saturating_duration_since(**t) < MINUTE).count();
                if in_minute >= self.cfg.accept_per_minute {
                    let i = e.accept.len() - self.cfg.accept_per_minute;
                    wait = wait.max(MINUTE.saturating_sub(now.saturating_duration_since(e.accept[i])));
                }
                if wait > Duration::ZERO {
                    return Err(wait.max(Duration::from_secs(1)));
                }
                e.accept.push_back(now);
            }
            LimitKind::Other => {
                if e.other.len() >= self.cfg.other_per_minute {
                    let i = e.other.len() - self.cfg.other_per_minute;
                    let wait = MINUTE.saturating_sub(now.saturating_duration_since(e.other[i]));
                    return Err(wait.max(Duration::from_secs(1)));
                }
                e.other.push_back(now);
            }
        }
        Ok(())
    }

    /// Addresses currently tracked.
    pub fn tracked(&self) -> usize {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).map.len()
    }
}

/// Everything the limit layer needs: per-address limiter, global bucket, in-flight cap, and the
/// trusted proxies that decide whether `X-Forwarded-For` is read.
pub struct DoorLimits {
    pub addr: AddrLimiter,
    pub bucket: TokenBucket,
    in_flight: Arc<tokio::sync::Semaphore>,
    trusted: Vec<IpAddr>,
    /// The path of the strict class (`LimitKind::Accept`), `POST` only.
    strict_path: &'static str,
}

impl DoorLimits {
    /// The GUEST door's limits: the strict class is `POST` on the guest accept path. Another door
    /// must NOT call this: it would leave its own strict path in the loose class (120 a minute
    /// instead of the strict budget). Use [`DoorLimits::with_strict_path`] with the door's own path.
    pub fn new(cfg: LimitConfig, trusted: Vec<IpAddr>) -> Self {
        Self::with_strict_path(cfg, trusted, ACCEPT_PATH)
    }

    /// As [`DoorLimits::new`] with the path of the strict class chosen by the door.
    pub fn with_strict_path(cfg: LimitConfig, trusted: Vec<IpAddr>, strict_path: &'static str) -> Self {
        Self {
            bucket: TokenBucket::new_at(cfg.global_per_second, cfg.global_burst, Instant::now()),
            in_flight: Arc::new(tokio::sync::Semaphore::new(cfg.max_in_flight)),
            addr: AddrLimiter::new(cfg),
            trusted: trusted.into_iter().map(normalize_ip).collect(),
            strict_path,
        }
    }
}

/// The per-address limits, then the global bucket and the in-flight cap. Runs before the Host check and
/// before any body is read, so every request is bounded.
pub(crate) async fn limit_layer(State(l): State<Arc<DoorLimits>>, req: Request, next: Next) -> Response {
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() else {
        // Not served through `serve_with`: never run without knowing who is asking.
        return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "error": "no peer address", "code": "internal" })))
            .into_response();
    };
    let now = Instant::now();
    let ip = client_ip(peer.ip(), req.headers(), &l.trusted);
    let kind = if req.method() == axum::http::Method::POST && req.uri().path() == l.strict_path {
        LimitKind::Accept
    } else {
        LimitKind::Other
    };
    // The per-address limit first: a request it refuses must not spend the shared budget, or one
    // flooding address would drain the bucket and every guest would get 503.
    if let Err(wait) = l.addr.check_at(ip, kind, now) {
        return too_many(wait);
    }
    if !l.bucket.try_take_at(now) {
        return busy();
    }
    let Ok(_permit) = l.in_flight.clone().try_acquire_owned() else { return busy() };
    next.run(req).await
}
