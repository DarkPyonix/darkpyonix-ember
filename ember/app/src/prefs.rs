//! Per-device UI choices: the last-used new-session combination per project (FR-L8), the
//! project selected last, and the remembered "Open IDE" target per project (FR-L7, FR-W7b).
//!
//! Pins, archive marks and titles (FR-L9) and computer assignments (FR-L4) live on the main
//! server so every client sees them (`PATCH /sessions/{id}`, `/projects/…/computers/…`). Older
//! prefs files carried `pinned`, `archived` and `titles`; those keys are ignored when read and
//! dropped on the next save. Older files also carried one global `ide_target`; since D56 the
//! remembered IDE is per project (`ide_targets`), so the old key is ignored when read (it names no
//! project to file it under) and dropped on the next save.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const PREFS_FORMAT: u32 = 1;

/// The combination a new session in a project starts from (FR-L8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastUsed {
    pub agent: String,
    /// Legacy field kept for backwards-compatible deserialization; accounts now come from the
    /// harness profile (FR-L8, FR-L17).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Computer id; `None`: the session's default (the main server itself).
    #[serde(default)]
    pub computer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Effort key (e.g. `"High"`, `"Medium"`, `"Low"`) from the model picker (FR-L15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Permission mode key (`"ask"`, `"edits"`, `"bypass"`, `"plan"`) from the mode picker (FR-L16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
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
    /// The IDE last picked in the chooser, per project (FR-W7b, INTENT D56): project name to
    /// `ember`, `vscode` or `gateway`. A project with no entry follows the computer's effective
    /// `default_ide` ([`crate::ide::effective_default_ide`]).
    #[serde(default)]
    pub ide_targets: BTreeMap<String, String>,
    /// System, Light or Dark (FR-L28). Older files have none, which reads as System.
    #[serde(default)]
    pub theme: crate::ui::theme::ThemePref,
    /// The session screen's view: `multi`, `single` or `status` (FR-L18).
    #[serde(default)]
    pub session_view: Option<String>,
    /// Pane tabs per project (FR-L18).
    #[serde(default)]
    pub panes: BTreeMap<String, crate::ui::session::ProjectPanes>,
    /// Team character style per project id (FR-L22). Absent means the default, 몽글.
    /// TODO(server): the project stores its own style once the server has the field.
    #[serde(default)]
    pub character_styles: BTreeMap<String, crate::ui::characters::CharStyle>,
    /// Per-member character overrides per project id (FR-L22, #489). Fallback when the server has
    /// none for the project.
    #[serde(default)]
    pub character_overrides: BTreeMap<String, BTreeMap<String, ember_client::wire::CharacterOverride>>,
    /// The editor's shortcut set (FR-L30). Older files have none, which reads as VS Code.
    #[serde(default)]
    pub keymap: crate::ui::keymap::Keymap,
    /// The editor's auto-save (Code-OSS `files.autoSave`, FR-L31.6). Older files have none, which reads
    /// as off, Code-OSS's default.
    #[serde(default)]
    pub auto_save: crate::ui::editor_states::AutoSave,
    /// How many days the editor keeps a hot exit backup of unsaved buffers (FR-L31.6a); an older
    /// one is removed when an editor window opens. Older files have none, which reads as 30.
    #[serde(default = "hot_exit_backup_days_default")]
    pub hot_exit_backup_days: u32,
    /// The editor window's zoom level, Code-OSS's `window.zoomLevel` (FR-L31.15). Older files
    /// have none, which reads as 0 (the system's size).
    #[serde(default)]
    pub editor_zoom_level: i8,
    /// The editor's workspace trust answers (FR-L31.18), keyed by
    /// [`crate::ui::workspace_trust::trust_key`] (computer and folder). `true` is trusted, `false`
    /// restricted mode. Older files have none: every folder asks once.
    #[serde(default)]
    pub workspace_trust: BTreeMap<String, bool>,
    /// Extension ids disabled in the Ember 에디터, per computer id (FR-L31.14). Kept on the device
    /// because Code-OSS keeps its disabled list in the workbench's own storage, not on the server.
    /// Ids are stored in lower case and sorted.
    #[serde(default)]
    pub disabled_extensions: BTreeMap<String, Vec<String>>,
    /// The main server's peer address, as typed in 설정 (FR-N9): a peer id,
    /// `<peer id>@<ip>:<port>`, or the `PeerAddr` JSON the server prints. `None` (older files):
    /// plain HTTP as before. Read at start (`config::server_choice`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_peer: Option<String>,
    /// The relay for that reach, as `EMBER_RELAY_URL` takes it. `None`: the environment's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_relay: Option<String>,
}

fn format_default() -> u32 {
    PREFS_FORMAT
}

fn hot_exit_backup_days_default() -> u32 {
    crate::editor_backup::DEFAULT_BACKUP_DAYS
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            format: PREFS_FORMAT,
            last_used: BTreeMap::new(),
            last_project: None,
            ide_targets: BTreeMap::new(),
            theme: crate::ui::theme::ThemePref::default(),
            session_view: None,
            panes: BTreeMap::new(),
            character_styles: BTreeMap::new(),
            character_overrides: BTreeMap::new(),
            keymap: crate::ui::keymap::Keymap::default(),
            auto_save: crate::ui::editor_states::AutoSave::default(),
            hot_exit_backup_days: hot_exit_backup_days_default(),
            editor_zoom_level: 0,
            workspace_trust: BTreeMap::new(),
            disabled_extensions: BTreeMap::new(),
            server_peer: None,
            server_relay: None,
        }
    }
}

