//! Pure multi-window editor group model and tab detach gesture state machine.
//!
//! Keeps all editor windows within a single process: one primary (`Main`) window and zero or
//! more torn-off (`Auxiliary`) windows. Documents live in the shared editor session; each window
//! group owns its tab strip (`EditorTabs`) and per-tab view state (cursor, selection, scroll).

use std::collections::BTreeMap;

use crate::ui::editor::EditorTabs;

/// Distance (in dp) past the tab strip bounds required to trigger a tab detach.
pub const DETACH_THRESHOLD_DP: f64 = 48.0;

/// Chrome layout for an editor window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowChrome {
    /// Primary workbench window with explorer, panels, tab strip, editor, and status bar.
    Main,
    /// Torn-off auxiliary window with tab strip, editor area, and status bar only.
    Auxiliary,
}

/// Stable identifier for a window group within one editor process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowGroupId(pub u64);

/// 0-based cursor position matching the bridge wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    pub line: u32,
    pub column: u32,
}

/// Normalised selection range (`start <= end`) in 0-based coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: CursorPosition,
    pub end: CursorPosition,
}

/// Scroll offset in logical pixels / dp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollOffset {
    pub top: f64,
    pub left: f64,
}

/// Per-tab view state preserved across detach and merge operations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TabViewState {
    pub cursor: Option<CursorPosition>,
    pub selection: Option<SelectionRange>,
    pub scroll: Option<ScrollOffset>,
}

/// One editor window's tab strip, chrome role, and per-tab view state.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowGroup {
    pub id: WindowGroupId,
    pub restore_key: String,
    pub chrome: WindowChrome,
    pub tabs: EditorTabs,
    pub view_states: BTreeMap<String, TabViewState>,
}

impl WindowGroup {
    /// Index of the currently active tab in this group, if any.
    pub fn active_tab_index(&self) -> Option<usize> {
        let current = self.tabs.current_path()?;
        self.tabs.paths().iter().position(|p| *p == current)
    }

    /// Number of open tabs in this group.
    pub fn tab_count(&self) -> usize {
        self.tabs.paths().len()
    }

    /// Whether this group has no open tabs.
    pub fn is_empty(&self) -> bool {
        self.tabs.paths().is_empty()
    }

    /// View state for `path`, if recorded.
    pub fn view_state(&self, path: &str) -> Option<&TabViewState> {
        self.view_states.get(path)
    }

    /// Record or replace the view state for `path`.
    pub fn set_view_state(&mut self, path: impl Into<String>, state: TabViewState) {
        self.view_states.insert(path.into(), state);
    }
}

/// Multi-window editor model holding the primary window and any auxiliary windows.
///
/// Invariants:
/// - While open (`is_open()`), the primary (`Main`) window is always present as the first group.
/// - Closing the primary window closes all auxiliary windows.
/// - Closing the last tab in an auxiliary window closes that auxiliary window.
/// - Closing the last tab in the primary window keeps the primary window open and empty.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorWindows {
    groups: Vec<WindowGroup>,
    next_id: u64,
}

impl EditorWindows {
    /// Create a multi-window model with a single primary (`Main`) window.
    pub fn new(main_restore_key: impl Into<String>) -> Self {
        let main = WindowGroup {
            id: WindowGroupId(1),
            restore_key: main_restore_key.into(),
            chrome: WindowChrome::Main,
            tabs: EditorTabs::default(),
            view_states: BTreeMap::new(),
        };
        Self {
            groups: vec![main],
            next_id: 2,
        }
    }

    /// Whether the editor session still has an open window.
    pub fn is_open(&self) -> bool {
        !self.groups.is_empty()
    }

    /// All open window groups in creation order (primary window first).
    pub fn groups(&self) -> &[WindowGroup] {
        &self.groups
    }

    /// Look up a window group by id.
    pub fn group(&self, id: WindowGroupId) -> Option<&WindowGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// Look up a mutable window group by id.
    pub fn group_mut(&mut self, id: WindowGroupId) -> Option<&mut WindowGroup> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// The primary window group, if the editor is still open.
    pub fn main_group(&self) -> Option<&WindowGroup> {
        self.groups.iter().find(|g| g.chrome == WindowChrome::Main)
    }

    /// Mutable access to the primary window group, if the editor is still open.
    pub fn main_group_mut(&mut self) -> Option<&mut WindowGroup> {
        self.groups.iter_mut().find(|g| g.chrome == WindowChrome::Main)
    }

