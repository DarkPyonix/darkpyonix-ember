//! The enrol door: an optional listener of its own for the computer enrol flow (SPEC FR-C2e,
//! FR-C2f [provisional], `docs/design/COMPUTERS.md`).
//!
//! The door has exactly two routes (step C3a): `POST /enrol/redeem` and
//! `GET /enrol/status/{enrol_id}`. A request first passes the limit layer, the timeout, the
//! Host and Origin guard and the body limit, which can answer 429, 503, 408, 403, 400, 405 and
//! 413; what reaches the router is 404 except those two routes. Any other method on those two
//! paths is 404 too. The door holds the shared [`Enrol`] and nothing else of the core.
//!
//! Where each secret may appear: the code only in the redeem request body; the poll secret in
//! the redeem answer body and then only in the `Authorization: Bearer` header of a poll (never in
//! a query string or a path, so no access log or proxy log records it); the node token only in
//! the one poll answer that delivers it. None of them is formatted into a log line or an error.
//!
//! Off by default. With `EMBER_ENROL_LISTEN` unset or empty nothing is bound and no router is
//! built (`main.rs` asks [`EnrolDoor::from_env`] and builds nothing on `None`).
//!
//! | Variable | Meaning |
//! | -------- | ------- |
//! | `EMBER_ENROL_LISTEN` | `ip:port` of the door. A bad value, the owner's address (`EMBER_LISTEN`), the guest door's address (`EMBER_GUEST_LISTEN`, when that door is on) or a non-loopback address without `EMBER_ENROL_LISTEN_ALLOW_REMOTE=1` is a startup error. |
//! | `EMBER_ENROL_LISTEN_ALLOW_REMOTE` | `1` allows a non-loopback bind (a warning is logged at startup). The door speaks plain HTTP: the owner puts TLS in front of it. |
//! | `EMBER_ENROL_TRUSTED_PROXIES` | comma list of IPs. `X-Forwarded-For` is read only when the TCP peer is in this list. Its own variable: the guest door's has no effect here. |
//! | `EMBER_ENROL_ALLOWED_HOSTS` | comma list of host names accepted in `Host`. |
//! | `EMBER_ENROL_PUBLIC_URL` | `http(s)://host[:port]` the node is told to use; its host is accepted in `Host`. |
//!
//! The door is built only from the shared pieces in [`crate::door`], never from the guest door's
//! constants: its limits are [`DoorLimits::with_strict_path`] with the numbers below and an
//! explicit [`LimitConfig`]. The guest door's constructor and the config's default carry the
//! guest door's path and numbers and are not used here. It is never wrapped by `auth::protect`
//! and never merged into the owner router.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::connect_info::ConnectInfo;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::json;

use crate::computers::enrol::{Delivery, Enrol, NodeInfo, Refused};
use crate::door::config::{DoorEnv, Spec};
use crate::door::harden::{not_found, stack, Guard};
use crate::door::limits::{client_ip, too_many};
pub use crate::door::limits::{DoorLimits, LimitConfig};
pub use crate::door::DoorError;

pub const ENROL_LISTEN_VAR: &str = "EMBER_ENROL_LISTEN";
pub const ENROL_ALLOW_REMOTE_VAR: &str = "EMBER_ENROL_LISTEN_ALLOW_REMOTE";
pub const ENROL_TRUSTED_PROXIES_VAR: &str = "EMBER_ENROL_TRUSTED_PROXIES";
pub const ENROL_ALLOWED_HOSTS_VAR: &str = "EMBER_ENROL_ALLOWED_HOSTS";
pub const ENROL_PUBLIC_URL_VAR: &str = "EMBER_ENROL_PUBLIC_URL";

