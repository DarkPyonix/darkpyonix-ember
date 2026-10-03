//! Per-user preferences kept on this device: the last-used new-session combination per project
//! (FR-L8), the remembered "Open IDE" target (FR-L7), and pins, archive marks and renames
//! (FR-L9).
//!
//! TODO(server FR-S4): pins, archive and renames belong on the main server so every client
//! sees them; it has no API for them yet, so they are local until it does.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const PREFS_FORMAT: u32 = 1;

/// The combination a new session in a project starts from (FR-L8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastUsed {
    pub agent: String,
    /// `None`: let the server's router choose.
    #[serde(default)]
    pub account: Option<String>,
    /// Computer id; `None`: the session's default (the main server itself).
    #[serde(default)]
    pub computer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default = "format_default")]
    pub format: u32,
    #[serde(default)]
    pub last_used: BTreeMap<String, LastUsed>,
    /// The project selected on the main screen last time.
    #[serde(default)]
    pub last_project: Option<String>,
    /// `ember`, `vscode` or `gateway`.
    #[serde(default)]
    pub ide_target: Option<String>,
    #[serde(default)]
    pub pinned: BTreeSet<String>,
    #[serde(default)]
    pub archived: BTreeSet<String>,
    /// Local titles, by session id.
    #[serde(default)]
    pub titles: BTreeMap<String, String>,
}

fn format_default() -> u32 {
    PREFS_FORMAT
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            format: PREFS_FORMAT,
            last_used: BTreeMap::new(),
            last_project: None,
            ide_target: None,
            pinned: BTreeSet::new(),
            archived: BTreeSet::new(),
            titles: BTreeMap::new(),
        }
    }
}

impl Prefs {
    pub fn toggle_pin(&mut self, id: &str) {
        if !self.pinned.remove(id) {
            self.pinned.insert(id.to_string());
        }
    }

    pub fn toggle_archive(&mut self, id: &str) {
        if !self.archived.remove(id) {
            self.archived.insert(id.to_string());
            // An archived session is out of the way; it is not also pinned to the top.
            self.pinned.remove(id);
        }
    }

    /// An empty title clears the local rename.
    pub fn rename(&mut self, id: &str, title: &str) {
        let t = title.trim();
        if t.is_empty() {
            self.titles.remove(id);
        } else {
            self.titles.insert(id.to_string(), t.to_string());
        }
    }
}

/// Read prefs; a missing or unreadable file is the defaults (prefs never block start-up).
pub fn load(path: &Path) -> Prefs {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Prefs>(&bytes) {
            Ok(p) if p.format == PREFS_FORMAT => p,
            Ok(p) => {
                tracing::info!(format = p.format, "ignoring prefs in another format");
                Prefs::default()
            }
            Err(e) => {
                tracing::warn!("ignoring unreadable prefs {}: {e}", path.display());
                Prefs::default()
            }
        },
        Err(_) => Prefs::default(),
    }
}

/// Write atomically (temp file + rename), like the client cache.
pub fn save(path: &Path, prefs: &Prefs) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(prefs).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l8_last_used_round_trips_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/prefs.json");
        assert_eq!(load(&path), Prefs::default());
        let mut p = Prefs::default();
        p.last_used.insert(
            "ember".into(),
            LastUsed { agent: "codex".into(), account: Some("a1".into()), computer: Some("c1".into()), model: Some("o3".into()), cwd: "/w".into() },
        );
        p.ide_target = Some("vscode".into());
        save(&path, &p).unwrap();
        assert_eq!(load(&path), p);
        std::fs::write(&path, b"{nope").unwrap();
        assert_eq!(load(&path), Prefs::default());
    }

    #[test]
    fn fr_l9_pin_archive_rename() {
        let mut p = Prefs::default();
        p.toggle_pin("s");
        assert!(p.pinned.contains("s"));
        p.toggle_archive("s");
        assert!(p.archived.contains("s") && !p.pinned.contains("s"));
        p.toggle_archive("s");
        assert!(!p.archived.contains("s"));
        p.rename("s", "  New name ");
        assert_eq!(p.titles["s"], "New name");
        p.rename("s", " ");
        assert!(!p.titles.contains_key("s"));
    }
}
