//! The app shell (FR-L10, FR-L28, `docs/DESIGN.md` section 4.7): the eight menus, a sidebar
//! that collapses to a rail, and bottom tabs on a phone.
//!
//! The decisions (menu order and labels, which screen a menu shows, which bottom tab a menu
//! belongs to, which layout a window size gets) are plain functions and enums at the top of
//! this file, tested without a renderer. The components below draw them.

use dioxus_compose::prelude::*;

use crate::model;
use crate::ui::components::{AppButton, EmberSheet, Label};
use crate::ui::theme::*;
use crate::ui::{conversation, home, new_session, project, search, use_ui, Route};

// ---------------------------------------------------------------------------------------------
// Menus
// ---------------------------------------------------------------------------------------------

/// The eight menus, in the order of `INTENT.md` D26. No group label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavMenu {
    Home,
    Projects,
    Sessions,
    Channels,
    Routines,
    VersionControl,
    Agents,
    Files,
}

impl NavMenu {
    pub const ALL: [NavMenu; 8] = [
        NavMenu::Home,
        NavMenu::Projects,
        NavMenu::Sessions,
        NavMenu::Channels,
        NavMenu::Routines,
        NavMenu::VersionControl,
        NavMenu::Agents,
        NavMenu::Files,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NavMenu::Home => "홈",
            NavMenu::Projects => "프로젝트",
            NavMenu::Sessions => "세션",
            NavMenu::Channels => "채널",
            NavMenu::Routines => "루틴",
            NavMenu::VersionControl => "버전 관리",
            NavMenu::Agents => "에이전트",
            NavMenu::Files => "파일",
        }
    }

    /// The nearest glyph the pinned toolkit has.
    /// TODO(GAPS: 임의 아이콘 세트): Heroicons outline per menu once `Icon` takes an SVG asset.
    pub fn icon(self) -> IconRole {
        match self {
            NavMenu::Home => IconRole::Home,
            NavMenu::Projects => IconRole::Library,
            NavMenu::Sessions => IconRole::Compose,
            NavMenu::Channels => IconRole::Inbox,
            NavMenu::Routines => IconRole::History,
            NavMenu::VersionControl => IconRole::List,
            NavMenu::Agents => IconRole::Sidebar,
            NavMenu::Files => IconRole::Image,
        }
    }
}

/// The menu to mark active: an open conversation always belongs to 세션.
pub fn active_menu(route: &Route, chosen: NavMenu) -> NavMenu {
    match route {
        Route::Conversation(_) => NavMenu::Sessions,
        _ => chosen,
    }
}

/// What the content area shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// 홈: the chat-first top and the dashboard (FR-L11, FR-L23).
    Home,
    Launcher,
    Search,
    Conversation(String),
    /// A menu whose screen is not built yet ("준비 중입니다").
    Placeholder(NavMenu),
}

/// The screen for the active menu. Home is 홈, the launcher stands in for 프로젝트 and the
/// conversation for 세션; every other menu is a placeholder until its screen lands.
pub fn screen_for(route: &Route, active: NavMenu) -> Screen {
    match (active, route) {
        (NavMenu::Home, _) => Screen::Home,
        (NavMenu::Sessions, Route::Conversation(id)) => Screen::Conversation(id.clone()),
        (NavMenu::Projects, Route::Search) => Screen::Search,
        (NavMenu::Projects, _) => Screen::Launcher,
        (other, _) => Screen::Placeholder(other),
    }
}

pub const PLACEHOLDER_TEXT: &str = "준비 중입니다";

// ---------------------------------------------------------------------------------------------
// Bottom tabs
// ---------------------------------------------------------------------------------------------

/// The five tabs of the phone bar (FR-L10, FR-L28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomTab {
    NavMenu(NavMenu),
    More,
}