    /// Locate which group and index currently hold `path`.
    pub fn find_tab(&self, path: &str) -> Option<(WindowGroupId, usize)> {
        for g in &self.groups {
            if let Some(i) = g.tabs.paths().iter().position(|p| *p == path) {
                return Some((g.id, i));
            }
        }
        None
    }

    fn group_pos(&self, id: WindowGroupId) -> Option<usize> {
        self.groups.iter().position(|g| g.id == id)
    }

    /// Detach the tab at `tab_index` in `from_group` into a newly created `Auxiliary` window.
    ///
    /// Preserves dirty, pinned, and preview marks as well as the tab's view state (cursor,
    /// selection, scroll). Returns the new group's id, or `None` if `from_group` or `tab_index`
    /// is invalid, or if the tab is the only tab in its group (detaching the sole tab of the
    /// primary window is refused to avoid leaving an empty primary window via drag-out, and
    /// dragging the sole tab of an auxiliary window moves that existing window).
    pub fn detach_tab(
        &mut self,
        from_group: WindowGroupId,
        tab_index: usize,
        restore_key: impl Into<String>,
    ) -> Option<WindowGroupId> {
        let from_pos = self.group_pos(from_group)?;
        let snapshot = {
            let group = &self.groups[from_pos];
            let paths = group.tabs.paths();
            if tab_index >= paths.len() || paths.len() <= 1 {
                return None;
            }
            let path = paths[tab_index].to_owned();
            TabSnapshot {
                dirty: group.tabs.is_dirty(&path),
                pinned: group.tabs.is_pinned(&path),
                preview: group.tabs.is_preview(&path),
                view_state: group.view_states.get(&path).cloned(),
                path,
            }
        };

        let from = &mut self.groups[from_pos];
        from.tabs.close(&snapshot.path);
        from.view_states.remove(&snapshot.path);

        let new_id = WindowGroupId(self.next_id);
        self.next_id += 1;

        let mut new_group = WindowGroup {
            id: new_id,
            restore_key: restore_key.into(),
            chrome: WindowChrome::Auxiliary,
            tabs: EditorTabs::default(),
            view_states: BTreeMap::new(),
        };
        insert_snapshot(&mut new_group, snapshot, 0);
        self.groups.push(new_group);
        Some(new_id)
    }

    /// Move the tab at `tab_index` from `from_group` into `to_group` at `insert_at`.
    ///
    /// Preserves dirty, pinned, preview marks and view state, activates the moved tab in
    /// `to_group`, and removes `from_group` if it was an `Auxiliary` window that became empty.
    /// Returns `false` if either group is missing, `from_group == to_group`, or `tab_index` is
    /// out of range.
    pub fn merge_tab(
        &mut self,
        from_group: WindowGroupId,
        tab_index: usize,
        to_group: WindowGroupId,
        insert_at: usize,
    ) -> bool {
        if from_group == to_group {
            return false;
        }
        let Some(from_pos) = self.group_pos(from_group) else {
            return false;
        };
        if self.group_pos(to_group).is_none() {
            return false;
        }

        let snapshot = {
            let group = &self.groups[from_pos];
            let paths = group.tabs.paths();
            let Some(&path_ref) = paths.get(tab_index) else {
                return false;
            };
            let path = path_ref.to_owned();
            TabSnapshot {
                dirty: group.tabs.is_dirty(&path),
                pinned: group.tabs.is_pinned(&path),
                preview: group.tabs.is_preview(&path),
                view_state: group.view_states.get(&path).cloned(),
                path,
            }
        };

        let remove_from = {
            let from = &mut self.groups[from_pos];
            from.tabs.close(&snapshot.path);
            from.view_states.remove(&snapshot.path);
            from.chrome == WindowChrome::Auxiliary && from.is_empty()
        };

        if let Some(to_pos) = self.group_pos(to_group) {
            insert_snapshot(&mut self.groups[to_pos], snapshot, insert_at);
        }

        if remove_from && let Some(pos) = self.group_pos(from_group) {
            self.groups.remove(pos);
        }
        true
    }

