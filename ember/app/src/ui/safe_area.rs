//! Title-bar safe area (FR-L33, #213): how far the top strip of the window is covered by the
//! operating system's window controls, so a screen can draw its own header in that strip instead
//! of leaving an empty band above it.
//!
//! This file is pure (no toolkit calls). [`safe_area`] is the only place the numbers come from;
//! every screen reads them through it and through [`top_bar_pad`].
//!
//! TODO(compose-rust#345): the toolkit's `Chrome::FullSize` and `WindowSafeArea { top, leading,
//! trailing }` are approved in its SPEC but not built in the pinned v0.0.4 (v0.0.5 has no
//! caption inset either). When they land, only the body of [`safe_area`] changes: it returns
//! the toolkit's value, and [`safe_area_for`] and the constants below are deleted.

use crate::ui::shell::{caption_reserved_width, Screen};

/// The window controls' footprint along the top edge, in dp. `top` is the height of the strip
/// they occupy, `leading` the width at the left (macOS traffic lights), `trailing` the width at
/// the right (Windows caption buttons).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SafeArea {
    pub top: f32,
    pub leading: f32,
    pub trailing: f32,
}

/// Extra padding for one top bar: what it must keep clear at each end.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EdgePad {
    pub leading: f32,
    pub trailing: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Other,
}

/// macOS traffic lights: three 12 dp buttons at 20 dp spacing, inset 20 dp, plus a 14 dp gap
/// (placeholder until the toolkit reports it).
pub const MACOS_TRAFFIC_LIGHTS_LEADING_DP: f32 = 78.0;
/// Height of the macOS title-bar strip the traffic lights centre in (placeholder).
pub const MACOS_TITLE_BAR_TOP_DP: f32 = 28.0;
/// Whether the window opens with content under the macOS title bar. False until the toolkit
/// can open it that way (TODO(compose-rust#345)); with false the OS draws its own title bar and
/// nothing needs clearing.
pub const MACOS_FULL_SIZE_CHROME: bool = false;

pub fn current_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Other
    }
}

/// The safe area for `platform`. Windows keeps its caption buttons' width at the right (the
/// toolkit draws them, #340, #204, #205); macOS clears the traffic lights only when the window
/// is full size; Linux and mobile have nothing to clear.
pub fn safe_area_for(platform: Platform, macos_full_size: bool) -> SafeArea {
    match platform {
        Platform::Windows => SafeArea {
            top: crate::ui::shell::WINDOWS_CAPTION_HEIGHT_DP,
            leading: 0.0,
            trailing: caption_reserved_width(true),
        },
        Platform::MacOs if macos_full_size => SafeArea {
            top: MACOS_TITLE_BAR_TOP_DP,
            leading: MACOS_TRAFFIC_LIGHTS_LEADING_DP,
            trailing: 0.0,
        },
        _ => SafeArea::default(),
    }
}

/// The single swap point for the toolkit's `WindowSafeArea` (TODO(compose-rust#345)).
pub fn safe_area() -> SafeArea {
    safe_area_for(current_platform(), MACOS_FULL_SIZE_CHROME)
}

/// Padding a top bar adds so its content avoids the window controls. A bar clears the leading
/// inset only when it touches the window's left edge, and the trailing inset only when it
/// touches the right edge.
pub fn top_bar_pad(area: SafeArea, at_leading_edge: bool, at_trailing_edge: bool) -> EdgePad {
    EdgePad {
        leading: if at_leading_edge { area.leading } else { 0.0 },
        trailing: if at_trailing_edge { area.trailing } else { 0.0 },
    }
}

/// Extra height a left-edge column (the sidebar) adds above its first row so it starts below the
/// traffic lights.
pub fn sidebar_top_inset(area: SafeArea) -> f32 {
    if area.leading > 0.0 { area.top } else { 0.0 }
}

/// Whether the screen draws its own header in the title-bar strip. Such a screen takes the
/// safe area itself, so the shell must not add a separate empty strip above it.
pub fn screen_owns_title_bar(screen: &Screen) -> bool {
    use crate::ui::shell::NavMenu;
    matches!(
        screen,
        Screen::Conversation(_) | Screen::Placeholder(NavMenu::Sessions)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::shell::NavMenu;

    #[test]
    fn fr_l33_windows_reserves_the_caption_buttons_at_the_trailing_edge() {
        let a = safe_area_for(Platform::Windows, false);
        assert_eq!(a.trailing, 138.0);
        assert_eq!(a.leading, 0.0);
        assert_eq!(a.top, 32.0);
    }

    #[test]
    fn fr_l33_macos_clears_the_traffic_lights_only_in_full_size_mode() {
        assert_eq!(safe_area_for(Platform::MacOs, false), SafeArea::default());
        let a = safe_area_for(Platform::MacOs, true);
        assert_eq!(a.leading, MACOS_TRAFFIC_LIGHTS_LEADING_DP);
        assert_eq!(a.trailing, 0.0);
        assert!(a.top > 0.0);
    }

    #[test]
    fn fr_l33_linux_and_mobile_have_no_safe_area() {
        assert_eq!(safe_area_for(Platform::Other, true), SafeArea::default());
        assert_eq!(safe_area_for(Platform::Other, false), SafeArea::default());
    }

    #[test]
    fn fr_l33_a_top_bar_clears_only_the_edge_it_touches() {
        let win = safe_area_for(Platform::Windows, false);
        assert_eq!(top_bar_pad(win, false, true), EdgePad { leading: 0.0, trailing: 138.0 });
        assert_eq!(top_bar_pad(win, true, false), EdgePad::default());
        let mac = safe_area_for(Platform::MacOs, true);
        assert_eq!(top_bar_pad(mac, true, false).leading, MACOS_TRAFFIC_LIGHTS_LEADING_DP);
        assert_eq!(top_bar_pad(mac, false, true), EdgePad::default());
    }

    #[test]
    fn fr_l33_only_the_macos_sidebar_gets_a_top_inset() {
        assert_eq!(sidebar_top_inset(safe_area_for(Platform::Windows, false)), 0.0);
        assert_eq!(sidebar_top_inset(safe_area_for(Platform::Other, false)), 0.0);
        assert_eq!(
            sidebar_top_inset(safe_area_for(Platform::MacOs, true)),
            MACOS_TITLE_BAR_TOP_DP
        );
    }

    #[test]
    fn fr_l33_the_session_screen_owns_the_title_bar_strip() {
        assert!(screen_owns_title_bar(&Screen::Conversation("s".into())));
        assert!(screen_owns_title_bar(&Screen::Placeholder(NavMenu::Sessions)));
        assert!(!screen_owns_title_bar(&Screen::Launcher));
        assert!(!screen_owns_title_bar(&Screen::Home));
    }
}
