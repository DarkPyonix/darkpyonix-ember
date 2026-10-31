//! Transport configuration. Plain data: no backend types.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::{AddressDirectory, PeerId, PkarrStore, SecretKey, SignalChannel};

/// Environment variable choosing the transport backend: `iroh` (the default) or `letify`.
/// Every endpoint of a fleet must use the same value until the rollout is complete (SPEC `FR-N5`).
pub const BACKEND_ENV: &str = "EMBER_TRANSPORT_BACKEND";

/// Which backend [`crate::Transport::bind`] builds (SPEC `FR-N5`, `INTENT.md` D36).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// iroh: QUIC, hole punching, the hub's iroh relay.
    Iroh,
    /// letify-link: a Noise session over TCP paths, the main server as relay (cargo feature
    /// `letify`).
    Letify,
}

impl Default for BackendKind {
    /// iroh stays the default until all endpoints switch together. A build without the `iroh`
    /// feature has only letify.
    fn default() -> Self {
        if cfg!(feature = "iroh") {
            Self::Iroh
        } else {
            Self::Letify
        }
    }
}

impl BackendKind {
    /// Parses a backend name (`iroh`, `letify`, `letify-link`), case-insensitive.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "iroh" => Some(Self::Iroh),
            "letify" | "letify-link" => Some(Self::Letify),
            _ => None,
        }
    }

    /// Reads [`BACKEND_ENV`]; unset, empty or unknown means the default (an unknown value is
    /// logged).
    pub fn from_env() -> Self {
        match std::env::var(BACKEND_ENV) {
            Ok(v) if v.trim().is_empty() => Self::default(),
            Ok(v) => Self::parse(&v).unwrap_or_else(|| {
                tracing::warn!(value = %v, "{BACKEND_ENV} is not iroh or letify; using the default backend");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Iroh => "iroh",
            Self::Letify => "letify",
        })
    }
}

/// A UDP address observer (the main server's QAD responder, SPEC `FR-N2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObserverAddr {
    pub addr: SocketAddr,
    /// The responder's peer id. With it, replies must be signed by that key.
    pub node: Option<PeerId>,
}

impl ObserverAddr {
    /// Parses `host:port` or `host:port#<peer id hex>`.
    pub fn parse(value: &str) -> Option<Self> {
        let (addr, node) = match value.trim().split_once('#') {
            Some((a, n)) => (a, Some(n.parse::<PeerId>().ok()?)),
            None => (value.trim(), None),
        };
        Some(Self { addr: addr.parse().ok()?, node })
    }
}

/// Settings of the letify-link backend. Plain data: no letify-link type (SPEC `FR-N5`). The
/// iroh backend ignores all of it.
///
/// The relay a node or client uses is [`TransportConfig::relay`] with the main server written as
/// `letify://<server peer id hex>@host:port`.
#[derive(Clone, Debug)]
pub struct LetifyConfig {
    /// Run the relay role on this TCP address. Set on the main server only (SPEC `FR-N2`); the
    /// relay admits the peers it is told about ([`crate::Transport::allow_peer`], or
    /// [`crate::PeerGate::attach`]) and the server itself.
    pub relay_listen: Option<SocketAddr>,
    /// Admit every peer that completes the relay handshake, not only allowed ones. Tests only.
    pub relay_open: bool,
    /// Answer address-observation (QAD) requests on this UDP address. Set on the main server.
    pub qad_listen: Option<SocketAddr>,
    /// Ask this responder for our public address and publish it in our record.
    pub observer: Option<ObserverAddr>,
    /// Addresses to publish for TCP dialing, instead of the ones derived from the bound address
    /// and the observed public address.
    pub advertise: Vec<SocketAddr>,
    /// The rendezvous channel. `None` uses the hub's pkarr mailbox when a hub or
    /// [`LetifyConfig::pkarr`] is configured, and otherwise no rendezvous (so no relayed path).
    pub signal: Option<Arc<dyn SignalChannel>>,
    /// The pkarr directory. `None` uses [`TransportConfig::hub`] over HTTP.
    pub pkarr: Option<Arc<dyn PkarrStore>>,
    /// How long a session without any path waits for a new one before its connections fail.
    pub resume_window: Duration,
    /// Use letify-link's phone profile (smaller windows, slower keepalive).
    pub mobile: bool,
    /// How often the pkarr mailbox of each watched peer is read.
    pub poll_interval: Duration,
    /// How long one dial of one candidate path may take before it is retried. A relayed dial
    /// includes the rendezvous round trip.
    pub dial_timeout: Duration,
    /// How long [`crate::Transport::connect`] tries all candidate paths before it gives up.
    pub connect_timeout: Duration,
}

