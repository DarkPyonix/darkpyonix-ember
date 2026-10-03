//! ember server and the darkpyonix.dev hub (SPEC `FR-N2`; contract: `hub.openapi.yaml` v0.3.0,
//! design: `docs/design/HUB-INTEGRATION.md`).
//!
//! - **Registration.** [`ServerHub::start_link`] asks the hub to let this server's transport
//!   identity join the user's GitHub account as its `main_server`; the person approves the user
//!   code in a browser; a background task polls, then stores the device token **sealed** with
//!   the server's `secret.key` (the FR-U5 scheme, AAD `ember/hub-device-token/v1:<endpoint>`) in
//!   `hub_registration` (store migration 8), and switches the running transport to the hub's
//!   directory and relay.
//! - **Revocation.** [`ServerHub::check`] (and the watcher from [`ServerHub::spawn_watch`]) asks
//!   `GET /v1/me`; a `401` means the hub removed this server. That is recorded (`revoked_at`),
//!   shown in [`HubStatus`], logged, and the token is no longer used.
//! - **Computers from the hub.** [`ServerHub::add_computer`] registers one of the account's
//!   devices as a computer by its endpoint id alone: no `PeerAddr` is pasted, the transport
//!   resolves it through the hub directory.
//! - **Approving other devices.** As the account's main server it may look up and approve a
//!   user code ([`ServerHub::decide_code`]): a node shows its code, the user types it into Ember.
//! - **Devices allow-list sync (opt-in).** [`ServerHub::sync_devices`] makes the `hub` rows of
//!   the devices table (FR-N3) equal to the account's device list.
//!
//! The router ([`api`]) is local only (TCP), like device management.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ember_hub::{
    check_registration, Device, DeviceLink, HubClient, HubConfig, HubError, LinkCodeInfo, LinkError, PendingLink,
    Registration, RegistrationState, Role,
};
use ember_transport::{PeerAddr, PeerId, SecretKey, Transport};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::accounts::Accounts;
use crate::computers::{Computer, ComputerError, Computers};
use crate::devices::{DeviceError, Devices, SyncReport};
use crate::store::Store;

pub mod api;
pub mod schema;

/// `1` turns on the periodic devices allow-list sync from the hub account (default off).
pub const SYNC_DEVICES_ENV: &str = "EMBER_HUB_SYNC_DEVICES";
/// How often the registration (and, if on, the device list) is checked.
pub const WATCH_PERIOD: Duration = Duration::from_secs(60);

const AAD_PREFIX: &str = "ember/hub-device-token/v1:";