/// The redeem route, the strict class of the limits: `POST` here is counted against the strict
/// per-address budget whatever the answer (FR-C2f). The limiter compares the path without the
/// query, so `/enrol/redeem?x=1` is the same route and the same class; `/enrol/redeem/` is neither.
pub const ENROL_REDEEM_PATH: &str = "/enrol/redeem";
/// The poll route's prefix; the enrol id follows (`GET /enrol/status/{enrol_id}`).
pub const ENROL_STATUS_PREFIX: &str = "/enrol/status/";
/// FR-C2d: one poll per enrolment in this long (429 with `Retry-After` inside it).
pub const ENROL_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// FR-C2d: enrolments the poll limiter remembers at most. When full after pruning, a poll for a
/// new id is refused (fail closed).
pub const ENROL_POLL_MAX_ENTRIES: usize = 1024;
/// FR-C2d: a poll limiter entry older than this is dropped when a new one is inserted.
pub const ENROL_POLL_ENTRY_TTL: Duration = Duration::from_secs(60);
/// The longest enrol id the poll route looks at (ids are 36-character UUIDs); a longer one is the
/// plain 404 and enters no table.
pub const ENROL_MAX_ID_CHARS: usize = 64;
/// The longest poll secret read from the header (real ones are 4 + 96 characters); a longer one is
/// treated as no secret.
const MAX_SECRET_BYTES: usize = 256;
/// `Retry-After` of a redeem refused as rate limited, seconds (the core's failure window).
const REDEEM_RETRY_AFTER_SECS: u64 = 60;

/// FR-C2f (leader decision 2026-11-06, adjustable): redeems per address, strict class.
pub const ENROL_ACCEPT_PER_MINUTE: usize = 10;
/// FR-C2f: redeems per address in an hour, strict class.
pub const ENROL_ACCEPT_PER_HOUR: usize = 30;
/// FR-C2f: every other request per address.
pub const ENROL_OTHER_PER_MINUTE: usize = 60;
/// FR-C2f: token bucket of the whole door, requests a second.
pub const ENROL_GLOBAL_PER_SECOND: f64 = 5.0;
/// FR-C2f: token bucket burst.
pub const ENROL_GLOBAL_BURST: f64 = 10.0;
/// FR-C2f: requests in flight at once on the door.
pub const ENROL_MAX_IN_FLIGHT: usize = 8;
/// FR-C2f: addresses the per-address limiter remembers at most.
pub const ENROL_MAX_TRACKED_ADDRS: usize = 10_000;
/// FR-C2f: open TCP connections the door holds at once; the next one is closed at accept.
pub const ENROL_MAX_CONNECTIONS: usize = 32;
/// FR-C2e: a client that does not send a whole request header within this long is disconnected.
pub const ENROL_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// FR-C2e: the most a request body may be (413 above it).
pub const ENROL_MAX_BODY_BYTES: usize = 4096;
/// FR-C2e: a request that takes longer than this (handler included) is answered 408.
pub const ENROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The door's validated configuration. Cheap to clone.
#[derive(Clone, Debug)]
pub struct EnrolDoor {
    pub listen: SocketAddr,
    /// `scheme://authority`, no trailing slash.
    pub public_url: Option<String>,
    /// Lower-case host names accepted in `Host` (without port).
    hosts: Arc<HashSet<String>>,
    /// Peers whose `X-Forwarded-For` is believed (`EMBER_ENROL_TRUSTED_PROXIES`).
    trusted_proxies: Arc<Vec<IpAddr>>,
}

impl EnrolDoor {
    /// Reads the process environment. `owner_listen` is `EMBER_LISTEN`'s address, `guest_listen`
    /// the guest door's when that door is on.
    pub fn from_env(owner_listen: SocketAddr, guest_listen: Option<SocketAddr>) -> Result<Option<Self>, DoorError> {
        Self::from_lookup(|k| std::env::var(k).ok(), owner_listen, guest_listen)
    }