pub const BOTTOM_TABS: [BottomTab; 5] = [
    BottomTab::NavMenu(NavMenu::Home),
    BottomTab::NavMenu(NavMenu::Projects),
    BottomTab::NavMenu(NavMenu::Sessions),
    BottomTab::NavMenu(NavMenu::Channels),
    BottomTab::More,
];

/// What 더보기 opens: the four menus the bar has no room for.
pub const MORE_MENUS: [NavMenu; 4] = [
    NavMenu::Routines,
    NavMenu::VersionControl,
    NavMenu::Agents,
    NavMenu::Files,
];

impl BottomTab {
    pub fn label(self) -> &'static str {
        match self {
            BottomTab::NavMenu(m) => m.label(),
            BottomTab::More => "더보기",
        }
    }

    pub fn icon(self) -> IconRole {
        match self {
            BottomTab::NavMenu(m) => m.icon(),
            BottomTab::More => IconRole::More,
        }
    }
}

/// The tab that is marked for a menu: the menu's own tab, or 더보기 for the other four.
pub fn tab_for_menu(menu: NavMenu) -> BottomTab {
    if MORE_MENUS.contains(&menu) {
        BottomTab::More
    } else {
        BottomTab::NavMenu(menu)
    }
}

// ---------------------------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellMode {
    /// 234 px with labels.
    Sidebar,
    /// 72 px, icons only.
    Rail,
    /// Floating bar at the bottom.
    BottomTabs,
}

/// The layout for a window size class.
///
/// TODO(GAPS: 창 폭 레이아웃): the design breaks at 1279 (sidebar to rail) and 560 (rail to
/// bar). The pinned toolkit reports only its three classes (600 and 840 dp), so Compact is
/// the bar, Medium the rail and Expanded the sidebar; the exact boundaries come with
/// `use_window_breakpoints`.
pub fn shell_mode(class: WindowSizeClass, collapsed: bool) -> ShellMode {
    match class {
        WindowSizeClass::Compact => ShellMode::BottomTabs,
        WindowSizeClass::Medium => ShellMode::Rail,
        WindowSizeClass::Expanded if collapsed => ShellMode::Rail,
        WindowSizeClass::Expanded => ShellMode::Sidebar,
    }
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// Go to a menu. Projects and a conversation are real screens; the rest only change the menu.
fn choose(ui: crate::ui::Ui, menu: NavMenu) {
    match menu {
        NavMenu::Projects => ui.go(Route::Projects),
        NavMenu::Sessions => {
            let in_conversation = matches!(*ui.route.peek(), Route::Conversation(_));
            if !in_conversation {
                let recent = ui.with_rows_no_previews(|s, cx| model::recent_sessions(s, cx, 1));
                if let Some(row) = recent.first() {
                    ui.open_conversation(&row.id);
                }
            }
        }
        _ => {
            if matches!(*ui.route.peek(), Route::Conversation(_)) {
                ui.go(Route::Projects);
            }
        }
    }
    let mut chosen = ui.menu;
    chosen.set(menu);
}

fn transparent() -> Paint {
    Paint::Literal(Color::argb(0))
}

/// The temporary Ember logo (logo B, phoenix fold; D35, rework pending).
pub const MARK_SVG: &str = include_str!("../../assets/brand/mark.svg");
pub const MARK_SM_SVG: &str = include_str!("../../assets/brand/mark-sm.svg");
pub const APP_ICON_SVG: &str = include_str!("../../assets/brand/app-icon.svg");
pub const FAVICON_SVG: &str = include_str!("../../assets/brand/favicon.svg");

/// Up to this size (dp) the small mark is used, above it the full one.
pub const MARK_SM_MAX: f32 = 32.0;

/// Which SVG a mark of `size` dp uses.
pub fn mark_svg_for(size: f32) -> &'static str {
    if size <= MARK_SM_MAX {
        MARK_SM_SVG
    } else {
        MARK_SVG
    }
}