#[derive(Debug, thiserror::Error)]
pub enum HubApiError {
    #[error("this server is not registered with the hub; start with POST /api/v1/hub/link")]
    NotRegistered,
    #[error("the hub removed this server (revoked at {0}); its key cannot rejoin: remove transport.key and the registration, restart, and register again")]
    Revoked(i64),
    #[error("this server is already registered with the hub as {0:?}")]
    AlreadyRegistered(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Link(LinkError),
    #[error(transparent)]
    Hub(HubError),
    #[error(transparent)]
    Computer(#[from] ComputerError),
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for HubApiError {
    fn from(e: rusqlite::Error) -> Self {
        HubApiError::Other(e.into())
    }
}

/// A pending link, as shown to the user.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PendingView {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_at: i64,
}

impl From<&PendingLink> for PendingView {
    fn from(p: &PendingLink) -> Self {
        PendingView {
            user_code: p.user_code.clone(),
            verification_uri: p.verification_uri.clone(),
            verification_uri_complete: p.verification_uri_complete.clone(),
            expires_at: p.expires_at,
        }
    }
}

/// `GET /api/v1/hub`.
#[derive(Debug, Clone, Serialize)]
pub struct HubStatus {
    pub hub_url: String,
    pub relay_url: Option<String>,
    /// This server's transport identity (its endpoint id on the hub).
    pub endpoint_id: PeerId,
    pub registered: bool,
    pub device: Option<Device>,
    pub registered_at: Option<i64>,
    pub revoked: bool,
    pub revoked_at: Option<i64>,
    pub pending: Option<PendingView>,
    /// Why the last link attempt failed.
    pub last_error: Option<String>,
    pub sync_devices: bool,
}

/// One of the account's devices, as `GET /api/v1/hub/devices` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct HubDeviceView {
    #[serde(flatten)]
    pub device: Device,
    /// This server itself.
    pub this_server: bool,
    /// The computer registered for this device, if any.
    pub computer_id: Option<String>,
    /// On this server's devices allow-list (FR-N3).
    pub allowed: bool,
}

#[derive(Default)]
struct LinkState {
    pending: Option<PendingView>,
    task: Option<JoinHandle<()>>,
    last_error: Option<String>,
}

/// The server's hub integration. Shared as `Arc`.
pub struct ServerHub {
    config: HubConfig,
    store: Arc<Store>,
    accounts: Arc<Accounts>,
    key: SecretKey,
    transport: OnceLock<Transport>,
    devices: Arc<Devices>,
    sync_devices: AtomicBool,
    link: Mutex<LinkState>,
    poll_interval: Option<Duration>,
}

impl ServerHub {
    /// `key` is the server's transport key (its identity on the hub). Attach the bound
    /// transport with [`ServerHub::attach_transport`] (it needs [`ServerHub::active_token`] to be
    /// bound first).
    pub fn new(
        config: HubConfig,
        store: Arc<Store>,
        accounts: Arc<Accounts>,
        key: SecretKey,
        devices: Arc<Devices>,
    ) -> Arc<ServerHub> {
        let sync = matches!(std::env::var(SYNC_DEVICES_ENV).as_deref(), Ok("1" | "on" | "true" | "yes"));
        Arc::new(ServerHub {
            config,
            store,
            accounts,
            key,
            transport: OnceLock::new(),
            devices,
            sync_devices: AtomicBool::new(sync),
            link: Mutex::new(LinkState::default()),
            poll_interval: None,
        })
    }

    /// Polls links at this interval instead of the hub's (tests). Before sharing.
    pub fn set_poll_interval(self: &mut Arc<Self>, interval: Duration) {
        Arc::get_mut(self).expect("set_poll_interval before sharing").poll_interval = Some(interval);
    }

    /// The server's transport: switched to the hub's directory and relay when registration
    /// completes, and warmed with address hints. Set once.
    pub fn attach_transport(&self, transport: Transport) {
        if self.transport.set(transport).is_err() {
            tracing::warn!("hub: a transport was already attached");
        }
    }

    pub fn config(&self) -> &HubConfig {
        &self.config
    }

    pub fn peer_id(&self) -> PeerId {
        self.key.peer_id()
    }

    // ------------------------------------------------------------------ stored registration

    fn aad(&self, endpoint: &PeerId) -> Vec<u8> {
        format!("{AAD_PREFIX}{endpoint}").into_bytes()
    }

