//! Keymap actions of the Ember 에디터 window that are not a tab or tool-window toggle
//! (FR-L31.15, #447), and the issue each action that is not built yet waits for.
//!
//! Code-OSS (OSE 1.139) decides what an action does. An action the window cannot do yet says so
//! with the issue that builds it, so the notice and the palette row name the same issue as
//! `docs/SPEC.md`.

use super::editor::{Side, Stripes, ToolWindow};
use super::keymap::Action;

/// The not-ready text, as the rest of the client writes it.
pub const NOT_READY: &str = "준비 중입니다";

/// How many closed editors 닫은 편집기 다시 열기 remembers. Code-OSS keeps 20
/// (`HistoryService.MAX_RECENTLY_CLOSED_EDITORS`).
pub const MAX_CLOSED_EDITORS: usize = 20;

/// The open issue an action waits for, when the window cannot do it yet. `None` for an action
/// the window does.
pub fn pending_issue(action: Action) -> Option<u32> {
    match action {
        // Code-OSS runs these through the debug service (Run Without Debugging, Stop).
        Action::RunConfig | Action::Stop => Some(445),
        // A second terminal needs the terminal list (FR-P6).
        Action::NewTerminal => Some(337),
        // The agent window on the right stripe.
        Action::Ask => Some(766),
        _ => None,
    }
}

/// The notice an action that is not ready posts: its label, the not-ready text and the issue.
pub fn not_ready_notice(action: Action) -> String {
    match pending_issue(action) {
        Some(n) => format!("{}: {NOT_READY} (#{n})", action.label()),
        None => format!("{}: {NOT_READY}", action.label()),
    }
}

/// The notice of a tool window with no panel yet (the right stripe). The notification centre
/// belongs with the extension notifications (#448); the agent window is #766.
pub fn tool_window_notice(w: ToolWindow) -> String {
    match w {
        ToolWindow::Notifications => format!("{}: {NOT_READY} (#448)", w.label()),
        ToolWindow::Agent => format!("{}: {NOT_READY} (#766)", w.label()),
        _ => format!("{}: {NOT_READY}", w.label()),
    }
}

/// The hint a disabled palette row shows in place of its chord.
pub fn not_ready_hint(action: Action) -> String {
    match pending_issue(action) {
        Some(n) => format!("준비 중 (#{n})"),
        None => "준비 중".to_string(),
    }
}

/// View: Toggle Maximized Panel (`workbench.action.toggleMaximizedPanel`). A hidden panel opens
/// (the terminal, the panel's default view) and is maximized; a shown one flips between
/// maximized and its own size. Returns the new maximized state.
pub fn toggle_maximized_panel(stripes: &mut Stripes, maximized: bool) -> bool {
    if stripes.selected(Side::Bottom).is_none() {
        stripes.toggle(ToolWindow::Terminal);
        return true;
    }
    !maximized
}

/// The zoom level after a zoom action (View: Zoom In, Zoom Out, Reset Zoom), one step of 1.2x
/// each, kept in the Renderer's range. `None` for any other action.
pub fn zoom_level_after(action: Action, level: i8) -> Option<i8> {
    let next = match action {
        Action::ZoomIn => level.saturating_add(1),
        Action::ZoomOut => level.saturating_sub(1),
        Action::ZoomReset => 0,
        _ => return None,
    };
    Some(compose_rust::zoom::clamp_level(next))
}

/// The editors closed most recently last, for 닫은 편집기 다시 열기
/// (`workbench.action.reopenClosedEditor`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClosedEditors {
    /// The path and the tab index it had when it closed.
    paths: Vec<(String, usize)>,
}

impl ClosedEditors {
    /// Remember a closed editor. Closing the same file again moves it to the top; the oldest
    /// drops off past [`MAX_CLOSED_EDITORS`].
    pub fn push(&mut self, path: &str) {
        self.push_at(path, usize::MAX);
    }

    /// Remember a closed editor with the tab index it had, so it reopens in its old place as in
    /// Code-OSS.
    pub fn push_at(&mut self, path: &str, index: usize) {
        self.paths.retain(|(p, _)| p != path);
        self.paths.push((path.to_string(), index));
        if self.paths.len() > MAX_CLOSED_EDITORS {
            self.paths.remove(0);
        }
    }

    /// The editor to reopen, skipping the ones `is_open` says are open again (Code-OSS does not
    /// reopen an editor that is already open).
    pub fn pop(&mut self, is_open: impl Fn(&str) -> bool) -> Option<String> {
        self.pop_at(is_open).map(|(p, _)| p)
    }

