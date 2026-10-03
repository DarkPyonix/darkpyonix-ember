//! Devices allowed to reach this server over the transport (SPEC `FR-N3`).
//!
//! A device is a transport peer id (a client's Ed25519 key) with a name. The table is the
//! allow-list of the server's [`PeerGate`]: connections from peers not in it are closed at
//! accept, before any request is read. [`Devices::remove`] is revocation: the row goes and every
//! open connection of that peer is closed at once.
//!
//! Managing devices is a local, administrative action: the [`api`] router is served on the
//! server's TCP listener only, never over the transport (a device must not be able to add or
//! revoke devices).
//!
//! Rows come from two places, recorded in `source`: added by hand (`local`), or synced from the
//! user's darkpyonix.dev hub account (`hub`, opt-in: [`crate::hub`], `FR-N2`). A sync adds the
//! account's devices and removes `hub` rows whose device the hub no longer lists — removing a
//! device on the hub revokes it here too. Rows added by hand are never touched by a sync.

use std::sync::Arc;

use ember_transport::{PeerGate, PeerId};
use rusqlite::params;
use serde::Serialize;

use crate::store::{now_ms, Store};

pub mod api;
pub mod schema;

/// An allowed device.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Device {
    pub peer_id: PeerId,
    pub name: String,
    pub created_at: i64,
    /// `local` (added by hand) or `hub` (synced from the hub account).
    pub source: String,
}

/// Where a device row came from.
pub const SOURCE_LOCAL: &str = "local";
pub const SOURCE_HUB: &str = "hub";

/// What a hub sync changed.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SyncReport {
    pub added: Vec<PeerId>,
    pub removed: Vec<PeerId>,
    /// Connections closed by the removals.
    pub closed: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("device {0} not found")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for DeviceError {
    fn from(e: rusqlite::Error) -> Self {
        DeviceError::Other(e.into())
    }
}

/// The devices table plus the gate it feeds. Cheap to share (`Arc`).
pub struct Devices {
    store: Arc<Store>,
    gate: PeerGate,
}

impl Devices {
    /// Loads the allow-list from the store into a new gate.
    pub fn open(store: Arc<Store>) -> Result<Arc<Devices>, DeviceError> {
        let devices = Devices { store, gate: PeerGate::allow_list(Vec::<PeerId>::new()) };
        let peers: Vec<PeerId> = devices.list()?.into_iter().map(|d| d.peer_id).collect();
        devices.gate.set_allowed(peers);
        Ok(Arc::new(devices))
    }

    /// The gate to serve the transport listener with ([`ember_transport::http::HttpListener::with_gate`]).
    pub fn gate(&self) -> &PeerGate {
        &self.gate
    }

