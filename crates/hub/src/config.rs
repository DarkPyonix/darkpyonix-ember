//! Which hub to use, and the transport configuration that goes with it.

use ember_transport::{HubDirectory, RelayConfig, SecretKey, TransportConfig, RELAY_URL_ENV};
use reqwest::Url;

use crate::client::HubClient;
use crate::types::HubInfo;

/// The hub's base URL. Unset: [`DEFAULT_HUB_URL`]. `off` / `none` / `disabled`: no hub.
pub const HUB_URL_ENV: &str = "EMBER_HUB_URL";
/// The hub's relay, when it cannot be derived from the hub URL (or to override it).
pub const HUB_RELAY_URL_ENV: &str = "EMBER_HUB_RELAY_URL";
pub const DEFAULT_HUB_URL: &str = "https://darkpyonix.dev";

/// Where [`HubConfig::relay_url`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaySource {
    /// `https://relay.<hub host>` (or none, for an IP-address / `localhost` hub).
    Derived,
    /// `EMBER_HUB_RELAY_URL` / [`HubConfig::with_relay`]: never replaced by discovery.
    Explicit,
    /// The hub's `GET /v1/config` `relay_urls`.
    Hub,
}

/// A hub: its API base URL, its relay and its address directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubConfig {
    /// `https://darkpyonix.dev` (no trailing slash).
    pub url: String,
    /// The hub's (first) relay (`/relay` on the relay host, given to the transport as the relay
    /// base URL). `None` when it could not be derived (an IP-address hub, e.g. a test fake) and
    /// was not given: the transport then keeps its own relay setting.
    pub relay_url: Option<String>,
    /// Further relays the hub advertised in `/v1/config` (after `relay_url`).
    pub extra_relay_urls: Vec<String>,
    pub relay_source: RelaySource,
    /// The pkarr directory the hub advertised (`pkarr_url`); `None`: `<url>/pkarr`.
    pub pkarr_override: Option<String>,
    /// What `GET /v1/config` said ([`HubConfig::discover`]); `None`: not asked, or the hub does
    /// not serve it (then everything is derived as before and the device list is polled).
    pub info: Option<HubInfo>,
}

impl HubConfig {
    /// A hub at `url`; the relay is derived ([`HubConfig::derive_relay_url`]).
    pub fn new(url: impl Into<String>) -> Self {
        let url = url.into().trim().trim_end_matches('/').to_string();
        let relay_url = Self::derive_relay_url(&url);
        Self {
            url,
            relay_url,
            extra_relay_urls: Vec::new(),
            relay_source: RelaySource::Derived,
            pkarr_override: None,
            info: None,
        }
    }

    /// Sets the relay explicitly (discovery keeps it).
    pub fn with_relay(mut self, relay_url: Option<String>) -> Self {
        self.relay_url = relay_url;
        self.extra_relay_urls.clear();
        self.relay_source = RelaySource::Explicit;
        self
    }