impl Prefs {
    /// The project's team character style; 몽글 when none was chosen.
    pub fn character_style(&self, project: &str) -> crate::ui::characters::CharStyle {
        self.character_styles
            .get(project)
            .copied()
            .unwrap_or_default()
    }

    /// The style this device remembers for the project, if any (the fallback of FR-L26).
    pub fn saved_character_style(&self, project: &str) -> Option<crate::ui::characters::CharStyle> {
        self.character_styles.get(project).copied()
    }

    /// The per-member character overrides this device remembers for `project` (FR-L22, #489).
    pub fn saved_character_overrides(
        &self,
        project: &str,
    ) -> Option<&BTreeMap<String, ember_client::wire::CharacterOverride>> {
        self.character_overrides.get(project)
    }

    pub fn set_character_overrides(
        &mut self,
        project: &str,
        overrides: BTreeMap<String, ember_client::wire::CharacterOverride>,
    ) {
        if overrides.is_empty() {
            self.character_overrides.remove(project);
        } else {
            self.character_overrides.insert(project.to_string(), overrides);
        }
    }

    /// A rename moves the remembered style to the new name (FR-L26).
    pub fn move_character_style(&mut self, old: &str, new: &str) {
        if old != new {
            if let Some(st) = self.character_styles.remove(old) {
                self.character_styles.entry(new.to_string()).or_insert(st);
            }
            if let Some(ov) = self.character_overrides.remove(old) {
                self.character_overrides.entry(new.to_string()).or_insert(ov);
            }
        }
    }

    /// A rename moves every per-project entry to the new name (FR-L26, #412): last-used
    /// combination, pane tabs, character style and the last selected project. If the new name
    /// already has an entry, it is kept and the old one is dropped, because the new key is the
    /// one this device used most recently under that name.
    pub fn rename_project_keys(&mut self, old: &str, new: &str) {
        if old == new {
            return;
        }
        if let Some(v) = self.last_used.remove(old) {
            self.last_used.entry(new.to_string()).or_insert(v);
        }
        if let Some(v) = self.panes.remove(old) {
            self.panes.entry(new.to_string()).or_insert(v);
        }
        if let Some(v) = self.ide_targets.remove(old) {
            self.ide_targets.entry(new.to_string()).or_insert(v);
        }
        self.move_character_style(old, new);
        if self.last_project.as_deref() == Some(old) {
            self.last_project = Some(new.to_string());
        }
    }

    /// The IDE the user last picked in `project` (FR-W7b); `None` when none was picked there.
    pub fn picked_ide(&self, project: &str) -> Option<crate::ide::IdeKind> {
        self.ide_targets.get(project).and_then(|k| crate::ide::IdeKind::from_key(k))
    }

    /// The IDE every "IDE 열기" header launches for a session of `project` (FR-L7, FR-W7b): the
    /// project's pick, else `default_ide`, the session's computer's effective default
    /// ([`crate::ide::effective_default_ide`] of the header's IDE listing). `None`: the button
    /// opens the chooser.
    pub fn ide_for(&self, project: &str, default_ide: Option<crate::ide::IdeKind>) -> Option<crate::ide::IdeKind> {
        self.picked_ide(project).or(default_ide)
    }

