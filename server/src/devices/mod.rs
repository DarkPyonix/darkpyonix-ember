//! Devices allowed to reach this server over the transport (SPEC `FR-N3`).
//!
//! A device is a transport peer id (a client's Ed25519 key) with a name. The table is the
//! allow-list of the server's [`PeerGate`]: connections from peers not in it are closed at
//! accept, before any request is read. [`Devices::remove`] is revocation: the row goes and every
//! open connection of that peer is closed at once.
//!
//! Managing devices is a local, administrative action: the [`api`] router is served on the
//! server's TCP listener only, never over the transport (a device must not be able to add or
//! revoke devices). Registration through the darkpyonix.dev hub (`FR-N2`, GitHub sign-in) will
//! add rows here; it is out of scope for now.

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
        let mut stmt = conn.prepare("SELECT peer_id, name, created_at FROM devices ORDER BY created_at, name")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (peer, name, created_at) = row?;
            match peer.parse::<PeerId>() {
                Ok(peer_id) => out.push(Device { peer_id, name, created_at }),
                Err(e) => tracing::warn!("ignoring device row with invalid peer id {peer:?}: {e}"),
            }
        }
        Ok(out)
    }

    /// Allows `peer`. Adding an existing peer renames it.
    pub fn add(&self, peer: PeerId, name: &str) -> Result<Device, DeviceError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DeviceError::BadRequest("device name must not be empty".into()));
        }
        let d = Device { peer_id: peer, name: name.to_string(), created_at: now_ms() };
        self.store.conn().execute(
            "INSERT INTO devices (peer_id, name, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(peer_id) DO UPDATE SET name = ?2",
            params![peer.to_string(), d.name, d.created_at],
        )?;
        self.gate.allow(peer);
        tracing::info!(peer = %peer.fmt_short(), name = %d.name, "device allowed");
        Ok(d)
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
}
