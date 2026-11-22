//! Pairing the app with a server through the darkpyonix.dev hub (SPEC `FR-N9`, `FR-N2`, #1044).
//!
//! 1. [`start`]: the app links its device key (`device.key`, the same key the transport uses)
//!    to the signed-in GitHub account as a `client` device and gets a user code to approve on
//!    the hub.
//! 2. [`finish`]: waits for the approval and keeps the registration in `<data dir>/hub.json`
//!    (mode 0600, [`registration_file`]).
//! 3. [`servers`]: the account's main servers.
//! 4. [`pair`]: the chosen server's peer id and the relay of its hub record, as [`Saved`], which
//!    [`Saved::apply`] writes into the prefs fields 설정 writes (`server_peer`, `server_relay`).
//!    Read at the next start, like every 설정 change.
//!
//! A device the hub removed reads [`REVOKED`] and pairs with nothing; the removal is recorded in
//! `hub.json`. The hub is `EMBER_HUB_URL` (default `https://api.darkpyonix.dev`). No UI here;
//! `ui/cr/hub_pair.rs` draws it.

use std::path::Path;

use ember_hub::pairing::{self, PairError};
use ember_hub::{DeviceLink, HubClient, HubConfig, HubError, LinkError, Registration, RegistrationFile, Role};
use ember_transport::{PeerId, SecretKey};

use crate::prefs::Prefs;

/// The hub is turned off (`EMBER_HUB_URL=off`).
pub const HUB_OFF: &str = "허브가 꺼져 있습니다 (EMBER_HUB_URL=off).";
/// The hub could not be reached.
pub const HUB_UNREACHABLE: &str = "허브에 연결하지 못했습니다. 네트워크를 확인하고 다시 시도합니다.";
/// The code was denied on the hub.
pub const DENIED: &str = "허브에서 이 기기의 연결이 거부되었습니다.";
/// The code expired before it was approved.
pub const EXPIRED: &str = "코드가 승인되기 전에 만료되었습니다. 다시 시작합니다.";
/// The key is already in the account, or was removed and not re-admitted.
pub const ALREADY: &str = "이 기기는 이미 허브 계정에 있거나, 계정에서 제거된 뒤 다시 허용되지 않았습니다. 허브에서 이 기기를 다시 허용한 다음 연결합니다.";
/// The registration was lost between the claim and saving it.
pub const LOST: &str = "허브 등록 정보를 잃었습니다. 허브에서 이 기기를 제거하고 다시 허용한 다음 연결합니다.";
/// The hub removed this device.
pub const REVOKED: &str = "이 기기는 허브 계정에서 제거되었습니다. 서버 목록을 볼 수 없습니다.";
/// No registration yet.
pub const NOT_REGISTERED: &str = "이 기기는 아직 허브에 등록되지 않았습니다. 먼저 GitHub 계정으로 연결합니다.";
/// The chosen device is not a main server of the account.
pub const NOT_A_SERVER: &str = "고른 기기는 이 계정의 서버가 아닙니다.";
/// `hub.json` could not be read or written.
pub const FILE_ERROR: &str = "허브 등록 파일을 읽거나 쓰지 못했습니다.";

/// `<data dir>/hub.json`, the app's registration (0600).
pub fn registration_file(data_dir: &Path) -> RegistrationFile {
    RegistrationFile::in_dir(data_dir)
}

/// The hub from the environment; `Err` when it is turned off.
pub fn hub_config() -> Result<HubConfig, &'static str> {
    HubConfig::from_env().ok_or(HUB_OFF)
}

/// The name the app registers under: the platform, since the hub's name is shown to the owner
/// next to the code and the app has no name of its own to offer.
pub fn device_name() -> String {
    format!("Ember 앱 ({})", std::env::consts::OS)
}

/// Starts linking `key` to the account as a `client` device. Show
/// [`DeviceLink::user_code`] and the verification URL, then call [`finish`].
pub async fn start(hub: &HubConfig, key: &SecretKey, name: &str) -> Result<DeviceLink, &'static str> {
    DeviceLink::start(&HubClient::new(&hub.url), key, name, Role::Client).await.map_err(|e| link_text(&e))
}

/// Waits until the code is approved, denied or expired, and saves the registration.
pub async fn finish(link: &DeviceLink, file: &RegistrationFile) -> Result<Registration, &'static str> {
    let reg = link.wait().await.map_err(|e| link_text(&e))?;
    file.save(&reg).map_err(|e| {
        tracing::warn!("saving the hub registration to {}: {e}", file.path().display());
        FILE_ERROR
    })?;
    Ok(reg)
}

