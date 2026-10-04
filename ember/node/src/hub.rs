//! ember node and the darkpyonix.dev hub (SPEC `FR-N2`; design: `docs/design/HUB-INTEGRATION.md`).
//!
//! - **Registration** (`ember-node hub register`): the node's transport key joins the user's
//!   GitHub account as a `computer` through a device link. The person approves the printed user
//!   code at the verification URL (or types it into Ember, which approves it as the account's
//!   main server). The device token and the read-only resolve token are kept in
//!   `<state dir>/hub.json`, mode 0600. A link waiting for approval is remembered in
//!   `<state dir>/hub-link.json` and resumed (`GET /v1/device-links/{id}`) if `register` runs
//!   again. A node the hub removed can rejoin with the same key after the account owner
//!   re-admits it on the hub (FR-H11); the new link is approved in the browser.
//! - **App record** (FR-H10): after registering and at start the node reports
//!   `{kind: "ember-node", version, services: ["ember-node"]}` with its own token.
//! - **Leaving** (`ember-node hub forget`): removes the node on the hub with its own token, then
//!   deletes `hub.json` (`--local`: only the file).
//! - **Transport**: a registered node publishes its address to the hub's directory and uses the
//!   hub's relay ([`transport_config`]); the server then adds it by picking it from the account's
//!   device list instead of pasting an address.
//! - **Discovery**: the relay and directory come from the hub's `GET /v1/config` when it serves
//!   it ([`discovered_transport_config`]); otherwise `https://relay.<host>` and `<hub>/pkarr`.
//! - **Revocation**: [`spawn_watch`] holds a long-poll on the account's device list when the hub
//!   offers it (a removal is seen at once), else asks the hub every minute. When the hub removed
//!   the device (`401 device_removed`, or a code-less `401` from a hub older than the codes) it records
//!   `revoked_at` in `hub.json`, stops resolving through the hub, drops servers it admitted
//!   because of the hub, and logs an error. `401 invalid_credentials` is logged, not revoked.
//! - **Allowed servers from the hub** (opt-in, `EMBER_NODE_HUB_ALLOW_SERVERS=1`): the account's
//!   `main_server` devices are admitted in addition to `allowed-peers` (FR-N3), so the server's
//!   peer id need not be copied either.

use std::path::Path;
use std::time::Duration;

use ember_hub::{
    check_registration, ensure_resolve_token, Device, DeviceApp, DeviceLink, DeviceWatcher, HubClient, HubConfig,
    HubError, LinkError, PendingLink, Registration, RegistrationFile, RegistrationState, Role,
};
use serde::{Deserialize, Serialize};
use ember_transport::{PeerGate, PeerId, SecretKey, Transport, TransportConfig};
use tokio::task::JoinHandle;

/// `1` admits the hub account's main servers over the transport (in addition to the allow-list).
pub const ALLOW_SERVERS_ENV: &str = "EMBER_NODE_HUB_ALLOW_SERVERS";
/// How often the registration is checked.
pub const WATCH_PERIOD: Duration = Duration::from_secs(60);

pub fn allow_servers_from_env() -> bool {
    matches!(std::env::var(ALLOW_SERVERS_ENV).as_deref(), Ok("1" | "on" | "true" | "yes"))
}

/// The link a `register` is waiting on (`<state dir>/hub-link.json`).
pub const PENDING_LINK_FILE: &str = "hub-link.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingLinkRecord {
    hub_url: String,
    link_id: String,
}

/// `<state dir>/hub.json`.
pub fn registration_file(state_dir: &Path) -> RegistrationFile {
    RegistrationFile::in_dir(state_dir)
}

/// What this node reports as its app on the hub (FR-H10).
pub fn node_app() -> DeviceApp {
    DeviceApp::new("ember-node", env!("CARGO_PKG_VERSION"), &[crate::client::NODE_SERVICE])
}

fn load_pending_link(state_dir: &Path) -> Option<PendingLinkRecord> {
    let data = std::fs::read(state_dir.join(PENDING_LINK_FILE)).ok()?;
    serde_json::from_slice(&data).ok()
}

