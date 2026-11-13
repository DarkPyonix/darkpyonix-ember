//! The 프로젝트 tool window's menus (owner placement rule 2026-11-13, rules 1 and 2;
//! `docs/DESIGN.md` 6.3 "Where features beyond the mockup go").
//!
//! The header keeps the mockup's three actions (collapse all, locate the open file, hide,
//! `editor.js:54`) and one "+" button whose menu holds 새 파일 and 새 폴더. Tree rows carry no
//! inline icons: cut, copy, paste, rename (F2), delete, closing the file's tab and select all are
//! in the tree's context menu, in Code-OSS's explorer order.
//!
<<<<<<< HEAD
//! v0.0.7: #417 fixed; workaround kept until checked on a display (dioxus-compose has to forward
//! compose-rust's pointer events as oncontextmenu and ondoubleclick; its pin moves separately).
//! Until right-click reaches the editor window, two stand-ins open the same items: the "선택한
//! 항목" section of the header "+" menu and Shift+F10 (or the context-menu key) on the focused
//! tree. Remove both once right-click works there.
=======
//! TODO(v0.0.7 pin, compose-rust#417): right-click does not reach the app until the pin moves, so
//! two stand-ins open the same items: the "선택한 항목" section of the header "+" menu and
//! Shift+F10 (or the context-menu key) on the focused tree. Remove both once right-click works.
//! TODO(compose-rust#100): the v0.0.6 renderer sends only Enter as a key event, so Shift+F10 does
//! not reach the tree in a real run yet (#832); until then the "+" menu is the one that works.
>>>>>>> origin/develop

use std::cell::Cell;

/// One item of the tree's menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeMenuItem {
    NewFile,
    NewFolder,
    Cut,
    Copy,
    Paste,
    Rename,
    Delete,
    /// Close the selected file's editor tab.
    CloseTab,
    SelectAll,
}

impl TreeMenuItem {
    pub fn label(self) -> &'static str {
        match self {
            TreeMenuItem::NewFile => "새 파일",
            TreeMenuItem::NewFolder => "새 폴더",
            TreeMenuItem::Cut => "잘라내기",
            TreeMenuItem::Copy => "복사",
            TreeMenuItem::Paste => "붙여넣기",
            TreeMenuItem::Rename => "이름 바꾸기",
            TreeMenuItem::Delete => "삭제",
            TreeMenuItem::CloseTab => "탭 닫기",
            TreeMenuItem::SelectAll => "모두 선택",
        }
    }

    /// The shortcut shown beside the item, as the tree's own keys.
    pub fn hint(self, mac: bool) -> &'static str {
        match (self, mac) {
            (TreeMenuItem::Cut, true) => "⌘X",
            (TreeMenuItem::Cut, false) => "Ctrl+X",
            (TreeMenuItem::Copy, true) => "⌘C",
            (TreeMenuItem::Copy, false) => "Ctrl+C",
            (TreeMenuItem::Paste, true) => "⌘V",
            (TreeMenuItem::Paste, false) => "Ctrl+V",
            (TreeMenuItem::Rename, _) => "F2",
            (TreeMenuItem::Delete, true) => "⌫",
            (TreeMenuItem::Delete, false) => "Delete",
            _ => "",
        }
    }
}

/// One line of a tree menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeMenuEntry {
    Item(TreeMenuItem, bool),
    Sep,
    /// A section heading (`.ed-mlab`).
    Heading(&'static str),
}

/// What the menus can do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeMenuState {
    /// A row (not the root folder) is selected.
    pub has_selection: bool,
    /// The clipboard holds cut or copied rows.
    pub has_clipboard: bool,
    /// The selected row is a file open in a tab.
    pub tab_open: bool,
}

/// The items about the selection, in Code-OSS's explorer groups: cut, copy, paste | rename,
/// delete | (Ember) close the tab, select all.
fn selection_entries(st: TreeMenuState) -> Vec<TreeMenuEntry> {
    use TreeMenuEntry::{Item, Sep};
    use TreeMenuItem::*;
    vec![
        Item(Cut, st.has_selection),
        Item(Copy, st.has_selection),
        Item(Paste, st.has_clipboard),
        Sep,
        Item(Rename, st.has_selection),
        Item(Delete, st.has_selection),
        Sep,
        Item(CloseTab, st.tab_open),
        Item(SelectAll, true),
    ]
}

/// The tree's context menu: new file and folder, then the selection's items.
pub fn context_entries(st: TreeMenuState) -> Vec<TreeMenuEntry> {
    use TreeMenuEntry::{Item, Sep};
    let mut out = vec![Item(TreeMenuItem::NewFile, true), Item(TreeMenuItem::NewFolder, true), Sep];
    out.extend(selection_entries(st));
    out
}

/// The header "+" menu: new file and folder; until right-click works, a "선택한 항목" section with
/// the context menu's selection items (the stand-in opener).
pub fn plus_entries(st: TreeMenuState) -> Vec<TreeMenuEntry> {
    use TreeMenuEntry::{Heading, Item, Sep};
    let mut out = vec![Item(TreeMenuItem::NewFile, true), Item(TreeMenuItem::NewFolder, true)];
    if PLUS_MENU_HOLDS_SELECTION {
        out.extend([Sep, Heading(SELECTION_HEADING)]);
        out.extend(selection_entries(st));
    }
    out
}