    /// Reorder a tab within `group_id` from `from_index` to `to_index`, preserving active tab
    /// identity. Returns whether the tab moved.
    pub fn reorder_tab(
        &mut self,
        group_id: WindowGroupId,
        from_index: usize,
        to_index: usize,
    ) -> bool {
        let Some(group) = self.group_mut(group_id) else {
            return false;
        };
        let paths = group.tabs.paths();
        if from_index >= paths.len() || to_index >= paths.len() {
            return false;
        }
        let path = paths[from_index].to_owned();
        group.tabs.move_to(&path, to_index)
    }

    /// Close `path` in `group_id`.
    ///
    /// If `group_id` is an `Auxiliary` window and becomes empty, the auxiliary window is closed.
    /// If `group_id` is the `Main` window, it stays open even when empty.
    pub fn close_tab(&mut self, group_id: WindowGroupId, path: &str) -> bool {
        let Some(pos) = self.group_pos(group_id) else {
            return false;
        };
        let present = self.groups[pos].tabs.paths().contains(&path);
        if !present {
            return false;
        }
        self.groups[pos].tabs.close(path);
        self.groups[pos].view_states.remove(path);
        if self.groups[pos].chrome == WindowChrome::Auxiliary && self.groups[pos].is_empty() {
            self.groups.remove(pos);
        }
        true
    }

    /// Close `group_id` and return every group id that was closed.
    ///
    /// Closing the `Main` window closes all `Auxiliary` windows as well. Closing an `Auxiliary`
    /// window closes only that window.
    pub fn close_group(&mut self, group_id: WindowGroupId) -> Vec<WindowGroupId> {
        let Some(pos) = self.group_pos(group_id) else {
            return Vec::new();
        };
        if self.groups[pos].chrome == WindowChrome::Main {
            let closed: Vec<WindowGroupId> = self.groups.iter().map(|g| g.id).collect();
            self.groups.clear();
            closed
        } else {
            let removed = self.groups.remove(pos);
            vec![removed.id]
        }
    }
}

struct TabSnapshot {
    path: String,
    dirty: bool,
    pinned: bool,
    preview: bool,
    view_state: Option<TabViewState>,
}

fn insert_snapshot(group: &mut WindowGroup, snap: TabSnapshot, insert_at: usize) {
    if snap.preview && !snap.dirty && !snap.pinned {
        group.tabs.open_preview(&snap.path);
    } else {
        group.tabs.open(&snap.path);
    }
    if snap.pinned {
        group.tabs.pin(&snap.path);
    }
    if snap.dirty {
        group.tabs.set_dirty(&snap.path, true);
    }
    group.tabs.move_to(&snap.path, insert_at);
    group.tabs.activate(&snap.path);
    if let Some(vs) = snap.view_state {
        group.view_states.insert(snap.path, vs);
    }
}

/// 2D point in window logical pixels (dp).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Axis-aligned rectangle in window logical pixels (dp).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Rect {
    pub const fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Self { left, top, right, bottom }
    }

    /// Euclidean distance from `p` to the rectangle (0.0 when `p` lies on or inside the rect).
    pub fn distance_to(&self, p: Point) -> f64 {
        let dx = (self.left - p.x).max(0.0).max(p.x - self.right);
        let dy = (self.top - p.y).max(0.0).max(p.y - self.bottom);
        (dx * dx + dy * dy).sqrt()
    }
}

/// Decision produced by [`DetachGesture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureOutcome {
    /// Stay in the same tab strip and reorder toward the given target index.
    Reorder(usize),
    /// Tear the tab out into a new auxiliary window (emitted at most once per gesture).
    Detach,
}

/// Pure 48 dp threshold decision state machine for tab drag-out vs tab reorder.
///
/// While the pointer stays within `threshold_dp` (default 48.0 dp) of `strip_bounds`, moves
/// evaluate to `Reorder(target_index)`. As soon as the pointer moves strictly past
/// `threshold_dp` outside `strip_bounds`, the gesture latches and emits `Detach` exactly once;
/// subsequent moves and `finish()` never emit `Detach` again and never revert to `Reorder`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetachGesture {
    origin: Point,
    strip_bounds: Rect,
    tab_count: usize,
    source_index: usize,
    current: Point,
    threshold_dp: f64,
    detached: bool,
    finished: bool,
}

impl DetachGesture {
    /// Start a drag gesture on tab `source_index` inside `strip_bounds`.
    pub fn begin(origin: Point, strip_bounds: Rect, tab_count: usize, source_index: usize) -> Self {
        Self {
            origin,
            strip_bounds,
            tab_count,
            source_index,
            current: origin,
            threshold_dp: DETACH_THRESHOLD_DP,
            detached: false,
            finished: false,
        }
    }

