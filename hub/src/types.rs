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
    /// Any other device (an ember node; for now also an ember client, see the spec gaps in
    /// `docs/design/HUB-INTEGRATION.md`).
    Computer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::MainServer => "main_server",
            Role::Computer => "computer",
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
}

/// Body of `POST /v1/device-links`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRequest {
    pub endpoint_id: PeerId,
    pub name: String,
    pub role: Role,
}

/// `201` of `POST /v1/device-links`.
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

/// Result of one `POST /v1/device-links/{link_id}/token`.
#[derive(Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// `202`: not decided yet.
    Pending,
    /// `201`: approved; the token is shown exactly once.
    Approved { device: Device, device_token: String },
}

impl fmt::Debug for ClaimOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClaimOutcome::Pending => f.write_str("Pending"),
            ClaimOutcome::Approved { device, .. } => {
                f.debug_struct("Approved").field("device", device).field("device_token", &"<redacted>").finish()
            }
        }
    }
}

/// `200` of `GET /v1/link-codes/{user_code}`: what approving would let in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCodeInfo {
    pub user_code: String,
    pub endpoint_id: PeerId,
    pub name: String,
    pub role: Role,
    pub expires_at: i64,
}

/// `200` of `GET /v1/me`.
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