fn save_pending_link(state_dir: &Path, rec: &PendingLinkRecord) -> anyhow::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    std::fs::write(state_dir.join(PENDING_LINK_FILE), serde_json::to_vec(rec)?)?;
    Ok(())
}

fn clear_pending_link(state_dir: &Path) {
    let _ = std::fs::remove_file(state_dir.join(PENDING_LINK_FILE));
}

/// Gives a stored registration a resolve token if it has none (registered before the hub
/// issued them) and saves it. Errors are logged; the registration is returned either way.
pub async fn ensure_resolve(file: &RegistrationFile, mut reg: Registration) -> Registration {
    match ensure_resolve_token(&mut reg).await {
        Ok(true) => {
            if let Err(e) = file.save(&reg) {
                tracing::warn!("cannot store the hub resolve token: {e}");
            }
        }
        Ok(false) => {}
        Err(e) => tracing::warn!("could not get a hub resolve token (resolving peers through the hub is off): {e}"),
    }
    reg
}

/// Reports the node's app on the hub with its own token (`PATCH /v1/devices/{id} {app}`),
/// unless the stored device already shows it. Failures are logged (the record is a hint).
pub async fn publish_app(file: &RegistrationFile, reg: &Registration) {
    let app = node_app();
    if reg.is_revoked() || reg.device.app.as_ref() == Some(&app) {
        return;
    }
    match reg.client().set_app(&reg.endpoint_id(), Some(&app)).await {
        Ok(device) => {
            let mut updated = reg.clone();
            updated.device = device;
            if let Err(e) = file.save(&updated) {
                tracing::debug!("cannot store the device after reporting the app: {e}");
            }
        }
        Err(e) => tracing::debug!("could not report the app to the hub: {e}"),
    }
}

/// The stored registration, if active (not revoked).
pub fn active_registration(state_dir: &Path) -> anyhow::Result<Option<Registration>> {
    Ok(registration_file(state_dir).load()?.filter(|r| !r.is_revoked()))
}

/// Joins the account as `name`: starts a link (or resumes the one a previous run was waiting
/// on), calls `show` with what the person must see, waits for approval and stores the
/// registration. An active registration is returned as is. A revoked one tries again with the
/// same key: the hub refuses (`409`) unless the owner re-admitted it (FR-H11).
pub async fn register(
    state_dir: &Path,
    key: &SecretKey,
    hub: &HubConfig,
    name: &str,
    poll_interval: Option<Duration>,
    show: impl FnOnce(&PendingLink),
) -> anyhow::Result<Registration> {
    let file = registration_file(state_dir);
    if let Some(reg) = file.load()? {
        if reg.endpoint_id() != key.peer_id() {
            anyhow::bail!(
                "{} belongs to another key ({}); remove it to register this node",
                file.path().display(),
                reg.endpoint_id()
            );
        }
        if !reg.is_revoked() {
            return Ok(reg);
        }
    }
    let client = HubClient::new(&hub.url);
    let resumed = match load_pending_link(state_dir).filter(|p| p.hub_url == client.base_url()) {
        Some(p) => match DeviceLink::resume_by_id(&client, key, &p.link_id).await {
            Ok(link) => Some(link),
            Err(LinkError::Hub(e)) => return Err(anyhow::anyhow!("hub registration failed: {e}")),
            Err(LinkError::Expired(_)) => {
                clear_pending_link(state_dir);
                None
            }
            Err(e) => {
                clear_pending_link(state_dir);
                return Err(link_error(e, &key.peer_id()));
            }
        },
        None => None,
    };
    let mut link = match resumed {
        Some(l) => l,
        None => {
            let l = DeviceLink::start(&client, key, name, Role::Computer).await.map_err(|e| link_error(e, &key.peer_id()))?;
            save_pending_link(
                state_dir,
                &PendingLinkRecord { hub_url: client.base_url().to_string(), link_id: l.pending().link_id.clone() },
            )?;
            l
        }
    };
    if let Some(i) = poll_interval {
        link = link.with_poll_interval(i);
    }
    show(link.pending());
    let result = link.wait().await;
    clear_pending_link(state_dir);
    let reg = result.map_err(|e| link_error(e, &key.peer_id()))?;
    file.save(&reg)?;
    let reg = ensure_resolve(&file, reg).await;
    publish_app(&file, &reg).await;
    Ok(file.load()?.unwrap_or(reg))
}

