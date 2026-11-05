//! Title-bar safe area (FR-L33, #213): how far the top strip of the window is covered by the
//! operating system's window controls, so a screen can draw its own header in that strip instead
//! of leaving an empty band above it.
//!
//! On macOS the window opens with `Chrome::FullSize` (compose-rust v0.0.6, #345): content starts
//! at the top and the traffic lights float over it. The toolkit reports their footprint as
//! `WindowSafeArea { top, leading, trailing }`, and [`safe_area`] returns it. On Windows the
//! toolkit draws the caption buttons and Ember reserves their width itself. Elsewhere there is
//! nothing to clear. Everything but [`safe_area`] is pure.

use crate::ui::shell::{caption_reserved_width, ShellMode, WINDOWS_CAPTION_HEIGHT_DP};

/// The window controls' footprint along the top edge, in dp. `top` is the height of the strip
/// they occupy, `leading` the width at the left (macOS traffic lights), `trailing` the width at
/// the right (Windows caption buttons).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SafeArea {
    pub top: f32,
    pub leading: f32,
    pub trailing: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Other,
}

pub fn current_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Other
    }
}

/// The safe area for `platform`, given what the toolkit reported (`toolkit`). macOS takes the
/// toolkit's value (traffic-light group width and strip height); Windows keeps its caption
/// buttons' width at the right (the toolkit draws them, #340, #204, #205); Linux and mobile have
/// nothing to clear.
pub fn safe_area_for(platform: Platform, toolkit: SafeArea) -> SafeArea {
    match platform {
        Platform::MacOs => toolkit,
        Platform::Windows => SafeArea {
            top: WINDOWS_CAPTION_HEIGHT_DP,
            leading: 0.0,
            trailing: caption_reserved_width(true),
        },
        Platform::Other => SafeArea::default(),
    }
}

/// The toolkit's window safe area, read through the composition's own state so a screen
/// recomposes when the renderer reports it. Zero until the first report.
pub fn toolkit_safe_area() -> SafeArea {
    let a = compose_rust::runtime::current_window_size().safe_area;
    SafeArea { top: a.top, leading: a.leading, trailing: a.trailing }
}

/// The safe area of the window the caller is composed in.
pub fn safe_area() -> SafeArea {
    let platform = current_platform();
    let toolkit = if platform == Platform::MacOs { toolkit_safe_area() } else { SafeArea::default() };
    safe_area_for(platform, toolkit)
}

/// Height of the empty strip a content column adds on top so its first row clears the traffic
/// lights. Only a column that touches the window's left edge (the shell without a sidebar or
/// rail) needs it; with a sidebar the lights float over the sidebar and the content starts at
/// the top.
pub fn title_strip_height(area: SafeArea, at_leading_edge: bool) -> f32 {
    if at_leading_edge && area.leading > 0.0 { area.top } else { 0.0 }
}

/// Extra height a left-edge column (the sidebar) adds above its first row so it starts below the
/// traffic lights.
pub fn sidebar_top_inset(area: SafeArea) -> f32 {
    if area.leading > 0.0 { area.top } else { 0.0 }
}

/// What each layout reserves at the top for the traffic lights.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TopReservation {
    /// Added above the first sidebar or rail row.
    pub sidebar_inset: f32,
    /// A strip above the content column, when it touches the window's left edge.
    pub content_strip: f32,
}

/// The reservation for `mode`: with a sidebar or rail the lights float over it and the content
/// starts at the top; with bottom tabs the whole content column starts below the lights.
pub fn top_reservation(area: SafeArea, mode: ShellMode) -> TopReservation {
    let bottom_tabs = mode == ShellMode::BottomTabs;
    TopReservation {
        sidebar_inset: if bottom_tabs { 0.0 } else { sidebar_top_inset(area) },
        content_strip: title_strip_height(area, bottom_tabs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHTS: SafeArea = SafeArea { top: 28.0, leading: 72.0, trailing: 0.0 };

    #[test]
    fn fr_l33_windows_reserves_the_caption_buttons_at_the_trailing_edge() {
        let a = safe_area_for(Platform::Windows, LIGHTS);
        assert_eq!(a, SafeArea { top: 32.0, leading: 0.0, trailing: 138.0 });
    }

    #[test]
    fn fr_l33_macos_takes_the_toolkits_traffic_light_footprint() {
        assert_eq!(safe_area_for(Platform::MacOs, LIGHTS), LIGHTS);
        assert_eq!(safe_area_for(Platform::MacOs, SafeArea::default()), SafeArea::default());
    }

    #[test]
    fn fr_l33_linux_and_mobile_have_no_safe_area() {
        assert_eq!(safe_area_for(Platform::Other, LIGHTS), SafeArea::default());
    }

    #[test]
    fn fr_l33_a_left_edge_column_clears_the_strip_and_a_sidebar_column_does_not() {
        assert_eq!(title_strip_height(LIGHTS, true), 28.0);
        assert_eq!(title_strip_height(LIGHTS, false), 0.0);
        let win = safe_area_for(Platform::Windows, LIGHTS);
        assert_eq!(title_strip_height(win, true), 0.0);
    }

    #[test]
    fn fr_l33_only_the_macos_sidebar_gets_a_top_inset() {
        assert_eq!(sidebar_top_inset(safe_area_for(Platform::Windows, LIGHTS)), 0.0);
        assert_eq!(sidebar_top_inset(safe_area_for(Platform::Other, LIGHTS)), 0.0);
        assert_eq!(sidebar_top_inset(safe_area_for(Platform::MacOs, LIGHTS)), 28.0);
    }

    #[test]
    fn fr_l33_each_layout_reserves_the_top_where_the_lights_are() {
        let sidebar = top_reservation(LIGHTS, ShellMode::Sidebar);
        assert_eq!(sidebar, TopReservation { sidebar_inset: 28.0, content_strip: 0.0 });
        assert_eq!(top_reservation(LIGHTS, ShellMode::Rail), sidebar);
        let tabs = top_reservation(LIGHTS, ShellMode::BottomTabs);
        assert_eq!(tabs, TopReservation { sidebar_inset: 0.0, content_strip: 28.0 });
        let win = safe_area_for(Platform::Windows, LIGHTS);
        assert_eq!(top_reservation(win, ShellMode::BottomTabs), TopReservation::default());
        assert_eq!(top_reservation(win, ShellMode::Sidebar), TopReservation::default());
    }
}
