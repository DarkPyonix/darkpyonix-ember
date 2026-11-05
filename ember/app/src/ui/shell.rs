//! The app shell (FR-L10, FR-L28, `docs/DESIGN.md` section 4.7): the eight menus, a sidebar
//! that collapses to a rail, and bottom tabs on a phone.
//!
//! The decisions (menu order and labels, which screen a menu shows, which bottom tab a menu
//! belongs to, which layout a window size gets) are plain functions and enums at the top of
//! this file, tested without a renderer. The components below draw them.

use crate::ui::Route;


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

    /// The mockup's icon for the menu (Heroicons outline, 1.5 stroke), as an SVG whose stroke is
    /// `currentColor`. The toolkit's `IconRole` set has no such glyphs, so the app registers
    /// these itself.
    pub fn svg(self) -> &'static str {
        match self {
            NavMenu::Home => include_str!("../../assets/nav/home.svg"),
            NavMenu::Projects => include_str!("../../assets/nav/folder.svg"),
            NavMenu::Sessions => include_str!("../../assets/nav/grid.svg"),
            NavMenu::Channels => include_str!("../../assets/nav/hash.svg"),
            NavMenu::Routines => include_str!("../../assets/nav/repeat.svg"),
            NavMenu::VersionControl => include_str!("../../assets/nav/git.svg"),
            NavMenu::Agents => include_str!("../../assets/nav/cpu.svg"),
            NavMenu::Files => include_str!("../../assets/nav/arrows-right-left.svg"),
        }
    }
}

pub const ICON_MORE_SVG: &str = include_str!("../../assets/nav/more.svg");
pub const ICON_SUN_SVG: &str = include_str!("../../assets/nav/sun.svg");
pub const ICON_MOON_SVG: &str = include_str!("../../assets/nav/moon.svg");
pub const ICON_GEAR_SVG: &str = include_str!("../../assets/nav/gear.svg");

/// `svg` at `px` dp with its `currentColor` stroke set to `argb` (alpha dropped).
pub fn tinted_svg(svg: &str, px: f32, argb: u32) -> String {
    let size = format!("{}", px.round() as i32);
    svg.replace("currentColor", &format!("#{:06x}", argb & 0x00ff_ffff))
        .replacen("width=\"24\" height=\"24\"", &format!("width=\"{size}\" height=\"{size}\""), 1)
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
    /// 파일: device file transfer (FR-L27).
    Files,
    Launcher,
    Search,
    Conversation(String),
    /// 에이전트: harnesses, models, profiles, network, usage (FR-L20).
    Agents,
    /// 루틴 (FR-L25).
    Routines,
    /// 버전 관리 (FR-L19).
    VersionControl,
    /// 채널 (FR-L12).
    Channels,
    /// A menu whose screen is not built yet ("준비 중입니다").
    Placeholder(NavMenu),
}

