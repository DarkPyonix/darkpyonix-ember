//! The ⋯ menu of the Ember 에디터 window (FR-L31, #795): a fixed overlay under the top bar, outside
//! the editor island that clips, whose groups open as submenus beside it as Code-OSS's menus do, so
//! every item is reachable without scrolling (the toolkit's wheel may not reach the page).

use std::cell::Cell;

/// A group of the ⋯ menu that opens as a submenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submenu {
    /// The current tab's items (FR-L31.5).
    Tab,
    /// 코드 정리 and the language features (FR-L31.13).
    Code,
    /// 자동 저장 (FR-L31.6).
    AutoSave,
    /// 단축키 체계 (FR-L30).
    Keymap,
}

/// One row of the menu's first level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopRow {
    Group(Submenu),
    /// A plain item; the count of these is all the layout needs.
    Item,
}

/// The height of every row (`.ed-menu button`), the menu's padding and the gap under the top bar.
pub const ROW_H: f32 = 34.0;
pub const PAD: f32 = 4.0;
/// The menu's top: the top bar's row (52 px) and 2 px.
pub const TOP: f32 = 54.0;
/// Width of the menu and of each submenu.
pub const WIDTH: f32 = 260.0;
/// The smallest window height every item must fit in, below the 900 px of the captures.
pub const MIN_WINDOW_H: f32 = 600.0;

/// The first level in order: the tab group (when a tab is current), the six file and navigation
/// items, 코드, the palette, 자동 저장, 단축키 체계 and 에디터 닫기.
pub fn top_rows(has_tab: bool) -> Vec<TopRow> {
    let mut rows = Vec::new();
    if has_tab {
        rows.push(TopRow::Group(Submenu::Tab));
    }
    rows.extend([TopRow::Item; 6]);
    rows.push(TopRow::Group(Submenu::Code));
    rows.push(TopRow::Item);
    rows.push(TopRow::Group(Submenu::AutoSave));
    rows.push(TopRow::Group(Submenu::Keymap));
    rows.push(TopRow::Item);
    rows
}

/// Where a submenu's top is: level with its row in the first level.
pub fn submenu_top(rows: &[TopRow], sub: Submenu) -> Option<f32> {
    rows.iter().position(|r| *r == TopRow::Group(sub)).map(|i| TOP + PAD + i as f32 * ROW_H - PAD)
}

/// The height of a menu of `n` rows.
pub fn menu_height(n: usize) -> f32 {
    n as f32 * ROW_H + 2.0 * PAD + 2.0
}

/// The lowest point the menu or any of its submenus reaches, for submenus of the given sizes.
pub fn lowest_point(has_tab: bool, sizes: &[(Submenu, usize)]) -> f32 {
    let rows = top_rows(has_tab);
    let mut low = TOP + menu_height(rows.len());
    for (sub, n) in sizes {
        if let Some(top) = submenu_top(&rows, *sub) {
            low = low.max(top + menu_height(*n));
        }
    }
    low
}

/// The submenu's inline position: left of the menu, top level with its row.
pub fn submenu_style(top: f32) -> String {
    format!("position:fixed; top:{top}px; right:calc(var(--sp2) + {WIDTH}px + 4px); width:{WIDTH}px; z-index:41;")
}

/// The menu's own position: fixed under the top bar at the right.
pub fn menu_style() -> String {
    format!("position:fixed; top:{TOP}px; right:var(--sp2); width:{WIDTH}px; z-index:41;")
}

thread_local! {
    static OPEN: Cell<Option<Submenu>> = const { Cell::new(None) };
}

/// The submenu now open.
pub fn open_submenu() -> Option<Submenu> {
    OPEN.with(|o| o.get())
}

/// A click on a group's row opens its submenu, or closes it when it is open.
pub fn toggle_submenu(sub: Submenu) {
    OPEN.with(|o| o.set(if o.get() == Some(sub) { None } else { Some(sub) }));
}

/// Opening or closing the menu starts with no submenu open.
pub fn close_submenu() {
    OPEN.with(|o| o.set(None));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_main_menu_and_its_submenus_fit_the_window_without_scrolling() {
        // #795: the flat menu ran past the window; grouped it fits a 600 px window, with every group
        // at its real size (tab items 8, code 7, auto-save 3, keymaps 2).
        let sizes = [(Submenu::Tab, 8), (Submenu::Code, 7), (Submenu::AutoSave, 3), (Submenu::Keymap, 2)];
        for has_tab in [true, false] {
            let low = lowest_point(has_tab, &sizes);
            assert!(low <= MIN_WINDOW_H, "the menu reaches {low} px, past a {MIN_WINDOW_H} px window");
        }
        // A menu and its submenu side by side fit the 768 px wide window.
        assert!(2.0 * WIDTH + 4.0 + 16.0 <= 768.0);
    }

    #[test]
    fn fr_l31_main_menu_submenu_sits_level_with_its_row() {
        let rows = top_rows(true);
        assert_eq!(submenu_top(&rows, Submenu::Tab), Some(TOP));
        assert_eq!(submenu_top(&rows, Submenu::Code), Some(TOP + 7.0 * ROW_H));
        assert_eq!(submenu_top(&top_rows(false), Submenu::Tab), None);
        assert_eq!(top_rows(false).len(), 11);
    }

    #[test]
    fn fr_l31_main_menu_submenu_toggles_and_resets() {
        close_submenu();
        toggle_submenu(Submenu::Code);
        assert_eq!(open_submenu(), Some(Submenu::Code));
        toggle_submenu(Submenu::Keymap);
        assert_eq!(open_submenu(), Some(Submenu::Keymap));
        toggle_submenu(Submenu::Keymap);
        assert_eq!(open_submenu(), None);
        toggle_submenu(Submenu::Tab);
        close_submenu();
        assert_eq!(open_submenu(), None);
    }

    #[test]
    fn fr_l31_main_menu_is_fixed_outside_the_editor_island() {
        assert!(menu_style().contains("position:fixed;"));
        let page = include_str!("editor_workbench.rs");
        assert!(!page.contains("if *s.menu_open.read() { {menu(&s, keymap)} }\n                        if let Some((menu_path"), "the menu is still drawn inside the clipped editor area");
        assert!(page.contains("if *s.menu_open.read() { {menu(&s, keymap)} }\n                    if *s.palette_open.read()"), "the menu is not drawn at the page level");
    }
}
