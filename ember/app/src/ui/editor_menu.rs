//! The ⋯ menu of the Ember 에디터 window (FR-L31, #795): a fixed overlay under the tab strip's ⋯,
//! outside the editor island that clips, whose groups open as submenus beside it as Code-OSS's menus
//! do, so every item is reachable without scrolling (the toolkit's wheel may not reach the page).
//!
//! The tab strip's ⋯ is its only opener: the mockup's top bar has no ⋯ (`editor.js:75-76`). The
//! mockup's items come first (`editor.js:77`: 저장, 최근 파일, 코드 정리, 줄로 이동, 다른 탭 모두 닫기,
//! the palette, 단축키 체계), then a separator and the items Ember adds (owner placement rule
//! 2026-11-13, rule 5, `docs/DESIGN.md` 6.3).

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
    /// The line between the mockup's items and Ember's (`.ed-msep`).
    Sep,
}

/// One plain item of the first level, in the order they are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopItem {
    Save,
    Recent,
    GotoLine,
    /// 다른 탭 모두 닫기, only while a tab is current.
    CloseOthers,
    Palette,
    FindInFile,
    ReopenClosed,
    Settings,
    CloseEditor,
}

/// One entry of the first level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Item(TopItem),
    Group(Submenu),
    Sep,
}

/// The first level in order: the mockup's items, a separator, then Ember's.
pub fn entries(has_tab: bool) -> Vec<Entry> {
    let mut out = vec![
        Entry::Item(TopItem::Save),
        Entry::Item(TopItem::Recent),
        Entry::Group(Submenu::Code),
        Entry::Item(TopItem::GotoLine),
    ];
    if has_tab {
        out.push(Entry::Item(TopItem::CloseOthers));
    }
    out.extend([Entry::Item(TopItem::Palette), Entry::Group(Submenu::Keymap), Entry::Sep]);
    out.extend([Entry::Item(TopItem::FindInFile), Entry::Item(TopItem::ReopenClosed), Entry::Item(TopItem::Settings)]);
    out.push(Entry::Group(Submenu::AutoSave));
    if has_tab {
        out.push(Entry::Group(Submenu::Tab));
    }
    out.push(Entry::Item(TopItem::CloseEditor));
    out
}

/// The height of every row (`.ed-menu button`), the menu's padding and the gap under the top bar.
pub const ROW_H: f32 = 34.0;
pub const PAD: f32 = 4.0;
/// The separator's height: 1 px and `var(--sp1)` above and below (`.ed-msep`).
pub const SEP_H: f32 = 9.0;
/// The menu's top: under the tab strip, as the mockup's `.ed-menu` (`top:calc(var(--tabh) + 2px)`
/// in the editor island): the top bar (52 px), the gap (8), the tab strip (40) and 2 px.
pub const TOP: f32 = 102.0;
/// Width of the menu and of each submenu.
pub const WIDTH: f32 = 260.0;
/// The smallest window height every item must fit in, below the 900 px of the captures.
pub const MIN_WINDOW_H: f32 = 600.0;

/// The first level as the layout sees it.
pub fn top_rows(has_tab: bool) -> Vec<TopRow> {
    entries(has_tab)
        .into_iter()
        .map(|e| match e {
            Entry::Item(_) => TopRow::Item,
            Entry::Group(sub) => TopRow::Group(sub),
            Entry::Sep => TopRow::Sep,
        })
        .collect()
}

fn row_h(row: &TopRow) -> f32 {
    if *row == TopRow::Sep { SEP_H } else { ROW_H }
}

/// Where a submenu's top would be: level with its row in the first level.
pub fn submenu_top(rows: &[TopRow], sub: Submenu) -> Option<f32> {
    let i = rows.iter().position(|r| *r == TopRow::Group(sub))?;
    let above: f32 = rows[..i].iter().map(row_h).sum();
    Some(TOP + PAD + above - PAD)
}

/// Where a submenu of `n` rows opens: level with its row, moved up so it ends inside a window of
/// [`MIN_WINDOW_H`], never above the menu's own top.
pub fn submenu_top_fitted(rows: &[TopRow], sub: Submenu, n: usize) -> Option<f32> {
    submenu_top(rows, sub).map(|top| top.min(MIN_WINDOW_H - menu_height(n)).max(TOP))
}

/// The height of a menu of `n` rows.
pub fn menu_height(n: usize) -> f32 {
    n as f32 * ROW_H + 2.0 * PAD + 2.0
}

/// The first level's height.
pub fn top_height(rows: &[TopRow]) -> f32 {
    rows.iter().map(row_h).sum::<f32>() + 2.0 * PAD + 2.0
}

/// The lowest point the menu or any of its submenus reaches, for submenus of the given sizes.
pub fn lowest_point(has_tab: bool, sizes: &[(Submenu, usize)]) -> f32 {
    let rows = top_rows(has_tab);
    let mut low = TOP + top_height(&rows);
    for (sub, n) in sizes {
        if let Some(top) = submenu_top_fitted(&rows, *sub, *n) {
            low = low.max(top + menu_height(*n));
        }
    }
    low
}

/// The menu's right edge: the editor island's (the right stripe and the gap) and `var(--sp2)`.
const RIGHT: &str = "calc(var(--stw) + var(--gap) + var(--sp2))";

/// The submenu's inline position: left of the menu, top level with its row.
pub fn submenu_style(top: f32) -> String {
    format!("position:fixed; grid-row:1; grid-column:1; top:{top}px; right:calc(var(--stw) + var(--gap) + var(--sp2) + {WIDTH}px + 4px); width:{WIDTH}px; z-index:41;")
}

