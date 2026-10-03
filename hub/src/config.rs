//! Which hub to use, and the transport configuration that goes with it.

use ember_transport::{HubDirectory, RelayConfig, SecretKey, TransportConfig, RELAY_URL_ENV};
use reqwest::Url;

/// The hub's base URL. Unset: [`DEFAULT_HUB_URL`]. `off` / `none` / `disabled`: no hub.
pub const HUB_URL_ENV: &str = "EMBER_HUB_URL";
/// The hub's relay, when it cannot be derived from the hub URL (or to override it).
pub const HUB_RELAY_URL_ENV: &str = "EMBER_HUB_RELAY_URL";
pub const DEFAULT_HUB_URL: &str = "https://darkpyonix.dev";

/// A hub: its API base URL and its relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubConfig {
    /// `https://darkpyonix.dev` (no trailing slash).
    pub url: String,
    /// The hub's relay (`/relay` on the relay host, given to the transport as the relay base
    /// URL). `None` when it could not be derived (an IP-address hub, e.g. a test fake) and was
    /// not given: the transport then keeps its own relay setting.
    pub relay_url: Option<String>,
}

impl HubConfig {
    /// A hub at `url`; the relay is derived ([`HubConfig::derive_relay_url`]).
    pub fn new(url: impl Into<String>) -> Self {
        let url = url.into().trim().trim_end_matches('/').to_string();
        let relay_url = Self::derive_relay_url(&url);
        Self { url, relay_url }
    }

    pub fn with_relay(mut self, relay_url: Option<String>) -> Self {
        self.relay_url = relay_url;
        self
    }

    /// From `EMBER_HUB_URL` (default [`DEFAULT_HUB_URL`]) and `EMBER_HUB_RELAY_URL`. `None` when
    /// the hub is turned off.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var(HUB_URL_ENV).unwrap_or_default();
        Self::parse(&raw, std::env::var(HUB_RELAY_URL_ENV).ok().as_deref())
    }

    /// Whether `EMBER_HUB_URL` is set to a hub (not unset, not off).
    pub fn explicitly_enabled() -> bool {
        let raw = std::env::var(HUB_URL_ENV).unwrap_or_default();
        !raw.trim().is_empty() && Self::parse(&raw, None).is_some()
    }

    /// [`HubConfig::from_env`] on given values (for tests).
    pub fn parse(hub_url: &str, relay_url: Option<&str>) -> Option<Self> {
        let hub_url = hub_url.trim();
        let mut config = match hub_url.to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" | "0" => return None,
            "" | "default" => Self::new(DEFAULT_HUB_URL),
            _ => Self::new(hub_url),
        };
        if let Some(relay) = relay_url.map(str::trim).filter(|r| !r.is_empty()) {
            config.relay_url = Some(relay.trim_end_matches('/').to_string());
        }
        Some(config)
    }

    /// The relay host of a hub: `https://<host>` → `https://relay.<host>` (the hub's two-host
    /// layout: the Worker cannot take the relay's UDP). An IP-address or `localhost` hub has no
    /// derivable relay. The hub does not advertise its relay (see the spec gaps in
    /// `docs/design/HUB-INTEGRATION.md`).
    pub fn derive_relay_url(hub_url: &str) -> Option<String> {
        let url = Url::parse(hub_url).ok()?;
        let host = url.host_str()?;
        if host == "localhost" || host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
            return None;
        }
        let mut relay = url.clone();
        relay.set_host(Some(&format!("relay.{host}"))).ok()?;
        relay.set_path("");
        Some(relay.as_str().trim_end_matches('/').to_string())
    }

    /// `<hub>/pkarr`: the address directory in the pkarr relay protocol.
    pub fn pkarr_url(&self) -> String {
        format!("{}/pkarr", self.url)
    }

    /// The relay setting for a device of this hub: `EMBER_RELAY_URL` if set (explicit wins),
    /// else the hub's relay, else the transport default.
    pub fn relay_config(&self) -> RelayConfig {
        if std::env::var_os(RELAY_URL_ENV).is_some() {
            return RelayConfig::from_env();
        }
        match &self.relay_url {
            Some(r) => RelayConfig::Custom(vec![r.clone()]),
            None => RelayConfig::Default,
        }
    }

    /// The directory setting: publish through the hub, resolve with `token` (if registered).
    pub fn directory(&self, token: Option<String>) -> HubDirectory {
        HubDirectory::new(self.pkarr_url()).token(token)
    }

    /// A transport configuration for a device of this hub: the hub's relay and its address
    /// directory. `token` is the device token once registered.
    pub fn transport_config(&self, key: SecretKey, token: Option<String>) -> TransportConfig {
        TransportConfig::new(key).relay(self.relay_config()).hub(self.directory(token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_urls() {
        let d = HubConfig::parse("", None).unwrap();
        assert_eq!(d.url, "https://darkpyonix.dev");
        assert_eq!(d.relay_url.as_deref(), Some("https://relay.darkpyonix.dev"));
        assert_eq!(d.pkarr_url(), "https://darkpyonix.dev/pkarr");
        assert!(HubConfig::parse("off", None).is_none());

        let local = HubConfig::parse("http://127.0.0.1:9000/", None).unwrap();
        assert_eq!(local.url, "http://127.0.0.1:9000");
        assert_eq!(local.relay_url, None);
        let local = HubConfig::parse("http://127.0.0.1:9000", Some("http://127.0.0.1:9001")).unwrap();
        assert_eq!(local.relay_url.as_deref(), Some("http://127.0.0.1:9001"));

        let staging = HubConfig::new("https://staging.darkpyonix.dev");
        assert_eq!(staging.relay_url.as_deref(), Some("https://relay.staging.darkpyonix.dev"));
    }
}