    /// Override the detach threshold (defaults to [`DETACH_THRESHOLD_DP`]).
    pub fn with_threshold(mut self, threshold_dp: f64) -> Self {
        self.threshold_dp = threshold_dp;
        self
    }

    /// Pointer origin where the gesture started.
    pub fn origin(&self) -> Point {
        self.origin
    }

    /// Bounds of the tab strip for this gesture.
    pub fn strip_bounds(&self) -> Rect {
        self.strip_bounds
    }

    /// Latest pointer position seen by this gesture.
    pub fn current_point(&self) -> Point {
        self.current
    }

    /// Whether this gesture has already latched into `Detach`.
    pub fn is_detached(&self) -> bool {
        self.detached
    }

    /// Advance the gesture with a new pointer position.
    ///
    /// Returns `Some(GestureOutcome::Detach)` once when the pointer first exceeds
    /// `threshold_dp` outside `strip_bounds`, `Some(GestureOutcome::Reorder(idx))` while within
    /// the threshold and not yet detached, and `None` once detached or finished.
    pub fn update(&mut self, pointer: Point) -> Option<GestureOutcome> {
        self.current = pointer;
        if self.finished || self.detached {
            return None;
        }
        if self.strip_bounds.distance_to(pointer) > self.threshold_dp {
            self.detached = true;
            return Some(GestureOutcome::Detach);
        }
        Some(GestureOutcome::Reorder(self.target_index(pointer)))
    }

    /// Complete the gesture on pointer release.
    ///
    /// Returns `Some(GestureOutcome::Reorder(idx))` if the gesture never detached, or `None`
    /// if `Detach` was already emitted (or `finish` was already called). This guarantees a
    /// single gesture never commits both `Reorder` and `Detach`.
    pub fn finish(&mut self) -> Option<GestureOutcome> {
        if self.finished {
            return None;
        }
        self.finished = true;
        if self.detached {
            None
        } else {
            Some(GestureOutcome::Reorder(self.target_index(self.current)))
        }
    }

