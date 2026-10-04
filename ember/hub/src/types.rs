//! Wire types of the hub API (`hub.openapi.yaml` components and response bodies).

use std::fmt;

use ember_transport::PeerId;
use serde::{Deserialize, Serialize};

/// A device's role in its account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The account's ember server. Has account rights: may approve link codes and remove
    /// devices with its device token.
    MainServer,
    /// Runs work (an ember node). May publish shares; no account rights.
    Computer,
    /// Only connects to the account's other devices (a phone, a laptop running the Ember
    /// client): list devices, publish and resolve addresses, use the relay. No account rights,
    /// no names, no shares. Provisional in the hub contract (FR-H1).
    Client,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::MainServer => "main_server",
            Role::Computer => "computer",
            Role::Client => "client",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `components.schemas.Device`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// The device's endpoint id: its transport peer id.
    pub endpoint_id: PeerId,
    pub name: String,
    pub role: Role,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds of the last relay admission, presence report or address publish.
    #[serde(default)]
    pub last_seen: Option<i64>,
    /// Connected to the relay host now, as it last reported.
    #[serde(default)]
    pub online: bool,
    /// What the device says it runs (FR-H10); a hint only.
    #[serde(default)]
    pub app: Option<DeviceApp>,
}

/// Body of `POST /device-links`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRequest {
    pub endpoint_id: PeerId,
    pub name: String,
    pub role: Role,
}

/// `201` of `POST /device-links`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingLink {
    pub link_id: String,
    /// `BCDF-GHJK`: shown to the person, who approves it on the hub (or in ember server).
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    /// Signed with the endpoint key on every poll (proof of possession).
    pub challenge: String,
    /// Seconds between polls.
    pub interval: u64,
    /// Unix seconds.
    pub expires_at: i64,
}

/// Result of one `POST /device-links/{link_id}/token`.
#[derive(Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// `202`: not decided yet.
    Pending,
    /// `201`: approved; the tokens are shown exactly once. `resolve_token` (`dpr_...`) is the
    /// read-only token for `GET /pkarr/{key}?token=` (NFR-H2); `None` from a hub older than it.
    Approved { device: Device, device_token: String, resolve_token: Option<String> },
}

impl fmt::Debug for ClaimOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClaimOutcome::Pending => f.write_str("Pending"),
            ClaimOutcome::Approved { device, .. } => {
                f.debug_struct("Approved")
                    .field("device", device)
                    .field("device_token", &"<redacted>")
                    .field("resolve_token", &"<redacted>")
                    .finish()
            }
        }
    }
}

/// `200` of `GET /link-codes/{user_code}`: what approving would let in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCodeInfo {
    pub user_code: String,
    pub endpoint_id: PeerId,
    pub name: String,
    pub role: Role,
    pub expires_at: i64,
}

/// `200` of `GET /me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub account_id: String,
    pub github_login: String,
    /// `session` or `device`.
    pub via: String,
    #[serde(default)]
    pub endpoint_id: Option<PeerId>,
}

/// `components.schemas.AddressRecord`: a device's latest signed address record, decoded by the
/// hub. Informational (hints, display); the transport resolves through the signed packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressRecord {
    pub endpoint_id: PeerId,
    #[serde(default)]
    pub relay_urls: Vec<String>,
    /// `ip:port` strings.
    #[serde(default)]
    pub direct_addresses: Vec<String>,
    pub published_at_us: i64,
    /// The pkarr relay payload, base64url without padding.
    pub signed_packet: String,
}

impl AddressRecord {
    /// As a transport address (unparsable direct addresses are skipped).
    pub fn to_peer_addr(&self) -> ember_transport::PeerAddr {
        ember_transport::PeerAddr {
            peer: self.endpoint_id,
            relays: self.relay_urls.clone(),
            direct: self.direct_addresses.iter().filter_map(|a| a.parse().ok()).collect(),
        }
    }
}

/// `200` of `GET /config` (public, cacheable for 5 minutes, FR-H8). Fields the hub may add
/// later are ignored; every field is optional here so a partial answer still decodes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubInfo {
    /// The hub deployment's own version (display only).
    #[serde(default)]
    pub hub_version: Option<String>,
    /// P2P relays for the transport (`https://relay.darkpyonix.dev/`).
    #[serde(default)]
    pub relay_urls: Vec<String>,
    /// Base URL for the pkarr publisher and resolver (`https://darkpyonix.dev/pkarr`).
    #[serde(default)]
    pub pkarr_url: Option<String>,
    /// Where a person approves user codes (`https://darkpyonix.dev/link`).
    #[serde(default)]
    pub link_url: Option<String>,
}