    /// As [`EnrolDoor::from_env`] with the variables read through `get` (tests). The listen must
    /// differ from the owner's and, when the guest door is on, from the guest door's; the owner's
    /// is checked first.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        owner_listen: SocketAddr,
        guest_listen: Option<SocketAddr>,
    ) -> Result<Option<Self>, DoorError> {
        let spec = Spec {
            listen_var: ENROL_LISTEN_VAR,
            allow_remote_var: ENROL_ALLOW_REMOTE_VAR,
            trusted_var: ENROL_TRUSTED_PROXIES_VAR,
            hosts_var: ENROL_ALLOWED_HOSTS_VAR,
            public_url_var: ENROL_PUBLIC_URL_VAR,
            noun: "enrol door",
        };
        let mut others: Vec<(&str, SocketAddr)> = vec![("EMBER_LISTEN", owner_listen)];
        if let Some(g) = guest_listen {
            others.push(("EMBER_GUEST_LISTEN", g));
        }
        let Some(env) = DoorEnv::parse(get, &others, &spec)? else { return Ok(None) };
        Ok(Some(Self {
            listen: env.listen,
            public_url: env.public_url,
            hosts: Arc::new(env.hosts),
            trusted_proxies: Arc::new(env.trusted),
        }))
    }

    /// The door's two routes over the shared `enrol`: `POST /enrol/redeem` and
    /// `GET /enrol/status/{enrol_id}`. Every other path and every other method on these two is
    /// 404. No proxy is trusted here: the client is the TCP peer. A served door uses
    /// [`EnrolDoor::routes_for`] (what [`EnrolDoor::app`] does), which reads
    /// `X-Forwarded-For` from the door's trusted proxies only.
    pub fn routes(enrol: Arc<Enrol>) -> Router {
        Self::routes_with(enrol, Arc::new(Vec::new()))
    }

    /// As [`EnrolDoor::routes`] with this door's trusted proxies, so the address recorded for the
    /// owner is the same `client_ip` the limit layer counts.
    pub fn routes_for(&self, enrol: Arc<Enrol>) -> Router {
        Self::routes_with(enrol, self.trusted_proxies.clone())
    }

    fn routes_with(enrol: Arc<Enrol>, trusted: Arc<Vec<IpAddr>>) -> Router {
        let state = Arc::new(Routes {
            enrol,
            trusted,
            polls: PollLimiter::new(),
            warned: WarnOnce::new(),
        });
        Router::new()
            .route(ENROL_REDEEM_PATH, post(redeem))
            .route("/enrol/status/{enrol_id}", get(poll))
            .method_not_allowed_fallback(not_found)
            .with_state(state)
    }

    /// Wraps `routes` with the door's guards, without rate limits (in-process tests drive it with
    /// `oneshot`, which has no peer address). A door that is served must use
    /// [`EnrolDoor::wrap_limited`].
    pub fn wrap(&self, routes: Router) -> Router {
        self.build(routes, None)
    }

    /// As [`EnrolDoor::wrap`], and every request first passes the limits of `limits`: per
    /// address (429), then the global bucket and the in-flight cap (503). A request with no
    /// `ConnectInfo<SocketAddr>` is refused (500), never let through.
    pub fn wrap_limited(&self, routes: Router, limits: Arc<DoorLimits>) -> Router {
        self.build(routes, Some(limits))
    }

    /// The finished router `main.rs` serves: [`EnrolDoor::routes_for`] wrapped by
    /// [`EnrolDoor::wrap_limited`] with [`EnrolDoor::limits`]. `enrol` is the one instance the
    /// owner's routes use too, so a redeem here is the owner's to confirm and the failure
    /// windows are shared.
    pub fn app(&self, enrol: Arc<Enrol>) -> Router {
        self.wrap_limited(self.routes_for(enrol), self.limits())
    }

    /// The limits of this door: the enrol numbers, this door's trusted proxies, and the redeem
    /// path as the strict class. The guest door's constructor would leave the redeem path in the
    /// loose class, so it is not used here.
    pub fn limits(&self) -> Arc<DoorLimits> {
        let cfg = LimitConfig {
            accept_per_minute: ENROL_ACCEPT_PER_MINUTE,
            accept_per_hour: ENROL_ACCEPT_PER_HOUR,
            other_per_minute: ENROL_OTHER_PER_MINUTE,
            global_per_second: ENROL_GLOBAL_PER_SECOND,
            global_burst: ENROL_GLOBAL_BURST,
            max_in_flight: ENROL_MAX_IN_FLIGHT,
            max_addrs: ENROL_MAX_TRACKED_ADDRS,
        };
        Arc::new(DoorLimits::with_strict_path(cfg, self.trusted_proxies.to_vec(), ENROL_REDEEM_PATH))
    }

    fn build(&self, routes: Router, limits: Option<Arc<DoorLimits>>) -> Router {
        let guard = Guard { hosts: self.hosts.clone(), max_body: ENROL_MAX_BODY_BYTES };
        let app = routes.fallback(not_found);
        stack(app, guard, ENROL_REQUEST_TIMEOUT, limits)
    }

    /// Binds the door's socket.
    pub async fn bind(&self) -> std::io::Result<tokio::net::TcpListener> {
        tokio::net::TcpListener::bind(self.listen).await
    }

    /// Serves `app` on `listener` until `shutdown` resolves, with [`ENROL_HEADER_READ_TIMEOUT`].
    pub async fn serve(
        listener: tokio::net::TcpListener,
        app: Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        Self::serve_with(listener, app, shutdown, ENROL_HEADER_READ_TIMEOUT).await
    }

    /// As [`EnrolDoor::serve`] with a chosen header-read timeout (tests use a short one).
    pub async fn serve_with(
        listener: tokio::net::TcpListener,
        app: Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
        header_read_timeout: Duration,
    ) -> std::io::Result<()> {
        Self::serve_capped(listener, app, shutdown, header_read_timeout, ENROL_MAX_CONNECTIONS).await
    }

    /// As [`EnrolDoor::serve_with`] with a chosen cap on open connections (tests use a small
    /// one). A connection over the cap is closed at once, before any task is spawned for it.
    pub async fn serve_capped(
        listener: tokio::net::TcpListener,
        app: Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
        header_read_timeout: Duration,
        max_connections: usize,
    ) -> std::io::Result<()> {
        crate::door::serve::serve_capped("enrol door", listener, app, shutdown, header_read_timeout, max_connections).await
    }

    /// What the owner router prints in the enrol command (FR-C1): the door's public URL when set
    /// (validated by `parse_public_url` when the environment was read), else `http://{listen}`
    /// only when the listen address names one interface (not 0.0.0.0 or ::), else `Unknown`.
    /// `None` (no door) is `Off`.
    pub fn door_url(door: Option<&EnrolDoor>) -> crate::computers::enrol::DoorUrl {
        use crate::computers::enrol::DoorUrl;
        let Some(d) = door else { return DoorUrl::Off };
        match (&d.public_url, d.listen.ip().is_unspecified()) {
            (Some(u), _) => DoorUrl::Url(u.clone()),
            (None, false) => DoorUrl::Url(format!("http://{}", d.listen)),
            (None, true) => DoorUrl::Unknown,
        }
    }

    /// Whether the door binds an address other than loopback.
    pub fn is_remote(&self) -> bool {
        !self.listen.ip().is_loopback()
    }
}

