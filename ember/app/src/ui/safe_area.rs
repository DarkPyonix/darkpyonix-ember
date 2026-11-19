//! Title-bar safe area (FR-L33, #213): how far the top strip of the window is covered by the
//! window controls, so a screen can draw its own header in that strip instead of leaving an
//! empty band above it.
//!
//! The toolkit reports the controls' footprint as `WindowSafeArea { top, leading, trailing }`
//! on every system (compose-rust v0.0.11, #847), and Ember reserves exactly that, never assuming
//! a side: macOS reports the traffic lights at the leading edge, Windows its 46x32 caption group
//! at the trailing edge, Linux the desktop's own set (or nothing where the window manager draws
//! the title bar). Everything but [`safe_area`] is pure.

use crate::ui::shell::ShellMode;

/// The window controls' footprint along the top edge, in dp. `top` is the height of the strip
/// they occupy, `leading` the width at the left and `trailing` the width at the right.
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
    Linux,
    /// Mobile and anything else: the toolkit's default window, no chrome asked for.
    Other,
}

pub fn current_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(all(target_os = "linux", not(target_os = "android"))) {
        Platform::Linux
    } else {
        Platform::Other
    }
}

/// The window chrome every Ember window asks for on `platform` (FR-L33), `None` for the
/// toolkit's default. The UI is the same on every system except this strip: macOS takes
/// `FullSize`, content under the system's traffic lights; everywhere else the default
/// (`Modern`), where the renderer draws the platform's own caption buttons whatever the design
/// system (compose-rust v0.0.11, #847).
pub fn chrome_for(platform: Platform) -> Option<compose_rust::schema::Chrome> {
    match platform {
        Platform::MacOs => Some(compose_rust::schema::Chrome::FullSize),
        Platform::Windows | Platform::Linux | Platform::Other => None,
    }
}

/// The safe area for a toolkit report: the report itself, on every system. Kept as a function
/// of the platform so a caller states which system a fixture stands for.
pub fn safe_area_for(_platform: Platform, toolkit: SafeArea) -> SafeArea {
    toolkit
}

/// The toolkit's window safe area, read through the composition's own state so a screen
/// recomposes when the renderer reports it. Zero until the first report.
pub fn toolkit_safe_area() -> SafeArea {
    let a = compose_rust::runtime::current_window_size().safe_area;
    SafeArea { top: a.top, leading: a.leading, trailing: a.trailing }
}

/// The safe area of the window the caller is composed in.
pub fn safe_area() -> SafeArea {
    let a = toolkit_safe_area();
    safe_area_from_toolkit(a.top, a.leading, a.trailing)
}

/// The safe area for a toolkit report. The editor page (dioxus) reads the report through
/// `dioxus_compose::use_window_safe_area`, which renders the page again when it changes
/// (FR-L33.1, #875); a compose-rust screen reads it through [`safe_area`].
pub fn safe_area_from_toolkit(top: f32, leading: f32, trailing: f32) -> SafeArea {
    safe_area_for(current_platform(), SafeArea { top, leading, trailing })
}

/// Height of the empty strip a content column adds on top so its first row clears the window
/// controls. A column that touches the window's left edge (the shell without a sidebar or rail)
/// clears a leading set; every content column touches the right edge, so it clears a trailing
/// set. With a sidebar a leading set floats over the sidebar instead.
pub fn title_strip_height(area: SafeArea, at_leading_edge: bool) -> f32 {
    if (at_leading_edge && area.leading > 0.0) || area.trailing > 0.0 { area.top } else { 0.0 }
}

/// Extra height a left-edge column (the sidebar) adds above its first row so it starts below a
/// leading set of controls.
pub fn sidebar_top_inset(area: SafeArea) -> f32 {
    if area.leading > 0.0 { area.top } else { 0.0 }
}

/// What each layout reserves at the top for the window controls.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TopReservation {
    /// Added above the first sidebar or rail row.
    pub sidebar_inset: f32,
    /// A strip above the content column.
    pub content_strip: f32,
    /// The width kept clear at the right end of that strip.
    pub trailing: f32,
}