/// The menu's own position: fixed under the tab strip's ⋯ at the island's right. `grid-row` and
/// `grid-column` keep the overlay in the top bar's cell, so it adds no implicit row to the page grid
/// (design review 2026-11-13 A6).
pub fn menu_style() -> String {
    format!("position:fixed; grid-row:1; grid-column:1; top:{TOP}px; right:{RIGHT}; width:{WIDTH}px; z-index:41;")
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

/// Whether the ⋯ menu is drawn (#831). A modal (the trust prompt, the unsaved-changes sheet, an
/// extension's modal message) blocks the workbench as Code-OSS's modal dialogs do, so the menu is
/// not drawn above its scrim; the renderer does not order fixed layers by z-index alone.
pub fn menu_shown(menu_open: bool, modal_up: bool) -> bool {
    menu_open && !modal_up
}

/// The menu's state after a click on the tab strip's ⋯ (#831): it toggles, and never opens while a
/// modal is up.
pub fn toggled(menu_open: bool, modal_up: bool) -> bool {
    !menu_open && !modal_up
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
        // The menu opens under the tab strip now (TOP 102), with the separator row.
        for has_tab in [true, false] {
            let low = lowest_point(has_tab, &sizes);
            assert!(low <= MIN_WINDOW_H, "the menu reaches {low} px, past a {MIN_WINDOW_H} px window");
        }
        // A menu and its submenu side by side fit the 768 px wide window.
        const { assert!(2.0 * WIDTH + 4.0 + 16.0 <= 768.0) };
    }

    #[test]
    fn fr_l31_main_menu_submenu_sits_level_with_its_row() {
        let rows = top_rows(true);
        assert_eq!(submenu_top(&rows, Submenu::Code), Some(TOP + 2.0 * ROW_H));
        assert_eq!(submenu_top(&rows, Submenu::Keymap), Some(TOP + 6.0 * ROW_H));
        assert_eq!(submenu_top(&rows, Submenu::AutoSave), Some(TOP + 10.0 * ROW_H + SEP_H));
        assert_eq!(submenu_top(&top_rows(false), Submenu::Tab), None);
        assert_eq!(top_rows(false).len(), 12);
        // A long submenu low in the menu moves up to end inside the window.
        let fitted = submenu_top_fitted(&rows, Submenu::Tab, 8).unwrap();
        assert!(fitted + menu_height(8) <= MIN_WINDOW_H);
        assert!(fitted >= TOP);
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

    /// Owner placement rule 2026-11-13 (rule 5), design review A14: the mockup's items first, a
    /// separator, then Ember's (find in file, reopen, settings, auto-save, the tab group, close editor).
    #[test]
    fn design_a14_menu_lists_the_mockup_items_then_a_separator_then_embers() {
        use Entry::{Group, Item, Sep};
        assert_eq!(
            entries(true),
            vec![
                Item(TopItem::Save), Item(TopItem::Recent), Group(Submenu::Code), Item(TopItem::GotoLine),
                Item(TopItem::CloseOthers), Item(TopItem::Palette), Group(Submenu::Keymap), Sep,
                Item(TopItem::FindInFile), Item(TopItem::ReopenClosed), Item(TopItem::Settings),
                Group(Submenu::AutoSave), Group(Submenu::Tab), Item(TopItem::CloseEditor),
            ]
        );
        assert!(!entries(false).contains(&Item(TopItem::CloseOthers)));
        assert!(!entries(false).contains(&Group(Submenu::Tab)));
        assert_eq!(entries(false).iter().filter(|e| **e == Sep).count(), 1);
    }

    /// Design review A14: the top bar has no ⋯; the tab strip's ⋯ opens the menu.
    #[test]
    fn design_a14_only_the_tab_strip_opens_the_menu() {
        let page = include_str!("editor_workbench.rs");
        assert!(!page.contains("title: \"메뉴\", \"aria-label\": \"메뉴\""), "the top bar still has a ⋯ button");
        assert!(page.contains("title: \"탭 메뉴\", \"aria-label\": \"탭 메뉴\""), "the tab strip lost its ⋯ button");
    }

    #[test]
    fn fr_l31_main_menu_is_fixed_outside_the_editor_island() {
        assert!(menu_style().contains("position:fixed;"));
        let page = include_str!("editor_workbench.rs");
        assert!(!page.contains("if *s.menu_open.read() { {menu(&s, keymap)} }\n                        if let Some((menu_path"), "the menu is still drawn inside the clipped editor area");
        // At the page level, in the overlays' box that takes no grid row (FR-L31.26).
        assert!(page.contains("div { class: \"ed-layers\",\n                        if em_shown { {menu(&s, keymap)} }\n                        if *s.palette_open.read()"), "the menu is not drawn at the page level");
    }

    #[test]
    fn fr_l31_main_menu_is_blocked_by_a_modal() {
        assert!(menu_shown(true, false));
        assert!(!menu_shown(true, true), "the menu is drawn above a modal's scrim");
        assert!(!toggled(false, true), "the menu opens while a modal is up");
        assert!(toggled(false, false));
        assert!(!toggled(true, false));
        let page = include_str!("editor_workbench.rs");
        assert!(page.contains("crate::ui::editor_menu::toggled(open, modal_up(&menu_s))"), "the ⋯ button ignores the modal");
        assert!(page.contains("if em_shown { {menu(&s, keymap)} }"), "the page does not gate the menu on the modal");
    }
}
