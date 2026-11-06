//! The guest door: an optional second listener that serves only the guest router
//! (SPEC FR-T14 [provisional], INTENT D22, D37).
//!
//! Off by default. With `EMBER_GUEST_LISTEN` unset or empty nothing is bound and the guest router
//! is not constructed (`main.rs` asks [`GuestDoor::from_env`] and builds nothing on `None`).
//!
//! | Variable | Meaning |
//! | -------- | ------- |
//! | `EMBER_GUEST_LISTEN` | `ip:port` of the door. A bad value or the owner's own address is a startup error, and so is a non-loopback address without `EMBER_GUEST_LISTEN_ALLOW_REMOTE=1`. |
//! | `EMBER_GUEST_LISTEN_ALLOW_REMOTE` | `1` allows a non-loopback bind (a warning is logged at startup). The door speaks plain HTTP: the owner puts TLS in front of it. |
//! | `EMBER_GUEST_TRUSTED_PROXIES` | comma list of IPs. `X-Forwarded-For` is read only when the TCP peer is in this list (right-most untrusted entry); otherwise the header is ignored. An invalid entry is a startup error. Behind a reverse proxy WITHOUT this variable every guest is keyed to the proxy's address and shares one budget. |
//! | `EMBER_GUEST_ALLOWED_HOSTS` | comma list of host names accepted in `Host`. |
//! | `EMBER_GUEST_PUBLIC_URL` | `http(s)://host[:port]` guests use; builds the invite link and its host is accepted in `Host`. Scheme and authority only. |
//!
//! The door is wrapped only by [`GuestDoor::wrap`]: a Host check (the listen IP, the allowed
//! list, the public URL's host; no "any IP literal" rule), an Origin check (absent, or equal to
//! `Host`, or an allowed non-loopback host; a loopback Origin different from `Host` is refused),
//! a refusal of `Upgrade` requests (no WebSocket for guests), and response headers. It is never
//! wrapped by `auth::protect`: the server token is not the guest credential class.
//!
//! `GET /join` serves the guest page (`guest_door/join.html` with `join.js` inlined). Its policy
//! allows the one inline script and the one style block by SHA-256, computed at startup from the
//! embedded text. The page adds no route: it talks to the four guest routes only.
//!
//! Limits (PR 5b, [`GuestDoor::wrap_limited`], numbers are a leader decision and adjustable):
//! per remote address (IPv4 exact, IPv6 reduced to its /64) accept attempts 5 a minute and 20 an
//! hour, failures counted, other requests 120 a minute (429 with `Retry-After`); a global token
//! bucket of 50 requests a second (burst 100) and at most 64 requests in flight (503); a header
//! read timeout of 10 s on the connection and at most 256 open connections ([`GuestDoor::serve_with`]). A door served without
//! `wrap_limited` has no limits and must not be exposed to a network.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

use crate::api::origin::host_of;
use crate::door::err;
use crate::door::harden::{forbid, not_found, origin_ok, request_host, response_headers, Guard};
use crate::door::limits::{busy, normalize_ip, too_many};

pub use crate::door::harden::with_timeout;
pub use crate::door::limits::{addr_key, client_ip, TokenBucket};
pub use crate::door::DoorError;

pub const LISTEN_VAR: &str = "EMBER_GUEST_LISTEN";
pub const ALLOW_REMOTE_VAR: &str = "EMBER_GUEST_LISTEN_ALLOW_REMOTE";
pub const ALLOWED_HOSTS_VAR: &str = "EMBER_GUEST_ALLOWED_HOSTS";
pub const PUBLIC_URL_VAR: &str = "EMBER_GUEST_PUBLIC_URL";
pub const TRUSTED_PROXIES_VAR: &str = "EMBER_GUEST_TRUSTED_PROXIES";

/// The accept route, counted against the strict per-address limit whatever the outcome.
pub const ACCEPT_PATH: &str = "/api/guest/accept";

