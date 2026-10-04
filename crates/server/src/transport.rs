//! ember server on the peer-to-peer transport (SPEC `FR-N1`, `FR-N3`, `FR-N5`).
//!
//! The server holds one [`Transport`] with a persistent identity (`<data dir>/transport.key`).
//! It is used two ways:
//!
//! - **Outgoing, to nodes**: computers registered by peer ([`crate::computers`]) are dialed
//!   through one shared [`Dialer`] for service `ember-node/1`. The node admits the server only
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
use ember_transport::{Dialer, PeerGate, SecretKey, Transport, TransportConfig};

/// Transport service name of the server API for clients.
pub const SERVER_SERVICE: &str = "ember-server/1";
/// Transport service name of the node API (re-exported for convenience).
pub use ember_node::client::NODE_SERVICE;

pub const ENABLE_ENV: &str = "EMBER_TRANSPORT";
pub const KEY_FILE: &str = "transport.key";

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
    Ok(Transport::bind(TransportConfig::from_env(load_key(data_dir)?)).await?)
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
    match hub {
        Some(h) if registered || ember_hub::HubConfig::explicitly_enabled() => h.transport_config(key, token),
        _ => TransportConfig::from_env(key),
    }
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
    Ok(async move { axum::serve(HttpListener::with_gate(listener, gate), app).await })
}