    /// Record a pick in the chooser as `project`'s IDE (FR-W7b). A session with no known project
    /// (an empty name) records nothing, so one stray pick never becomes every such session's.
    pub fn set_ide_target(&mut self, project: &str, kind: crate::ide::IdeKind) {
        if project.is_empty() {
            return;
        }
        self.ide_targets.insert(project.to_string(), kind.key().to_string());
    }

    pub fn set_character_style(&mut self, project: &str, style: crate::ui::characters::CharStyle) {
        self.character_styles.insert(project.to_string(), style);
    }

    /// The extension ids disabled on `computer` (FR-L31.14), in lower case.
    pub fn disabled_extensions_for(&self, computer: &str) -> Vec<String> {
        self.disabled_extensions.get(computer).cloned().unwrap_or_default()
    }

    /// Enables or disables one extension on `computer`. Ids compare without regard to case; a
    /// computer with nothing disabled keeps no entry.
    pub fn set_extension_enabled(&mut self, computer: &str, id: &str, enabled: bool) {
        let key = id.trim().to_lowercase();
        if key.is_empty() {
            return;
        }
        let list = self.disabled_extensions.entry(computer.to_string()).or_default();
        list.retain(|d| *d != key);
        if !enabled {
            list.push(key);
            list.sort();
        }
        if list.is_empty() {
            self.disabled_extensions.remove(computer);
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
            LastUsed {
                agent: "codex".into(),
                account: Some("a1".into()),
                computer: Some("c1".into()),
                model: Some("o3".into()),
                effort: Some("Medium".into()),
                mode: Some("edits".into()),
                cwd: "/w".into(),
            },
        );
        p.set_ide_target("ember", crate::ide::IdeKind::VsCode);
        save(&path, &p).unwrap();
        assert_eq!(load(&path), p);
        std::fs::write(&path, b"{nope").unwrap();
        assert_eq!(load(&path), Prefs::default());
    }

    #[test]
    fn fr_l8_last_used_round_trips_effort_and_mode_and_loads_older_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");

        // Older prefs files lack `effort` and `mode` and still carry `account`.
        std::fs::write(
            &path,
            br#"{"format":1,"last_used":{"ember":{"agent":"codex","account":"a1","computer":"c1","model":"gpt-5","cwd":"/w"}}}"#,
        )
        .unwrap();
        let old = load(&path);
        let old_entry = old.last_used.get("ember").expect("ember entry");
        assert_eq!(old_entry.agent, "codex");
        assert_eq!(old_entry.account.as_deref(), Some("a1"));
        assert_eq!(old_entry.model.as_deref(), Some("gpt-5"));
        assert_eq!(old_entry.effort, None);
        assert_eq!(old_entry.mode, None);