// ---- the routes -------------------------------------------------------------------------------

/// What the handlers share. No `Debug`: it holds the core.
struct Routes {
    enrol: Arc<Enrol>,
    trusted: Arc<Vec<IpAddr>>,
    polls: PollLimiter,
    warned: WarnOnce,
}

/// One poll per [`ENROL_POLL_INTERVAL`] per enrol id, in a table of at most
/// [`ENROL_POLL_MAX_ENTRIES`] ids. The core has no poll limit, so the door keeps it. The handler
/// asks the limiter only after `Enrol::verify_poll` accepted the secret, so the key space is only
/// the enrolments whose poll secret the caller holds: an attacker without a valid secret cannot
/// occupy a slot, and the limiter never learns an invented id. The residual risk is that probes
/// with a wrong secret are no longer limited per id; they are bounded by the per-address loose
/// limit and the global bucket of the door. When the table is full after pruning, a new id is
/// refused (fail closed): the trade is that a flood of enrolments the caller holds secrets for
/// can make polls for new ids wait, never that the table grows.
pub struct PollLimiter {
    last: Mutex<HashMap<String, Instant>>,
}

impl Default for PollLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl PollLimiter {
    pub fn new() -> Self {
        Self { last: Mutex::new(HashMap::new()) }
    }

    /// Spends one poll of `enrol_id` at `now`. `Err(wait)` is how long until a poll would pass;
    /// a refused poll is not recorded.
    pub fn check_at(&self, enrol_id: &str, now: Instant) -> Result<(), Duration> {
        let mut m = self.last.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = m.get_mut(enrol_id) {
            let age = now.saturating_duration_since(*t);
            if age < ENROL_POLL_INTERVAL {
                return Err(ENROL_POLL_INTERVAL - age);
            }
            *t = now;
            return Ok(());
        }
        m.retain(|_, t| now.saturating_duration_since(*t) < ENROL_POLL_ENTRY_TTL);
        if m.len() >= ENROL_POLL_MAX_ENTRIES {
            // Full of entries that are all younger than the TTL: wait for the oldest to age out.
            let wait = m
                .values()
                .map(|t| ENROL_POLL_ENTRY_TTL.saturating_sub(now.saturating_duration_since(*t)))
                .min()
                .unwrap_or(ENROL_POLL_INTERVAL);
            return Err(wait.max(Duration::from_secs(1)));
        }
        m.insert(enrol_id.to_string(), now);
        Ok(())
    }

