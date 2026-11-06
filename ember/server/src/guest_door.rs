//! The guest door: an optional second listener that serves only the guest router
//! (SPEC FR-T14 [provisional], INTENT D22, D37).
//!
//! Off by default. With `EMBER_GUEST_LISTEN` unset or empty nothing is bound and the guest router
//! is not constructed (`main.rs` asks [`GuestDoor::from_env`] and builds nothing on `None`).
//!
//! | Variable | Meaning |
//! | -------- | ------- |
//! | `EMBER_GUEST_LISTEN` | `ip:port` of the door, loopback only for now. A bad value, the owner's own address, or any non-loopback address (with or without `EMBER_GUEST_LISTEN_ALLOW_REMOTE`) is a startup error. |
//! | `EMBER_GUEST_LISTEN_ALLOW_REMOTE` | Inert until PR 5b (per-address limiters, header-read timeout, trusted proxies), which makes it effective again: `1` will then allow a non-loopback bind (plain HTTP; put TLS in front). |
//! | `EMBER_GUEST_ALLOWED_HOSTS` | comma list of host names accepted in `Host`. |
//! | `EMBER_GUEST_PUBLIC_URL` | `http(s)://host[:port]` guests use; builds the invite link and its host is accepted in `Host`. Scheme and authority only. |
//!
//! The door is wrapped only by [`GuestDoor::wrap`]: a Host check (the listen IP, the allowed
//! list, the public URL's host; no "any IP literal" rule), an Origin check (absent, or equal to
//! `Host`, or an allowed non-loopback host; a loopback Origin different from `Host` is refused),
//! a refusal of `Upgrade` requests (no WebSocket for guests), and response headers. It is never
//! wrapped by `auth::protect`: the server token is not the guest credential class.
//!
//! `GET /join` serves an embedded placeholder page. The rate limits per remote address and the
//! global cap are not here yet (PR 5b); the door must not be exposed to a network without them.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

use crate::api::origin::host_of;

pub const LISTEN_VAR: &str = "EMBER_GUEST_LISTEN";
pub const ALLOW_REMOTE_VAR: &str = "EMBER_GUEST_LISTEN_ALLOW_REMOTE";
pub const ALLOWED_HOSTS_VAR: &str = "EMBER_GUEST_ALLOWED_HOSTS";
pub const PUBLIC_URL_VAR: &str = "EMBER_GUEST_PUBLIC_URL";

/// The one non-API route the door serves.
pub const JOIN_ROUTE: (&str, &str) = ("GET", "/join");

/// The most a request body may be on the door, for every route including `accept` and unknown
/// paths: a larger `Content-Length` is 413 before any handler, and body extractors stop at it.
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// A request that takes longer than this (handler included) is answered 408. A slow header read
/// on the connection itself is not covered: that needs a hyper builder or tower-http, which this
/// crate does not depend on.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// `Content-Security-Policy` of `/join`: no script, no style, nothing framed, requests to the
/// door itself only. The real guest page (a later client task) needs its own policy.
pub const JOIN_CSP: &str = "default-src 'none'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

const JOIN_PAGE: &str = "<!doctype html>\n<html lang=\"ko\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>Ember</title></head><body>\
<p>Ember 채널 초대 링크입니다. 게스트 화면은 아직 준비 중입니다.</p>\
</body></html>\n";

/// A startup error of the door's configuration.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DoorError(pub String);

fn err<T>(m: impl Into<String>) -> Result<T, DoorError> {
    Err(DoorError(m.into()))
}

