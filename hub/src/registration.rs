//! A device's registration with the hub, where it is kept, and noticing its removal.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ember_transport::PeerId;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::client::{HubClient, HubError};
use crate::types::{Device, Me};

/// File name of ember node's registration in its state dir.
pub const REGISTRATION_FILE: &str = "hub.json";

/// Current Unix time in seconds.
pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// This device's membership in a hub account.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub hub_url: String,
    pub device: Device,
    /// `dpd_...`. Never logged (redacting `Debug`).
    pub device_token: String,
    pub registered_at: i64,
    /// Set when the hub was found to have removed the device ([`check_registration`]). A
    /// revoked registration is kept (so it can be shown) but its token is not used.
    #[serde(default)]
    pub revoked_at: Option<i64>,
}

impl Registration {
    pub fn endpoint_id(&self) -> PeerId {
        self.device.endpoint_id
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// A client for this registration's hub carrying its token.
    pub fn client(&self) -> HubClient {
        HubClient::new(&self.hub_url).with_token(&self.device_token)
    }

    /// The token, unless revoked.
    pub fn active_token(&self) -> Option<String> {
        (!self.is_revoked()).then(|| self.device_token.clone())
    }
}

impl fmt::Debug for Registration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registration")
            .field("hub_url", &self.hub_url)
            .field("device", &self.device)
            .field("device_token", &"<redacted>")
            .field("registered_at", &self.registered_at)
            .field("revoked_at", &self.revoked_at)
            .finish()
    }
}

/// A registration kept in a file (ember node: `<state dir>/hub.json`), mode 0600, replaced
/// atomically.
#[derive(Debug, Clone)]
pub struct RegistrationFile {
    path: PathBuf,
}

impl RegistrationFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<state dir>/hub.json`.
    pub fn in_dir(state_dir: &Path) -> Self {
        Self::new(state_dir.join(REGISTRATION_FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored registration; `None` if there is none. A file readable by group or others is
    /// tightened to 0600 first.
    pub fn load(&self) -> std::io::Result<Option<Registration>> {
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&self.path)?.permissions().mode();
            if mode & 0o077 != 0 {
                tracing::warn!(path = %self.path.display(), "hub registration was readable by others; setting mode 0600");
                std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        serde_json::from_slice(&data).map(Some).map_err(std::io::Error::other)
    }

    /// Writes `reg` (0600, through a temporary file and a rename).
    pub fn save(&self, reg: &Registration) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(reg).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &self.path)
    }

    /// Records that the hub removed the device (keeps the file, sets `revoked_at`).
    pub fn mark_revoked(&self) -> std::io::Result<Option<Registration>> {
        let Some(mut reg) = self.load()? else { return Ok(None) };
        if reg.revoked_at.is_none() {
            reg.revoked_at = Some(now_secs());
            self.save(&reg)?;
        }
        Ok(Some(reg))
    }

    /// Forgets the registration (local only; the hub still lists the device until it is removed
    /// there).
    pub fn remove(&self) -> std::io::Result<bool> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// What the hub says about this device's registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationState {
    /// Not checked yet.
    Unknown,
    /// The token works.
    Active(Me),
    /// The hub refuses the token: the device was removed from the account (tokens do not
    /// expire, so a token that worked before and now gets `401` was revoked).
    Revoked,
    /// The hub could not be asked (network, 5xx); the last known state still holds.
    Unreachable(String),
}

impl RegistrationState {
    pub fn is_revoked(&self) -> bool {
        matches!(self, RegistrationState::Revoked)
    }
}

/// Asks the hub whether `client`'s token still works (`GET /v1/me`).
pub async fn check_registration(client: &HubClient) -> RegistrationState {
    match client.me().await {
        Ok(me) => RegistrationState::Active(me),
        Err(HubError::Unauthorized(_)) => RegistrationState::Revoked,
        Err(e) => RegistrationState::Unreachable(e.to_string()),
    }
}

/// Checks the registration now and then every `period`; the receiver sees each change. Stops
/// after a revocation is seen (it is final: removed keys are not reused) or when every receiver
/// is dropped.
pub fn watch_registration(
    client: HubClient,
    period: Duration,
) -> (watch::Receiver<RegistrationState>, JoinHandle<()>) {
    let (tx, rx) = watch::channel(RegistrationState::Unknown);
    let task = tokio::spawn(async move {
        loop {
            let state = check_registration(&client).await;
            let revoked = state.is_revoked();
            if revoked {
                tracing::warn!(hub = %client.base_url(), "the hub removed this device; its hub token no longer works");
            }
            tx.send_if_modified(|old| {
                // An unreachable hub does not replace a known state's kind with noise on every
                // tick, but the first one (or a change of message) is reported.
                if *old == state {
                    false
                } else {
                    *old = state;
                    true
                }
            });
            if revoked || tx.is_closed() {
                break;
            }
            tokio::time::sleep(period).await;
        }
    });
    (rx, task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;
    use ember_transport::SecretKey;

    #[test]
    fn file_round_trip_is_private_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let file = RegistrationFile::in_dir(&dir.path().join("state"));
        assert!(file.load().unwrap().is_none());
        let reg = Registration {
            hub_url: "https://darkpyonix.dev".into(),
            device: Device {
                endpoint_id: SecretKey::generate().peer_id(),
                name: "mini".into(),
                role: Role::Computer,
                created_at: 1,
                last_seen: None,
                online: false,
            },
            device_token: "dpd_secret".into(),
            registered_at: 2,
            revoked_at: None,
        };
        assert!(!format!("{reg:?}").contains("dpd_secret"));
        file.save(&reg).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(file.load().unwrap().unwrap(), reg);
        let revoked = file.mark_revoked().unwrap().unwrap();
        assert!(revoked.is_revoked() && revoked.active_token().is_none());
        assert!(file.load().unwrap().unwrap().is_revoked());
        assert!(file.remove().unwrap());
        assert!(file.load().unwrap().is_none());
    }
}