/// Leader decision (2026-11-06), adjustable: accept attempts per address.
pub const ACCEPT_PER_MINUTE: usize = 5;
pub const ACCEPT_PER_HOUR: usize = 20;
/// Every other guest request, per address.
pub const OTHER_PER_MINUTE: usize = 120;
/// Token bucket of the whole door: requests a second and burst.
pub const GLOBAL_PER_SECOND: f64 = 50.0;
pub const GLOBAL_BURST: f64 = 100.0;
/// Requests in flight at once on the door.
pub const MAX_IN_FLIGHT: usize = 64;
/// Addresses the limiter remembers at most; when full the oldest-touched is evicted.
pub const MAX_TRACKED_ADDRS: usize = 10_000;
/// The limiter drops idle addresses on every this-many-th call.
pub const SWEEP_EVERY: u64 = 256;
/// A client that does not send a whole request header within this long is disconnected.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Open TCP connections the door holds at once; the next one is closed at accept.
pub const MAX_CONNECTIONS: usize = 256;

/// The one non-API route the door serves.
pub const JOIN_ROUTE: (&str, &str) = ("GET", "/join");

/// The most a request body may be on the door, for every route including `accept` and unknown
/// paths: a larger `Content-Length` is 413 before any handler, and body extractors stop at it.
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// A request that takes longer than this (handler included) is answered 408. The slow header read
/// on the connection itself is [`HEADER_READ_TIMEOUT`], set by [`GuestDoor::serve_with`].
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The page template and its script, embedded. The script is inlined into the page, so the page
/// is one request and the script hash in the policy is directly valid in `script-src`.
const JOIN_HTML: &str = include_str!("guest_door/join.html");
const JOIN_JS: &str = include_str!("guest_door/join.js");
const SCRIPT_MARK: &str = "__JOIN_JS__";

/// The `/join` page, its policy and the exact texts the policy hashes.
pub struct JoinPage {
    pub html: String,
    pub script: String,
    pub style: String,
    pub csp: String,
}

fn sha256_source(text: &str) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(Sha256::digest(text.as_bytes())))
}

/// Builds the page once: the hashes are computed from the embedded text itself, so the header
/// cannot drift from the content. `script-src` and `style-src` allow exactly those two texts and
/// nothing else (no `unsafe-inline`, no `unsafe-eval`); requests go to the door itself only.
fn build_join_page() -> JoinPage {
    build_join_page_from(JOIN_HTML, JOIN_JS)
}

/// Line endings as a browser sees them: CRLF and a lone CR become LF (the HTML parser does this
/// to inline script text, so the hash must be of the LF text whatever the checkout's endings).
pub fn normalize_eol(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// [`build_join_page`] over given texts (public so a test can feed CRLF input).
pub fn build_join_page_from(html_template: &str, js: &str) -> JoinPage {
    let js = normalize_eol(js);
    let html = normalize_eol(html_template).replacen(SCRIPT_MARK, &js, 1);
    let style = {
        let s = html.find("<style>").expect("join.html has a style block") + "<style>".len();
        let e = html[s..].find("</style>").expect("style block closes") + s;
        html[s..e].to_string()
    };
    let csp = format!(
        "default-src 'none'; script-src {}; style-src {}; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        sha256_source(&js),
        sha256_source(&style),
    );
    JoinPage { html, script: js, style, csp }
}

/// The `/join` page, built on first use (the door builds it at startup).
pub fn join_page() -> &'static JoinPage {
    static PAGE: std::sync::OnceLock<JoinPage> = std::sync::OnceLock::new();
    PAGE.get_or_init(build_join_page)
}

/// The door's validated configuration. Cheap to clone.
#[derive(Clone, Debug)]
pub struct GuestDoor {
    pub listen: SocketAddr,
    /// `scheme://authority`, no trailing slash.
    pub public_url: Option<String>,
    /// Lower-case host names accepted in `Host` (without port).
    hosts: Arc<HashSet<String>>,
    /// Peers whose `X-Forwarded-For` is believed (`EMBER_GUEST_TRUSTED_PROXIES`).
    trusted_proxies: Arc<Vec<IpAddr>>,
}

impl GuestDoor {
    /// Reads the process environment. `owner_listen` is `EMBER_LISTEN`'s address.
    pub fn from_env(owner_listen: SocketAddr) -> Result<Option<Self>, DoorError> {
        Self::from_lookup(|k| std::env::var(k).ok(), owner_listen)
    }

