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

/// Everything needed to bind a transport.
#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub secret_key: SecretKey,
    pub relay: RelayConfig,
    /// Our own directory (later: the darkpyonix.dev address directory). Publishes our
    /// addresses and resolves peers'. Optional.
    pub directory: Option<Arc<dyn AddressDirectory>>,
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
}