    /// The stored registration (token unsealed), if any.
    pub fn registration(&self) -> Result<Option<Registration>, HubApiError> {
        let row = self
            .store
            .conn()
            .query_row(
                "SELECT hub_url, endpoint_id, device_json, token_nonce, token_ciphertext, registered_at, revoked_at
                 FROM hub_registration WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((hub_url, endpoint, device_json, nonce, ct, registered_at, revoked_at)) = row else {
            return Ok(None);
        };
        let endpoint: PeerId = endpoint.parse().map_err(|e| anyhow::anyhow!("hub_registration endpoint: {e}"))?;
        let device: Device = serde_json::from_str(&device_json).map_err(anyhow::Error::from)?;
        let token = self.accounts.secrets().open(&nonce, &ct, &self.aad(&endpoint))?;
        let device_token = String::from_utf8(token.to_vec()).map_err(|_| anyhow::anyhow!("hub token is not UTF-8"))?;
        Ok(Some(Registration { hub_url, device, device_token, registered_at, revoked_at }))
    }

    fn save_registration(&self, reg: &Registration) -> Result<(), HubApiError> {
        let endpoint = reg.endpoint_id();
        let (nonce, ct) = self.accounts.secrets().seal(reg.device_token.as_bytes(), &self.aad(&endpoint));
        let device_json = serde_json::to_string(&reg.device).map_err(anyhow::Error::from)?;
        self.store.conn().execute(
            "INSERT INTO hub_registration
                 (id, hub_url, endpoint_id, device_json, token_nonce, token_ciphertext, registered_at, revoked_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET hub_url = ?1, endpoint_id = ?2, device_json = ?3, token_nonce = ?4,
                 token_ciphertext = ?5, registered_at = ?6, revoked_at = ?7",
            params![reg.hub_url, endpoint.to_string(), device_json, nonce, ct, reg.registered_at, reg.revoked_at],
        )?;
        Ok(())
    }

    fn mark_revoked(&self) -> Result<Option<i64>, HubApiError> {
        let now = ember_hub::now_secs();
        self.store
            .conn()
            .execute("UPDATE hub_registration SET revoked_at = ?1 WHERE id = 1 AND revoked_at IS NULL", params![now])?;
        if let Some(t) = self.transport.get() {
            t.set_directory_token(None);
        }
        tracing::warn!(
            hub = %self.config.url,
            "the hub removed this server from the account: hub directory and device list are off until it registers again"
        );
        Ok(self.registration()?.and_then(|r| r.revoked_at))
    }

    /// Forgets the local registration (the hub keeps listing the device until removed there).
    pub fn forget(&self) -> Result<bool, HubApiError> {
        Ok(self.store.conn().execute("DELETE FROM hub_registration WHERE id = 1", [])? > 0)
    }

    /// The active registration's token, for binding the transport at start.
    pub fn active_token(&self) -> Option<String> {
        self.registration().ok().flatten().and_then(|r| r.active_token())
    }

    /// A client with this server's token; errors if unregistered or revoked.
    pub fn client(&self) -> Result<HubClient, HubApiError> {
        match self.registration()? {
            None => Err(HubApiError::NotRegistered),
            Some(r) => match r.revoked_at {
                Some(at) => Err(HubApiError::Revoked(at)),
                None => Ok(HubClient::new(&r.hub_url).with_token(&r.device_token)),
            },
        }
    }

    /// Maps a hub error, recording a revocation when the token stopped working.
    fn hub_err(&self, e: HubError) -> HubApiError {
        if e.is_unauthorized() {
            match self.mark_revoked() {
                Ok(Some(at)) => return HubApiError::Revoked(at),
                Ok(None) => return HubApiError::NotRegistered,
                Err(e) => return e,
            }
        }
        match e {
            HubError::NotFound(m) => HubApiError::NotFound(m),
            e => HubApiError::Hub(e),
        }
    }

    // ------------------------------------------------------------------ status and linking

    pub fn status(&self) -> Result<HubStatus, HubApiError> {
        let reg = self.registration()?;
        let link = self.link.lock().unwrap();
        Ok(HubStatus {
            hub_url: self.config.url.clone(),
            relay_url: self.config.relay_url.clone(),
            endpoint_id: self.peer_id(),
            registered: reg.as_ref().is_some_and(|r| !r.is_revoked()),
            device: reg.as_ref().map(|r| r.device.clone()),
            registered_at: reg.as_ref().map(|r| r.registered_at),
            revoked: reg.as_ref().is_some_and(|r| r.is_revoked()),
            revoked_at: reg.as_ref().and_then(|r| r.revoked_at),
            pending: link.pending.clone(),
            last_error: link.last_error.clone(),
            sync_devices: self.sync_devices.load(Ordering::Relaxed),
        })
    }

    /// Starts joining the user's account as `main_server` named `name`, and returns what to show
    /// the user. A link already in progress is returned as is.
    pub async fn start_link(self: &Arc<Self>, name: &str) -> Result<PendingView, HubApiError> {
        if let Some(r) = self.registration()? {
            match r.revoked_at {
                None => return Err(HubApiError::AlreadyRegistered(r.device.name)),
                Some(at) => return Err(HubApiError::Revoked(at)),
            }
        }
        let in_progress = self.link.lock().unwrap().pending.clone();
        if let Some(p) = in_progress.filter(|p| p.expires_at >= ember_hub::now_secs()) {
            return Ok(p);
        }
        let name = if name.trim().is_empty() { default_name() } else { name.trim().to_string() };
        let client = HubClient::new(&self.config.url);
        let mut link = DeviceLink::start(&client, &self.key, &name, Role::MainServer).await.map_err(HubApiError::Link)?;
        if let Some(i) = self.poll_interval {
            link = link.with_poll_interval(i);
        }
        let view = PendingView::from(link.pending());
        {
            let mut state = self.link.lock().unwrap();
            if let Some(old) = state.task.take() {
                old.abort();
            }
            state.pending = Some(view.clone());
            state.last_error = None;
        }
        let this = self.clone();
        let task = tokio::spawn(async move {
            let finished = match link.wait().await {
                Ok(reg) => this.complete(&reg).await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            if let Err(e) = &finished {
                tracing::warn!("hub link did not complete: {e}");
            }
            let mut state = this.link.lock().unwrap();
            state.pending = None;
            state.task = None;
            state.last_error = finished.err();
        });
        let mut state = self.link.lock().unwrap();
        if state.pending.is_some() {
            state.task = Some(task);
        }
        Ok(view)
    }

    /// Stops waiting for a pending link.
    pub fn cancel_link(&self) -> bool {
        let mut state = self.link.lock().unwrap();
        if let Some(task) = state.task.take() {
            task.abort();
        }
        state.pending.take().is_some()
    }

    async fn complete(&self, reg: &Registration) -> Result<(), HubApiError> {
        self.save_registration(reg)?;
        tracing::info!(name = %reg.device.name, hub = %reg.hub_url, "registered with the hub as main server");
        if let Some(t) = self.transport.get() {
            t.enable_hub(self.config.directory(Some(reg.device_token.clone())))
                .map_err(|e| anyhow::anyhow!("enabling the hub directory: {e}"))?;
            if let Err(e) = t.set_relays(self.config.relay_config()).await {
                tracing::warn!("could not switch to the hub's relay: {e}");
            }
        }
        if self.sync_devices.load(Ordering::Relaxed) {
            if let Err(e) = self.sync_devices().await {
                tracing::warn!("device sync after registration failed: {e}");
            }
        }
        Ok(())
    }

    /// Asks the hub whether this server is still registered; records a revocation.
    pub async fn check(&self) -> Result<RegistrationState, HubApiError> {
        let Some(reg) = self.registration()? else { return Ok(RegistrationState::Unknown) };
        if reg.is_revoked() {
            return Ok(RegistrationState::Revoked);
        }
        let state = check_registration(&reg.client()).await;
        if state.is_revoked() {
            self.mark_revoked()?;
        }
        Ok(state)
    }

    /// Checks the registration every `period` (and syncs devices when that is on). Stops once
    /// revoked.
    pub fn spawn_watch(self: &Arc<Self>, period: Duration) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(hub) = weak.upgrade() else { break };
                match hub.check().await {
                    Ok(RegistrationState::Revoked) => break,
                    Ok(RegistrationState::Active(_)) if hub.sync_devices.load(Ordering::Relaxed) => {
                        if let Err(e) = hub.sync_devices().await {
                            tracing::warn!("hub device sync failed: {e}");
                        }
                    }
                    Ok(RegistrationState::Unreachable(e)) => tracing::debug!("hub unreachable: {e}"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("hub registration check failed: {e}"),
                }
                drop(hub);
                tokio::time::sleep(period).await;
            }
        })
    }