    /// As [`GuestDoor::from_env`] with the variables read through `get` (tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, owner_listen: SocketAddr) -> Result<Option<Self>, DoorError> {
        let raw = get(LISTEN_VAR).unwrap_or_default();
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let listen: SocketAddr = match raw.parse() {
            Ok(a) => a,
            Err(_) => return err(format!("{LISTEN_VAR}={raw:?} is not an ip:port address")),
        };
        // An unspecified address (0.0.0.0, ::) on either side takes the port on every interface.
        let same_port = listen.port() == owner_listen.port();
        if listen == owner_listen
            || (same_port && (listen.ip().is_unspecified() || owner_listen.ip().is_unspecified()))
        {
            return err(format!("{LISTEN_VAR} must differ from EMBER_LISTEN ({owner_listen})"));
        }
        // A non-loopback bind needs the owner's explicit word; main.rs logs a warning when it happens.
        if !listen.ip().is_loopback() && get(ALLOW_REMOTE_VAR).map(|v| v.trim() == "1") != Some(true) {
            return err(format!(
                "{LISTEN_VAR}={listen} is not a loopback address: set {ALLOW_REMOTE_VAR}=1 to expose the \
                 guest door to a network (it speaks plain HTTP: put TLS in front of it)"
            ));
        }
        let mut trusted: Vec<IpAddr> = Vec::new();
        for e in get(TRUSTED_PROXIES_VAR).unwrap_or_default().split(',') {
            let e = e.trim();
            if e.is_empty() {
                continue;
            }
            match e.parse::<IpAddr>() {
                Ok(ip) => trusted.push(normalize_ip(ip)),
                Err(_) => return err(format!("{TRUSTED_PROXIES_VAR}: {e:?} is not an IP address")),
            }
        }
        let public_url = match get(PUBLIC_URL_VAR).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            Some(u) => Some(parse_public_url(&u)?),
            None => None,
        };
        let mut hosts: HashSet<String> = HashSet::new();
        if !listen.ip().is_unspecified() {
            hosts.insert(listen.ip().to_string().to_ascii_lowercase());
        }
        if listen.ip().is_loopback() {
            hosts.insert("localhost".into());
        }
        for h in get(ALLOWED_HOSTS_VAR).unwrap_or_default().split(',') {
            let h = host_of(h.trim());
            if !h.is_empty() {
                hosts.insert(h);
            }
        }
        if let Some(u) = &public_url {
            let authority = u.split_once("://").map(|x| x.1).unwrap_or("");
            hosts.insert(host_of(authority));
        }
        Ok(Some(Self { listen, public_url, hosts: Arc::new(hosts), trusted_proxies: Arc::new(trusted) }))
    }

    /// Wraps a guest router (`channels::guest::guest_router*`) with the door's routes and guards,
    /// without rate limits (in-process tests drive it with `oneshot`, which has no peer address).
    /// A door that is served must use [`GuestDoor::wrap_limited`].
    pub fn wrap(&self, guest: Router) -> Router {
        self.build(guest, None)
    }

    /// As [`GuestDoor::wrap`], and every request first passes the limits of `limits`: the global
    /// bucket and the in-flight cap (503), then the per-address limits (429). A request with no
    /// `ConnectInfo<SocketAddr>` is refused (500), never let through. This is what `main.rs` serves.
    pub fn wrap_limited(&self, guest: Router, limits: Arc<DoorLimits>) -> Router {
        self.build(guest, Some(limits))
    }

    /// The limits of this door with the default numbers and this door's trusted proxies.
    pub fn limits(&self) -> Arc<DoorLimits> {
        Arc::new(DoorLimits::new(LimitConfig::default(), self.trusted_proxies.to_vec()))
    }

    fn build(&self, guest: Router, limits: Option<Arc<DoorLimits>>) -> Router {
        let _ = join_page(); // hashes computed once, at startup
        let guard = Guard { hosts: self.hosts.clone() };
        let app = guest
            .route(JOIN_ROUTE.1, get(join))
            .fallback(not_found)
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .layer(middleware::from_fn_with_state(guard, guard_layer));
        let mut app = with_timeout(app, REQUEST_TIMEOUT);
        if let Some(l) = limits {
            app = app.layer(middleware::from_fn_with_state(l, limit_layer));
        }
        app.layer(middleware::from_fn(response_headers))
    }

    /// Binds the door's socket.
    pub async fn bind(&self) -> std::io::Result<tokio::net::TcpListener> {
        tokio::net::TcpListener::bind(self.listen).await
    }

    /// Serves `app` on `listener` until `shutdown` resolves, with [`HEADER_READ_TIMEOUT`].
    pub async fn serve(
        listener: tokio::net::TcpListener,
        app: Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        Self::serve_with(listener, app, shutdown, HEADER_READ_TIMEOUT).await
    }

    /// As [`GuestDoor::serve`] with a chosen header-read timeout (tests use a short one). HTTP/1
    /// only, through hyper's server builder (axum's own `serve` has no such timeout). The peer
    /// address is put in every request as `ConnectInfo<SocketAddr>`. On `shutdown` the listener
    /// closes and open connections finish their request (the caller bounds the wait).
    pub async fn serve_with(
        listener: tokio::net::TcpListener,
        app: Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
        header_read_timeout: Duration,
    ) -> std::io::Result<()> {
        Self::serve_capped(listener, app, shutdown, header_read_timeout, MAX_CONNECTIONS).await
    }

    /// As [`GuestDoor::serve_with`] with a chosen cap on open connections (tests use a small
    /// one). A connection over the cap is closed at once, before any task is spawned for it.
    pub async fn serve_capped(
        listener: tokio::net::TcpListener,
        app: Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
        header_read_timeout: Duration,
        max_connections: usize,
    ) -> std::io::Result<()> {
        use hyper_util::rt::{TokioIo, TokioTimer};
        use hyper_util::server::graceful::GracefulShutdown;
        use tower::ServiceExt;

        let graceful = GracefulShutdown::new();
        let conns = Arc::new(tokio::sync::Semaphore::new(max_connections));
        let mut dropped: u64 = 0;
        let mut shutdown = Box::pin(shutdown);
        loop {
            let (stream, peer) = tokio::select! {
                r = listener.accept() => match r {
                    Ok(x) => x,
                    Err(e) => {
                        // Out of descriptors and the like: back off, never spin, never stop the door.
                        tracing::warn!("guest door accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                },
                _ = &mut shutdown => break,
            };
            let Ok(permit) = conns.clone().try_acquire_owned() else {
                dropped += 1;
                tracing::debug!("guest door at its connection cap, dropped {peer} ({dropped} dropped so far)");
                drop(stream);
                continue;
            };
            let app = app.clone();
            let svc = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(ConnectInfo(peer));
                app.clone().oneshot(req)
            });
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder.timer(TokioTimer::new());
            builder.header_read_timeout(header_read_timeout);
            let conn = graceful.watch(builder.serve_connection(TokioIo::new(stream), svc));
            tokio::spawn(async move {
                let _permit = permit; // held for the life of the connection
                if let Err(e) = conn.await {
                    tracing::debug!("guest door connection closed: {e}");
                }
            });
        }
        drop(listener);
        graceful.shutdown().await;
        Ok(())
    }

    /// The `guest_door` object of an invite response.
    pub fn state_json(door: Option<&GuestDoor>) -> Value {
        match door {
            Some(d) => json!({ "state": "on", "listen": d.listen.to_string(), "public_url": d.public_url }),
            None => json!({ "state": "off", "listen": null, "public_url": null }),
        }
    }

    /// Whether the door binds an address other than loopback.
    pub fn is_remote(&self) -> bool {
        !self.listen.ip().is_loopback()
    }
}