/// The door's validated configuration. Cheap to clone.
#[derive(Clone, Debug)]
pub struct GuestDoor {
    pub listen: SocketAddr,
    /// `scheme://authority`, no trailing slash.
    pub public_url: Option<String>,
    /// Lower-case host names accepted in `Host` (without port).
    hosts: Arc<HashSet<String>>,
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
        // Inert until PR 5b: the variable is read so the message can name it, but it unlocks nothing.
        if !listen.ip().is_loopback() {
            let _ = get(ALLOW_REMOTE_VAR);
            return err(format!(
                "{LISTEN_VAR}={listen}: the guest door is loopback only until the per-address rate \
                 limiters and the header-read timeout land (invite PR 5b); {ALLOW_REMOTE_VAR} has no \
                 effect yet"
            ));
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
        Ok(Some(Self { listen, public_url, hosts: Arc::new(hosts) }))
    }

    /// Wraps a guest router (`channels::guest::guest_router*`) with the door's routes and guards.
    /// This is the only way the door builds an app.
    pub fn wrap(&self, guest: Router) -> Router {
        let guard = Guard { hosts: self.hosts.clone() };
        let app = guest
            .route(JOIN_ROUTE.1, get(join))
            .fallback(not_found)
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .layer(middleware::from_fn_with_state(guard, guard_layer));
        with_timeout(app, REQUEST_TIMEOUT).layer(middleware::from_fn(response_headers))
    }

    /// Binds the door's socket.
    pub async fn bind(&self) -> std::io::Result<tokio::net::TcpListener> {
        tokio::net::TcpListener::bind(self.listen).await
    }

    /// Serves `app` (built by [`GuestDoor::wrap`]) on `listener` until `shutdown` resolves.
    /// The peer address is available to layers as `ConnectInfo<SocketAddr>` (PR 5b).
    pub async fn serve(
        listener: tokio::net::TcpListener,
        app: Router,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(shutdown)
            .await
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

/// Answers 408 when the request (handler included) exceeds `limit`.
pub fn with_timeout(app: Router, limit: Duration) -> Router {
    app.layer(middleware::from_fn_with_state(limit, timeout_layer))
}

async fn timeout_layer(State(limit): State<Duration>, req: Request, next: Next) -> Response {
    match tokio::time::timeout(limit, next.run(req)).await {
        Ok(r) => r,
        Err(_) => (StatusCode::REQUEST_TIMEOUT, axum::Json(json!({ "error": "request timed out", "code": "timeout" })))
            .into_response(),
    }
}

#[derive(Clone)]
struct Guard {
    hosts: Arc<HashSet<String>>,
}

fn forbid(why: &str) -> Response {
    (StatusCode::FORBIDDEN, axum::Json(json!({ "error": why, "code": "forbidden" }))).into_response()
}

fn is_loopback_name(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().map(|a| a.is_loopback()).unwrap_or(false)
}

fn request_host(req: &Request) -> Option<String> {
    let mut all = req.headers().get_all(header::HOST).iter();
    match (all.next(), all.next()) {
        (Some(v), None) => v.to_str().ok().map(str::to_string),
        (None, _) => req.uri().authority().map(|a| a.to_string()),
        _ => None,
    }
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

/// Absent is allowed. Present: its authority equals `Host` (case-insensitive), or its host is
/// an allowed name that is not loopback. `null`, a bad value, another site, and a loopback
/// Origin different from `Host` are refused.
fn origin_ok(headers: &HeaderMap, host: &str, allowed: &HashSet<String>) -> bool {
    let mut all = headers.get_all(header::ORIGIN).iter();
    let Some(first) = all.next() else { return true };
    if all.next().is_some() {
        return false;
    }
    let Ok(origin) = first.to_str() else { return false };
    let Some((scheme, authority)) = origin.split_once("://") else { return false };
    if (scheme != "http" && scheme != "https") || authority.is_empty() || authority.contains(['/', '@', ' ', '\\']) {
        return false;
    }
    if authority.eq_ignore_ascii_case(host) {
        return true;
    }
    let h = host_of(authority);
    !is_loopback_name(&h) && allowed.contains(&h)
}

/// Headers on every response of the door.
async fn response_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

async fn not_found() -> Response {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}

async fn join() -> Response {
    let mut r = Response::new(Body::from(JOIN_PAGE));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(JOIN_CSP));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}
