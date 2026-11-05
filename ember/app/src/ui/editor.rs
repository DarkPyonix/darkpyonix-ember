//! Editor target and tab state.

use dioxus_compose::prelude::*;


/// What the editor was opened for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorTarget {
    pub project: String,
    /// The project's folder on its computer, when the server said (`IdeLaunch.folder`).
    pub folder: String,
    pub endpoint: String,
    pub token: String,
}

/// The status bar's cursor position, "N행 M열".
pub fn position_text(line: usize, column: usize) -> String {
    format!("{line}행 {column}열")
}

/// The last path segment.
pub fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The open files. A tab holds its unsaved mark, so closing a tab drops it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorTabs {
    tabs: Vec<(String, bool)>,
    current: Option<usize>,
}

impl EditorTabs {
    fn index(&self, path: &str) -> Option<usize> {
        self.tabs.iter().position(|(p, _)| p == path)
    }

    /// Open a file, or activate it when it is already open.
    pub fn open(&mut self, path: &str) {
        let i = match self.index(path) {
            Some(i) => i,
            None => {
                self.tabs.push((path.to_string(), false));
                self.tabs.len() - 1
            }
        };
        self.current = Some(i);
    }

    pub fn activate(&mut self, path: &str) {
        if let Some(i) = self.index(path) {
            self.current = Some(i);
        }
    }

    /// Close a tab. Closing the current one activates the tab that takes its place, else the
    /// one before it; closing another one keeps the current tab.
    pub fn close(&mut self, path: &str) {
        let Some(i) = self.index(path) else { return };
        self.tabs.remove(i);
        self.current = match self.current {
            _ if self.tabs.is_empty() => None,
            Some(c) if c == i => Some(i.min(self.tabs.len() - 1)),
            Some(c) if c > i => Some(c - 1),
            other => other,
        };
    }

    pub fn set_dirty(&mut self, path: &str, dirty: bool) {
        if let Some(i) = self.index(path) {
            self.tabs[i].1 = dirty;
        }
    }

    pub fn is_dirty(&self, path: &str) -> bool {
        self.index(path).is_some_and(|i| self.tabs[i].1)
    }

    pub fn has_unsaved(&self) -> bool {
        self.tabs.iter().any(|(_, d)| *d)
    }

    pub fn paths(&self) -> Vec<&str> {
        self.tabs.iter().map(|(p, _)| p.as_str()).collect()
    }

    pub fn current_path(&self) -> Option<&str> {
        self.current.map(|i| self.tabs[i].0.as_str())
    }
}

#[component]
pub fn EditorScreen(target: EditorTarget) -> Element {
    super::editor_workbench::screen(target)
}

