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
//! ([`LISTEN_ENV`]). With the relay role on, a device that has only the line [`device_line`]
//! prints (peer id plus this server's relay) reaches the server through that relay (`FR-N9`, #898).

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
/// random UDP port and the relay's observer has its own port), on the QUIC backend its UDP socket.
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

/// Whether `cfg` makes this endpoint the fleet's relay: the letify backend with a relay address.
pub fn runs_relay_role(cfg: &TransportConfig) -> bool {
    cfg.backend == BackendKind::Letify && cfg.letify.relay_listen.is_some()
}

/// The port of this server's own relay in `local` (`letify://<our peer id>@<ip>:<port>`), `None`
/// when `local` names no relay of ours (another server's relay from `EMBER_RELAY_URL`).
fn own_relay_port(local: &PeerAddr) -> Option<u16> {
    let own = format!("letify://{}@", local.peer);
    local.relays.iter().find_map(|url| {
        let (_, port) = url.strip_prefix(&own)?.trim_end_matches('/').rsplit_once(':')?;
        port.parse::<u16>().ok()
    })
}

/// The line a person pastes into the app's address field (SPEC `FR-N9`, #863, #898).
///
/// With the relay role on (`relay_role`, [`runs_relay_role`]) it names this server's own relay on
/// its fixed port, through which a device reaches the server with no direct address and no hub:
/// `ember server device address: <peer id> relay letify://<peer id>@<host>:<relay port>`, and
/// with a fixed [`LISTEN_ENV`] as well, `<peer id>@<host>:<port> relay letify://...`. Without the
/// relay role it is [`device_address_line`]. `<host>` is the bind IP of [`LISTEN_ENV`] when it is
/// not unspecified, else the first address the transport advertises, else [`HOST_PLACEHOLDER`]
/// with a note to replace it. The relay's host may be replaced by a DNS name; the direct address
/// must stay an IP.
pub fn device_line(listen: Option<SocketAddr>, relay_role: bool, local: &PeerAddr) -> Option<String> {
    let Some(relay_port) = relay_role.then(|| own_relay_port(local)).flatten() else {
        return device_address_line(listen, local);
    };
    let me = local.peer;
    let advertised = local.direct.iter().find(|a| !a.ip().is_unspecified());
    let fixed_ip = listen.map(|l| l.ip()).filter(|ip| !ip.is_unspecified());
    let relay_host = match fixed_ip.or(advertised.map(|a| a.ip())) {
        Some(ip) => SocketAddr::new(ip, relay_port).to_string(),
        None => format!("{HOST_PLACEHOLDER}:{relay_port}"),
    };
    let direct = listen.map(|l| match (fixed_ip, advertised) {
        (Some(_), _) => l.to_string(),
        (None, Some(a)) => a.to_string(),
        (None, None) => format!("{HOST_PLACEHOLDER}:{}", l.port()),
    });
    let mut line = match direct {
        Some(d) => format!("{DEVICE_ADDRESS_PREFIX}{me}@{d} relay letify://{me}@{relay_host}"),
        None => format!("{DEVICE_ADDRESS_PREFIX}{me} relay letify://{me}@{relay_host}"),
    };
    if line.contains(HOST_PLACEHOLDER) {
        line.push_str(&format!(" (replace {HOST_PLACEHOLDER} with this computer's address or name)"));
    }
    Some(line)
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

    #[test]
    fn fr_n9_device_line_names_the_own_relay_when_the_relay_role_is_on() {
        let me = me();
        let local = PeerAddr {
            peer: me,
            relays: vec![format!("letify://{me}@203.0.113.4:7443")],
            direct: vec!["203.0.113.4:51234".parse().unwrap()],
        };
        // No fixed transport port: the peer id and the relay, never the random direct port.
        assert_eq!(
            device_line(None, true, &local).unwrap(),
            format!("ember server device address: {me} relay letify://{me}@203.0.113.4:7443")
        );
        // A fixed transport port too: the direct address first, then the relay.
        let any: SocketAddr = "0.0.0.0:7444".parse().unwrap();
        assert_eq!(
            device_line(Some(any), true, &local).unwrap(),
            format!("{DEVICE_ADDRESS_PREFIX}{me}@203.0.113.4:51234 relay letify://{me}@203.0.113.4:7443")
        );
        let lan: SocketAddr = "192.168.0.10:7444".parse().unwrap();
        assert_eq!(
            device_line(Some(lan), true, &local).unwrap(),
            format!("{DEVICE_ADDRESS_PREFIX}{me}@192.168.0.10:7444 relay letify://{me}@192.168.0.10:7443")
        );
        let v6 = PeerAddr { direct: vec!["[2001:db8::1]:5000".parse().unwrap()], ..local.clone() };
        assert!(device_line(None, true, &v6).unwrap().ends_with(&format!("letify://{me}@[2001:db8::1]:7443")));
        // Without the relay role, or with another server's relay, it is the #863 line.
        assert_eq!(device_line(None, false, &local), None);
        assert_eq!(device_line(Some(lan), false, &local), device_address_line(Some(lan), &local));
        let other = SecretKey::from_bytes([12; 32]).peer_id();
        let elsewhere = PeerAddr { relays: vec![format!("letify://{other}@relay.example:7443")], ..local.clone() };
        assert_eq!(device_line(None, true, &elsewhere), None);
    }

    #[test]
    fn fr_n9_device_line_without_a_known_address_keeps_a_placeholder() {
        let me = me();
        // Bound on 0.0.0.0 with nothing advertised or observed: local_addr names the relay on
        // 127.0.0.1 and no direct address.
        let local = PeerAddr { peer: me, relays: vec![format!("letify://{me}@127.0.0.1:7443")], direct: vec![] };
        let line = device_line(None, true, &local).unwrap();
        assert_eq!(
            line,
            format!("{DEVICE_ADDRESS_PREFIX}{me} relay letify://{me}@<host>:7443 (replace <host> with this computer's address or name)")
        );
        let line = device_line(Some("0.0.0.0:7444".parse().unwrap()), true, &local).unwrap();
        assert!(line.starts_with(&format!("{DEVICE_ADDRESS_PREFIX}{me}@<host>:7444 relay letify://{me}@<host>:7443 (replace ")), "{line}");
    }

    #[test]
    fn fr_n9_relay_role_follows_the_backend_and_the_relay_address() {
        let cfg = TransportConfig::new(SecretKey::from_bytes([13; 32])).backend(BackendKind::Letify);
        assert!(!runs_relay_role(&cfg));
        let cfg = with_main_server_roles(cfg, 7443);
        assert!(runs_relay_role(&cfg));
    }
}
