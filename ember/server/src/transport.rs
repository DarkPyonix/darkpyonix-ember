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
//! `EMBER_RELAY_URL` picks the relays; `EMBER_TRANSPORT_LISTEN` fixes the address it binds
//! ([`LISTEN_ENV`]).

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::Path;

use ember_transport::http::HttpListener;
use ember_transport::{BackendKind, Dialer, PeerAddr, PeerGate, SecretKey, Transport, TransportConfig};

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

/// The fixed address the transport binds (SPEC `FR-N9`, #863, provisional), `ip:port`: on letify
/// its TCP listener (its UDP sockets do not follow it: the address observer's client binds a
/// random UDP port and the relay's observer has its own port), on iroh its QUIC UDP socket.
/// Unset or empty: a random port, as before. A device's saved `<peer id>@<ip>:<port>` then
/// survives a server restart. This is the transport door, where every peer is admitted only by
/// its device key (`FR-N7`, the [`PeerGate`]), so a non-loopback address such as `0.0.0.0:<port>`
/// is allowed here, unlike the plain HTTP listeners that D37 keeps on loopback.
pub const LISTEN_ENV: &str = "EMBER_TRANSPORT_LISTEN";

/// Parses a [`LISTEN_ENV`] value: `None` or empty is `Ok(None)`; anything that is not an
/// `ip:port` with a port other than 0 is an error naming the variable, so a typo stops the start
/// instead of falling back to a random port.
pub fn listen_from(value: Option<&str>) -> anyhow::Result<Option<SocketAddr>> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    let addr: SocketAddr = v
        .parse()
        .map_err(|e| anyhow::anyhow!("{LISTEN_ENV}={v:?} is not an ip:port address ({e}); for example 0.0.0.0:7444"))?;
    if addr.port() == 0 {
        anyhow::bail!("{LISTEN_ENV}={v:?} names port 0, a random port; leave {LISTEN_ENV} unset for that");
    }
    Ok(Some(addr))
}

/// [`listen_from`] on the process environment.
pub fn listen_from_env() -> anyhow::Result<Option<SocketAddr>> {
    listen_from(std::env::var(LISTEN_ENV).ok().as_deref())
}

/// `cfg` bound at `listen` when it is set; unchanged otherwise.
pub fn with_listen(mut cfg: TransportConfig, listen: Option<SocketAddr>) -> TransportConfig {
    if let Some(addr) = listen {
        cfg.bind_addr = Some(addr);
    }
    cfg
}

/// The start of the line the server prints for a device (SPEC `FR-N9`, #863).
pub const DEVICE_ADDRESS_PREFIX: &str = "ember server device address: ";
/// Stands for the server's address in that line when nothing better is known.
pub const HOST_PLACEHOLDER: &str = "<host>";

/// The line a person pastes into the app's address field, only with a fixed [`LISTEN_ENV`]:
/// `ember server device address: <peer id>@<host>:<port>`. `<host>:<port>` is `listen` when its
/// IP is not unspecified; else the first address the transport advertises (`local.direct`:
/// `EMBER_LETIFY_ADVERTISE` as written, or the observed public address with the listen port);
/// else [`HOST_PLACEHOLDER`] with the listen port and a note to replace it, since a server cannot
/// know the name a device reaches it by.
pub fn device_address_line(listen: Option<SocketAddr>, local: &PeerAddr) -> Option<String> {
    let listen = listen?;
    let me = local.peer;
    if !listen.ip().is_unspecified() {
        return Some(format!("{DEVICE_ADDRESS_PREFIX}{me}@{listen}"));
    }
    Some(match local.direct.iter().find(|a| !a.ip().is_unspecified()) {
        Some(addr) => format!("{DEVICE_ADDRESS_PREFIX}{me}@{addr}"),
        None => format!(
            "{DEVICE_ADDRESS_PREFIX}{me}@{HOST_PLACEHOLDER}:{} (replace {HOST_PLACEHOLDER} with this computer's address or name)",
            listen.port()
        ),
    })
}

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

/// Binds the real transport with the server's persistent key and the relay and the listen
/// address ([`LISTEN_ENV`]) from the environment.
pub async fn bind(data_dir: &Path) -> anyhow::Result<Transport> {
    let cfg = with_listen(TransportConfig::from_env(load_key(data_dir)?), listen_from_env()?);
    Ok(Transport::bind(main_server_roles_from_env(cfg)).await?)
}