    // ------------------------------------------------------------------ the account's devices

    /// The account's devices, annotated for this server.
    pub async fn devices(&self, computers: &Computers) -> Result<Vec<HubDeviceView>, HubApiError> {
        let list = self.client()?.devices().await.map_err(|e| self.hub_err(e))?;
        let registered = computers.registry().list()?;
        let allowed = self.devices.list()?;
        Ok(list
            .into_iter()
            .map(|d| HubDeviceView {
                this_server: d.endpoint_id == self.peer_id(),
                computer_id: registered
                    .iter()
                    .find(|c| c.peer.as_ref().is_some_and(|p| p.peer == d.endpoint_id))
                    .map(|c| c.id.clone()),
                allowed: allowed.iter().any(|a| a.peer_id == d.endpoint_id),
                device: d,
            })
            .collect())
    }

    /// Registers the account's device `id` as a computer (no address is entered: the
    /// transport resolves it through the hub). `name` defaults to the device's hub name;
    /// `token` is the node's API bearer token.
    pub async fn add_computer(
        &self,
        computers: &Computers,
        id: PeerId,
        name: Option<&str>,
        token: &str,
    ) -> Result<Computer, HubApiError> {
        if id == self.peer_id() {
            return Err(HubApiError::BadRequest("that device is this server".into()));
        }
        let client = self.client()?;
        let device = client.device(&id).await.map_err(|e| match self.hub_err(e) {
            HubApiError::NotFound(_) => HubApiError::NotFound(format!("device {id} is not in this hub account")),
            e => e,
        })?;
        // Warm the transport with the published record (a hint; the hub resolver has the same).
        if let (Some(t), Ok(rec)) = (self.transport.get(), client.addresses(&id).await) {
            t.add_peer_addr(rec.to_peer_addr());
        }
        let name = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&device.name);
        Ok(computers.register_peer(name, &PeerAddr::new(id), token)?)
    }

