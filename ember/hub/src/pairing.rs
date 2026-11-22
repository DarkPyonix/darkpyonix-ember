//! A client device pairing with its account's main server through the hub (SPEC `FR-N9`,
//! `FR-N2` Defaults, #1044).
//!
//! 1. The device links itself as a [`Role::Client`] ([`crate::DeviceLink::start`]); the person
//!    approves the code on the hub, or the main server approves it with its own token.
//! 2. [`servers`]: the account's main servers, read with the device token (`GET /devices`).
//! 3. [`pair`]: the chosen server's peer id and the `letify://` relay its hub record names
//!    (`GET /devices/{id}/addresses`), for the device to store instead of a typed address.
//!
//! A device the hub removed gets [`PairError::Revoked`] (`401 device_removed`) and pairs with
//! nothing. Whether the server admits the device is the server's business (its allow-list, or
//! its hub device sync, `FR-N3`); pairing does not change it.

use ember_transport::PeerId;

use crate::client::HubError;
use crate::registration::Registration;
use crate::types::{AddressRecord, Device, Role};

/// Why a device could not list or pair with a server.
#[derive(Debug, thiserror::Error)]
pub enum PairError {
    /// The hub removed this device from the account (or the registration says so already).
    #[error("this device was removed from the hub account")]
    Revoked,
    /// The chosen device is not one of the account's main servers.
    #[error("{} is not a main server of this account", .0.fmt_short())]
    NotAServer(PeerId),
    #[error(transparent)]
    Hub(HubError),
}

impl From<HubError> for PairError {
    fn from(e: HubError) -> Self {
        if e.is_revocation() {
            PairError::Revoked
        } else {
            PairError::Hub(e)
        }
    }
}

/// What a device stores to reach the server it paired with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    /// The server's peer id, as the hub lists it.
    pub server: PeerId,
    /// The server's name on the hub (display only).
    pub name: String,
    /// The `letify://` relays the server's record names, comma separated as `EMBER_RELAY_URL`
    /// takes them. `None` when the record names none (or nothing is published yet): the
    /// device's own relay setting then applies.
    pub relay: Option<String>,
}

/// The account's main servers, listed with `reg`'s device token.
pub async fn servers(reg: &Registration) -> Result<Vec<Device>, PairError> {
    if reg.is_revoked() {
        return Err(PairError::Revoked);
    }
    let devices = reg.client().devices().await?;
    Ok(devices.into_iter().filter(|d| d.role == Role::MainServer && d.endpoint_id != reg.endpoint_id()).collect())
}

/// Pairs with `server`, one of [`servers`]: its peer id and the relay its hub record names.
pub async fn pair(reg: &Registration, server: &PeerId) -> Result<Pairing, PairError> {
    let found = servers(reg).await?.into_iter().find(|d| d.endpoint_id == *server);
    let Some(device) = found else {
        return Err(PairError::NotAServer(*server));
    };
    let relays = match reg.client().addresses(server).await {
        Ok(rec) if rec.endpoint_id == *server => letify_relays(Some(&rec)),
        Ok(rec) => {
            tracing::warn!(asked = %server.fmt_short(), got = %rec.endpoint_id.fmt_short(), "the hub answered another device's record; ignored");
            Vec::new()
        }
        // Nothing published yet: the server's peer id alone, the relay left to the device.
        Err(HubError::NotFound(_)) => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    Ok(Pairing { server: *server, name: device.name, relay: (!relays.is_empty()).then(|| relays.join(",")) })
}

/// The `letify://` relays a main server's record names (INTENT D36: the main server is the
/// relay). Relays of other kinds (an https relay of the legacy backend) are left out.
pub fn letify_relays(record: Option<&AddressRecord>) -> Vec<String> {
    record.map(|r| r.relay_urls.iter().filter(|u| u.starts_with("letify://")).cloned().collect()).unwrap_or_default()
}
