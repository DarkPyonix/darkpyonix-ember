//! The app on the darkpyonix.dev hub (SPEC `FR-N9`, `FR-N2`, INTENT D36, #1044): register this
//! device's key as a `client` device and pair with one of the account's main servers, so the
//! owner does not copy a peer id or a relay by hand.
//!
//! - **Registration**: the device-link flow (`docs/design/HUB-INTEGRATION.md`) with
//!   [`Role::Client`] and the app's device key (`<data dir>/device.key`). [`start`] returns the
//!   link, whose user code and approval URL 설정 shows ([`Approval`]); [`finish`] polls until the
//!   person approves it and keeps the registration in `<data dir>/hub.json` (mode 0600,
//!   [`RegistrationFile`]).
//! - **Servers**: [`main_servers`] lists the account's `main_server` devices with the relay each
//!   one names in its signed address record on the hub.
//! - **Pairing**: [`pair`] gives the values 설정 saves ([`Pairing::apply`]): the server address is
//!   the hub record's peer id, and the relay is the `letify://` relay from the server's record.
//! - **Removal**: a registration the hub removed (`401 device_removed`) is recorded in
//!   `hub.json` (`revoked_at`) and cannot pair ([`HubPairError::Removed`]).
//!
//! The hand-typed address in 설정 stays beside this path (`ui/cr/this_device.rs`). The app's
//! transport does not use the hub directory yet: it reaches the paired server through the saved
//! peer id and relay, as with a typed address.

use std::path::Path;

use ember_hub::{
    AddressRecord, DeviceLink as HubLink, HubClient, HubConfig, HubError, LinkError, Registration, RegistrationFile,
    Role,
};
use ember_transport::{PeerId, SecretKey};

use crate::prefs::Prefs;

/// The name this device registers under on the hub (1 to 64 characters).
pub const DEVICE_NAME: &str = "Ember app";

/// Korean lines 설정 shows.
pub const REMOVED: &str = "허브에서 이 기기가 제거되었습니다. 계정 소유자가 허브에서 이 기기를 다시 허용한 뒤 다시 등록합니다.";
pub const NOT_REGISTERED: &str = "이 기기는 아직 허브에 등록되어 있지 않습니다.";
pub const HUB_OFF: &str = "허브가 꺼져 있습니다 (EMBER_HUB_URL=off).";
pub const NO_RELAY: &str = "허브의 서버 기록에 릴레이 주소가 아직 없습니다. 서버의 릴레이가 켜져 있고 서버가 허브에 등록되어 있는지 확인합니다.";
pub const NOT_A_SERVER: &str = "이 기기는 계정의 메인 서버가 아닙니다.";
pub const DENIED: &str = "허브에서 등록이 거부되었습니다.";
pub const EXPIRED: &str = "승인되기 전에 등록 코드가 만료되었습니다. 다시 등록합니다.";
pub const ALREADY: &str = "이 기기 키는 이미 허브에 등록되어 있거나 제거되었습니다. 제거된 기기는 계정 소유자가 허브에서 다시 허용해야 합니다.";
pub const HUB_UNREACHABLE: &str = "허브에 연결하지 못했습니다. 네트워크를 확인합니다.";
pub const SAVE_FAILED: &str = "허브 등록 정보를 저장하지 못했습니다.";

/// Why registering or pairing did not work.
#[derive(Debug, thiserror::Error)]
pub enum HubPairError {
    /// The hub removed this device (`401 device_removed`), now or earlier (`revoked_at`).
    #[error("the hub removed this device from the account")]
    Removed,
    #[error("this device is not registered with the hub")]
    NotRegistered,
    #[error("the peer {0} is not a main server of the account")]
    NotAServer(PeerId),
    #[error("the hub's record of {0} names no letify relay")]
    NoRelay(PeerId),
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(transparent)]
    Hub(HubError),
    #[error("hub.json: {0}")]
    Io(#[from] std::io::Error),
}