impl HubInfo {
    /// Whether `GET /devices` may be long-polled. The API only grows (core INTENT D16) and the
    /// spec documents `wait` on `GET /devices` wherever `/config` is served, so a hub that
    /// answered `/config` is asked; the watcher falls back to polling if `wait` is ignored.
    pub fn supports_devices_wait(&self) -> bool {
        true
    }
}

/// `components.schemas.DeviceApp` (FR-H10, provisional): what a device says it runs. Only the
/// device itself may set it (`PATCH /devices/{id}` with its own token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceApp {
    /// `^[a-z][a-z0-9-]{0,31}$`: `ember-server`, `ember-node`.
    pub kind: String,
    /// `^[0-9A-Za-z][0-9A-Za-z.+-]{0,31}$`.
    pub version: String,
    /// Up to 16 unique labels, each `^[a-z][a-z0-9-]{0,31}$`.
    #[serde(default)]
    pub services: Vec<String>,
}

impl DeviceApp {
    /// An app record; `services` are transport service names (`ember-node/1`) turned into the
    /// hub's label form ([`service_label`]).
    pub fn new(kind: &str, version: &str, services: &[&str]) -> Self {
        let mut labels: Vec<String> = Vec::new();
        for s in services {
            let l = service_label(s);
            if !l.is_empty() && !labels.contains(&l) && labels.len() < 16 {
                labels.push(l);
            }
        }
        let version: String = version
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-') { c } else { '-' })
            .take(32)
            .collect();
        Self { kind: service_label(kind), version, services: labels }
    }
}

/// A name in the hub's label form `^[a-z][a-z0-9-]{0,31}$`: lowercased, `/` → `-v`
/// (`ember-node/1` → `ember-node-v1`), other characters → `-`, trimmed to 32. Empty if no
/// letter to start with.
pub fn service_label(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_ascii_lowercase().chars() {
        match c {
            'a'..='z' | '0'..='9' | '-' => out.push(c),
            '/' => out.push_str("-v"),
            _ => out.push('-'),
        }
    }
    let out = out.trim_start_matches(|c: char| !c.is_ascii_lowercase());
    out.chars().take(32).collect::<String>().trim_end_matches('-').to_string()
}

/// `status` of `GET /device-links/{link_id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    Pending,
    Approved,
    Denied,
    Claimed,
    Expired,
}

/// `200` of `GET /device-links/{link_id}` (no credentials): a link's state, for a device
/// that restarted while waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkInfo {
    pub link_id: String,
    pub status: LinkStatus,
    pub endpoint_id: PeerId,
    pub name: String,
    pub role: Role,
    pub user_code: String,
    pub verification_uri_complete: String,
    pub challenge: String,
    pub interval: u64,
    pub expires_at: i64,
}

impl LinkInfo {
    /// As the pending link a device keeps polling.
    pub fn to_pending(&self) -> PendingLink {
        let verification_uri =
            self.verification_uri_complete.split('?').next().unwrap_or(&self.verification_uri_complete).to_string();
        PendingLink {
            link_id: self.link_id.clone(),
            user_code: self.user_code.clone(),
            verification_uri,
            verification_uri_complete: self.verification_uri_complete.clone(),
            challenge: self.challenge.clone(),
            interval: self.interval,
            expires_at: self.expires_at,
        }
    }
}

/// `200` of `POST /devices/{endpoint_id}/readmit` (a signed-in session only, FR-H11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Readmission {
    pub endpoint_id: PeerId,
    /// Unix seconds; the removed key must start its device link before this.
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_info_decodes_and_ignores_unknown_fields() {
        let i: HubInfo = serde_json::from_str(
            r#"{"hub_version":"2026.10.3","relay_urls":["https://relay.x/"],"pkarr_url":"https://x/pkarr","link_url":"https://x/link","extra":true}"#,
        )
        .unwrap();
        assert!(i.supports_devices_wait());
        assert_eq!(i.hub_version.as_deref(), Some("2026.10.3"));
        assert_eq!(i.link_url.as_deref(), Some("https://x/link"));
        let i: HubInfo = serde_json::from_str(r#"{"relay_urls":[]}"#).unwrap();
        assert!(i.supports_devices_wait(), "decided by /config being served, not by a version");
    }

    #[test]
    fn app_labels_follow_the_hub_patterns() {
        assert_eq!(service_label("ember-node/1"), "ember-node-v1");
        assert_eq!(service_label("Ember Server"), "ember-server");
        assert_eq!(service_label("1abc"), "abc");
        let app = DeviceApp::new("ember-node", "0.1.0+dirty", &["ember-node/1", "ember-node/1"]);
        assert_eq!(app.services, vec!["ember-node-v1"]);
        assert_eq!(app.version, "0.1.0+dirty");
        assert_eq!(serde_json::to_value(Role::Client).unwrap(), "client");
    }
}
