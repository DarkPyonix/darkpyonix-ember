//! ember server on the peer-to-peer transport (SPEC `FR-N1`, `FR-N3`, `FR-N5`).
//!
//! The server holds one [`Transport`] with a persistent identity (`<data dir>/transport.key`).
//! It is used two ways:
//!
//! - **Outgoing, to nodes**: computers registered by peer ([`crate::computers`]) are dialed
//!   through one shared [`Dialer`] for service `ember-node`. The node admits the server only
//!   if the server's peer id is on its allow-list.
//! - **Incoming, from clients**: the API is served on service [`SERVER_SERVICE`] to devices on
//!   the [`crate::devices`] allow-list (a [`PeerGate`]); other peers are closed at accept. The
//!   TCP listener stays for local use (and is the only place devices are managed).
//!
//! `EMBER_TRANSPORT=1` turns it on (off by default, since binding contacts relay servers);
//! `EMBER_RELAY_URL` picks the relays.

use std::future::Future;
use std::io;
use std::path::Path;

use ember_transport::http::HttpListener;
use ember_transport::{BackendKind, Dialer, PeerGate, SecretKey, Transport, TransportConfig};

/// Transport service name of the server API for clients.
pub const SERVER_SERVICE: &str = "ember-server";
/// Transport service name of the node API (re-exported for convenience).
pub use ember_node::client::NODE_SERVICE;

pub const ENABLE_ENV: &str = "EMBER_TRANSPORT";
pub const KEY_FILE: &str = "transport.key";

/// Port of the main server's relay (TCP) and address observer (UDP) on the letify backend.
/// Unset: [`DEFAULT_RELAY_PORT`]. `0` picks separate free TCP and UDP ports (tests).
pub const RELAY_PORT_ENV: &str = "EMBER_SERVER_RELAY_PORT";
/// `off` turns the automatic relay and observer roles off: `EMBER_SERVER_RELAY=off`.
pub const RELAY_ROLE_ENV: &str = "EMBER_SERVER_RELAY";
/// The default relay and observer port of the main server (SPEC `FR-N2`).
pub const DEFAULT_RELAY_PORT: u16 = 7443;

/// The relay port from [`RELAY_PORT_ENV`], or [`DEFAULT_RELAY_PORT`].
pub fn relay_port_from_env() -> u16 {
    std::env::var(RELAY_PORT_ENV).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(DEFAULT_RELAY_PORT)
}

fn relay_role_enabled() -> bool {
    !matches!(
        std::env::var(RELAY_ROLE_ENV).unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "off" | "none" | "disabled" | "false" | "no"
    )
}

/// Makes the main server the relay and the address observer of the fleet (SPEC `FR-N2`, INTENT
/// D36): on the letify backend, and unless set explicitly (`EMBER_LETIFY_RELAY_LISTEN`,
/// `EMBER_LETIFY_QAD_LISTEN`), the relay listens on TCP `0.0.0.0:<port>` and the observer on UDP
/// `0.0.0.0:<port>`. Other backends are left alone.
pub fn with_main_server_roles(mut cfg: TransportConfig, port: u16) -> TransportConfig {
    if cfg.backend != BackendKind::Letify {
        return cfg;
    }
    let any = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    cfg.letify.relay_listen.get_or_insert(any);
    cfg.letify.qad_listen.get_or_insert(any);
    cfg
}

/// [`with_main_server_roles`] on the port from the environment, unless `EMBER_SERVER_RELAY=off`.
fn main_server_roles_from_env(cfg: TransportConfig) -> TransportConfig {
    if relay_role_enabled() {
        with_main_server_roles(cfg, relay_port_from_env())
    } else {
        cfg
    }
}

/// Whether `EMBER_TRANSPORT` asks for the transport.
pub fn enabled_from_env() -> bool {
    matches!(
        std::env::var(ENABLE_ENV).unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes"
    )
}

/// Binds the real transport with the server's persistent key and the relay from the
/// environment.
pub async fn bind(data_dir: &Path) -> anyhow::Result<Transport> {
    Ok(Transport::bind(main_server_roles_from_env(TransportConfig::from_env(load_key(data_dir)?))).await?)
}

/// The server's persistent transport key (`<data dir>/transport.key`, created on first use):
/// its identity to nodes, clients and the hub.
pub fn load_key(data_dir: &Path) -> anyhow::Result<SecretKey> {
    Ok(SecretKey::load_or_generate(data_dir.join(KEY_FILE))?)
}

/// The transport configuration with the hub (FR-N2): when this server is `registered`, or the
/// hub was named explicitly (`EMBER_HUB_URL`), the hub's relay and address directory, resolving
/// with `token` (the registration's resolve token, `dpr_`; never the device token: NFR-H2);
/// otherwise the plain environment configuration (the directory is turned on at runtime once
/// the server registers, see [`crate::hub`]).
pub fn config_with_hub(
    key: SecretKey,
    hub: Option<&ember_hub::HubConfig>,
    registered: bool,
    token: Option<String>,
) -> TransportConfig {
    let cfg = match hub {
        Some(h) if registered || ember_hub::HubConfig::explicitly_enabled() => h.transport_config(key, token),
        _ => TransportConfig::from_env(key),
    };
    main_server_roles_from_env(cfg)
}

/// The server's transport handle plus the dialer every node client shares.
#[derive(Clone, Debug)]
pub struct ServerTransport {
    pub transport: Transport,
    pub dialer: Dialer,
}

impl ServerTransport {
    pub fn new(transport: Transport) -> Self {
        Self { dialer: Dialer::new(transport.clone()), transport }
    }
}

/// Starts listening on [`SERVER_SERVICE`] now and returns the future that serves `app` to peers
/// `gate` admits (until dropped, or forever once the transport closes).
pub fn serve(
    transport: &Transport,
    app: axum::Router,
    gate: PeerGate,
) -> io::Result<impl Future<Output = io::Result<()>> + Send + 'static> {
    let listener = transport.listen(SERVER_SERVICE).map_err(io::Error::other)?;
    // A backend that enforces admission below the gate (the relay role of the main server) is
    // told about every allow and revoke (FR-N2, FR-N3).
    gate.attach(transport);
    Ok(async move { axum::serve(HttpListener::with_gate(listener, gate), app).await })
}

/// Like [`serve`], with each call checked against the calling device's scope (SPEC `FR-N8`,
/// #366): `app` is wrapped by [`crate::devices::scope::enforce`] and served with the caller's
/// [`ember_transport::PeerId`] as connect info, and `devices.gate()` admits the peers. This is
/// what `main.rs` serves; [`serve`] stays for tests and tools that need no scope.
pub fn serve_scoped(
    transport: &Transport,
    app: axum::Router,
    devices: std::sync::Arc<crate::devices::Devices>,
) -> io::Result<impl Future<Output = io::Result<()>> + Send + 'static> {
    let listener = transport.listen(SERVER_SERVICE).map_err(io::Error::other)?;
    let gate = devices.gate().clone();
    gate.attach(transport);
    let app = crate::devices::scope::enforce(app, devices)
        .into_make_service_with_connect_info::<ember_transport::PeerId>();
    Ok(async move { axum::serve(HttpListener::with_gate(listener, gate), app).await })
}
