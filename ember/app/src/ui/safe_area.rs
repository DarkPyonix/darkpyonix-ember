//! Title-bar safe area (FR-L33, #213): how far the top strip of the window is covered by the
//! operating system's window controls, so a screen can draw its own header in that strip instead
//! of leaving an empty band above it.
//!
//! On macOS the window opens with `Chrome::FullSize` (compose-rust v0.0.6, #345): content starts
//! at the top and the traffic lights float over it. The toolkit reports their footprint as
//! `WindowSafeArea { top, leading, trailing }`, and [`safe_area`] returns it. On Windows and
//! Linux the window keeps the system's own caption (`Chrome::System`, see [`chrome_for`]), so the
//! real caption buttons sit outside the content and there is nothing to clear. Everything but
//! [`safe_area`] is pure.

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
/// toolkit's default. The UI is the same on every system except this strip.
///
/// - macOS: `FullSize`, content under the system's own traffic lights.
/// - Windows and Linux: `System`, the system's own caption with its real buttons (Snap Layouts
///   on Windows). In compose-rust v0.0.10 any other value makes the renderer draw the caption
///   buttons in the scene in the theme's design system (`ComponentRules.caption`), and Ember's
///   theme is Cupertino, so they came out as fake traffic lights at the left.
/// - Mobile: nothing.
pub fn chrome_for(platform: Platform) -> Option<compose_rust::schema::Chrome> {
    use compose_rust::schema::Chrome;
    match platform {
        Platform::MacOs => Some(Chrome::FullSize),
        Platform::Windows | Platform::Linux => Some(Chrome::System),
        Platform::Other => None,
    }
}

/// Whether the scene itself draws the caption buttons on `platform`, so content must keep their
/// strip clear. True only for a chrome other than `System` off macOS.
pub fn scene_draws_caption(platform: Platform) -> bool {
    use compose_rust::schema::Chrome;
    matches!(platform, Platform::Windows | Platform::Linux)
        && matches!(chrome_for(platform), Some(Chrome::Modern) | Some(Chrome::FullSize))
}