/// v0.0.7: #417 fixed; workaround kept until checked on a display (false once right-click opens
/// the context menu in the dioxus-compose window).
pub const PLUS_MENU_HOLDS_SELECTION: bool = true;
/// The heading of the stand-in section of the "+" menu.
pub const SELECTION_HEADING: &str = "선택한 항목";

/// The mockup's header actions, in order, beside the one "+" button that comes first.
pub const HEADER_ACTIONS: [&str; 4] = ["추가", "모두 접기", "열린 파일 찾기", "숨기기"];

/// Where a tree menu is open.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TreeMenuAt {
    /// The header "+" menu, under its button.
    Plus,
    /// The context menu at the pointer.
    Pointer { x: f64, y: f64 },
    /// The context menu from the keyboard (Shift+F10), under the header.
    Keyboard,
}

impl TreeMenuAt {
    /// The menu's inline position (fixed, so the clipping tool window does not cut it).
    pub fn style(self) -> String {
        match self {
            TreeMenuAt::Plus | TreeMenuAt::Keyboard => {
                "position:fixed; top:106px; left:calc(var(--stw) + var(--gap) + var(--sp3)); right:auto; width:240px; z-index:41;".to_string()
            }
            TreeMenuAt::Pointer { x, y } => format!("position:fixed; left:{x}px; top:{y}px; right:auto; width:240px; z-index:41;"),
        }
    }

    pub fn entries(self, st: TreeMenuState) -> Vec<TreeMenuEntry> {
        match self {
            TreeMenuAt::Plus => plus_entries(st),
            TreeMenuAt::Pointer { .. } | TreeMenuAt::Keyboard => context_entries(st),
        }
    }
}

thread_local! {
    static OPEN: Cell<Option<TreeMenuAt>> = const { Cell::new(None) };
}

/// The tree menu now open.
pub fn open_menu() -> Option<TreeMenuAt> {
    OPEN.with(|o| o.get())
}

/// Opens a tree menu, replacing any other.
pub fn open(at: TreeMenuAt) {
    OPEN.with(|o| o.set(Some(at)));
}

/// A second click on "+" closes its menu.
pub fn toggle_plus() {
    OPEN.with(|o| o.set(if o.get() == Some(TreeMenuAt::Plus) { None } else { Some(TreeMenuAt::Plus) }));
}

pub fn close() {
    OPEN.with(|o| o.set(None));
}

#[cfg(test)]
mod tests {
    use super::*;
    use TreeMenuEntry::{Heading, Item, Sep};
    use TreeMenuItem::*;

    const SELECTED: TreeMenuState = TreeMenuState { has_selection: true, has_clipboard: false, tab_open: true };

    #[test]
    fn design_rule2_context_menu_holds_the_row_actions_in_code_oss_order() {
        assert_eq!(
            context_entries(SELECTED),
            vec![
                Item(NewFile, true), Item(NewFolder, true), Sep,
                Item(Cut, true), Item(Copy, true), Item(Paste, false), Sep,
                Item(Rename, true), Item(Delete, true), Sep,
                Item(CloseTab, true), Item(SelectAll, true),
            ]
        );
        let none = context_entries(TreeMenuState::default());
        assert!(none.contains(&Item(Rename, false)) && none.contains(&Item(Delete, false)) && none.contains(&Item(CloseTab, false)));
        assert_eq!(Rename.hint(true), "F2");
    }

    #[test]
    fn design_rule1_plus_menu_is_new_file_and_folder_then_the_stand_in_section() {
        let entries = plus_entries(SELECTED);
        assert_eq!(&entries[..2], &[Item(NewFile, true), Item(NewFolder, true)]);
        assert_eq!(&entries[2..4], &[Sep, Heading("선택한 항목")]);
        assert!(entries.contains(&Item(Rename, true)) && entries.contains(&Item(Delete, true)) && entries.contains(&Item(CloseTab, true)));
        const { assert!(PLUS_MENU_HOLDS_SELECTION, "the stand-in goes once dioxus-compose forwards right-click (compose-rust#417), not before") };
    }

    #[test]
    fn design_rule1_header_is_plus_and_the_mockups_three_actions() {
        assert_eq!(HEADER_ACTIONS, ["추가", "모두 접기", "열린 파일 찾기", "숨기기"]);
    }

    #[test]
    fn design_rule2_menus_open_close_and_toggle() {
        close();
        toggle_plus();
        assert_eq!(open_menu(), Some(TreeMenuAt::Plus));
        toggle_plus();
        assert_eq!(open_menu(), None);
        open(TreeMenuAt::Keyboard);
        assert_eq!(open_menu().map(|m| m.entries(SELECTED)), Some(context_entries(SELECTED)));
        open(TreeMenuAt::Pointer { x: 10.0, y: 20.0 });
        assert!(open_menu().unwrap().style().contains("left:10px; top:20px;"));
        close();
        assert_eq!(open_menu(), None);
    }
}