impl HubPairError {
    /// The Korean line 설정 shows.
    pub fn text_ko(&self) -> &'static str {
        match self {
            HubPairError::Removed => REMOVED,
            HubPairError::NotRegistered => NOT_REGISTERED,
            HubPairError::NotAServer(_) => NOT_A_SERVER,
            HubPairError::NoRelay(_) => NO_RELAY,
            HubPairError::Link(LinkError::Denied) => DENIED,
            HubPairError::Link(LinkError::Expired(_)) => EXPIRED,
            HubPairError::Link(LinkError::AlreadyRegistered(_) | LinkError::ClaimedElsewhere) => ALREADY,
            HubPairError::Link(LinkError::Hub(e)) | HubPairError::Hub(e) if e.is_revocation() => REMOVED,
            HubPairError::Link(LinkError::Hub(_)) | HubPairError::Hub(_) => HUB_UNREACHABLE,
            HubPairError::Io(_) => SAVE_FAILED,
        }
    }
}

/// The hub this app uses: `EMBER_HUB_URL`, default `https://api.darkpyonix.dev`; `None` when it
/// is turned off.
pub fn hub_from_env() -> Option<HubClient> {
    HubConfig::from_env().map(|c| HubClient::new(&c.url))
}

/// `<data dir>/hub.json`.
pub fn registration_file(data_dir: &Path) -> RegistrationFile {
    RegistrationFile::in_dir(data_dir)
}

/// The stored registration, if any (a file that cannot be read counts as none, with a log line).
pub fn load(data_dir: &Path) -> Option<Registration> {
    match registration_file(data_dir).load() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("hub registration in {}: {e}", data_dir.display());
            None
        }
    }
}

/// What the person approves: the user code and where to enter it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub user_code: String,
    /// The approval URL with the code filled in (`verification_uri_complete`).
    pub url: String,
}

impl Approval {
    pub fn of(link: &HubLink) -> Approval {
        let p = link.pending();
        Approval { user_code: p.user_code.clone(), url: p.verification_uri_complete.clone() }
    }
}

/// Starts registering `key` with the hub as a `client` device named `name`.
pub async fn start(hub: &HubClient, key: &SecretKey, name: &str) -> Result<HubLink, HubPairError> {
    Ok(HubLink::start(hub, key, name, Role::Client).await?)
}

/// Waits until the person approves the link, then keeps the registration in `file` (0600).
pub async fn finish(link: &HubLink, file: &RegistrationFile) -> Result<Registration, HubPairError> {
    let reg = link.wait().await?;
    file.save(&reg)?;
    Ok(reg)
}

/// A main server of the account, as the hub lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainServer {
    pub peer: PeerId,
    pub name: String,
    pub online: bool,
    /// The `letify://` relay its signed address record names; `None` when it published none yet.
    pub relay: Option<String>,
}

/// The values 설정 saves for a paired server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    /// The hub record's peer id (64 hex characters): the saved server address.
    pub peer: String,
    /// The `letify://` relay from the server's hub record: the saved relay.
    pub relay: String,
}

impl Pairing {
    /// Saves the pairing as the server address and relay (`Prefs::server_peer`,
    /// `Prefs::server_relay`), read at the next start like a typed address.
    pub fn apply(&self, prefs: &mut Prefs) {
        prefs.server_peer = Some(self.peer.clone());
        prefs.server_relay = Some(self.relay.clone());
    }
}

/// The first `letify://` relay of a record.
fn relay_of(record: &AddressRecord) -> Option<String> {
    record.letify_relays().into_iter().next()
}

/// A call made with the device token failed: a revocation is recorded in `file` and reads as
/// [`HubPairError::Removed`].
fn on_hub_error(e: HubError, file: Option<&RegistrationFile>) -> HubPairError {
    if e.is_revocation() {
        if let Some(f) = file {
            if let Err(io) = f.mark_revoked() {
                tracing::warn!("recording the hub removal in {}: {io}", f.path().display());
            }
        }
        HubPairError::Removed
    } else {
        HubPairError::Hub(e)
    }
}