/// One main server of the account, as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRow {
    pub peer: PeerId,
    pub name: String,
    pub online: bool,
}

/// The account's main servers.
pub async fn servers(file: &RegistrationFile) -> Result<Vec<ServerRow>, &'static str> {
    let reg = load(file)?;
    let listed = pairing::servers(&reg).await.map_err(|e| pair_text(file, &e))?;
    Ok(listed.into_iter().map(|d| ServerRow { peer: d.endpoint_id, name: d.name, online: d.online }).collect())
}

/// What pairing stores: the values of 설정's two fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The server's peer id (64 hex characters), from the hub.
    pub server_peer: String,
    /// The relay of the server's hub record, as `EMBER_RELAY_URL` takes it; `None` when the
    /// record names none.
    pub server_relay: Option<String>,
}

impl Saved {
    /// Writes the values into the prefs fields 설정 writes.
    pub fn apply(&self, prefs: &mut Prefs) {
        prefs.server_peer = Some(self.server_peer.clone());
        prefs.server_relay = self.server_relay.clone();
    }
}

/// Pairs with `server`: its peer id and the relay of its hub record.
pub async fn pair(file: &RegistrationFile, server: &PeerId) -> Result<Saved, &'static str> {
    let reg = load(file)?;
    let p = pairing::pair(&reg, server).await.map_err(|e| pair_text(file, &e))?;
    Ok(Saved { server_peer: p.server.to_string(), server_relay: p.relay })
}

/// The registration in `file`; a revoked one reads [`REVOKED`].
fn load(file: &RegistrationFile) -> Result<Registration, &'static str> {
    match file.load() {
        Ok(Some(reg)) if reg.is_revoked() => Err(REVOKED),
        Ok(Some(reg)) => Ok(reg),
        Ok(None) => Err(NOT_REGISTERED),
        Err(e) => {
            tracing::warn!("reading the hub registration {}: {e}", file.path().display());
            Err(FILE_ERROR)
        }
    }
}

fn link_text(e: &LinkError) -> &'static str {
    match e {
        LinkError::Denied => DENIED,
        LinkError::Expired(_) => EXPIRED,
        LinkError::AlreadyRegistered(_) => ALREADY,
        LinkError::ClaimedElsewhere => LOST,
        LinkError::Hub(h) => hub_text(h),
    }
}

/// The sentence for a pairing error; a revocation is recorded in `file` first.
fn pair_text(file: &RegistrationFile, e: &PairError) -> &'static str {
    match e {
        PairError::Revoked => {
            if let Err(err) = file.mark_revoked() {
                tracing::warn!("recording the hub revocation in {}: {err}", file.path().display());
            }
            REVOKED
        }
        PairError::NotAServer(_) => NOT_A_SERVER,
        PairError::Hub(h) => hub_text(h),
    }
}

fn hub_text(e: &HubError) -> &'static str {
    tracing::info!("hub call failed: {e}");
    HUB_UNREACHABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_n9_hub_pairing_sentences_are_korean() {
        for t in [HUB_OFF, HUB_UNREACHABLE, DENIED, EXPIRED, ALREADY, LOST, REVOKED, NOT_REGISTERED, NOT_A_SERVER, FILE_ERROR] {
            assert!(!t.is_ascii(), "{t}");
            assert!(!t.contains('\u{2014}'), "{t}");
        }
        let name = device_name();
        assert!(!name.is_empty() && name.chars().count() <= 64, "the hub takes 1 to 64 characters: {name}");
    }

    #[test]
    fn fr_n9_saved_values_go_into_the_settings_fields() {
        let peer = SecretKey::from_bytes([3; 32]).peer_id().to_string();
        let relay = format!("letify://{peer}@203.0.113.4:7443");
        let saved = Saved { server_peer: peer.clone(), server_relay: Some(relay.clone()) };
        let mut prefs = Prefs { server_relay: Some("letify://old".into()), ..Prefs::default() };
        saved.apply(&mut prefs);
        assert_eq!((prefs.server_peer.as_deref(), prefs.server_relay.as_deref()), (Some(peer.as_str()), Some(relay.as_str())));
        // What 설정 would save for the same two fields is the same.
        assert_eq!(crate::ui::cr::this_device::save_values(&peer, &relay), Ok(Some((peer.clone(), Some(relay)))));
        // A record with no relay clears an older typed relay, so the hub's server is not reached
        // through another server's relay.
        Saved { server_peer: peer, server_relay: None }.apply(&mut prefs);
        assert_eq!(prefs.server_relay, None);
    }
}