    /// [`ClosedEditors::pop`] with the tab index the editor had.
    pub fn pop_at(&mut self, is_open: impl Fn(&str) -> bool) -> Option<(String, usize)> {
        while let Some((p, i)) = self.paths.pop() {
            if !is_open(&p) {
                return Some((p, i));
            }
        }
        None
    }

    /// Forget the editors at or below a path that was deleted, so they are not reopened.
    pub fn forget_below(&mut self, path: &str) {
        self.paths.retain(|(p, _)| !super::editor::is_at_or_below(p, path));
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_15_pending_actions_name_their_issue() {
        assert_eq!(not_ready_notice(Action::RunConfig), "다시 실행: 준비 중입니다 (#445)");
        assert_eq!(not_ready_notice(Action::Stop), "중지: 준비 중입니다 (#445)");
        assert_eq!(not_ready_notice(Action::NewTerminal), "새 터미널: 준비 중입니다 (#337)");
        assert_eq!(not_ready_hint(Action::NewTerminal), "준비 중 (#337)");
        // Format Document is built by the language features (FR-L31.13), so it waits for nothing.
        assert_eq!(pending_issue(Action::Format), None);
        assert!(tool_window_notice(ToolWindow::Notifications).ends_with("(#448)"));
        assert!(tool_window_notice(ToolWindow::Agent).ends_with("(#766)"));
        assert_eq!(not_ready_hint(Action::Ask), "준비 중 (#766)");
        assert_eq!(not_ready_notice(Action::Ask), "에이전트에게 묻기: 준비 중입니다 (#766)");
        for a in Action::ALL {
            assert!(!not_ready_notice(a).contains('\u{2014}'));
        }
    }

    #[test]
    fn fr_l31_15_every_action_is_built_or_waits_for_an_issue() {
        // An action is done, or it waits for an issue.
        for a in Action::ALL {
            let done = super::super::editor::is_implemented(a);
            let waits = pending_issue(a).is_some();
            assert!(done != waits, "{a:?}");
        }
    }

    #[test]
    fn fr_l31_15_zoom_steps_and_resets_within_the_range() {
        use compose_rust::zoom::{MAX_ZOOM_LEVEL, MIN_ZOOM_LEVEL};
        assert_eq!(zoom_level_after(Action::ZoomIn, 0), Some(1));
        assert_eq!(zoom_level_after(Action::ZoomOut, 0), Some(-1));
        assert_eq!(zoom_level_after(Action::ZoomReset, 5), Some(0));
        assert_eq!(zoom_level_after(Action::ZoomIn, MAX_ZOOM_LEVEL), Some(MAX_ZOOM_LEVEL));
        assert_eq!(zoom_level_after(Action::ZoomOut, MIN_ZOOM_LEVEL), Some(MIN_ZOOM_LEVEL));
        assert_eq!(zoom_level_after(Action::Save, 3), None);
    }

    #[test]
    fn fr_l31_15_maximize_opens_a_hidden_panel_and_toggles_a_shown_one() {
        let mut s = Stripes::default();
        s.hide(Side::Bottom);
        assert!(toggle_maximized_panel(&mut s, false));
        assert_eq!(s.selected(Side::Bottom), Some(ToolWindow::Terminal));
        assert!(!toggle_maximized_panel(&mut s, true));
        assert!(toggle_maximized_panel(&mut s, false));
        assert_eq!(s.selected(Side::Bottom), Some(ToolWindow::Terminal));
    }

    #[test]
    fn fr_l31_15_reopen_closed_editor_is_most_recent_first_and_capped() {
        let mut c = ClosedEditors::default();
        c.push("/w/a");
        c.push("/w/b");
        c.push("/w/a");
        assert_eq!(c.pop(|_| false).as_deref(), Some("/w/a"));
        assert_eq!(c.pop(|_| false).as_deref(), Some("/w/b"));
        assert_eq!(c.pop(|_| false), None);
        for i in 0..30 {
            c.push(&format!("/w/{i}"));
        }
        let mut n = 0;
        while c.pop(|_| false).is_some() {
            n += 1;
        }
        assert_eq!(n, MAX_CLOSED_EDITORS);
        c.push("/w/x");
        c.push("/w/y");
        assert_eq!(c.pop(|p| p == "/w/y").as_deref(), Some("/w/x"));
        c.push("/w/d/one");
        c.push("/w/e");
        c.forget_below("/w/d");
        assert_eq!(c.pop(|_| false).as_deref(), Some("/w/e"));
        assert!(c.is_empty());
    }
}
