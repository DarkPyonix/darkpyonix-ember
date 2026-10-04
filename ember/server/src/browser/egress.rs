//! A project's browser egress choice (FR-R1), and keeping it across server restarts.
//!
//! The choice is what the user picked (direct, a proxy URL, or a registered computer), not the
//! proxy URL Chrome is started with: a computer resolves to a loopback SOCKS5 listener in this
//! process ([`crate::computers::egress`]) whose port changes on every server start, so the
//! computer id is what is stored and it is resolved again each time Chrome starts.

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::store::{now_ms, Store};

/// Where a project's browser sends its traffic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Egress {
    /// The ember server's own network.
    #[default]
    Direct,
    /// A configured proxy (`socks5://`, `socks4://`, `http://`, `https://`; no credentials).
    Proxy { url: String },
    /// A registered computer's ember node (its `/v1/egress` SOCKS5 exit). The local computer
    /// (`"local"`) is [`Egress::Direct`].
    Computer { id: String },
}

impl Egress {
    /// `None` = direct, `Some(url)` = that proxy.
    pub fn from_proxy(proxy: Option<String>) -> Egress {
        match proxy {
            None => Egress::Direct,
            Some(url) => Egress::Proxy { url },
        }
    }

    /// The computer id, if this egress is a computer.
    pub fn computer(&self) -> Option<&str> {
        match self {
            Egress::Computer { id } => Some(id),
            _ => None,
        }
    }
}

/// Turns a computer id into the proxy URL Chrome uses. Implemented by
/// [`crate::computers::Computers`] (starting its loopback SOCKS5 listener on first use).
pub trait EgressResolver: Send + Sync {
    /// `Ok(None)`: the computer is this server (direct). `Err`: unknown computer, or the
    /// listener could not be started.
    fn proxy_for_computer(&self, computer_id: &str) -> anyhow::Result<Option<String>>;
}

/// Persists each project's [`Egress`].
pub trait EgressStore: Send + Sync {
    fn load(&self, project: &str) -> anyhow::Result<Option<Egress>>;
    /// [`Egress::Direct`] removes the record.
    fn save(&self, project: &str, egress: &Egress) -> anyhow::Result<()>;
}

/// Store migration 5 (`user_version` 4 → 5). See [`crate::store`] for the mechanism.
pub const MIGRATION: &str = "
-- FR-R1: a project's browser egress. No row = direct.
CREATE TABLE IF NOT EXISTS browser_egress (
    project     TEXT PRIMARY KEY,
    -- 'proxy' or 'computer'.
    kind        TEXT NOT NULL,
    -- The proxy URL, or the computer id.
    value       TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);
";

impl EgressStore for Store {
    fn load(&self, project: &str) -> anyhow::Result<Option<Egress>> {
        let row = self
            .conn()
            .query_row(
                "SELECT kind, value FROM browser_egress WHERE project = ?1",
                params![project],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(match row {
            None => None,
            Some((kind, value)) => Some(match kind.as_str() {
                "proxy" => Egress::Proxy { url: value },
                "computer" => Egress::Computer { id: value },
                other => anyhow::bail!("unknown browser egress kind {other:?} for {project}"),
            }),
        })
    }

    fn save(&self, project: &str, egress: &Egress) -> anyhow::Result<()> {
        let conn = self.conn();
        let (kind, value) = match egress {
            Egress::Direct => {
                conn.execute("DELETE FROM browser_egress WHERE project = ?1", params![project])?;
                return Ok(());
            }
            Egress::Proxy { url } => ("proxy", url.as_str()),
            Egress::Computer { id } => ("computer", id.as_str()),
        };
        conn.execute(
            "INSERT INTO browser_egress (project, kind, value, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project) DO UPDATE SET kind = ?2, value = ?3, updated_at = ?4",
            params![project, kind, value, now_ms()],
        )?;
        Ok(())
    }
}

/// Projects whose browser egress is `computer_id` (a computer in use cannot be removed).
pub fn projects_using_computer(store: &Store, computer_id: &str) -> anyhow::Result<Vec<String>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT project FROM browser_egress WHERE kind = 'computer' AND value = ?1 ORDER BY project",
    )?;
    let rows = stmt.query_map(params![computer_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// For tests and embedding without a database.
#[derive(Default)]
pub struct MemoryEgressStore(std::sync::Mutex<std::collections::HashMap<String, Egress>>);

impl EgressStore for MemoryEgressStore {
    fn load(&self, project: &str) -> anyhow::Result<Option<Egress>> {
        Ok(self.0.lock().unwrap().get(project).cloned())
    }

    fn save(&self, project: &str, egress: &Egress) -> anyhow::Result<()> {
        let mut m = self.0.lock().unwrap();
        match egress {
            Egress::Direct => m.remove(project),
            e => m.insert(project.to_string(), e.clone()),
        };
        Ok(())
    }
}

/// Shared handles, as the manager keeps them.
pub type SharedEgressStore = Arc<dyn EgressStore>;
pub type SharedEgressResolver = Arc<dyn EgressResolver>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_round_trip_and_serde() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.load("p").unwrap(), None);
        s.save("p", &Egress::Proxy { url: "socks5://127.0.0.1:1".into() }).unwrap();
        assert_eq!(s.load("p").unwrap(), Some(Egress::Proxy { url: "socks5://127.0.0.1:1".into() }));
        s.save("p", &Egress::Computer { id: "c1".into() }).unwrap();
        assert_eq!(s.load("p").unwrap(), Some(Egress::Computer { id: "c1".into() }));
        assert_eq!(projects_using_computer(&s, "c1").unwrap(), vec!["p".to_string()]);
        s.save("p", &Egress::Direct).unwrap();
        assert_eq!(s.load("p").unwrap(), None);

        let j = serde_json::to_value(Egress::Computer { id: "c1".into() }).unwrap();
        assert_eq!(j, serde_json::json!({ "kind": "computer", "id": "c1" }));
        let back: Egress = serde_json::from_value(serde_json::json!({ "kind": "direct" })).unwrap();
        assert_eq!(back, Egress::Direct);
    }
}