        // New prefs round-trip `effort` and `mode` and omit `account` when `None`.
        let mut p = Prefs::default();
        p.last_used.insert(
            "ember".into(),
            LastUsed {
                agent: "claude-code".into(),
                account: None,
                computer: None,
                model: Some("opus".into()),
                effort: Some("High".into()),
                mode: Some("plan".into()),
                cwd: "/work".into(),
            },
        );
        save(&path, &p).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"effort\": \"High\""), "effort persisted: {raw}");
        assert!(raw.contains("\"mode\": \"plan\""), "mode persisted: {raw}");
        assert!(!raw.contains("\"account\""), "None account omitted: {raw}");
        assert_eq!(load(&path), p);
    }

    #[test]
    fn fr_l31_15_editor_zoom_level_persists_and_defaults_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, br#"{"format":1}"#).unwrap();
        assert_eq!(load(&path).editor_zoom_level, 0);
        let p = Prefs {
            editor_zoom_level: -2,
            ..Prefs::default()
        };
        save(&path, &p).unwrap();
        assert_eq!(load(&path).editor_zoom_level, -2);
    }

    #[test]
    fn fr_l28_theme_choice_persists_and_defaults_to_system() {
        use crate::ui::theme::ThemePref;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, br#"{"format":1}"#).unwrap();
        assert_eq!(load(&path).theme, ThemePref::System);
        let p = Prefs {
            theme: ThemePref::Dark,
            ..Prefs::default()
        };
        save(&path, &p).unwrap();
        assert_eq!(load(&path).theme, ThemePref::Dark);
    }

    // FR-L22: the project character style is kept per device, default 몽글.
    #[test]
    fn fr_l22_character_style_round_trips_per_project() {
        use crate::ui::characters::CharStyle;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, br#"{"format":1}"#).unwrap();
        assert_eq!(load(&path).character_style("ember"), CharStyle::Mongle);
        let mut p = Prefs::default();
        p.set_character_style("ember", CharStyle::Crew);
        save(&path, &p).unwrap();
        let back = load(&path);
        assert_eq!(back.character_style("ember"), CharStyle::Crew);
        assert_eq!(back.character_style("other"), CharStyle::Mongle);
        back.clone().set_character_style("ember", CharStyle::Mongle);
    }

    #[test]
    fn older_prefs_with_local_pins_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(
            &path,
            br#"{"format":1,"ide_target":"ember","pinned":["s"],"archived":["t"],"titles":{"s":"x"}}"#,
        )
        .unwrap();
        let p = load(&path);
        // The global `ide_target` of older files is ignored (FR-W7b).
        assert!(p.ide_targets.is_empty());
        save(&path, &p).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("pinned"), "dropped on save");
        assert!(!saved.contains("\"ide_target\""), "dropped on save");
    }

    /// SPEC FR-W7b, INTENT D56: the IDE picked in a project is that project's default only. After
    /// a pick of Gateway in project A, A launches Gateway directly and B, with no pick and no
    /// effective `default_ide` (no listing yet), opens the chooser.
    #[test]
    fn fr_w7b_the_remembered_ide_is_per_project() {
        use crate::ide::IdeKind;
        use crate::ui::cr::session::{IdeClick, ide_click};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let mut p = Prefs::default();
        assert_eq!(p.ide_for("A", None), None);
        p.set_ide_target("A", IdeKind::Gateway);
        save(&path, &p).unwrap();
        let p = load(&path);
        assert_eq!(p.picked_ide("A"), Some(IdeKind::Gateway));
        // The pick wins over the computer's default.
        assert_eq!(p.ide_for("A", Some(IdeKind::VsCode)), Some(IdeKind::Gateway));
        assert_eq!(ide_click(p.ide_for("A", None)), IdeClick::Launch(IdeKind::Gateway));
        assert_eq!(p.picked_ide("B"), None);
        assert_eq!(p.ide_for("B", None), None);
        assert_eq!(ide_click(p.ide_for("B", None)), IdeClick::Choose);
        // A later pick in A replaces A's only.
        let mut p = p;
        p.set_ide_target("A", IdeKind::VsCode);
        p.set_ide_target("B", IdeKind::Ember);
        assert_eq!(p.ide_for("A", None), Some(IdeKind::VsCode));
        assert_eq!(p.ide_for("B", None), Some(IdeKind::Ember));
        // A session with no project records nothing.
        p.set_ide_target("", IdeKind::Gateway);
        assert!(!p.ide_targets.contains_key(""));
        // A renamed project keeps its pick (FR-L26).
        p.rename_project_keys("A", "A2");
        assert_eq!(p.picked_ide("A2"), Some(IdeKind::VsCode));
        assert_eq!(p.picked_ide("A"), None);
        // An unknown key in the file is no pick.
        p.ide_targets.insert("C".into(), "cursor".into());
        assert_eq!(p.ide_for("C", None), None);
    }

    /// SPEC FR-W7b: a prefs file from before the per-project map (with or without the old global
    /// `ide_target`) loads with no picks, keeping its other choices.
    #[test]
    fn fr_w7b_old_prefs_without_the_map_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, br#"{"format":1,"ide_target":"gateway","last_project":"ember"}"#).unwrap();
        let p = load(&path);
        assert!(p.ide_targets.is_empty());
        assert_eq!(p.last_project.as_deref(), Some("ember"));
        assert_eq!(p.ide_for("ember", None), None);
        std::fs::write(&path, br#"{"format":1,"last_project":"ember"}"#).unwrap();
        let p = load(&path);
        assert!(p.ide_targets.is_empty());
        assert_eq!(p.last_project.as_deref(), Some("ember"));
        // The map round-trips.
        let mut p = p;
        p.set_ide_target("ember", crate::ide::IdeKind::Gateway);
        save(&path, &p).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"ide_targets\""));
        assert_eq!(load(&path), p);
    }

    #[test]
    fn fr_l26_prefs_key_moves_on_rename() {
        use crate::ui::characters::CharStyle;
        let mut p = Prefs::default();
        p.set_character_style("old", CharStyle::Crew);
        p.move_character_style("old", "new");
        assert_eq!(p.saved_character_style("new"), Some(CharStyle::Crew));
        assert_eq!(p.saved_character_style("old"), None);
        p.move_character_style("missing", "x");
        assert_eq!(p.saved_character_style("x"), None);
    }

    #[test]
    fn fr_l26_rename_moves_last_used() {
        let mut p = Prefs::default();
        p.last_used.insert("old".into(), LastUsed { agent: "claude".into(), ..Default::default() });
        p.rename_project_keys("old", "new");
        assert_eq!(p.last_used.get("new").map(|l| l.agent.as_str()), Some("claude"));
        assert!(!p.last_used.contains_key("old"));
    }

    #[test]
    fn fr_l26_rename_moves_pane_tabs() {
        use crate::ui::session::ProjectPanes;
        let mut p = Prefs::default();
        p.panes.insert("old".into(), ProjectPanes { tabs: Vec::new(), active: 3 });
        p.rename_project_keys("old", "new");
        assert_eq!(p.panes.get("new").map(|v| v.active), Some(3));
        assert!(!p.panes.contains_key("old"));
    }

    #[test]
    fn fr_l26_rename_moves_character_style_and_last_project() {
        use crate::ui::characters::CharStyle;
        let mut p = Prefs::default();
        p.set_character_style("old", CharStyle::Crew);
        p.last_project = Some("old".into());
        p.rename_project_keys("old", "new");
        assert_eq!(p.saved_character_style("new"), Some(CharStyle::Crew));
        assert_eq!(p.saved_character_style("old"), None);
        assert_eq!(p.last_project.as_deref(), Some("new"));
    }

    #[test]
    fn fr_l31_14_disabled_extensions_round_trip_per_computer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, br#"{"format":1}"#).unwrap();
        assert!(load(&path).disabled_extensions_for("local").is_empty(), "older files have none");
        let mut p = Prefs::default();
        p.set_extension_enabled("local", "RedHat.Java", false);
        p.set_extension_enabled("local", " golang.go ", false);
        p.set_extension_enabled("local", "redhat.java", false);
        p.set_extension_enabled("mini", "a.b", false);
        p.set_extension_enabled("local", "", false);
        save(&path, &p).unwrap();
        let back = load(&path);
        assert_eq!(back.disabled_extensions_for("local"), ["golang.go", "redhat.java"]);
        assert_eq!(back.disabled_extensions_for("mini"), ["a.b"]);
        assert!(back.disabled_extensions_for("other").is_empty());
        let mut back = back;
        back.set_extension_enabled("local", "REDHAT.java", true);
        back.set_extension_enabled("mini", "a.b", true);
        back.set_extension_enabled("mini", "never.disabled", true);
        assert_eq!(back.disabled_extensions_for("local"), ["golang.go"]);
        assert!(!back.disabled_extensions.contains_key("mini"), "an empty list keeps no entry");
    }

    #[test]
    fn fr_n9_server_peer_and_relay_persist_and_older_files_have_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(&path, br#"{"format":1}"#).unwrap();
        let old = load(&path);
        assert_eq!((old.server_peer.as_deref(), old.server_relay.as_deref()), (None, None));
        let p = Prefs {
            server_peer: Some("ab@127.0.0.1:1".into()),
            server_relay: Some("none".into()),
            ..Prefs::default()
        };
        save(&path, &p).unwrap();
        assert_eq!(load(&path), p);
        save(&path, &Prefs::default()).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("server_peer"), "unset is left out");
    }

    #[test]
    fn fr_l26_rename_keeps_existing_new_entry() {
        use crate::ui::session::ProjectPanes;
        let mut p = Prefs::default();
        p.panes.insert("old".into(), ProjectPanes { tabs: Vec::new(), active: 1 });
        p.panes.insert("new".into(), ProjectPanes { tabs: Vec::new(), active: 2 });
        p.rename_project_keys("old", "new");
        assert_eq!(p.panes.get("new").map(|v| v.active), Some(2));
        assert!(!p.panes.contains_key("old"));
        p.last_project = Some("other".into());
        p.rename_project_keys("old", "new");
        assert_eq!(p.last_project.as_deref(), Some("other"));
    }
}