    /// Ids currently remembered.
    pub fn len(&self) -> usize {
        self.last.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An id taken from a request, made safe for a log line (FR-C2g): at most 64 characters, and
/// every character that is not an ASCII letter, digit, `-` or `_` becomes `_`, so no control
/// character, newline or escape sequence an attacker put in the path can reach a log. Used at
/// every place the door logs an id, including the ids the core generated, for one uniform rule.
fn log_id(id: &str) -> String {
    id.chars().take(64).map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// FR-C2g: a refused redeem is logged at warn at most once per address per minute. Bounded;
/// when full of recent addresses nothing more is logged (the refusals are still counted by the
/// limiters).
struct WarnOnce {
    last: Mutex<HashMap<IpAddr, Instant>>,
}

impl WarnOnce {
    const MAX: usize = 1024;
    const EVERY: Duration = Duration::from_secs(60);

    fn new() -> Self {
        Self { last: Mutex::new(HashMap::new()) }
    }

    fn due(&self, ip: IpAddr, now: Instant) -> bool {
        let mut m = self.last.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = m.get_mut(&ip) {
            if now.saturating_duration_since(*t) < Self::EVERY {
                return false;
            }
            *t = now;
            return true;
        }
        if m.len() >= Self::MAX {
            m.retain(|_, t| now.saturating_duration_since(*t) < Self::EVERY);
            if m.len() >= Self::MAX {
                return false;
            }
        }
        m.insert(ip, now);
        true
    }
}

/// `POST /enrol/redeem` body. No `Debug`: `code` is a secret.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RedeemBody {
    code: String,
    node_public_info: NodeInfo,
}

fn reply(status: StatusCode, error: &str, code: &str) -> Response {
    (status, axum::Json(json!({ "error": error, "code": code }))).into_response()
}

fn unsupported() -> Response {
    reply(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported media type", "unsupported")
}

fn bad_request() -> Response {
    reply(StatusCode::BAD_REQUEST, "bad request", "bad_request")
}

/// The one answer for a wrong secret, a missing or bad header, an unknown id and an internal error
/// on the poll route.
fn poll_not_found() -> Response {
    reply(StatusCode::NOT_FOUND, "not found", "not_found")
}

/// `application/json` exactly (any case), parameters such as `charset` allowed; one header only.
fn is_json(headers: &HeaderMap) -> bool {
    let mut all = headers.get_all(header::CONTENT_TYPE).iter();
    let (Some(v), None) = (all.next(), all.next()) else { return false };
    v.to_str().is_ok_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
}

/// True when `e`, or an error in its `source()` chain, is `http_body_util::LengthLimitError`, the
/// size-limit error of `axum::body::to_bytes` (axum's wrapper keeps it as its `source()`; the
/// documented way to detect it). It is matched by TYPE, not by its text: no other error can be
/// taken for the limit.
fn is_length_limit(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if err.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        cur = err.source();
    }
    false
}

/// Calls of the two handlers, counted in unit tests only (compiled out of production), so a test
/// can prove a wrong method never reaches a handler.
#[cfg(test)]
pub(crate) static REDEEM_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static POLL_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

async fn redeem(State(st): State<Arc<Routes>>, req: Request) -> Response {
    #[cfg(test)]
    REDEEM_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // 1. The content type, before the body is read or parsed and before the code is looked at.
    if !is_json(req.headers()) {
        return unsupported();
    }
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() else {
        // Not served through the accept loop: never run without knowing who is asking.
        return reply(StatusCode::INTERNAL_SERVER_ERROR, "no peer address", "internal");
    };
    // The real client address, not the /64 key: the core shows it to the owner as the observed address.
    let ip = client_ip(peer.ip(), req.headers(), &st.trusted);
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, ENROL_MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) if is_length_limit(&e) => return reply(StatusCode::PAYLOAD_TOO_LARGE, "body too large", "too_large"),
        // A client that aborts or breaks the stream is a bad request, not an oversized one.
        Err(_) => return bad_request(),
    };
    // 2. A fixed text for every parse failure: serde's message could echo the input.
    let Ok(b) = serde_json::from_slice::<RedeemBody>(&bytes) else { return bad_request() };
    drop(bytes);
    let source = ip.to_string();
    match st.enrol.redeem(&b.code, &b.node_public_info, crate::store::now_ms(), &source) {
        Ok(r) => {
            tracing::info!(enrol = %log_id(&r.enrol_id), addr = %ip, "enrol code redeemed");
            let mut res = (
                StatusCode::OK,
                axum::Json(json!({
                    "enrol_id": r.enrol_id,
                    "poll_secret": r.poll_secret.expose(),
                    "expires_ms": r.expires_ms,
                })),
            )
                .into_response();
            res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
        Err(Refused::Generic) => {
            if st.warned.due(ip, Instant::now()) {
                tracing::warn!(addr = %ip, "enrol redeem refused");
            }
            reply(StatusCode::FORBIDDEN, Refused::Generic.message(), "forbidden")
        }
        Err(Refused::RateLimited) => {
            if st.warned.due(ip, Instant::now()) {
                tracing::warn!(addr = %ip, "enrol redeem rate limited");
            }
            let mut res = reply(StatusCode::TOO_MANY_REQUESTS, Refused::RateLimited.message(), "rate_limited");
            res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(REDEEM_RETRY_AFTER_SECS));
            res
        }
        Err(Refused::BadNodeInfo) => reply(StatusCode::BAD_REQUEST, Refused::BadNodeInfo.message(), "bad_request"),
    }
}