fn link_error(e: LinkError, id: &PeerId) -> anyhow::Error {
    match e {
        LinkError::AlreadyRegistered(_) => anyhow::anyhow!(
            "hub registration failed: this node ({id}) is registered, or was removed from the account and not \
             re-admitted. To rejoin with the same key, the account owner signs in on the hub, re-admits the \
             removed device (valid 15 minutes) and approves the new code there"
        ),
        e => anyhow::anyhow!("hub registration failed: {e}"),
    }
}

/// The hub of a registration, with `EMBER_HUB_RELAY_URL` applied (not yet discovered).
pub fn hub_config_for(reg: &Registration) -> HubConfig {
    HubConfig::parse(&reg.hub_url, std::env::var(ember_hub::HUB_RELAY_URL_ENV).ok().as_deref())
        .unwrap_or_else(|| HubConfig::new(&reg.hub_url))
}

/// Which hub the node uses now, and the token: the registration's hub, or `EMBER_HUB_URL` when
/// set explicitly (without a token).
fn hub_in_use(state_dir: &Path) -> anyhow::Result<Option<(HubConfig, Option<String>)>> {
    if let Some(reg) = active_registration(state_dir)? {
        // The resolver URL carries the resolve token, never the device token (NFR-H2).
        let token = reg.directory_token();
        return Ok(Some((hub_config_for(&reg), token)));
    }
    if HubConfig::explicitly_enabled() {
        if let Some(hub) = HubConfig::from_env() {
            return Ok(Some((hub, None)));
        }
    }
    Ok(None)
}

/// The node's transport configuration: with an active registration, the hub's relay and
/// directory (resolving with the resolve token); with `EMBER_HUB_URL` set but no registration, the hub's
/// relay and directory without a token; otherwise the plain environment configuration. The
/// relay is derived (`https://relay.<host>`); see [`discovered_transport_config`].
pub fn transport_config(state_dir: &Path, key: SecretKey) -> anyhow::Result<TransportConfig> {
    Ok(match hub_in_use(state_dir)? {
        Some((hub, token)) => hub.transport_config(key, token),
        None => TransportConfig::from_env(key),
    })
}

/// [`transport_config`], with the relay and directory the hub advertises in `GET /v1/config`
/// (falling back to the derived ones when it does not serve it or cannot be reached), after
/// fetching a resolve token for a registration that has none.
pub async fn discovered_transport_config(state_dir: &Path, key: SecretKey) -> anyhow::Result<TransportConfig> {
    if let Some(reg) = active_registration(state_dir)? {
        ensure_resolve(&registration_file(state_dir), reg).await;
    }
    Ok(match hub_in_use(state_dir)? {
        Some((hub, token)) => hub.discover().await.transport_config(key, token),
        None => TransportConfig::from_env(key),
    })
}

/// Turns the hub on for a running transport after a registration appeared (SIGHUP).
pub async fn apply_registration(transport: &Transport, state_dir: &Path, reg: &Registration) -> anyhow::Result<()> {
    let reg = ensure_resolve(&registration_file(state_dir), reg.clone()).await;
    let hub = hub_config_for(&reg).discover().await;
    transport.enable_hub(hub.directory(reg.directory_token()))?;
    transport.set_relays(hub.relay_config()).await?;
    Ok(())
}

/// Leaves the account: removes the node on the hub with its own token (FR-H1), then deletes
/// `hub.json`. `local_only` (or a revoked registration) skips the hub. An already removed or
/// unknown device is fine; an unreachable hub is an error (use `local_only`).
pub async fn leave(state_dir: &Path, local_only: bool) -> anyhow::Result<bool> {
    let file = registration_file(state_dir);
    let Some(reg) = file.load()? else {
        clear_pending_link(state_dir);
        return Ok(false);
    };
    if !local_only && !reg.is_revoked() {
        match reg.client().remove_device(&reg.endpoint_id()).await {
            Ok(()) => {}
            Err(e) if e.is_unauthorized() || matches!(e, HubError::NotFound(_)) => {
                tracing::info!("the hub no longer had this node: {e}")
            }
            Err(e) => anyhow::bail!("could not leave the hub account ({e}); `forget --local` only deletes {}", file.path().display()),
        }
    }
    clear_pending_link(state_dir);
    Ok(file.remove()?)
}