    /// Takes the relay and directory the hub advertised (`GET /v1/config`). An explicit relay
    /// (`EMBER_HUB_RELAY_URL`) is kept; an empty `relay_urls` or a missing `pkarr_url` keeps the
    /// derived value.
    pub fn apply_info(&mut self, info: HubInfo) {
        if self.relay_source != RelaySource::Explicit {
            let mut relays = info
                .relay_urls
                .iter()
                .map(|r| r.trim().trim_end_matches('/').to_string())
                .filter(|r| Url::parse(r).is_ok());
            if let Some(first) = relays.next() {
                self.relay_url = Some(first);
                self.extra_relay_urls = relays.collect();
                self.relay_source = RelaySource::Hub;
            }
        }
        if let Some(p) = info.pkarr_url.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            // Absolute, or a path on the hub (`/pkarr`).
            let resolved = Url::parse(p).or_else(|_| Url::parse(&format!("{}/", self.url)).and_then(|b| b.join(p)));
            match resolved {
                Ok(u) => self.pkarr_override = Some(u.as_str().trim_end_matches('/').to_string()),
                Err(e) => tracing::warn!(pkarr_url = p, "the hub advertised an unusable pkarr_url: {e}"),
            }
        }
        self.info = Some(info);
    }

    /// Asks the hub for its configuration (`GET /v1/config`) and applies it. Falls back to the
    /// derived URLs (`https://relay.<host>`, `<hub>/pkarr`) when the hub does not serve the
    /// endpoint (`404`) or cannot be reached; never fails.
    pub async fn discover(mut self) -> Self {
        match HubClient::new(&self.url).config().await {
            Ok(Some(info)) => {
                tracing::debug!(hub = %self.url, ?info, "hub configuration discovered");
                self.apply_info(info);
            }
            Ok(None) => tracing::debug!(hub = %self.url, "the hub has no /v1/config; relay and directory derived"),
            Err(e) => tracing::info!(hub = %self.url, "could not read the hub's /v1/config ({e}); relay and directory derived"),
        }
        self
    }

    /// Whether the hub advertised the device-list long-poll ([`HubInfo::supports_devices_wait`]).
    pub fn supports_devices_wait(&self) -> bool {
        self.info.as_ref().is_some_and(HubInfo::supports_devices_wait)
    }

    /// Every relay of this hub, first one first.
    pub fn relay_urls(&self) -> Vec<String> {
        self.relay_url.iter().chain(self.extra_relay_urls.iter()).cloned().collect()
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
            config = config.with_relay(Some(relay.trim_end_matches('/').to_string()));
        }
        Some(config)
    }

    /// The relay host of a hub: `https://<host>` → `https://relay.<host>` (the hub's two-host
    /// layout: the Worker cannot take the relay's UDP). An IP-address or `localhost` hub has no
    /// derivable relay. This is the fallback when the hub does not advertise its relay in
    /// `GET /v1/config` ([`HubConfig::discover`]).
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

    /// The address directory in the pkarr relay protocol: the advertised `pkarr_url`, else
    /// `<hub>/pkarr`.
    pub fn pkarr_url(&self) -> String {
        self.pkarr_override.clone().unwrap_or_else(|| format!("{}/pkarr", self.url))
    }

    /// The relay setting for a device of this hub: `EMBER_RELAY_URL` if set (explicit wins),
    /// else the hub's relay, else the transport default.
    pub fn relay_config(&self) -> RelayConfig {
        if std::env::var_os(RELAY_URL_ENV).is_some() {
            return RelayConfig::from_env();
        }
        match self.relay_urls() {
            relays if relays.is_empty() => RelayConfig::Default,
            relays => RelayConfig::Custom(relays),
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

    #[test]
    fn advertised_config_replaces_derived_but_not_explicit() {
        let info = HubInfo {
            relay_urls: vec!["https://relay-eu.example.net/".into(), "not a url".into(), "https://relay-us.example.net".into()],
            pkarr_url: Some("/dir/pkarr".into()),
            api_version: Some(1),
            hub_version: Some("test".into()),
            link_url: Some("https://hub.example.net/link".into()),
        };
        let mut c = HubConfig::new("https://hub.example.net");
        assert_eq!(c.relay_source, RelaySource::Derived);
        c.apply_info(info.clone());
        assert_eq!(c.relay_source, RelaySource::Hub);
        assert_eq!(c.relay_urls(), vec!["https://relay-eu.example.net", "https://relay-us.example.net"]);
        assert_eq!(c.pkarr_url(), "https://hub.example.net/dir/pkarr");
        assert!(c.supports_devices_wait());

        // EMBER_HUB_RELAY_URL wins over the hub's list; the directory still comes from the hub.
        let mut c = HubConfig::parse("https://hub.example.net", Some("https://my-relay.example.org")).unwrap();
        c.apply_info(info);
        assert_eq!(c.relay_source, RelaySource::Explicit);
        assert_eq!(c.relay_urls(), vec!["https://my-relay.example.org"]);
        assert_eq!(c.pkarr_url(), "https://hub.example.net/dir/pkarr");

        // An empty advertisement keeps the derived values.
        let mut c = HubConfig::new("https://hub.example.net");
        c.apply_info(HubInfo::default());
        assert_eq!(c.relay_url.as_deref(), Some("https://relay.hub.example.net"));
        assert_eq!(c.relay_source, RelaySource::Derived);
        assert_eq!(c.pkarr_url(), "https://hub.example.net/pkarr");
        assert!(!c.supports_devices_wait());
    }
}