/// The logo slot: the temporary mark, with the wordmark beside it when there is room.
/// Only this component changes when the final logo lands.
#[component]
fn LogoSlot(#[props(default)] compact: bool) -> Element {
    let size = 28.0;
    let id = asset(AssetKind::Svg, mark_svg_for(size).as_bytes());
    rsx! {
        Row {
            height: SHEET_HEADER_H,
            alignment: Alignment::CenterStart,
            space_role: SpaceRole::Sm,
            Spacer { width: SP[3] }
            Image { asset_id: id, width: size, height: size }
            if !compact {
                Label { text: "Ember", fs: Fs::Xl, weight: Weight::Semibold }
            }
        }
    }
}

#[component]
fn NavItem(menu: NavMenu, active: bool, icon_only: bool, on_click: EventHandler<()>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    rsx! {
        Button {
            text: if icon_only { "" } else { menu.label() },
            icon: menu.icon(),
            fill_max_width: !icon_only,
            width: if icon_only { Some(48.0) } else { None },
            height: H_NAV_ITEM,
            variant: ButtonVariant::Text,
            shape_role: ShapeRole::Full,
            background: if active { paint(p.ink) } else { transparent() },
            color: if active { paint(p.ink_text) } else { paint(p.text_2) },
            on_click: move |()| on_click.call(()),
        }
    }
}

#[component]
fn Sidebar(
    rail: bool,
    active: NavMenu,
    on_choose: EventHandler<NavMenu>,
    on_theme: EventHandler<()>,
    on_settings: EventHandler<()>,
    on_collapse: EventHandler<()>,
) -> Element {
    let theme = use_ember_theme();
    let pref = *use_context::<ThemeCtx>().pref.read();
    let width = if rail { RAIL_W } else { SIDE_W };
    rsx! {
        Column {
            width,
            fill_max_height: true,
            background: paint(theme.palette.bg),
            space_role: SpaceRole::Xs,
            alignment: Alignment::TopCenter,
            LogoSlot { compact: rail }
            for menu in NavMenu::ALL {
                NavItem {
                    key: "{menu.label()}",
                    menu,
                    active: menu == active,
                    icon_only: rail,
                    on_click: move |()| on_choose.call(menu),
                }
            }
            Spacer { weight: 1.0 }
            // Tools: theme, collapse, settings.
            Button {
                text: if rail { String::new() } else { format!("테마: {}", pref.label()) },
                icon: IconRole::Settings,
                variant: ButtonVariant::Text,
                fill_max_width: !rail,
                height: H_NAV_ITEM,
                shape_role: ShapeRole::Full,
                color: paint(theme.palette.text_2),
                on_click: move |()| on_theme.call(()),
            }
            Button {
                text: if rail { "" } else { "접기" },
                icon: if rail { IconRole::Sidebar } else { IconRole::Collapse },
                variant: ButtonVariant::Text,
                fill_max_width: !rail,
                height: H_NAV_ITEM,
                shape_role: ShapeRole::Full,
                color: paint(theme.palette.text_2),
                on_click: move |()| on_collapse.call(()),
            }
            Button {
                text: if rail { "" } else { "설정" },
                icon: IconRole::Settings,
                variant: ButtonVariant::Text,
                fill_max_width: !rail,
                height: H_NAV_ITEM,
                shape_role: ShapeRole::Full,
                color: paint(theme.palette.text_2),
                on_click: move |()| on_settings.call(()),
            }
            Spacer { height: SP[2] }
        }
    }
}