/// The screen for the active menu. Home is 홈, `Launcher` renders ProjectScreen for 프로젝트 and the
/// conversation stands in for 세션; every other menu is a placeholder until its screen lands.
pub fn screen_for(route: &Route, active: NavMenu) -> Screen {
    match (active, route) {
        (NavMenu::Home, _) => Screen::Home,
        (NavMenu::Files, _) => Screen::Files,
        (NavMenu::Routines, _) => Screen::Routines,
        (NavMenu::Sessions, Route::Conversation(id)) => Screen::Conversation(id.clone()),
        (NavMenu::Projects, Route::Search) => Screen::Search,
        (NavMenu::Projects, _) => Screen::Launcher,
        (NavMenu::Agents, _) => Screen::Agents,
        (NavMenu::VersionControl, _) => Screen::VersionControl,
        (NavMenu::Channels, _) => Screen::Channels,
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

    pub fn svg(self) -> &'static str {
        match self {
            BottomTab::NavMenu(m) => m.svg(),
            BottomTab::More => ICON_MORE_SVG,
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

/// Widest window (dp) that shows the icon rail; wider shows the full sidebar (mockup
/// `@media (max-width:1279px)`).
pub const RAIL_MAX_DP: f32 = 1279.0;
/// Widest window (dp) that shows the bottom tab bar (mockup `@media (max-width:560px)`).
pub const BAR_MAX_DP: f32 = 560.0;

/// The layout for a window width. 1024 and 768 get the rail, 390 the bar. The collapse toggle
/// turns a wide window's sidebar into the rail.
pub fn nav_mode(width_dp: f32, collapsed: bool) -> ShellMode {
    if width_dp <= BAR_MAX_DP {
        ShellMode::BottomTabs
    } else if width_dp <= RAIL_MAX_DP || collapsed {
        ShellMode::Rail
    } else {
        ShellMode::Sidebar
    }
}

/// The collapse choice after the toggle at `width_dp`. It exists only in the full-sidebar range
/// (above [`RAIL_MAX_DP`]); below, the width alone decides, so the toggle changes nothing and
/// the choice cannot leak to another breakpoint (#218).
pub fn toggled_collapse(width_dp: f32, collapsed: bool) -> bool {
    if width_dp > RAIL_MAX_DP { !collapsed } else { collapsed }
}

/// Live numbers for the navigation. `None` or 0 means no data feeds it yet, so nothing is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavCounts {
    /// Projects, shown as a muted count beside 프로젝트.
    pub projects: Option<usize>,
    /// Sessions waiting for approval, shown as a red badge on 세션.
    pub waiting: usize,
    /// Unread channel rooms, a red badge on 채널 (no live source yet).
    pub unread_rooms: usize,
    /// Enabled routines, a muted count beside 루틴 (no live source yet).
    pub routines: Option<usize>,
    /// Running file transfers, a muted count beside 파일 (no live source yet).
    pub transfers: usize,
}

/// What sits at the right of a menu row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavBadge {
    None,
    /// Plain muted number (sidebar only).
    Count(usize),
    /// Red badge (everywhere).
    Alert(usize),
}

pub fn nav_badge(menu: NavMenu, c: &NavCounts) -> NavBadge {
    let alert = |n: usize| if n > 0 { NavBadge::Alert(n) } else { NavBadge::None };
    match menu {
        NavMenu::Projects => c.projects.map_or(NavBadge::None, NavBadge::Count),
        NavMenu::Sessions => alert(c.waiting),
        NavMenu::Channels => alert(c.unread_rooms),
        NavMenu::Routines => c.routines.map_or(NavBadge::None, NavBadge::Count),
        NavMenu::Files if c.transfers > 0 => NavBadge::Count(c.transfers),
        _ => NavBadge::None,
    }
}

/// The red badge a bottom tab shows: the alert of its menu. 더보기 sums the alerts of the
/// menus it hides (none today).
pub fn tab_badge(tab: BottomTab, c: &NavCounts) -> Option<usize> {
    match tab {
        BottomTab::NavMenu(m) => match nav_badge(m, c) {
            NavBadge::Alert(n) => Some(n),
            _ => None,
        },
        BottomTab::More => {
            let n: usize = MORE_MENUS
                .iter()
                .map(|m| if let NavBadge::Alert(n) = nav_badge(*m, c) { n } else { 0 })
                .sum();
            (n > 0).then_some(n)
        }
    }
}

/// Badge text: the number, capped at `99+`.
pub fn badge_text(n: usize) -> String {
    if n > 99 { "99+".into() } else { n.to_string() }
}

/// The signed-in user shown in the sidebar footer. No account source exists yet, so this is
/// the mockup's demo user.
pub const DEMO_USER: (&str, &str) = ("BREW", "ncmnd");
/// The org line under the wordmark.
pub const ORG_NAME: &str = "darkpyonix";

/// First letter of a name, for the avatar.
pub fn avatar_initial(name: &str) -> String {
    name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()
}

/// Width of the three caption buttons Windows draws at the window's top-right (46 dp each).
/// compose-rust #340 draws them when the app takes the caption; `LocalWindowCaption.buttonsWidth`
/// reaches only a TopAppBar at the top of the tree, which Ember's shell does not open with, and
/// dioxus-compose at the pinned rev exposes no caption width. So Ember reserves it itself.
pub const WINDOWS_CAPTION_BUTTONS_DP: f32 = 138.0;
/// Windows 11 caption height (compose-rust #340).
pub const WINDOWS_CAPTION_HEIGHT_DP: f32 = 32.0;

/// Width to keep clear at the top-right: the caption buttons on Windows, nothing elsewhere.
pub fn caption_reserved_width(windows: bool) -> f32 {
    if windows { WINDOWS_CAPTION_BUTTONS_DP } else { 0.0 }
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
        assert_eq!(
            screen_for(&Route::Projects, NavMenu::Routines),
            Screen::Routines
        );
        assert_eq!(screen_for(&Route::Projects, NavMenu::Channels), Screen::Channels);
        assert_eq!(screen_for(&Route::Projects, NavMenu::Files), Screen::Files);
        assert_eq!(screen_for(&Route::Projects, NavMenu::Agents), Screen::Agents);
        assert_eq!(
            screen_for(&Route::Projects, NavMenu::VersionControl),
            Screen::VersionControl
        );
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
    fn fr_l10_nav_mode_follows_the_mockup_breakpoints() {
        // 1440: sidebar. 1280 is still the sidebar, 1279 and 1024 and 768 are the rail,
        // 561 is the rail, 560 and 390 are the bar.
        assert_eq!(nav_mode(1440.0, false), ShellMode::Sidebar);
        assert_eq!(nav_mode(1280.0, false), ShellMode::Sidebar);
        assert_eq!(nav_mode(1279.0, false), ShellMode::Rail);
        assert_eq!(nav_mode(1024.0, false), ShellMode::Rail);
        assert_eq!(nav_mode(768.0, false), ShellMode::Rail);
        assert_eq!(nav_mode(561.0, false), ShellMode::Rail);
        assert_eq!(nav_mode(560.0, false), ShellMode::BottomTabs);
        assert_eq!(nav_mode(390.0, false), ShellMode::BottomTabs);
        assert_eq!(nav_mode(390.0, true), ShellMode::BottomTabs);
    }

    #[test]
    fn nav_mode_does_not_remember_the_previous_width() {
        // 1440 -> 1024 -> 1440, and 768 -> 1024: the mode follows the width only.
        let c = false;
        assert_eq!(nav_mode(1440.0, c), ShellMode::Sidebar);
        assert_eq!(nav_mode(1024.0, c), ShellMode::Rail);
        assert_eq!(nav_mode(1440.0, c), ShellMode::Sidebar);
        assert_eq!(nav_mode(768.0, c), ShellMode::Rail);
        assert_eq!(nav_mode(1024.0, c), ShellMode::Rail);
    }

    #[test]
    fn collapse_toggle_applies_only_in_the_sidebar_range() {
        assert!(toggled_collapse(1440.0, false));
        assert!(!toggled_collapse(1440.0, true));
        // A click on the brand at 1024 or 768 must not store a choice for 1440.
        assert!(!toggled_collapse(1024.0, false));
        assert!(!toggled_collapse(768.0, false));
        assert_eq!(nav_mode(1440.0, toggled_collapse(1024.0, false)), ShellMode::Sidebar);
    }

    #[test]
    fn fr_l10_collapse_turns_the_sidebar_into_the_rail() {
        assert_eq!(nav_mode(1440.0, true), ShellMode::Rail);
        assert_eq!(nav_mode(1024.0, true), ShellMode::Rail);
    }

    #[test]
    fn fr_l10_every_menu_has_its_own_mockup_icon() {
        let mut seen = std::collections::BTreeSet::new();
        for m in NavMenu::ALL {
            let svg = m.svg();
            assert!(svg.trim_start().starts_with("<svg"), "{}", m.label());
            assert!(svg.contains("currentColor"), "{}", m.label());
            assert!(seen.insert(svg), "{} shares an icon", m.label());
        }
        assert!(BottomTab::More.svg().contains("<svg"));
    }

    #[test]
    fn tinted_svg_sets_colour_and_size() {
        let t = tinted_svg(NavMenu::Home.svg(), 18.0, 0xff1c1b19);
        assert!(t.contains("stroke=\"#1c1b19\""));
        assert!(t.contains("width=\"18\" height=\"18\""));
        assert!(!t.contains("currentColor"));
    }

    #[test]
    fn fr_l10_badges_come_from_live_counts_only() {
        let none = NavCounts::default();
        for m in NavMenu::ALL {
            assert_eq!(nav_badge(m, &none), NavBadge::None, "{}", m.label());
        }
        let c = NavCounts { projects: Some(5), waiting: 3, unread_rooms: 5, routines: Some(6), transfers: 2 };
        assert_eq!(nav_badge(NavMenu::Projects, &c), NavBadge::Count(5));
        assert_eq!(nav_badge(NavMenu::Sessions, &c), NavBadge::Alert(3));
        assert_eq!(nav_badge(NavMenu::Channels, &c), NavBadge::Alert(5));
        assert_eq!(nav_badge(NavMenu::Routines, &c), NavBadge::Count(6));
        assert_eq!(nav_badge(NavMenu::Files, &c), NavBadge::Count(2));
        assert_eq!(nav_badge(NavMenu::Home, &c), NavBadge::None);
    }

    #[test]
    fn fr_l28_tab_badges_are_red_alerts_only() {
        let c = NavCounts { projects: Some(5), waiting: 3, unread_rooms: 5, routines: Some(6), transfers: 2 };
        assert_eq!(tab_badge(BottomTab::NavMenu(NavMenu::Sessions), &c), Some(3));
        assert_eq!(tab_badge(BottomTab::NavMenu(NavMenu::Channels), &c), Some(5));
        assert_eq!(tab_badge(BottomTab::NavMenu(NavMenu::Projects), &c), None);
        assert_eq!(tab_badge(BottomTab::More, &c), None);
    }

    #[test]
    fn badge_text_caps_at_99() {
        assert_eq!(badge_text(3), "3");
        assert_eq!(badge_text(99), "99");
        assert_eq!(badge_text(100), "99+");
    }

    #[test]
    fn sidebar_footer_and_brand_text() {
        assert_eq!(ORG_NAME, "darkpyonix");
        assert_eq!(avatar_initial(DEMO_USER.0), "B");
        assert_eq!(avatar_initial(""), "");
    }

    #[test]
    fn caption_width_is_reserved_only_on_windows() {
        assert_eq!(caption_reserved_width(true), 138.0);
        assert_eq!(caption_reserved_width(false), 0.0);
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