    fn target_index(&self, pointer: Point) -> usize {
        if self.tab_count == 0 {
            return 0;
        }
        let width = self.strip_bounds.right - self.strip_bounds.left;
        if width <= 0.0 {
            return self.source_index.min(self.tab_count - 1);
        }
        let rel = ((pointer.x - self.strip_bounds.left) / width).clamp(0.0, 1.0);
        let slot = (rel * self.tab_count as f64).floor() as usize;
        slot.min(self.tab_count - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_view_state(line: u32, col: u32, scroll_top: f64) -> TabViewState {
        TabViewState {
            cursor: Some(CursorPosition { line, column: col }),
            selection: Some(SelectionRange {
                start: CursorPosition { line: line.saturating_sub(2), column: 0 },
                end: CursorPosition { line, column: col },
            }),
            scroll: Some(ScrollOffset { top: scroll_top, left: 12.0 }),
        }
    }

    #[test]
    fn fr_b1_detach_preserves_dirty_edits_and_view_state() {
        let mut wins = EditorWindows::new("proj/repo#0");
        let main_id = wins.main_group().unwrap().id;
        {
            let main = wins.main_group_mut().unwrap();
            main.tabs.open("src/main.rs");
            main.tabs.open("src/lib.rs");
            main.tabs.set_dirty("src/lib.rs", true);
            main.set_view_state("src/lib.rs", sample_view_state(41, 6, 1180.0));
        }

        let aux_id = wins
            .detach_tab(main_id, 1, "proj/repo#1")
            .expect("detaching second tab from main creates an auxiliary group");

        assert_eq!(wins.groups().len(), 2);
        let main = wins.group(main_id).unwrap();
        assert_eq!(main.chrome, WindowChrome::Main);
        assert_eq!(main.tabs.paths(), vec!["src/main.rs"]);
        assert!(main.view_state("src/lib.rs").is_none());

        let aux = wins.group(aux_id).unwrap();
        assert_eq!(aux.chrome, WindowChrome::Auxiliary);
        assert_eq!(aux.restore_key, "proj/repo#1");
        assert_eq!(aux.tabs.paths(), vec!["src/lib.rs"]);
        assert_eq!(aux.active_tab_index(), Some(0));
        assert!(aux.tabs.is_dirty("src/lib.rs"));
        assert_eq!(aux.view_state("src/lib.rs"), Some(&sample_view_state(41, 6, 1180.0)));
    }

    #[test]
    fn fr_b1_detach_rejects_invalid_index_and_sole_tab() {
        let mut wins = EditorWindows::new("proj/repo#0");
        let main_id = wins.main_group().unwrap().id;
        wins.main_group_mut().unwrap().tabs.open("a.rs");

        // Sole tab in main window cannot be detached.
        assert_eq!(wins.detach_tab(main_id, 0, "proj/repo#1"), None);
        // Out-of-bounds index is rejected.
        assert_eq!(wins.detach_tab(main_id, 5, "proj/repo#1"), None);
        // Unknown group is rejected.
        assert_eq!(wins.detach_tab(WindowGroupId(999), 0, "proj/repo#1"), None);

        wins.main_group_mut().unwrap().tabs.open("b.rs");
        let aux_id = wins.detach_tab(main_id, 1, "proj/repo#1").unwrap();
        // Sole tab in auxiliary window is a no-op (dragging moves that window itself).
        assert_eq!(wins.detach_tab(aux_id, 0, "proj/repo#2"), None);
        assert_eq!(wins.groups().len(), 2);
    }

    #[test]
    fn fr_b5_merge_back_moves_tab_and_closes_empty_auxiliary() {
        let mut wins = EditorWindows::new("proj/repo#0");
        let main_id = wins.main_group().unwrap().id;
        {
            let main = wins.main_group_mut().unwrap();
            main.tabs.open("a.rs");
            main.tabs.open("b.rs");
            main.tabs.open("c.rs");
            main.tabs.set_dirty("b.rs", true);
            main.set_view_state("b.rs", sample_view_state(10, 4, 320.0));
        }

        let aux_id = wins.detach_tab(main_id, 1, "proj/repo#1").unwrap();
        assert_eq!(wins.groups().len(), 2);

        // Merge b.rs back into main at index 0.
        assert!(wins.merge_tab(aux_id, 0, main_id, 0));
        // Emptied auxiliary group is closed automatically.
        assert!(wins.group(aux_id).is_none());
        assert_eq!(wins.groups().len(), 1);

        let main = wins.group(main_id).unwrap();
        assert_eq!(main.tabs.paths(), vec!["b.rs", "a.rs", "c.rs"]);
        assert_eq!(main.active_tab_index(), Some(0));
        assert!(main.tabs.is_dirty("b.rs"));
        assert_eq!(main.view_state("b.rs"), Some(&sample_view_state(10, 4, 320.0)));
    }

    #[test]
    fn fr_b5_closing_last_tab_in_aux_closes_aux_but_main_stays_empty() {
        let mut wins = EditorWindows::new("proj/repo#0");
        let main_id = wins.main_group().unwrap().id;
        {
            let main = wins.main_group_mut().unwrap();
            main.tabs.open("a.rs");
            main.tabs.open("b.rs");
        }
        let aux_id = wins.detach_tab(main_id, 1, "proj/repo#1").unwrap();

        assert!(wins.close_tab(aux_id, "b.rs"));
        assert!(wins.group(aux_id).is_none());
        assert_eq!(wins.groups().len(), 1);

        assert!(wins.close_tab(main_id, "a.rs"));
        assert!(wins.is_open());
        let main = wins.main_group().unwrap();
        assert!(main.is_empty());
        assert_eq!(main.active_tab_index(), None);
    }

    #[test]
    fn fr_b5_closing_main_group_closes_all_auxiliaries() {
        let mut wins = EditorWindows::new("proj/repo#0");
        let main_id = wins.main_group().unwrap().id;
        {
            let main = wins.main_group_mut().unwrap();
            main.tabs.open("a.rs");
            main.tabs.open("b.rs");
            main.tabs.open("c.rs");
        }
        let aux1 = wins.detach_tab(main_id, 1, "proj/repo#1").unwrap();
        let aux2 = wins.detach_tab(main_id, 1, "proj/repo#2").unwrap();
        assert_eq!(wins.groups().len(), 3);

        // Closing one auxiliary leaves main and the other auxiliary open.
        assert_eq!(wins.close_group(aux1), vec![aux1]);
        assert_eq!(wins.groups().len(), 2);

        // Closing main closes main and every remaining auxiliary.
        let closed = wins.close_group(main_id);
        assert_eq!(closed, vec![main_id, aux2]);
        assert!(!wins.is_open());
        assert!(wins.groups().is_empty());
    }

    #[test]
    fn fr_b5_reorder_within_group_preserves_active_tab_identity() {
        let mut wins = EditorWindows::new("proj/repo#0");
        let main_id = wins.main_group().unwrap().id;
        {
            let main = wins.main_group_mut().unwrap();
            main.tabs.open("a.rs");
            main.tabs.open("b.rs");
            main.tabs.open("c.rs");
            main.tabs.activate("b.rs");
        }

        // Move active tab b.rs (index 1) to index 2.
        assert!(wins.reorder_tab(main_id, 1, 2));
        let main = wins.group(main_id).unwrap();
        assert_eq!(main.tabs.paths(), vec!["a.rs", "c.rs", "b.rs"]);
        assert_eq!(main.tabs.current_path(), Some("b.rs"));
        assert_eq!(main.active_tab_index(), Some(2));

        // Move non-active tab a.rs (index 0) to index 2; b.rs remains active.
        assert!(wins.reorder_tab(main_id, 0, 2));
        let main = wins.group(main_id).unwrap();
        assert_eq!(main.tabs.paths(), vec!["c.rs", "b.rs", "a.rs"]);
        assert_eq!(main.tabs.current_path(), Some("b.rs"));
        assert_eq!(main.active_tab_index(), Some(1));
    }

    #[test]
    fn fr_b1_gesture_48dp_threshold_boundary_is_exact_and_one_shot() {
        let strip = Rect::new(0.0, 30.0, 300.0, 65.0);
        let mut g = DetachGesture::begin(Point::new(50.0, 45.0), strip, 3, 0);

        // Inside the strip: reorder.
        assert_eq!(g.update(Point::new(250.0, 45.0)), Some(GestureOutcome::Reorder(2)));
        assert!(!g.is_detached());

        // Exactly 48.0 dp below the strip bottom (65.0 + 48.0 = 113.0): still reorder, not detach.
        assert_eq!(g.update(Point::new(150.0, 113.0)), Some(GestureOutcome::Reorder(1)));
        assert!(!g.is_detached());

        // 48.1 dp below the strip bottom (65.0 + 48.1 = 113.1): triggers one-shot Detach.
        assert_eq!(g.update(Point::new(150.0, 113.1)), Some(GestureOutcome::Detach));
        assert!(g.is_detached());

        // Further moves (even moving back inside the strip) never emit Detach again or revert to Reorder.
        assert_eq!(g.update(Point::new(150.0, 200.0)), None);
        assert_eq!(g.update(Point::new(150.0, 45.0)), None);
        assert_eq!(g.finish(), None);
    }

    #[test]
    fn fr_b1_gesture_short_drag_commits_reorder_and_never_detaches() {
        let strip = Rect::new(0.0, 0.0, 400.0, 36.0);
        let mut g = DetachGesture::begin(Point::new(50.0, 18.0), strip, 4, 0);

        assert_eq!(g.update(Point::new(250.0, 18.0)), Some(GestureOutcome::Reorder(2)));
        assert_eq!(g.update(Point::new(350.0, 84.0)), Some(GestureOutcome::Reorder(3)));
        assert!(!g.is_detached());
        assert_eq!(g.finish(), Some(GestureOutcome::Reorder(3)));
        // Second finish is a no-op.
        assert_eq!(g.finish(), None);
    }

    #[test]
    fn fr_b1_gesture_diagonal_distance_uses_euclidean_metric() {
        let strip = Rect::new(0.0, 0.0, 200.0, 40.0);
        let mut g = DetachGesture::begin(Point::new(100.0, 20.0), strip, 2, 0);

        // dx = 30, dy = 36 -> sqrt(900 + 1296) = 46.86 <= 48.0 -> Reorder.
        assert!(matches!(g.update(Point::new(230.0, 76.0)), Some(GestureOutcome::Reorder(_))));
        // dx = 30, dy = 40 -> sqrt(900 + 1600) = 50.0 > 48.0 -> Detach.
        assert_eq!(g.update(Point::new(230.0, 80.0)), Some(GestureOutcome::Detach));
        assert_eq!(g.finish(), None);
    }
}