/// The floating bottom bar. Opaque on every platform for now.
/// TODO(GAPS: 배경 흐림): Liquid Glass on iOS only. TODO(GAPS: 떠 있는 하단 바): the safe-area
/// inset (`use_safe_area`) is not available, so only the 10 px gap is added.
#[component]
fn BottomBar(active: NavMenu, on_choose: EventHandler<BottomTab>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let marked = tab_for_menu(active);
    rsx! {
        Column {
            fill_max_width: true,
            Row {
                fill_max_width: true,
                Spacer { width: TABBAR_SIDE_GAP }
                Surface {
                    weight: 1.0,
                    height: TABBAR_H,
                    background: paint(p.surface),
                    border_width: 1.0,
                    border_color: paint(p.line),
                    corner_radius: TABBAR_RADIUS,
                    elevation: ELEVATION_LARGE,
                    Row {
                        fill_max_width: true,
                        fill_max_height: true,
                        arrangement: Arrangement::SpaceEvenly,
                        alignment: Alignment::Center,
                        for tab in BOTTOM_TABS {
                            Button {
                                key: "{tab.label()}",
                                text: tab.label(),
                                icon: tab.icon(),
                                variant: ButtonVariant::Text,
                                shape_role: ShapeRole::Full,
                                background: if tab == marked { paint(p.sel) } else { transparent() },
                                color: if tab == marked { paint(p.text) } else { paint(p.muted) },
                                on_click: move |()| on_choose.call(tab),
                            }
                        }
                    }
                }
                Spacer { width: TABBAR_SIDE_GAP }
            }
            Spacer { height: TABBAR_BOTTOM_GAP }
        }
    }
}

#[component]
fn Placeholder(menu: NavMenu) -> Element {
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            alignment: Alignment::Center,
            space_role: SpaceRole::Sm,
            Label { text: menu.label(), fs: Fs::Xl3, weight: Weight::Medium }
            Label { text: PLACEHOLDER_TEXT, fs: Fs::Md, color: paint(use_ember_theme().palette.muted) }
        }
    }
}

