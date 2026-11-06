//! The enrol door: an optional listener of its own for the computer enrol flow (SPEC FR-C2e,
//! FR-C2f [provisional], `docs/design/COMPUTERS.md`).
//!
//! This is the skeleton (step C2). The route table is EMPTY: the door answers 404 to everything.
//! The redeem and poll routes arrive in step C3 and are added in [`EnrolDoor::routes`]. The door
//! takes no `Enrol` and holds no secret.
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

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;

use crate::door::config::{DoorEnv, Spec};
use crate::door::harden::{stack, Guard};
pub use crate::door::limits::{DoorLimits, LimitConfig};
pub use crate::door::DoorError;

pub const ENROL_LISTEN_VAR: &str = "EMBER_ENROL_LISTEN";
pub const ENROL_ALLOW_REMOTE_VAR: &str = "EMBER_ENROL_LISTEN_ALLOW_REMOTE";
pub const ENROL_TRUSTED_PROXIES_VAR: &str = "EMBER_ENROL_TRUSTED_PROXIES";
pub const ENROL_ALLOWED_HOSTS_VAR: &str = "EMBER_ENROL_ALLOWED_HOSTS";
pub const ENROL_PUBLIC_URL_VAR: &str = "EMBER_ENROL_PUBLIC_URL";

/// The redeem route, the strict class of the limits: `POST` here is counted against the strict
/// per-address budget whatever the answer (FR-C2f). The route itself arrives in step C3.
pub const ENROL_REDEEM_PATH: &str = "/enrol/redeem";

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

    /// The door's routes. EMPTY in step C2: every request is 404. Step C3 adds
    /// `POST /enrol/redeem` and `GET /enrol/status/{enrol_id}` here.
    pub fn routes() -> Router {
        Router::new()
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

    /// The finished router `main.rs` serves: [`EnrolDoor::routes`] wrapped by
    /// [`EnrolDoor::wrap_limited`] with [`EnrolDoor::limits`].
    pub fn app(&self) -> Router {
        self.wrap_limited(Self::routes(), self.limits())
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
        let app = routes.fallback(crate::door::harden::not_found);
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

    /// Whether the door binds an address other than loopback.
    pub fn is_remote(&self) -> bool {
        !self.listen.ip().is_loopback()
    }
}