/// The safe area for `platform`, given what the toolkit reported (`toolkit`). macOS takes the
/// toolkit's value (traffic-light group width and strip height) as it is: since compose-rust
/// v0.0.7 (#468) the renderer measures it again after the window is shown, so a compact
/// FullSize window gets its first report and Ember keeps no measured fallback. On Windows the
/// caption buttons sit at the right: under `System` chrome they are outside the content and
/// nothing is reserved; if the scene drew them it would keep their width at the trailing edge
/// and never a leading inset. Linux and mobile have nothing to clear.
pub fn safe_area_for(platform: Platform, toolkit: SafeArea) -> SafeArea {
    match platform {
        Platform::MacOs => toolkit,
        Platform::Windows if scene_draws_caption(platform) => SafeArea {
            top: WINDOWS_CAPTION_HEIGHT_DP,
            leading: 0.0,
            trailing: caption_reserved_width(true),
        },
        Platform::Windows | Platform::Linux | Platform::Other => SafeArea::default(),
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
    let a = toolkit_safe_area();
    safe_area_from_toolkit(a.top, a.leading, a.trailing)
}

/// The safe area on this platform for a toolkit report. The editor page (dioxus) reads the report
/// through `dioxus_compose::use_window_safe_area`, which renders the page again when it changes
/// (FR-L33.1, #875); a compose-rust screen reads it through [`safe_area`].
pub fn safe_area_from_toolkit(top: f32, leading: f32, trailing: f32) -> SafeArea {
    let platform = current_platform();
    let toolkit = if platform == Platform::MacOs { SafeArea { top, leading, trailing } } else { SafeArea::default() };
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
    fn fr_l33_windows_and_linux_keep_the_systems_own_caption() {
        use compose_rust::schema::Chrome;
        assert_eq!(chrome_for(Platform::MacOs), Some(Chrome::FullSize));
        assert_eq!(chrome_for(Platform::Windows), Some(Chrome::System));
        assert_eq!(chrome_for(Platform::Linux), Some(Chrome::System));
        assert_eq!(chrome_for(Platform::Other), None);
        assert!(!scene_draws_caption(Platform::Windows));
        assert!(!scene_draws_caption(Platform::Linux));
        assert!(!scene_draws_caption(Platform::MacOs));
    }

    #[test]
    fn fr_l33_windows_reserves_nothing_and_never_a_leading_inset() {
        // The real caption sits above the content under `System`; even a report carrying a
        // leading footprint (lights) gives no leading inset on Windows.
        let a = safe_area_for(Platform::Windows, LIGHTS);
        assert_eq!(a, SafeArea::default());
        let caption = SafeArea { top: 32.0, leading: 0.0, trailing: 138.0 };
        let b = safe_area_for(Platform::Windows, caption);
        assert_eq!(b.leading, 0.0);
        for mode in [ShellMode::Sidebar, ShellMode::Rail, ShellMode::BottomTabs] {
            assert_eq!(top_reservation(b, mode), TopReservation::default(), "{mode:?}");
        }
        assert_eq!(title_strip_height(b, true), 0.0);
        // A trailing-only report (a scene-drawn Windows caption) never reserves a leading strip.
        assert_eq!(top_reservation(caption, ShellMode::BottomTabs), TopReservation::default());
        assert_eq!(sidebar_top_inset(caption), 0.0);
        assert_eq!(crate::ui::editor::editor_strip_padding(false, b.leading), "");
    }

    #[test]
    fn fr_l33_macos_takes_the_toolkits_traffic_light_footprint() {
        assert_eq!(safe_area_for(Platform::MacOs, LIGHTS), LIGHTS);
    }

    #[test]
    fn fr_l33_macos_keeps_no_fallback_for_a_zero_report() {
        // compose-rust#468 (v0.0.7): the renderer reports the footprint after the window is
        // shown, so a zero report is passed through until that report arrives.
        assert_eq!(safe_area_for(Platform::MacOs, SafeArea::default()), SafeArea::default());
    }

    #[test]
    fn fr_l33_linux_and_mobile_have_no_safe_area() {
        assert_eq!(safe_area_for(Platform::Other, LIGHTS), SafeArea::default());
        assert_eq!(safe_area_for(Platform::Linux, LIGHTS), SafeArea::default());
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

    #[test]
    fn fr_l33_2_full_screen_reserves_nothing_in_any_layout() {
        // In macOS full screen the lights are hidden and the toolkit reports zeros (#409).
        let full_screen = safe_area_for(Platform::MacOs, SafeArea::default());
        let windowed = safe_area_for(Platform::MacOs, LIGHTS);
        let table = [
            (ShellMode::Sidebar, TopReservation { sidebar_inset: 28.0, content_strip: 0.0 }),
            (ShellMode::Rail, TopReservation { sidebar_inset: 28.0, content_strip: 0.0 }),
            (ShellMode::BottomTabs, TopReservation { sidebar_inset: 0.0, content_strip: 28.0 }),
        ];
        for (mode, with_lights) in table {
            assert_eq!(top_reservation(windowed, mode), with_lights, "{mode:?} beside the lights");
            assert_eq!(top_reservation(full_screen, mode), TopReservation::default(), "{mode:?} in full screen");
        }
        assert_eq!(sidebar_top_inset(full_screen), 0.0);
        assert_eq!(title_strip_height(full_screen, true), 0.0);
        // A strip height with no buttons beside it (a report with only `top`) reserves nothing
        // either: there is nothing at the left edge to clear.
        let top_only = SafeArea { top: 28.0, leading: 0.0, trailing: 0.0 };
        assert_eq!(top_reservation(top_only, ShellMode::Sidebar), TopReservation::default());
        assert_eq!(top_reservation(top_only, ShellMode::BottomTabs), TopReservation::default());
    }
}