impl Default for LetifyConfig {
    fn default() -> Self {
        Self {
            relay_listen: None,
            relay_open: false,
            qad_listen: None,
            observer: None,
            advertise: Vec::new(),
            signal: None,
            pkarr: None,
            resume_window: Duration::from_secs(300),
            mobile: false,
            poll_interval: Duration::from_millis(1500),
            dial_timeout: Duration::from_secs(8),
            connect_timeout: Duration::from_secs(20),
        }
    }
}

impl LetifyConfig {
    /// Reads `EMBER_LETIFY_RELAY_LISTEN`, `EMBER_LETIFY_QAD_LISTEN`, `EMBER_LETIFY_OBSERVER`
    /// and `EMBER_LETIFY_ADVERTISE` (comma separated `ip:port`). Unusable values are logged and
    /// ignored.
    pub fn from_env() -> Self {
        fn var(name: &str) -> Option<String> {
            std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        }
        fn addr(name: &str) -> Option<SocketAddr> {
            let v = var(name)?;
            v.parse().map_err(|e| tracing::warn!(value = %v, "{name}: {e}")).ok()
        }
        let mut cfg = Self {
            relay_listen: addr("EMBER_LETIFY_RELAY_LISTEN"),
            qad_listen: addr("EMBER_LETIFY_QAD_LISTEN"),
            ..Self::default()
        };
        if let Some(v) = var("EMBER_LETIFY_OBSERVER") {
            cfg.observer = ObserverAddr::parse(&v);
            if cfg.observer.is_none() {
                tracing::warn!(value = %v, "EMBER_LETIFY_OBSERVER is not host:port[#peer id]");
            }
        }
        if let Some(v) = var("EMBER_LETIFY_ADVERTISE") {
            cfg.advertise = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        }
        cfg
    }
}

/// Environment variable naming the relay server(s): a comma-separated list of URLs, or
/// `none` / `off` / `disabled` to run without a relay. Unset means the backend's default relays.
pub const RELAY_URL_ENV: &str = "EMBER_RELAY_URL";

/// Which relay servers to use for hole-punch coordination and fallback (SPEC `FR-N2`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum RelayConfig {
    /// The backend's default public relays (for iroh: n0's production relays).
    #[default]
    Default,
    /// No relay: direct paths only (tests, LAN).
    Disabled,
    /// These relay URLs, e.g. `https://relay.darkpyonix.dev`.
    Custom(Vec<String>),
}

impl RelayConfig {
    /// Parses the value of [`RELAY_URL_ENV`].
    pub fn parse(value: &str) -> Self {
        let value = value.trim();
        match value.to_ascii_lowercase().as_str() {
            "" | "default" => Self::Default,
            "none" | "off" | "disabled" => Self::Disabled,
            _ => Self::Custom(
                value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            ),
        }
    }

    /// Reads [`RELAY_URL_ENV`]; unset means [`RelayConfig::Default`].
    pub fn from_env() -> Self {
        std::env::var(RELAY_URL_ENV).map(|v| Self::parse(&v)).unwrap_or_default()
    }
}

/// The darkpyonix.dev hub's address directory (SPEC `FR-N2`, hub `FR-H2`), as plain data.
///
/// The hub's `PUT`/`GET /pkarr/{key}` is the pkarr relay protocol, so the iroh backend uses
/// iroh's own pkarr publisher and resolver against [`HubDirectory::pkarr_url`]: packets are signed
/// with this endpoint's key and verified against the peer's key end to end, and republishing and
/// retry are iroh's. Nothing about pkarr leaks out of this crate.
///
/// - **Publishing** needs no credential (the packet signature authenticates it); the hub accepts
///   it only once this endpoint is a registered device, so an unregistered endpoint's publishes
///   are refused (403) and retried with backoff until it is registered.
/// - **Resolving** needs this device's hub token (`?token=` on the URL, which iroh keeps when it
///   appends the key). Without a token, peers are not resolved through the hub. The token can
///   be set or cleared later with [`crate::Transport::set_directory_token`] (after registration,
///   or when the hub revoked this device).
///
/// The in-memory backend ignores this setting.
#[derive(Clone, PartialEq, Eq)]
pub struct HubDirectory {
    /// `https://api.darkpyonix.dev/pkarr` (no trailing slash, no query).
    pub pkarr_url: String,
    /// This device's hub token (`dpd_...`), for resolving.
    pub token: Option<String>,
    /// Publish direct (IP) addresses too, not only the home relay. The hub only shows a record to
    /// devices of the same account, so this is on by default; turn it off to keep IP addresses
    /// away from the hub (peers then always start relayed and upgrade by hole punching).
    pub publish_direct: bool,
}

impl HubDirectory {
    pub fn new(pkarr_url: impl Into<String>) -> Self {
        Self { pkarr_url: pkarr_url.into(), token: None, publish_direct: true }
    }