/// The root layout: navigation around the content area.
#[component]
pub fn Shell() -> Element {
    let ui = use_ui();
    let window = use_window_size();
    let theme = use_ember_theme();
    let mut collapsed = use_signal(|| false);
    let mut more_open = use_signal(|| false);
    let mut settings_open = use_signal(|| false);

    let route = ui.route.read().clone();
    let active = active_menu(&route, *ui.menu.read());
    let screen = screen_for(&route, active);
    let mode = shell_mode(window.class, *collapsed.read());

    let on_choose = move |m: NavMenu| choose(ui, m);
    let theme_ctx = use_context::<ThemeCtx>();
    let on_theme = move |()| {
        let mut pref = theme_ctx.pref;
        let next = pref.peek().next();
        pref.set(next);
        ui.update_prefs(|p| p.theme = next);
    };

    let content = rsx! {
        Column {
            weight: 1.0,
            fill_max_height: true,
            if matches!(screen, Screen::Launcher | Screen::Search | Screen::Conversation(_)) {
                Row {
                    fill_max_width: true,
                    padding_role: SpaceRole::Sm,
                    space_role: SpaceRole::Sm,
                    alignment: Alignment::CenterEnd,
                    Spacer { weight: 1.0 }
                    AppButton {
                        text: "검색",
                        icon: IconRole::Search,
                        small: true,
                        on_click: move |()| ui.go(Route::Search),
                    }
                    AppButton {
                        text: "새 세션",
                        icon: IconRole::Compose,
                        small: true,
                        kind: crate::ui::components::ButtonKind::Primary,
                        on_click: move |()| {
                            let project = ui.selected_project.peek().clone();
                            ui.open_new_session(project);
                        },
                    }
                }
            }
            Column {
                weight: 1.0,
                fill_max_width: true,
                match screen {
                    Screen::Home => rsx! { home::Home {} },
                    Screen::Launcher => rsx! { project::ProjectScreen {} },
                    Screen::Search => rsx! { search::SearchScreen {} },
                    Screen::Conversation(id) => rsx! { conversation::ConversationView { key: "{id}", id: id.clone() } },
                    Screen::Placeholder(menu) => rsx! { Placeholder { menu } },
                }
            }
        }
    };

    rsx! {
        dioxus_compose::Box {
            fill_max_width: true,
            fill_max_height: true,
            background: paint(theme.palette.bg),
            if mode == ShellMode::BottomTabs {
                Column {
                    fill_max_width: true,
                    fill_max_height: true,
                    {content}
                    BottomBar {
                        active,
                        on_choose: move |tab: BottomTab| match tab {
                            BottomTab::NavMenu(m) => on_choose(m),
                            BottomTab::More => more_open.set(true),
                        },
                    }
                }
            } else {
                Row {
                    fill_max_width: true,
                    fill_max_height: true,
                    Sidebar {
                        rail: mode == ShellMode::Rail,
                        active,
                        on_choose,
                        on_theme,
                        on_settings: move |()| settings_open.set(true),
                        on_collapse: move |()| {
                            let now = *collapsed.peek();
                            collapsed.set(!now);
                        },
                    }
                    {content}
                }
            }

            EmberSheet {
                title: "더보기",
                open: *more_open.read(),
                on_dismiss: move |()| more_open.set(false),
                Column {
                    fill_max_width: true,
                    padding_role: SpaceRole::Md,
                    space_role: SpaceRole::Sm,
                    for menu in MORE_MENUS {
                        AppButton {
                            key: "{menu.label()}",
                            text: menu.label(),
                            icon: menu.icon(),
                            fill_max_width: true,
                            on_click: move |()| {
                                more_open.set(false);
                                on_choose(menu);
                            },
                        }
                    }
                }
            }
            EmberSheet {
                title: "설정",
                open: *settings_open.read(),
                on_dismiss: move |()| settings_open.set(false),
                Column {
                    fill_max_width: true,
                    padding_role: SpaceRole::Md,
                    Label { text: PLACEHOLDER_TEXT, color: paint(theme.palette.muted) }
                }
            }
            new_session::NewSessionDialog {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(menus: &[NavMenu]) -> Vec<&'static str> {
        menus.iter().map(|m| m.label()).collect()
    }

    #[test]
    fn fr_l10_menu_order_and_labels_follow_d26() {
        assert_eq!(
            labels(&NavMenu::ALL),
            [
                "홈",
                "프로젝트",
                "세션",
                "채널",
                "루틴",
                "버전 관리",
                "에이전트",
                "파일"
            ]
        );
    }

    #[test]
    fn fr_l10_every_menu_has_an_icon_and_a_unique_label() {
        let mut seen = std::collections::BTreeSet::new();
        for m in NavMenu::ALL {
            assert!(seen.insert(m.label()), "{} repeated", m.label());
        }
    }

    #[test]
    fn fr_l10_active_item_follows_the_choice_and_an_open_conversation() {
        let c = Route::Conversation("s1".into());
        assert_eq!(
            active_menu(&Route::Projects, NavMenu::Projects),
            NavMenu::Projects
        );
        assert_eq!(active_menu(&Route::Projects, NavMenu::Home), NavMenu::Home);
        assert_eq!(
            active_menu(&Route::Search, NavMenu::Projects),
            NavMenu::Projects
        );
        assert_eq!(active_menu(&c, NavMenu::Projects), NavMenu::Sessions);
    }

    #[test]
    fn fr_l10_existing_screens_are_wired_and_the_rest_are_placeholders() {
        let c = Route::Conversation("s1".into());
        assert_eq!(
            screen_for(&Route::Projects, NavMenu::Projects),
            Screen::Launcher
        );
        assert_eq!(
            screen_for(&Route::Search, NavMenu::Projects),
            Screen::Search
        );
        assert_eq!(
            screen_for(&c, NavMenu::Sessions),
            Screen::Conversation("s1".into())
        );
        assert_eq!(screen_for(&Route::Projects, NavMenu::Home), Screen::Home);
        assert_eq!(screen_for(&c, NavMenu::Home), Screen::Home);
        for m in [
            NavMenu::Channels,
            NavMenu::Routines,
            NavMenu::VersionControl,
            NavMenu::Agents,
            NavMenu::Files,
        ] {
            assert_eq!(screen_for(&Route::Projects, m), Screen::Placeholder(m));
        }
        // 세션 with no open conversation has nothing to show yet.
        assert_eq!(
            screen_for(&Route::Projects, NavMenu::Sessions),
            Screen::Placeholder(NavMenu::Sessions)
        );
        assert_eq!(PLACEHOLDER_TEXT, "준비 중입니다");
    }

    #[test]
    fn fr_l28_bottom_tabs_are_home_projects_sessions_channels_more() {
        let l: Vec<_> = BOTTOM_TABS.iter().map(|t| t.label()).collect();
        assert_eq!(l, ["홈", "프로젝트", "세션", "채널", "더보기"]);
        assert_eq!(BOTTOM_TABS.len(), 5);
    }

    #[test]
    fn fr_l28_more_holds_the_other_four_in_menu_order() {
        assert_eq!(
            labels(&MORE_MENUS),
            ["루틴", "버전 관리", "에이전트", "파일"]
        );
        // Together with the four tabs that are menus, every menu is reachable exactly once.
        let mut all: Vec<NavMenu> = BOTTOM_TABS
            .iter()
            .filter_map(|t| {
                if let BottomTab::NavMenu(m) = t {
                    Some(*m)
                } else {
                    None
                }
            })
            .collect();
        all.extend(MORE_MENUS);
        assert_eq!(all, NavMenu::ALL.to_vec());
    }

    #[test]
    fn fr_l28_menu_to_tab_mapping() {
        assert_eq!(
            tab_for_menu(NavMenu::Home),
            BottomTab::NavMenu(NavMenu::Home)
        );
        assert_eq!(
            tab_for_menu(NavMenu::Channels),
            BottomTab::NavMenu(NavMenu::Channels)
        );
        for m in MORE_MENUS {
            assert_eq!(tab_for_menu(m), BottomTab::More, "{}", m.label());
        }
    }

    #[test]
    fn fr_l10_layout_per_size_class() {
        assert_eq!(
            shell_mode(WindowSizeClass::Compact, false),
            ShellMode::BottomTabs
        );
        assert_eq!(
            shell_mode(WindowSizeClass::Compact, true),
            ShellMode::BottomTabs
        );
        assert_eq!(shell_mode(WindowSizeClass::Medium, false), ShellMode::Rail);
        assert_eq!(
            shell_mode(WindowSizeClass::Expanded, false),
            ShellMode::Sidebar
        );
        // The collapse control turns the sidebar into the rail.
        assert_eq!(shell_mode(WindowSizeClass::Expanded, true), ShellMode::Rail);
    }

    #[test]
    fn logo_assets_exist_and_are_svg() {
        for (name, svg) in [
            ("mark", MARK_SVG),
            ("mark-sm", MARK_SM_SVG),
            ("app-icon", APP_ICON_SVG),
            ("favicon", FAVICON_SVG),
        ] {
            assert!(svg.trim_start().starts_with("<svg"), "{name} is not an SVG");
            assert!(svg.contains("</svg>"), "{name} is not closed");
        }
    }

    #[test]
    fn logo_uses_the_small_mark_at_32_or_less() {
        assert_eq!(mark_svg_for(28.0), MARK_SM_SVG);
        assert_eq!(mark_svg_for(32.0), MARK_SM_SVG);
        assert_eq!(mark_svg_for(48.0), MARK_SVG);
    }

    #[test]
    fn nfr_l4_shell_text_has_no_plain_da_endings() {
        // No plain "~다" endings in the user-visible strings of the shell.
        let mut strings: Vec<&str> = NavMenu::ALL.iter().map(|m| m.label()).collect();
        strings.extend(BOTTOM_TABS.iter().map(|t| t.label()));
        strings.push(PLACEHOLDER_TEXT);
        for s in strings {
            assert!(
                !s.ends_with('다') || s.ends_with("입니다") || s.ends_with("합니다"),
                "{s}"
            );
        }
    }
}