/// The server's persistent transport key (`<data dir>/transport.key`, created on first use):
/// its identity to nodes, clients and the hub.
pub fn load_key(data_dir: &Path) -> anyhow::Result<SecretKey> {
    ember_node::keystore::load_transport_key(&data_dir.join(KEY_FILE))
}

/// The transport configuration with the hub (FR-N2): when this server is `registered`, or the
/// hub was named explicitly (`EMBER_HUB_URL`), the hub's relay and address directory, resolving
/// with `token` (the registration's resolve token, `dpr_`; never the device token: NFR-H2);
/// otherwise the plain environment configuration (the directory is turned on at runtime once
/// the server registers, see [`crate::hub`]). `device_token` turns on the hub's batched pkarr
/// watch when its `/config` advertises one (sent in the header only, #783). Bound at
/// [`LISTEN_ENV`] when it is set; a bad value is an error.
pub fn config_with_hub(
    key: SecretKey,
    hub: Option<&ember_hub::HubConfig>,
    registered: bool,
    token: Option<String>,
    device_token: Option<String>,
) -> anyhow::Result<TransportConfig> {
    let cfg = match hub {
        Some(h) if registered || ember_hub::HubConfig::explicitly_enabled() => {
            h.transport_config_with_watch(key, token, device_token)
        }
        _ => TransportConfig::from_env(key),
    };
    Ok(main_server_roles_from_env(with_listen(cfg, listen_from_env()?)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> ember_transport::PeerId {
        SecretKey::from_bytes([11; 32]).peer_id()
    }

    #[test]
    fn fr_n9_transport_listen_unset_good_or_a_startup_error() {
        assert_eq!(listen_from(None).unwrap(), None, "unset: a random port, as before");
        assert_eq!(listen_from(Some("  ")).unwrap(), None);
        assert_eq!(listen_from(Some(" 0.0.0.0:7444 ")).unwrap(), Some("0.0.0.0:7444".parse().unwrap()));
        assert_eq!(listen_from(Some("[::]:7444")).unwrap(), Some("[::]:7444".parse().unwrap()));
        for bad in ["7444", "mini.local:7444", "0.0.0.0", "0.0.0.0:99999", "0.0.0.0:0"] {
            let e = listen_from(Some(bad)).unwrap_err().to_string();
            assert!(e.contains(LISTEN_ENV), "{bad}: {e}");
        }
    }

    #[test]
    fn fr_n9_transport_listen_sets_the_bind_address() {
        let key = SecretKey::from_bytes([13; 32]);
        let addr: SocketAddr = "0.0.0.0:7444".parse().unwrap();
        let cfg = with_listen(TransportConfig::new(key.clone()), Some(addr));
        assert_eq!(cfg.bind_addr, Some(addr));
        assert_eq!(with_listen(TransportConfig::new(key.clone()), None).bind_addr, None, "unset: unchanged");
        // The main server roles keep it (they set only the relay and observer addresses).
        let cfg = with_main_server_roles(cfg.backend(BackendKind::Letify), 7443);
        assert_eq!(cfg.bind_addr, Some(addr));
    }

    #[test]
    fn fr_n9_device_address_line_names_the_fixed_port() {
        let me = me();
        let local = PeerAddr { peer: me, relays: vec![], direct: vec![] };
        assert_eq!(device_address_line(None, &local), None, "only with a fixed port");
        // A bind IP that is not unspecified is the host.
        let line = device_address_line(Some("192.168.0.10:7444".parse().unwrap()), &local).unwrap();
        assert_eq!(line, format!("ember server device address: {me}@192.168.0.10:7444"));
        // An unspecified bind IP: the first advertised or observed address.
        let any: SocketAddr = "0.0.0.0:7444".parse().unwrap();
        let advertised = PeerAddr { direct: vec!["203.0.113.4:7444".parse().unwrap()], ..local.clone() };
        assert_eq!(
            device_address_line(Some(any), &advertised).unwrap(),
            format!("{DEVICE_ADDRESS_PREFIX}{me}@203.0.113.4:7444")
        );
        // Nothing known: the placeholder and a note.
        let line = device_address_line(Some(any), &local).unwrap();
        assert!(line.starts_with(&format!("{DEVICE_ADDRESS_PREFIX}{me}@<host>:7444 (replace <host> ")), "{line}");
    }
}