/// The invite link, whether the owner must check it, and the `guest_door` object.
/// Door off: relative `/join#<token>`. Door on with a public URL: `<url>/join#<token>`.
/// Door on without: absolute from the listen address, `link_warning` true ("check that this
/// address is reachable from outside").
pub fn invite_link(door: Option<&GuestDoor>, token: &str) -> (String, bool, Value) {
    let (state, warn) = door_status(door);
    let link = match door {
        None => format!("/join#{token}"),
        Some(d) => match &d.public_url {
            Some(u) => format!("{u}/join#{token}"),
            None => format!("http://{}/join#{token}", d.listen),
        },
    };
    (link, warn, state)
}

/// The `guest_door` object and `link_warning`, the one source of both the invite response and
/// `GET /api/guest-door` (FR-T14). `link_warning` is true only when the door is on and no public
/// URL is set.
pub fn door_status(door: Option<&GuestDoor>) -> (Value, bool) {
    (GuestDoor::state_json(door), door.is_some_and(|d| d.public_url.is_none()))
}

/// The body of `GET /api/guest-door`: `{state, listen, public_url, link_warning}`.
pub fn status_body(door: Option<&GuestDoor>) -> Value {
    let (mut v, warn) = door_status(door);
    v["link_warning"] = json!(warn);
    v
}