/// The account's main servers (to admit them, `EMBER_NODE_HUB_ALLOW_SERVERS`).
pub async fn hub_servers(reg: &Registration) -> Result<Vec<PeerId>, ember_hub::HubError> {
    Ok(main_servers(reg.client().devices().await?))
}

fn main_servers(devices: Vec<Device>) -> Vec<PeerId> {
    devices.into_iter().filter(|d| d.role == Role::MainServer).map(|d| d.endpoint_id).collect()
}

/// `gate` = `base_allowed` ∪ `servers`.
fn admit_servers(gate: &PeerGate, base_allowed: &[PeerId], servers: Vec<PeerId>) {
    let mut all = base_allowed.to_vec();
    all.extend(servers.into_iter().filter(|p| !base_allowed.contains(p)));
    let closed = gate.set_allowed(all);
    if closed > 0 {
        tracing::info!(closed, "servers removed from the hub account disconnected");
    }
}

/// What the node does once the hub removed it.
fn on_revoked(
    file: &RegistrationFile,
    reg: &Registration,
    transport: &Transport,
    gate: &PeerGate,
    base_allowed: &[PeerId],
    allow_servers: bool,
) {
    tracing::error!(
        hub = %reg.hub_url,
        "the hub removed this node from the account: it is no longer published or resolved there. \
         Remove {} and the transport key, and register again to rejoin",
        file.path().display()
    );
    if let Err(e) = file.mark_revoked() {
        tracing::warn!("cannot record the revocation: {e}");
    }
    transport.set_directory_token(None);
    if allow_servers {
        gate.set_allowed(base_allowed.iter().copied());
    }
}

/// Watches the registration in `state_dir`. On revocation: marks `hub.json`, clears the
/// directory token, and (with `allow_servers`) narrows `gate` back to `base_allowed`. While
/// active with `allow_servers`, keeps `gate` = `base_allowed` ∪ the account's main servers.
///
/// When the hub advertises the device-list long-poll (`GET /v1/config`, asked once per watch),
/// a held `GET /v1/devices?wait=` sees removal and server changes as they happen; otherwise
/// the hub is asked every `period`.
pub fn spawn_watch(
    state_dir: &Path,
    transport: Transport,
    gate: PeerGate,
    base_allowed: Vec<PeerId>,
    allow_servers: bool,
    period: Duration,
) -> JoinHandle<()> {
    let file = registration_file(state_dir);
    tokio::spawn(async move {
        if let Ok(Some(reg)) = file.load() {
            publish_app(&file, &reg).await;
        }
        let mut long_poll: Option<Option<DeviceWatcher>> = None;
        loop {
            let reg = match file.load() {
                Ok(Some(r)) if !r.is_revoked() => r,
                Ok(_) => break,
                Err(e) => {
                    tracing::warn!("cannot read {}: {e}", file.path().display());
                    break;
                }
            };
            if long_poll.is_none() {
                let hub = hub_config_for(&reg).discover().await;
                long_poll = Some(
                    hub.supports_devices_wait()
                        .then(|| DeviceWatcher::new(reg.client(), hub.info.as_ref(), period).expecting(reg.endpoint_id())),
                );
            }
            if let Some(Some(w)) = long_poll.as_mut() {
                match w.next().await {
                    Ok(devices) if allow_servers => admit_servers(&gate, &base_allowed, main_servers(devices)),
                    Ok(_) => {}
                    Err(e) if e.is_revocation() => {
                        on_revoked(&file, &reg, &transport, &gate, &base_allowed, allow_servers);
                        break;
                    }
                    Err(HubError::InvalidCredentials(m)) => tracing::error!("the hub rejected this node's token: {m}"),
                    Err(e) => tracing::debug!("hub device list: {e}"),
                }
                // The watcher paces itself (held requests, backoff after errors).
                continue;
            }
            match check_registration(&reg.client()).await {
                RegistrationState::Revoked => {
                    on_revoked(&file, &reg, &transport, &gate, &base_allowed, allow_servers);
                    break;
                }
                RegistrationState::Active(_) if allow_servers => match hub_servers(&reg).await {
                    Ok(servers) => admit_servers(&gate, &base_allowed, servers),
                    Err(e) => tracing::debug!("hub device list: {e}"),
                },
                RegistrationState::Rejected(e) => tracing::error!("the hub rejected this node's token: {e}"),
                RegistrationState::Unreachable(e) => tracing::debug!("hub unreachable: {e}"),
                _ => {}
            }
            tokio::time::sleep(period).await;
        }
    })
}