/// The reservation for `mode`: with a sidebar or rail a leading set floats over it; with bottom
/// tabs the whole content column starts below it. A trailing set always takes a strip above the
/// content column, with its width kept clear at the right.
pub fn top_reservation(area: SafeArea, mode: ShellMode) -> TopReservation {
    let bottom_tabs = mode == ShellMode::BottomTabs;
    let content_strip = title_strip_height(area, bottom_tabs);
    TopReservation {
        sidebar_inset: if bottom_tabs { 0.0 } else { sidebar_top_inset(area) },
        content_strip,
        trailing: if content_strip > 0.0 { area.trailing } else { 0.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::editor::editor_strip_padding;

    const LIGHTS: SafeArea = SafeArea { top: 28.0, leading: 72.0, trailing: 0.0 };
    /// One report per system, as compose-rust v0.0.11 (#847) sends it.
    const MACOS: SafeArea = SafeArea { top: 28.0, leading: 70.0, trailing: 0.0 };
    const WINDOWS: SafeArea = SafeArea { top: 32.0, leading: 0.0, trailing: 138.0 };
    const LINUX_KN: SafeArea = SafeArea { top: 0.0, leading: 0.0, trailing: 0.0 };
    const LINUX_X11: SafeArea = SafeArea { top: 32.0, leading: 0.0, trailing: 96.0 };
    const MODES: [ShellMode; 3] = [ShellMode::Sidebar, ShellMode::Rail, ShellMode::BottomTabs];

    #[test]
    fn fr_l33_each_system_gets_its_chrome_from_one_helper() {
        use compose_rust::schema::Chrome;
        assert_eq!(chrome_for(Platform::MacOs), Some(Chrome::FullSize));
        assert_eq!(chrome_for(Platform::Windows), None, "toolkit default: Modern");
        assert_eq!(chrome_for(Platform::Linux), None, "toolkit default: Modern");
        assert_eq!(chrome_for(Platform::Other), None);
    }

    #[test]
    fn fr_l33_every_system_takes_the_report_as_it_is() {
        for (p, r) in [
            (Platform::MacOs, MACOS),
            (Platform::Windows, WINDOWS),
            (Platform::Linux, LINUX_KN),
            (Platform::Linux, LINUX_X11),
            (Platform::Other, LIGHTS),
        ] {
            assert_eq!(safe_area_for(p, r), r, "{p:?}");
        }
    }

    #[test]
    fn fr_l33_macos_report_reserves_the_leading_lights_only() {
        let a = safe_area_for(Platform::MacOs, MACOS);
        assert_eq!(top_reservation(a, ShellMode::Sidebar), TopReservation { sidebar_inset: 28.0, content_strip: 0.0, trailing: 0.0 });
        assert_eq!(top_reservation(a, ShellMode::Rail), TopReservation { sidebar_inset: 28.0, content_strip: 0.0, trailing: 0.0 });
        assert_eq!(top_reservation(a, ShellMode::BottomTabs), TopReservation { sidebar_inset: 0.0, content_strip: 28.0, trailing: 0.0 });
        assert_eq!(editor_strip_padding(a.leading, a.trailing), "padding-left:70px;");
    }

    #[test]
    fn fr_l33_windows_report_reserves_the_trailing_caption_and_no_leading_inset() {
        let a = safe_area_for(Platform::Windows, WINDOWS);
        for mode in MODES {
            assert_eq!(top_reservation(a, mode), TopReservation { sidebar_inset: 0.0, content_strip: 32.0, trailing: 138.0 }, "{mode:?}");
        }
        assert_eq!(sidebar_top_inset(a), 0.0);
        assert_eq!(editor_strip_padding(a.leading, a.trailing), "padding-right:138px;");
    }

    #[test]
    fn fr_l33_linux_reports_reserve_what_they_say() {
        let kn = safe_area_for(Platform::Linux, LINUX_KN);
        for mode in MODES {
            assert_eq!(top_reservation(kn, mode), TopReservation::default(), "{mode:?}");
        }
        assert_eq!(editor_strip_padding(kn.leading, kn.trailing), "");
        let x11 = safe_area_for(Platform::Linux, LINUX_X11);
        for mode in MODES {
            assert_eq!(top_reservation(x11, mode), TopReservation { sidebar_inset: 0.0, content_strip: 32.0, trailing: 96.0 }, "{mode:?}");
        }
        assert_eq!(editor_strip_padding(x11.leading, x11.trailing), "padding-right:96px;");
    }

    #[test]
    fn fr_l33_a_left_edge_column_clears_the_strip_and_a_sidebar_column_does_not() {
        assert_eq!(title_strip_height(LIGHTS, true), 28.0);
        assert_eq!(title_strip_height(LIGHTS, false), 0.0);
        assert_eq!(title_strip_height(WINDOWS, false), 32.0);
    }

    #[test]
    fn fr_l33_2_full_screen_reserves_nothing_in_any_layout() {
        // In macOS full screen the lights are hidden and the toolkit reports zeros (#409).
        let full_screen = SafeArea::default();
        for mode in MODES {
            assert_eq!(top_reservation(full_screen, mode), TopReservation::default(), "{mode:?} in full screen");
        }
        assert_eq!(sidebar_top_inset(full_screen), 0.0);
        assert_eq!(title_strip_height(full_screen, true), 0.0);
        // A strip height with no buttons beside it reserves nothing either.
        let top_only = SafeArea { top: 28.0, leading: 0.0, trailing: 0.0 };
        for mode in MODES {
            assert_eq!(top_reservation(top_only, mode), TopReservation::default(), "{mode:?}");
        }
    }
}