/// The poll secret from `Authorization: Bearer <secret>`, and nowhere else. `None` for no header,
/// more than one header, another scheme, an empty or over-long secret.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let mut all = headers.get_all(header::AUTHORIZATION).iter();
    let (Some(v), None) = (all.next(), all.next()) else { return None };
    let v = v.to_str().ok()?;
    let (scheme, secret) = v.split_once(' ')?;
    let secret = secret.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !secret.is_empty() && secret.len() <= MAX_SECRET_BYTES).then_some(secret)
}

async fn poll(State(st): State<Arc<Routes>>, id: Result<Path<String>, PathRejection>, headers: HeaderMap) -> Response {
    #[cfg(test)]
    POLL_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // The query string is never read: this handler has no extractor for it.
    let Ok(Path(id)) = id else { return poll_not_found() };
    if id.chars().count() > ENROL_MAX_ID_CHARS {
        return poll_not_found();
    }
    // Verify first, limit second, deliver third. A missing or bad header runs the same work with
    // an empty secret, which matches no stored hash. `verify_poll` has no side effect, so a
    // refused or rate-limited poll never consumes the credential.
    let secret = bearer(&headers).unwrap_or("");
    match st.enrol.verify_poll(&id, secret, crate::store::now_ms()) {
        Ok(true) => {}
        Ok(false) => return poll_not_found(),
        Err(_) => {
            tracing::warn!(enrol = %log_id(&id), "enrol poll failed");
            return poll_not_found();
        }
    }
    // Only a poll whose secret verified reaches the limiter and can take a slot.
    if let Err(wait) = st.polls.check_at(&id, Instant::now()) {
        return too_many(wait);
    }
    let delivery = match st.enrol.take_delivery(&id, secret, crate::store::now_ms()) {
        Ok(d) => d,
        Err(_) => {
            tracing::warn!(enrol = %log_id(&id), "enrol poll failed");
            return poll_not_found();
        }
    };
    let status = |s: &str| (StatusCode::OK, axum::Json(json!({ "status": s }))).into_response();
    match delivery {
        Delivery::Pending => status("waiting"),
        Delivery::Delivered => status("delivered"),
        Delivery::Denied => status("denied"),
        Delivery::Expired => status("expired"),
        Delivery::Unknown => poll_not_found(),
        Delivery::Joined { token, name, computer_id, server_peer_id } => {
            tracing::info!(enrol = %log_id(&id), "enrol credential delivered");
            let mut credential = json!({ "node_token": token.expose(), "name": name, "computer_id": computer_id });
            // The server's public transport id, only when the transport is on: the key is absent
            // otherwise, never null and never empty (FR-C2). No other answer carries it.
            if let (Some(sid), Some(map)) = (server_peer_id, credential.as_object_mut()) {
                map.insert("server_peer_id".into(), json!(sid));
            }
            let mut res =
                (StatusCode::OK, axum::Json(json!({ "status": "joined", "credential": credential }))).into_response();
            res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computers::enrol::DoorUrl;

    fn door_from(vars: &[(&str, &str)]) -> Option<EnrolDoor> {
        let owner: SocketAddr = "127.0.0.1:8740".parse().unwrap();
        let get = |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        EnrolDoor::from_lookup(get, owner, None).unwrap()
    }

    #[test]
    fn fr_c2_door_url_is_off_when_there_is_no_door() {
        assert_eq!(EnrolDoor::door_url(None), DoorUrl::Off);
        assert_eq!(EnrolDoor::door_url(door_from(&[]).as_ref()), DoorUrl::Off);
    }

    #[test]
    fn fr_c2_door_url_prefers_the_public_url() {
        let d = door_from(&[
            ("EMBER_ENROL_LISTEN", "0.0.0.0:8741"),
            ("EMBER_ENROL_LISTEN_ALLOW_REMOTE", "1"),
            ("EMBER_ENROL_PUBLIC_URL", "https://enrol.example.com/"),
        ]);
        assert_eq!(EnrolDoor::door_url(d.as_ref()), DoorUrl::Url("https://enrol.example.com".into()));
    }

    #[test]
    fn fr_c2_door_url_falls_back_to_a_specific_listen_address() {
        let d = door_from(&[("EMBER_ENROL_LISTEN", "127.0.0.1:8741")]);
        assert_eq!(EnrolDoor::door_url(d.as_ref()), DoorUrl::Url("http://127.0.0.1:8741".into()));
        let d = door_from(&[("EMBER_ENROL_LISTEN", "[::1]:8741")]);
        assert_eq!(EnrolDoor::door_url(d.as_ref()), DoorUrl::Url("http://[::1]:8741".into()));
    }

    #[test]
    fn fr_c2_door_url_is_unknown_for_an_unspecified_listen_without_a_public_url() {
        for l in ["0.0.0.0:8741", "[::]:8741"] {
            let d = door_from(&[("EMBER_ENROL_LISTEN", l), ("EMBER_ENROL_LISTEN_ALLOW_REMOTE", "1")]);
            assert_eq!(EnrolDoor::door_url(d.as_ref()), DoorUrl::Unknown, "{l}");
        }
    }

    #[tokio::test]
    async fn is_length_limit_tells_the_size_limit_from_a_broken_stream() {
        use axum::body::{Body, Bytes};
        let over = axum::body::to_bytes(Body::from(vec![0u8; 16]), 8).await.unwrap_err();
        assert!(is_length_limit(&over), "the real over-limit error: {over}");
        let broken = Body::from_stream(futures::stream::iter([Err::<Bytes, std::io::Error>(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "client went away",
        ))]));
        let other = axum::body::to_bytes(broken, 8).await.unwrap_err();
        assert!(!is_length_limit(&other), "a stream error is not the limit: {other}");
        let within = axum::body::to_bytes(Body::from(vec![0u8; 8]), 8).await;
        assert!(within.is_ok(), "exactly the limit passes");
    }

    #[test]
    fn log_id_keeps_only_a_short_safe_charset() {
        let safe = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        assert_eq!(log_id("5b0f2c2e-9a53_X"), "5b0f2c2e-9a53_X");
        for (what, raw) in [
            ("newline", "a\nINFO forged line".to_string()),
            ("esc", "a\x1b[31mred\x1b[0m".to_string()),
            ("nul", "a\0b".to_string()),
            ("long", "x".repeat(200)),
            ("unicode", "\u{ac00}\u{1f600}e\u{301}".to_string()),
            ("quote", "a\"b'c\\d".to_string()),
        ] {
            let out = log_id(&raw);
            assert!(safe(&out), "{what}: {out:?}");
            assert!(out.chars().count() <= 64, "{what}: {}", out.chars().count());
        }
        assert_eq!(log_id("a\nb"), "a_b");
        assert_eq!(log_id("a\x1bb"), "a_b");
        assert_eq!(log_id("a\0b"), "a_b");
        assert_eq!(log_id("a\"b"), "a_b");
        assert_eq!(log_id(&"x".repeat(200)), "x".repeat(64));
        assert_eq!(log_id("\u{ac00}\u{1f600}"), "__", "one underscore per character");
        assert_eq!(log_id(""), "");
    }

    #[test]
    fn warn_once_is_once_per_address_per_minute_and_bounded() {
        let w = WarnOnce::new();
        let t0 = Instant::now();
        let a: IpAddr = "203.0.113.1".parse().unwrap();
        let b: IpAddr = "203.0.113.2".parse().unwrap();
        assert!(w.due(a, t0));
        assert!(!w.due(a, t0 + Duration::from_secs(59)));
        assert!(w.due(b, t0 + Duration::from_secs(1)));
        assert!(w.due(a, t0 + Duration::from_secs(60)));
        let w = WarnOnce::new();
        for i in 0..WarnOnce::MAX {
            assert!(w.due(IpAddr::from([10, 0, (i >> 8) as u8, i as u8]), t0));
        }
        assert!(!w.due(a, t0), "full of recent addresses: nothing more is logged");
        assert!(w.due(a, t0 + Duration::from_secs(60)), "they aged out");
    }

    /// A HEAD, or any wrong method, never runs the redeem or poll handler (`take_delivery`
    /// delivers and wipes the credential in one call). The handlers count their calls in test
    /// builds; a legitimate request of each route must count exactly once, so the zero of the
    /// others is meaningful. Driven through the full stack, real limits and open limits, one fresh
    /// app per request so no limiter answers in place of the router.
    #[tokio::test]
    async fn no_wrong_method_runs_a_handler() {
        use std::sync::atomic::Ordering::SeqCst;
        use tower::ServiceExt;

        let store = Arc::new(crate::store::Store::open_in_memory().unwrap());
        let enrol = Enrol::new(store);
        let owner: SocketAddr = "127.0.0.1:8740".parse().unwrap();
        let d = EnrolDoor::from_lookup(
            |k| (k == ENROL_LISTEN_VAR).then(|| "127.0.0.1:9300".to_string()),
            owner,
            None,
        )
        .unwrap()
        .expect("door on");
        let big = LimitConfig {
            accept_per_minute: 100_000,
            accept_per_hour: 100_000,
            other_per_minute: 100_000,
            global_per_second: 1_000_000.0,
            global_burst: 1_000_000.0,
            max_in_flight: ENROL_MAX_IN_FLIGHT,
            max_addrs: ENROL_MAX_TRACKED_ADDRS,
        };
        let real = |d: &EnrolDoor, e: &Arc<Enrol>| d.app(e.clone());
        let open = |d: &EnrolDoor, e: &Arc<Enrol>| {
            d.wrap_limited(
                d.routes_for(e.clone()),
                Arc::new(DoorLimits::with_strict_path(big.clone(), Vec::new(), ENROL_REDEEM_PATH)),
            )
        };
        let send = |app: Router, method: &str, uri: &str| {
            let mut req = Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "127.0.0.1:9300")
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap();
            req.extensions_mut().insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 50], 40000))));
            async move { app.oneshot(req).await.unwrap().status() }
        };
        let calls = || (REDEEM_CALLS.load(SeqCst), POLL_CALLS.load(SeqCst));

        let builders: [(&str, &dyn Fn() -> Router); 2] =
            [("app", &|| real(&d, &enrol)), ("open_app", &|| open(&d, &enrol))];
        for (name, build) in builders {
            let before = calls();
            for m in ["HEAD", "GET", "PUT", "DELETE", "PATCH", "OPTIONS", "TRACE"] {
                let _ = send(build(), m, "/enrol/redeem").await;
            }
            for m in ["HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "TRACE"] {
                let _ = send(build(), m, "/enrol/status/x").await;
            }
            assert_eq!(calls(), before, "{name}: a wrong method ran a handler");
            // The counters are alive: the legitimate request of each route counts exactly once.
            let _ = send(build(), "POST", "/enrol/redeem").await;
            assert_eq!(calls(), (before.0 + 1, before.1), "{name}: POST /enrol/redeem counts once");
            let _ = send(build(), "GET", "/enrol/status/x").await;
            assert_eq!(calls(), (before.0 + 1, before.1 + 1), "{name}: GET /enrol/status/x counts once");
        }
    }
}