    pub fn token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    pub fn publish_direct(mut self, on: bool) -> Self {
        self.publish_direct = on;
        self
    }
}

impl std::fmt::Debug for HubDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubDirectory")
            .field("pkarr_url", &self.pkarr_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("publish_direct", &self.publish_direct)
            .finish()
    }
}

/// Everything needed to bind a transport.
#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub secret_key: SecretKey,
    pub relay: RelayConfig,
    /// Our own directory (later: the darkpyonix.dev address directory). Publishes our
    /// addresses and resolves peers'. Optional.
    pub directory: Option<Arc<dyn AddressDirectory>>,
    /// The darkpyonix.dev address directory (publish + resolve through the hub). Optional.
    pub hub: Option<HubDirectory>,
    /// Also use the backend's public lookup service (for iroh: n0's DNS/pkarr). Publishes this
    /// peer's id and addresses to a third-party service; off by default.
    pub public_lookup: bool,
    /// Local socket to bind (IPv4): UDP for iroh, TCP for letify. `None` = any interface,
    /// random port.
    pub bind_addr: Option<SocketAddr>,
    /// Which backend [`crate::Transport::bind`] builds.
    pub backend: BackendKind,
    /// Settings of the letify-link backend.
    pub letify: LetifyConfig,
}

impl TransportConfig {
    pub fn new(secret_key: SecretKey) -> Self {
        Self {
            secret_key,
            relay: RelayConfig::Default,
            directory: None,
            hub: None,
            public_lookup: false,
            bind_addr: None,
            backend: BackendKind::default(),
            letify: LetifyConfig::default(),
        }
    }

    /// Like [`TransportConfig::new`], with the relay read from [`RELAY_URL_ENV`], the backend
    /// from [`BACKEND_ENV`] and the letify settings from `EMBER_LETIFY_*`.
    pub fn from_env(secret_key: SecretKey) -> Self {
        Self {
            relay: RelayConfig::from_env(),
            backend: BackendKind::from_env(),
            letify: LetifyConfig::from_env(),
            ..Self::new(secret_key)
        }
    }

    pub fn backend(mut self, backend: BackendKind) -> Self {
        self.backend = backend;
        self
    }

    pub fn letify(mut self, letify: LetifyConfig) -> Self {
        self.letify = letify;
        self
    }

    pub fn relay(mut self, relay: RelayConfig) -> Self {
        self.relay = relay;
        self
    }

    pub fn directory(mut self, directory: Arc<dyn AddressDirectory>) -> Self {
        self.directory = Some(directory);
        self
    }

    pub fn hub(mut self, hub: HubDirectory) -> Self {
        self.hub = Some(hub);
        self
    }

    pub fn public_lookup(mut self, on: bool) -> Self {
        self.public_lookup = on;
        self
    }

    pub fn bind_addr(mut self, addr: SocketAddr) -> Self {
        self.bind_addr = Some(addr);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_env_parsing() {
        assert_eq!(RelayConfig::parse(""), RelayConfig::Default);
        assert_eq!(RelayConfig::parse("none"), RelayConfig::Disabled);
        assert_eq!(
            RelayConfig::parse("https://a.example, https://b.example"),
            RelayConfig::Custom(vec!["https://a.example".into(), "https://b.example".into()])
        );
    }

    #[test]
    fn backend_names_parse() {
        assert_eq!(BackendKind::parse("IROH"), Some(BackendKind::Iroh));
        assert_eq!(BackendKind::parse(" letify "), Some(BackendKind::Letify));
        assert_eq!(BackendKind::parse("letify-link"), Some(BackendKind::Letify));
        assert_eq!(BackendKind::parse("quic"), None);
    }

    #[test]
    fn observer_address_parses_with_and_without_a_pin() {
        let a = ObserverAddr::parse("203.0.113.4:7845").unwrap();
        assert_eq!(a.addr, "203.0.113.4:7845".parse().unwrap());
        assert!(a.node.is_none());
        let peer = SecretKey::generate().peer_id();
        let b = ObserverAddr::parse(&format!("203.0.113.4:7845#{peer}")).unwrap();
        assert_eq!(b.node, Some(peer));
        assert!(ObserverAddr::parse("not an address").is_none());
        assert!(ObserverAddr::parse("203.0.113.4:7845#zz").is_none());
    }

    #[test]
    fn hub_directory_debug_redacts_the_token() {
        let h = HubDirectory::new("https://api.darkpyonix.dev/pkarr").token(Some("dpd_secret".into()));
        let text = format!("{h:?}");
        assert!(!text.contains("dpd_secret"), "{text}");
        assert!(h.publish_direct);
    }
}