fn parse_public_url(u: &str) -> Result<String, DoorError> {
    let bad = || DoorError(format!("{PUBLIC_URL_VAR}={u:?} must be http(s)://host[:port] (scheme and authority only)"));
    let u = u.strip_suffix('/').unwrap_or(u);
    let (scheme, rest) = u.split_once("://").ok_or_else(bad)?;
    if scheme != "http" && scheme != "https" {
        return Err(bad());
    }
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ', '\\']) {
        return Err(bad());
    }
    if host_of(rest).is_empty() {
        return Err(bad());
    }
    Ok(format!("{scheme}://{rest}"))
}

/// Host, Origin and Upgrade checks, before any route or body.
async fn guard_layer(State(g): State<Guard>, req: Request, next: Next) -> Response {
    let Some(host) = request_host(&req) else { return forbid("host not allowed") };
    if host.is_empty() || host.contains(['@', '/', '\\', ' ']) || !g.hosts.contains(&host_of(&host)) {
        return forbid("host not allowed");
    }
    if !origin_ok(req.headers(), &host, &g.hosts) {
        return forbid("origin not allowed");
    }
    // HEAD would be answered by axum from every GET route; nothing needs it, so nothing leaks.
    if matches!(req.method().as_str(), "HEAD" | "TRACE" | "CONNECT") {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let too_big = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .is_some_and(|n| n > MAX_BODY_BYTES as u64);
    if too_big {
        return (StatusCode::PAYLOAD_TOO_LARGE, axum::Json(json!({ "error": "body too large", "code": "too_large" })))
            .into_response();
    }
    if req.headers().contains_key(header::UPGRADE) {
        return (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "no upgrade here", "code": "bad_request" })))
            .into_response();
    }
    next.run(req).await
}

async fn join() -> Response {
    let page = join_page();
    let mut r = Response::new(Body::from(page.html.clone()));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_str(&page.csp).expect("csp is ascii"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
    /// `POST /api/guest/accept`, counted whatever the answer.
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
}

impl DoorLimits {
    pub fn new(cfg: LimitConfig, trusted: Vec<IpAddr>) -> Self {
        Self {
            bucket: TokenBucket::new_at(cfg.global_per_second, cfg.global_burst, Instant::now()),
            in_flight: Arc::new(tokio::sync::Semaphore::new(cfg.max_in_flight)),
            addr: AddrLimiter::new(cfg),
            trusted: trusted.into_iter().map(normalize_ip).collect(),
        }
    }
}

/// The per-address limits, then the global bucket and the in-flight cap. Runs before the Host check and
/// before any body is read, so every request is bounded.
async fn limit_layer(State(l): State<Arc<DoorLimits>>, req: Request, next: Next) -> Response {
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() else {
        // Not served through `serve_with`: never run without knowing who is asking.
        return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "error": "no peer address", "code": "internal" })))
            .into_response();
    };
    let now = Instant::now();
    let ip = client_ip(peer.ip(), req.headers(), &l.trusted);
    let kind = if req.method() == axum::http::Method::POST && req.uri().path() == ACCEPT_PATH {
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
