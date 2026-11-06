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

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderValue};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

use crate::door::config::{DoorEnv, Spec};
use crate::door::harden::{not_found, stack, Guard};

pub use crate::door::harden::with_timeout;
pub use crate::door::limits::{addr_key, client_ip, TokenBucket};
pub use crate::door::limits::{AddrLimiter, DoorLimits, LimitConfig, LimitKind};
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
        let spec = Spec {
            listen_var: LISTEN_VAR,
            allow_remote_var: ALLOW_REMOTE_VAR,
            trusted_var: TRUSTED_PROXIES_VAR,
            hosts_var: ALLOWED_HOSTS_VAR,
            public_url_var: PUBLIC_URL_VAR,
            noun: "guest door",
        };
        let Some(env) = DoorEnv::parse(get, &[("EMBER_LISTEN", owner_listen)], &spec)? else { return Ok(None) };
        Ok(Some(Self {
            listen: env.listen,
            public_url: env.public_url,
            hosts: Arc::new(env.hosts),
            trusted_proxies: Arc::new(env.trusted),
        }))
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
        let guard = Guard { hosts: self.hosts.clone(), max_body: MAX_BODY_BYTES };
        let app = guest.route(JOIN_ROUTE.1, get(join)).fallback(not_found);
        stack(app, guard, REQUEST_TIMEOUT, limits)
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
        crate::door::serve::serve_capped("guest door", listener, app, shutdown, header_read_timeout, max_connections).await
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
