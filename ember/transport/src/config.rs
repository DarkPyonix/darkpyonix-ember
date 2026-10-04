//! Transport configuration. Plain data: no backend types.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::{AddressDirectory, SecretKey};

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
    /// Local UDP socket to bind (IPv4). `None` = any interface, random port.
    pub bind_addr: Option<SocketAddr>,
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
        }
    }

    /// Like [`TransportConfig::new`], with the relay read from [`RELAY_URL_ENV`].
    pub fn from_env(secret_key: SecretKey) -> Self {
        Self { relay: RelayConfig::from_env(), ..Self::new(secret_key) }
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
    fn hub_directory_debug_redacts_the_token() {
        let h = HubDirectory::new("https://api.darkpyonix.dev/pkarr").token(Some("dpd_secret".into()));
        let text = format!("{h:?}");
        assert!(!text.contains("dpd_secret"), "{text}");
        assert!(h.publish_direct);
    }
}