fn client_of(reg: &Registration) -> Result<HubClient, HubPairError> {
    if reg.is_revoked() {
        return Err(HubPairError::Removed);
    }
    Ok(reg.client())
}

async fn relay_from_hub(client: &HubClient, peer: &PeerId, file: Option<&RegistrationFile>) -> Result<Option<String>, HubPairError> {
    match client.addresses(peer).await {
        Ok(rec) => Ok(relay_of(&rec)),
        // Nothing published yet.
        Err(HubError::NotFound(_)) => Ok(None),
        Err(e) => Err(on_hub_error(e, file)),
    }
}

/// The account's main servers, with the relay each names on the hub. `file` records a removal.
pub async fn main_servers(reg: &Registration, file: Option<&RegistrationFile>) -> Result<Vec<MainServer>, HubPairError> {
    let client = client_of(reg)?;
    let devices = client.devices().await.map_err(|e| on_hub_error(e, file))?;
    let mut out = Vec::new();
    for d in devices.into_iter().filter(|d| d.role == Role::MainServer) {
        let relay = relay_from_hub(&client, &d.endpoint_id, file).await?;
        out.push(MainServer { peer: d.endpoint_id, name: d.name, online: d.online, relay });
    }
    Ok(out)
}

/// Pairs with the main server `server`: asks the hub again (a removed device is refused, and a
/// peer that is not a main server of the account too) and returns the hub record's peer id and
/// the `letify://` relay its record names.
pub async fn pair(reg: &Registration, file: Option<&RegistrationFile>, server: &PeerId) -> Result<Pairing, HubPairError> {
    let client = client_of(reg)?;
    let devices = client.devices().await.map_err(|e| on_hub_error(e, file))?;
    let Some(d) = devices.iter().find(|d| d.endpoint_id == *server && d.role == Role::MainServer) else {
        return Err(HubPairError::NotAServer(*server));
    };
    let relay = relay_from_hub(&client, &d.endpoint_id, file).await?.ok_or(HubPairError::NoRelay(*server))?;
    Ok(Pairing { peer: d.endpoint_id.to_string(), relay })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_n9_hub_errors_read_as_korean_sentences() {
        assert_eq!(HubPairError::Removed.text_ko(), REMOVED);
        assert_eq!(HubPairError::Hub(HubError::DeviceRemoved("x".into())).text_ko(), REMOVED);
        assert_eq!(HubPairError::Hub(HubError::Unreachable("x".into())).text_ko(), HUB_UNREACHABLE);
        assert_eq!(HubPairError::Link(LinkError::Denied).text_ko(), DENIED);
        let p = SecretKey::from_bytes([3; 32]).peer_id();
        assert_eq!(HubPairError::NoRelay(p).text_ko(), NO_RELAY);
        for t in [REMOVED, NOT_REGISTERED, NO_RELAY, NOT_A_SERVER, DENIED, EXPIRED, ALREADY, HUB_UNREACHABLE] {
            assert!(t.ends_with("다."), "{t}");
        }
    }

    #[test]
    fn fr_n9_a_pairing_is_saved_as_the_server_address_and_relay() {
        let peer = SecretKey::from_bytes([4; 32]).peer_id().to_string();
        let relay = format!("letify://{peer}@203.0.113.9:7443");
        let mut prefs = Prefs::default();
        Pairing { peer: peer.clone(), relay: relay.clone() }.apply(&mut prefs);
        assert_eq!((prefs.server_peer.as_deref(), prefs.server_relay.as_deref()), (Some(peer.as_str()), Some(relay.as_str())));
        // The saved values parse as the typed ones do.
        let setting = crate::device_link::PeerSetting::parse(&peer, Some(&relay)).unwrap();
        assert_eq!(setting.addr.peer.to_string(), peer);
    }
}