    pub fn list(&self) -> Result<Vec<Device>, DeviceError> {
        let conn = self.store.conn();
        let mut stmt =
            conn.prepare("SELECT peer_id, name, created_at, source FROM devices ORDER BY created_at, name")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (peer, name, created_at, source) = row?;
            match peer.parse::<PeerId>() {
                Ok(peer_id) => out.push(Device { peer_id, name, created_at, source }),
                Err(e) => tracing::warn!("ignoring device row with invalid peer id {peer:?}: {e}"),
            }
        }
        Ok(out)
    }

    /// Allows `peer`. Adding an existing peer renames it (and makes it a hand-added row, which a
    /// hub sync leaves alone).
    pub fn add(&self, peer: PeerId, name: &str) -> Result<Device, DeviceError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DeviceError::BadRequest("device name must not be empty".into()));
        }
        let d = Device { peer_id: peer, name: name.to_string(), created_at: now_ms(), source: SOURCE_LOCAL.into() };
        self.store.conn().execute(
            "INSERT INTO devices (peer_id, name, created_at, source) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(peer_id) DO UPDATE SET name = ?2, source = ?4",
            params![peer.to_string(), d.name, d.created_at, SOURCE_LOCAL],
        )?;
        self.gate.allow(peer);
        tracing::info!(peer = %peer.fmt_short(), name = %d.name, "device allowed");
        Ok(d)
    }

    /// Makes the `hub` rows equal to `listed` (the hub account's devices, without this server):
    /// adds missing ones, renames, and revokes `hub` rows no longer listed. Hand-added rows are
    /// not changed (a listed peer that already has a local row stays local).
    pub fn sync_from_hub(&self, listed: &[(PeerId, String)]) -> Result<SyncReport, DeviceError> {
        let current = self.list()?;
        let mut report = SyncReport::default();
        for (peer, name) in listed {
            let name = name.trim();
            let name = if name.is_empty() { "hub device" } else { name };
            match current.iter().find(|d| d.peer_id == *peer) {
                Some(d) if d.source != SOURCE_HUB => continue,
                Some(d) if d.name == name => continue,
                Some(_) => {
                    self.store.conn().execute(
                        "UPDATE devices SET name = ?2 WHERE peer_id = ?1 AND source = 'hub'",
                        params![peer.to_string(), name],
                    )?;
                }
                None => {
                    self.store.conn().execute(
                        "INSERT INTO devices (peer_id, name, created_at, source) VALUES (?1, ?2, ?3, 'hub')
                         ON CONFLICT(peer_id) DO NOTHING",
                        params![peer.to_string(), name, now_ms()],
                    )?;
                    self.gate.allow(*peer);
                    report.added.push(*peer);
                }
            }
        }
        for d in current.iter().filter(|d| d.source == SOURCE_HUB) {
            if !listed.iter().any(|(p, _)| *p == d.peer_id) {
                self.store
                    .conn()
                    .execute("DELETE FROM devices WHERE peer_id = ?1 AND source = 'hub'", params![d.peer_id.to_string()])?;
                report.closed += self.gate.revoke(&d.peer_id);
                report.removed.push(d.peer_id);
                tracing::info!(peer = %d.peer_id.fmt_short(), "device removed on the hub; revoked here");
            }
        }
        Ok(report)
    }

    /// Revokes `peer`: removes it and closes its open connections. Returns how many
    /// connections were closed.
    pub fn remove(&self, peer: &PeerId) -> Result<usize, DeviceError> {
        let n = self.store.conn().execute("DELETE FROM devices WHERE peer_id = ?1", params![peer.to_string()])?;
        // Revoke on the gate even if the row was already gone, so a stale live connection
        // cannot survive.
        let closed = self.gate.revoke(peer);
        if n == 0 {
            return Err(DeviceError::NotFound(peer.to_string()));
        }
        tracing::info!(peer = %peer.fmt_short(), closed, "device revoked");
        Ok(closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_transport::SecretKey;

    #[test]
    fn add_list_remove_feed_the_gate() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let a = SecretKey::generate().peer_id();
        let b = SecretKey::generate().peer_id();
        {
            let d = Devices::open(store.clone()).unwrap();
            assert!(!d.gate().is_allowed(&a));
            d.add(a, "phone").unwrap();
            d.add(b, "laptop").unwrap();
            d.add(b, "work laptop").unwrap();
            assert!(d.gate().is_allowed(&a) && d.gate().is_allowed(&b));
            assert!(matches!(d.add(a, " "), Err(DeviceError::BadRequest(_))));
        }
        // A new gate is loaded from the table.
        let d = Devices::open(store).unwrap();
        let list = d.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().find(|x| x.peer_id == b).unwrap().name, "work laptop");
        assert!(d.gate().is_allowed(&b));
        d.remove(&b).unwrap();
        assert!(!d.gate().is_allowed(&b));
        assert!(matches!(d.remove(&b), Err(DeviceError::NotFound(_))));
        assert_eq!(d.list().unwrap().len(), 1);
    }

    #[test]
    fn hub_sync_adds_renames_and_revokes_only_hub_rows() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let d = Devices::open(store).unwrap();
        let (mine, h1, h2) =
            (SecretKey::generate().peer_id(), SecretKey::generate().peer_id(), SecretKey::generate().peer_id());
        d.add(mine, "by hand").unwrap();

        let r = d.sync_from_hub(&[(h1, "phone".into()), (h2, "tablet".into()), (mine, "renamed on hub".into())]).unwrap();
        assert_eq!(r.added, vec![h1, h2]);
        assert!(d.gate().is_allowed(&h1) && d.gate().is_allowed(&h2));
        let list = d.list().unwrap();
        let by = |p: PeerId| list.iter().find(|x| x.peer_id == p).unwrap().clone();
        assert_eq!((by(mine).name.as_str(), by(mine).source.as_str()), ("by hand", SOURCE_LOCAL));
        assert_eq!(by(h1).source, SOURCE_HUB);

        // h2 removed on the hub, h1 renamed, and the hand-added row is not listed at all.
        let r = d.sync_from_hub(&[(h1, "my phone".into())]).unwrap();
        assert_eq!(r.removed, vec![h2]);
        assert!(r.added.is_empty());
        assert!(!d.gate().is_allowed(&h2));
        assert!(d.gate().is_allowed(&mine), "hand-added devices survive a sync");
        assert_eq!(d.list().unwrap().iter().find(|x| x.peer_id == h1).unwrap().name, "my phone");
    }
}
