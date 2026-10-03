//! ember node and the darkpyonix.dev hub (SPEC `FR-N2`; design: `docs/design/HUB-INTEGRATION.md`).
//!
//! - **Registration** (`ember-node hub register`): the node's transport key joins the user's
//!   GitHub account as a `computer` through a device link. The person approves the printed user
//!   code at the verification URL (or types it into Ember, which approves it as the account's
//!   main server). The device token is kept in `<state dir>/hub.json`, mode 0600.
//! - **Transport**: a registered node publishes its address to the hub's directory and uses the
//!   hub's relay ([`transport_config`]); the server then adds it by picking it from the account's
//!   device list instead of pasting an address.
//! - **Revocation**: [`spawn_watch`] asks the hub every minute; when the hub removed the device
//!   it records `revoked_at` in `hub.json`, stops resolving through the hub, drops servers it
//!   admitted because of the hub, and logs an error.
//! - **Allowed servers from the hub** (opt-in, `EMBER_NODE_HUB_ALLOW_SERVERS=1`): the account's
//!   `main_server` devices are admitted in addition to `allowed-peers` (FR-N3), so the server's
//!   peer id need not be copied either.

use std::path::Path;
use std::time::Duration;

use ember_hub::{
    check_registration, DeviceLink, HubConfig, LinkError, PendingLink, Registration, RegistrationFile,
    RegistrationState, Role,
};
use ember_transport::{PeerGate, PeerId, SecretKey, Transport, TransportConfig};
use tokio::task::JoinHandle;

/// `1` admits the hub account's main servers over the transport (in addition to the allow-list).
pub const ALLOW_SERVERS_ENV: &str = "EMBER_NODE_HUB_ALLOW_SERVERS";
/// How often the registration is checked.
pub const WATCH_PERIOD: Duration = Duration::from_secs(60);

pub fn allow_servers_from_env() -> bool {
    matches!(std::env::var(ALLOW_SERVERS_ENV).as_deref(), Ok("1" | "on" | "true" | "yes"))
}

/// `<state dir>/hub.json`.
pub fn registration_file(state_dir: &Path) -> RegistrationFile {
    RegistrationFile::in_dir(state_dir)
}

/// The stored registration, if active (not revoked).
pub fn active_registration(state_dir: &Path) -> anyhow::Result<Option<Registration>> {
    Ok(registration_file(state_dir).load()?.filter(|r| !r.is_revoked()))
}

/// Joins the account as `name`: starts a link, calls `show` with what the person must see,
/// waits for approval and stores the registration. An active registration is returned as is.
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
        anyhow::bail!(
            "the hub removed this node; removed keys cannot rejoin. Remove {} and {} and register again",
            file.path().display(),
            state_dir.join(crate::transport::KEY_FILE).display()
        );
    }
    let mut link = DeviceLink::start(&ember_hub::HubClient::new(&hub.url), key, name, Role::Computer)
        .await
        .map_err(link_error)?;
    if let Some(i) = poll_interval {
        link = link.with_poll_interval(i);
    }
    show(link.pending());
    let reg = link.wait().await.map_err(link_error)?;
    file.save(&reg)?;
    Ok(reg)
}

fn link_error(e: LinkError) -> anyhow::Error {
    anyhow::anyhow!("hub registration failed: {e}")
}

/// The node's transport configuration: with an active registration, the hub's relay and
/// directory (with the device token); with `EMBER_HUB_URL` set but no registration, the hub's
/// relay and directory without a token; otherwise the plain environment configuration.
pub fn transport_config(state_dir: &Path, key: SecretKey) -> anyhow::Result<TransportConfig> {
    if let Some(reg) = active_registration(state_dir)? {
        let hub = HubConfig::parse(&reg.hub_url, std::env::var(ember_hub::HUB_RELAY_URL_ENV).ok().as_deref())
            .unwrap_or_else(|| HubConfig::new(&reg.hub_url));
        return Ok(hub.transport_config(key, Some(reg.device_token)));
    }
    if HubConfig::explicitly_enabled() {
        if let Some(hub) = HubConfig::from_env() {
            return Ok(hub.transport_config(key, None));
        }
    }
    Ok(TransportConfig::from_env(key))
}

/// Turns the hub on for a running transport after a registration appeared (SIGHUP).
pub async fn apply_registration(transport: &Transport, reg: &Registration) -> anyhow::Result<()> {
    let hub = HubConfig::new(&reg.hub_url);
    transport.enable_hub(hub.directory(Some(reg.device_token.clone())))?;
    transport.set_relays(hub.relay_config()).await?;
    Ok(())
}

/// The account's main servers (to admit them, `EMBER_NODE_HUB_ALLOW_SERVERS`).
pub async fn hub_servers(reg: &Registration) -> Result<Vec<PeerId>, ember_hub::HubError> {
    Ok(reg
        .client()
        .devices()
        .await?
        .into_iter()
        .filter(|d| d.role == Role::MainServer)
        .map(|d| d.endpoint_id)
        .collect())
}

/// Watches the registration in `state_dir` every `period`. On revocation: marks `hub.json`,
/// clears the directory token, and (with `allow_servers`) narrows `gate` back to `base_allowed`.
/// While active with `allow_servers`, keeps `gate` = `base_allowed` ∪ the account's main servers.
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
        loop {
            let reg = match file.load() {
                Ok(Some(r)) if !r.is_revoked() => r,
                Ok(_) => break,
                Err(e) => {
                    tracing::warn!("cannot read {}: {e}", file.path().display());
                    break;
                }
            };
            match check_registration(&reg.client()).await {
                RegistrationState::Revoked => {
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
                    break;
                }
                RegistrationState::Active(_) if allow_servers => match hub_servers(&reg).await {
                    Ok(servers) => {
                        let mut all = base_allowed.clone();
                        all.extend(servers.into_iter().filter(|p| !base_allowed.contains(p)));
                        let closed = gate.set_allowed(all);
                        if closed > 0 {
                            tracing::info!(closed, "servers removed from the hub account disconnected");
                        }
                    }
                    Err(e) => tracing::debug!("hub device list: {e}"),
                },
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
                        println!("state: removed from the account");
                    } else {
                        match check_registration(&reg.client()).await {
                            RegistrationState::Active(me) => println!("state: active (account {})", me.github_login),
                            RegistrationState::Revoked => {
                                file.mark_revoked()?;
                                println!("state: removed from the account");
                            }
                            RegistrationState::Unreachable(e) => println!("state: unknown (hub unreachable: {e})"),
                            RegistrationState::Unknown => println!("state: unknown"),
                        }
                    }
                }
            }
            Ok(())
        }
        Some("forget") => {
            if file.remove()? {
                println!("forgot the hub registration (the device stays in the account until removed there)");
            }
            Ok(())
        }
        _ => anyhow::bail!("usage: ember-node hub <register [--name NAME] | status | forget>"),
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