    /// Removes one of the account's devices on the hub (it loses its token, record and relay
    /// connections there), and revokes it here when it was synced.
    pub async fn remove_device(&self, id: PeerId) -> Result<(), HubApiError> {
        if id == self.peer_id() {
            return Err(HubApiError::BadRequest("removing this server itself is done on darkpyonix.dev".into()));
        }
        self.client()?.remove_device(&id).await.map_err(|e| self.hub_err(e))?;
        if self.sync_devices.load(Ordering::Relaxed) {
            self.sync_devices().await?;
        }
        Ok(())
    }

    /// What a pending user code would let in.
    pub async fn lookup_code(&self, code: &str) -> Result<LinkCodeInfo, HubApiError> {
        self.client()?.link_code(code).await.map_err(|e| self.hub_err(e))
    }

    /// Approves or denies a pending user code into the account (as its main server).
    pub async fn decide_code(&self, code: &str, approve: bool) -> Result<(), HubApiError> {
        self.client()?.decide_link_code(code, approve).await.map_err(|e| self.hub_err(e))
    }

    pub fn set_sync_devices(&self, on: bool) {
        self.sync_devices.store(on, Ordering::Relaxed);
    }

    /// Makes the devices allow-list's `hub` rows equal to the account's devices (all of them
    /// but this server).
    pub async fn sync_devices(&self) -> Result<SyncReport, HubApiError> {
        let list = self.client()?.devices().await.map_err(|e| self.hub_err(e))?;
        let me = self.peer_id();
        let listed: Vec<(PeerId, String)> =
            list.into_iter().filter(|d| d.endpoint_id != me).map(|d| (d.endpoint_id, d.name)).collect();
        let report = self.devices.sync_from_hub(&listed)?;
        if !report.added.is_empty() || !report.removed.is_empty() {
            tracing::info!(added = report.added.len(), removed = report.removed.len(), "devices synced from the hub");
        }
        Ok(report)
    }
}

fn default_name() -> String {
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match host {
        Some(h) => format!("ember server ({h})").chars().take(64).collect(),
        None => "ember server".into(),
    }
}