/// `ember-node hub <register [--name N] | status | forget>`.
pub async fn cli(args: &[String]) -> anyhow::Result<()> {
    let dir = crate::config::default_state_dir()
        .ok_or_else(|| anyhow::anyhow!("no state dir (set EMBER_NODE_STATE_DIR or HOME)"))?;
    let file = registration_file(&dir);
    match args.first().map(String::as_str) {
        Some("register") => {
            let mut name = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--name" => name = it.next().cloned(),
                    other => anyhow::bail!("unknown argument {other:?}"),
                }
            }
            let hub = HubConfig::from_env().ok_or_else(|| anyhow::anyhow!("the hub is off ({}=off)", ember_hub::HUB_URL_ENV))?;
            let key = SecretKey::load_or_generate(dir.join(crate::transport::KEY_FILE))?;
            let name = name.unwrap_or_else(default_name);
            let reg = register(&dir, &key, &hub, &name, None, |p| {
                println!("To add this computer to your darkpyonix.dev account, open");
                println!("    {}", p.verification_uri_complete);
                println!("or enter the code {} at {} (or in Ember, as the account's server).", p.user_code, p.verification_uri);
                println!("Waiting for approval…");
            })
            .await?;
            println!("Registered as {:?} ({}) on {}.", reg.device.name, reg.endpoint_id(), reg.hub_url);
            println!("Restart ember node, or send it SIGHUP, to publish its address through the hub.");
            Ok(())
        }
        Some("status") => {
            match file.load()? {
                None => println!("not registered (ember-node hub register)"),
                Some(reg) => {
                    println!("hub: {}\nname: {}\nendpoint id: {}", reg.hub_url, reg.device.name, reg.endpoint_id());
                    if reg.is_revoked() {
                        println!("state: removed from the account (to rejoin with this key: the owner re-admits it on the hub, then `ember-node hub register`)");
                    } else {
                        match check_registration(&reg.client()).await {
                            RegistrationState::Active(me) => println!("state: active (account {})", me.github_login),
                            RegistrationState::Revoked => {
                                file.mark_revoked()?;
                                println!("state: removed from the account (to rejoin with this key: the owner re-admits it on the hub, then `ember-node hub register`)");
                            }
                            RegistrationState::Rejected(e) => println!("state: token rejected by the hub ({e})"),
                            RegistrationState::Unreachable(e) => println!("state: unknown (hub unreachable: {e})"),
                            RegistrationState::Unknown => println!("state: unknown"),
                        }
                    }
                }
            }
            Ok(())
        }
        Some("forget") => {
            let local_only = match args.get(1).map(String::as_str) {
                None => false,
                Some("--local") => true,
                Some(other) => anyhow::bail!("unknown argument {other:?}"),
            };
            if leave(&dir, local_only).await? {
                if local_only {
                    println!("forgot the hub registration here (the device stays in the account until removed there)");
                } else {
                    println!("left the hub account and forgot the registration");
                }
            }
            Ok(())
        }
        _ => anyhow::bail!("usage: ember-node hub <register [--name NAME] | status | forget [--local]>"),
    }
}

fn default_name() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().chars().take(64).collect::<String>())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "ember node".into())
}
