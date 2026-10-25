//! Design tokens (NFR-L3, `docs/DESIGN.md` section 2) and the System / Light / Dark choice
//! (FR-L28).
//!
//! Every colour, size, space and radius the shell and the shared components draw comes from
//! here, so a screen never copies a literal. The values are the mockup's `style.css` tokens.
//!
//! dioxus-compose at the pinned rev has no way to register an app palette as a theme
//! (GAPS-WORKLIST "앱 정의 디자인 토큰"), so the tokens are plain constants passed as
//! `Paint::Literal`, and the light/dark pair is picked here.
//! TODO(GAPS: 앱 정의 디자인 토큰): register the table as a theme once the toolkit can.

use dioxus_compose::prelude::*;
use serde::{Deserialize, Serialize};

/// The two palettes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Light,
    Dark,
}

/// The user's choice. Default System, which follows the OS (FR-L28).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePref {
    pub const ALL: [ThemePref; 3] = [ThemePref::System, ThemePref::Light, ThemePref::Dark];

    /// System, then Light, then Dark, then back (the order of the segmented choice).
    pub fn next(self) -> ThemePref {
        match self {
            ThemePref::System => ThemePref::Light,
            ThemePref::Light => ThemePref::Dark,
            ThemePref::Dark => ThemePref::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemePref::System => "시스템",
            ThemePref::Light => "라이트",
            ThemePref::Dark => "다크",
        }
    }

    /// The scheme to draw: an explicit choice wins, System takes what the OS reports.
    pub fn resolve(self, system: Scheme) -> Scheme {
        match self {
            ThemePref::System => system,
            ThemePref::Light => Scheme::Light,
            ThemePref::Dark => Scheme::Dark,
        }
    }

    /// What the toolkit's own widgets are told (the renderer resolves System itself).
    pub fn color_scheme(self) -> ColorScheme {
        match self {
            ThemePref::System => ColorScheme::FollowSystem,
            ThemePref::Light => ColorScheme::Light,
            ThemePref::Dark => ColorScheme::Dark,
        }
    }
}

/// The scheme the OS reports.
///
/// TODO(GAPS: 앱 정의 디자인 토큰): the pinned dioxus-compose resolves `FollowSystem` inside
/// the renderer and exposes no hook to read the result, so System draws the light palette
/// until one exists. An explicit Light or Dark choice is exact.
pub fn system_scheme() -> Scheme {
    Scheme::Light
}

/// One colour and its soft background pair (the data palette).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accent {
    pub strong: Color,
    pub soft: Color,
}

/// Every colour token of one scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Color,
    pub surface: Color,
    pub surface_2: Color,
    pub line: Color,
    pub text: Color,
    pub text_2: Color,
    pub muted: Color,
    pub hover: Color,
    pub sel: Color,
    pub ink: Color,
    pub ink_text: Color,
    pub track: Color,
    pub blue: Accent,
    pub orange: Accent,
    pub sky: Accent,
    pub grey: Accent,
    pub red: Accent,
    pub green: Accent,
}

const fn accent(strong: u32, soft: u32) -> Accent {
    Accent {
        strong: Color::rgb(strong),
        soft: Color::rgb(soft),
    }
}

pub const LIGHT: Palette = Palette {
    bg: Color::rgb(0xf3f2ee),
    surface: Color::rgb(0xffffff),
    surface_2: Color::rgb(0xf7f6f2),
    line: Color::rgb(0xebe9e4),
    text: Color::rgb(0x1c1b19),
    text_2: Color::rgb(0x56534d),
    muted: Color::rgb(0x8f8b83),
    hover: Color::rgb(0xf0eee9),
    sel: Color::rgb(0xecebe6),
    ink: Color::rgb(0x1c1b19),
    ink_text: Color::rgb(0xffffff),
    track: Color::rgb(0xecebe6),
    blue: accent(0x3f74d4, 0xe6eefb),
    orange: accent(0xe49b55, 0xfbeedf),
    sky: accent(0x9dbde9, 0xedf3fc),
    grey: accent(0xb4b2ac, 0xefeee9),
    red: accent(0xd65a4a, 0xfbe6e2),
    green: accent(0x4f9a6c, 0xe3f1e8),
};

pub const DARK: Palette = Palette {
    bg: Color::rgb(0x151514),
    surface: Color::rgb(0x1f1f1d),
    surface_2: Color::rgb(0x252523),
    line: Color::rgb(0x2f2e2b),
    text: Color::rgb(0xf0eee9),
    text_2: Color::rgb(0xc3bfb7),
    muted: Color::rgb(0x8b877f),
    hover: Color::rgb(0x2a2927),
    sel: Color::rgb(0x2f2e2b),
    ink: Color::rgb(0xf0eee9),
    ink_text: Color::rgb(0x151514),
    track: Color::rgb(0x2f2e2b),
    blue: accent(0x6b95e0, 0x1e2a3f),
    orange: accent(0xe6a76a, 0x3a2c1d),
    sky: accent(0x8fb1df, 0x1f2834),
    grey: accent(0x8d8b86, 0x2a2927),
    red: accent(0xe07264, 0x3b221e),
    green: accent(0x6fb287, 0x1d3125),
};

/// The resolved theme a component draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmberTheme {
    pub scheme: Scheme,
    pub palette: &'static Palette,
}

impl EmberTheme {
    pub fn new(scheme: Scheme) -> EmberTheme {
        let palette = match scheme {
            Scheme::Light => &LIGHT,
            Scheme::Dark => &DARK,
        };
        EmberTheme { scheme, palette }
    }

    /// The theme for a preference and the scheme the OS reports.
    pub fn select(pref: ThemePref, system: Scheme) -> EmberTheme {
        EmberTheme::new(pref.resolve(system))
    }

    /// The press tint: black at 7 % in light, 10 % in dark (DESIGN section 5).
    pub fn press_tint(&self) -> Color {
        match self.scheme {
            Scheme::Light => Color::argb(0x12_000000),
            Scheme::Dark => Color::argb(0x1a_000000),
        }
    }
}

pub fn paint(c: Color) -> Paint {
    Paint::Literal(c)
}

// ---------------------------------------------------------------------------------------------
// Type
// ---------------------------------------------------------------------------------------------

/// The nine steps of the type scale, and nothing in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fs {
    Xs2,
    Xs,
    Sm,
    Md,
    Lg,
    Xl,
    Xl2,
    Xl3,
    Xl4,
}

impl Fs {
    pub const ALL: [Fs; 9] = [
        Fs::Xs2,
        Fs::Xs,
        Fs::Sm,
        Fs::Md,
        Fs::Lg,
        Fs::Xl,
        Fs::Xl2,
        Fs::Xl3,
        Fs::Xl4,
    ];

    /// Font size in dp.
    pub const fn size(self) -> f32 {
        match self {
            Fs::Xs2 => 11.0,
            Fs::Xs => 12.0,
            Fs::Sm => 13.0,
            Fs::Md => 14.0,
            Fs::Lg => 15.0,
            Fs::Xl => 18.0,
            Fs::Xl2 => 28.0,
            Fs::Xl3 => 32.0,
            Fs::Xl4 => 40.0,
        }
    }

    pub const fn line_height(self) -> f32 {
        match self {
            Fs::Xs2 => 16.0,
            Fs::Xs => 18.0,
            Fs::Sm => 20.0,
            Fs::Md => 22.0,
            Fs::Lg => 22.0,
            Fs::Xl => 26.0,
            Fs::Xl2 => 34.0,
            Fs::Xl3 => 40.0,
            Fs::Xl4 => 44.0,
        }
    }

    /// Titles and large numbers (`fs-xl` and up) take -0.02em; everything else 0. In em, then
    /// converted to dp here, so small text never inherits a px value that squeezes it.
    pub fn letter_spacing(self) -> f32 {
        match self {
            Fs::Xl | Fs::Xl2 | Fs::Xl3 | Fs::Xl4 => -0.02 * self.size(),
            _ => 0.0,
        }
    }
}

/// The three weights. 700 is only for 11px badge numbers and 800 is not used, so neither is
/// representable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
}

impl Weight {
    pub const ALL: [Weight; 3] = [Weight::Regular, Weight::Medium, Weight::Semibold];

    pub const fn value(self) -> u16 {
        match self {
            Weight::Regular => 400,
            Weight::Medium => 500,
            Weight::Semibold => 600,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Spacing, sizes, radius, press
// ---------------------------------------------------------------------------------------------

/// `sp1`..`sp8`: 4, 8, 12, 16, 20, 24, 32. The mockup names seven values; the numbering
/// skips none here, so `SP[0]` is `sp1`.
pub const SP: [f32; 7] = [4.0, 8.0, 12.0, 16.0, 20.0, 24.0, 32.0];

pub const GAP_CARD: f32 = 16.0;
pub const GAP_BLOCK: f32 = 12.0;
/// Card header and body padding is 16 vertical, 20 horizontal.
pub const PAD_CARD_V: f32 = 16.0;
pub const PAD_CARD_H: f32 = 20.0;
pub const PAD_ROW_V: f32 = 12.0;
pub const PAD_ROW_H: f32 = 16.0;

pub const H_BTN: f32 = 40.0;
pub const H_CTL: f32 = 32.0;
pub const H_PILL: f32 = 24.0;
pub const H_PILL_SM: f32 = 20.0;
pub const H_NAV_ITEM: f32 = 38.0;
pub const SIDE_W: f32 = 234.0;
pub const RAIL_W: f32 = 72.0;
pub const TABBAR_H: f32 = 64.0;
pub const TABBAR_BOTTOM_GAP: f32 = 10.0;
pub const TABBAR_SIDE_GAP: f32 = 12.0;
pub const TABBAR_RADIUS: f32 = 26.0;
pub const SHEET_HEADER_H: f32 = 60.0;

pub const CORNER_2XS: f32 = 5.0;
pub const CORNER_XS: f32 = 6.0;
pub const CORNER_SM: f32 = 8.0;
pub const CORNER_MD: f32 = 12.0;
pub const CORNER_LG: f32 = 14.0;
pub const CORNER_XL: f32 = 24.0;
pub const CORNER_BUBBLE: f32 = 18.0;
pub const CORNER_TAIL: f32 = 6.0;
/// Pills are `ShapeRole::Full` in the toolkit; this is the token's value for tests.
pub const PILL: f32 = 999.0;

/// Squircle smoothing (Figma's corner smoothing). Unused until the toolkit can draw it.
/// TODO(GAPS: 스퀘어클): `Modifier::CornerCurve { smoothing }`; until then corners are plain
/// arcs of the same fixed radius, so only the curve differs and layout is the same.
pub const SQUIRCLE_SMOOTHING: f32 = 0.6;

/// Press: scale 0.975 in 180 ms, held at least 120 ms, back in 200 ms with no overshoot.
/// TODO(GAPS: 누름 피드백): the scale needs `PointerKinds::DOWN/UP`, which the pinned rev lacks
/// on Box, Row and Column. Buttons get the toolkit's own press feedback until then.
pub const PRESS_SCALE: f32 = 0.975;
pub const PRESS_IN_MS: u32 = 180;
pub const PRESS_HOLD_MS: u32 = 120;
pub const PRESS_OUT_MS: u32 = 200;

/// Elevation (dp) standing in for the card shadow, "0 1px 2px / 3 %" plus "0 1px 4px / 3 %".
/// TODO(GAPS: 그림자 2겹): the toolkit has a single `elevation`; the nearest value is used.
pub const ELEVATION_CARD: f32 = 1.0;
/// Popover and sheet shadow, "0 10px 32px / 10 %". TODO(GAPS: 그림자 2겹).
pub const ELEVATION_LARGE: f32 = 8.0;

// ---------------------------------------------------------------------------------------------
// Theme context
// ---------------------------------------------------------------------------------------------

/// The theme choice, provided as context by the root component. A signal, so it is `Copy`.
#[derive(Clone, Copy)]
pub struct ThemeCtx {
    pub pref: Signal<ThemePref>,
}

/// The theme to draw with now. Subscribes the caller to the preference.
pub fn use_ember_theme() -> EmberTheme {
    let ctx = use_context::<ThemeCtx>();
    EmberTheme::select(*ctx.pref.read(), system_scheme())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(c: Color) -> u32 {
        c.to_argb() & 0x00ff_ffff
    }

    #[test]
    fn colour_table_matches_design_md() {
        // (name, light, dark)
        let rows: [(&str, Color, Color, u32, u32); 12] = [
            ("bg", LIGHT.bg, DARK.bg, 0xf3f2ee, 0x151514),
            ("surface", LIGHT.surface, DARK.surface, 0xffffff, 0x1f1f1d),
            (
                "surface-2",
                LIGHT.surface_2,
                DARK.surface_2,
                0xf7f6f2,
                0x252523,
            ),
            ("line", LIGHT.line, DARK.line, 0xebe9e4, 0x2f2e2b),
            ("text", LIGHT.text, DARK.text, 0x1c1b19, 0xf0eee9),
            ("text-2", LIGHT.text_2, DARK.text_2, 0x56534d, 0xc3bfb7),
            ("muted", LIGHT.muted, DARK.muted, 0x8f8b83, 0x8b877f),
            ("hover", LIGHT.hover, DARK.hover, 0xf0eee9, 0x2a2927),
            ("sel", LIGHT.sel, DARK.sel, 0xecebe6, 0x2f2e2b),
            ("ink", LIGHT.ink, DARK.ink, 0x1c1b19, 0xf0eee9),
            (
                "ink-text",
                LIGHT.ink_text,
                DARK.ink_text,
                0xffffff,
                0x151514,
            ),
            ("track", LIGHT.track, DARK.track, 0xecebe6, 0x2f2e2b),
        ];
        for (name, l, d, el, ed) in rows {
            assert_eq!(hex(l), el, "light {name}");
            assert_eq!(hex(d), ed, "dark {name}");
            assert_eq!(l.to_argb() >> 24, 0xff, "light {name} is opaque");
        }
    }

    #[test]
    fn data_palette_matches_design_md() {
        // (name, light strong, dark strong, light soft, dark soft)
        let rows = [
            (
                "blue", LIGHT.blue, DARK.blue, 0x3f74d4, 0x6b95e0, 0xe6eefb, 0x1e2a3f,
            ),
            (
                "orange",
                LIGHT.orange,
                DARK.orange,
                0xe49b55,
                0xe6a76a,
                0xfbeedf,
                0x3a2c1d,
            ),
            (
                "sky", LIGHT.sky, DARK.sky, 0x9dbde9, 0x8fb1df, 0xedf3fc, 0x1f2834,
            ),
            (
                "grey", LIGHT.grey, DARK.grey, 0xb4b2ac, 0x8d8b86, 0xefeee9, 0x2a2927,
            ),
            (
                "red", LIGHT.red, DARK.red, 0xd65a4a, 0xe07264, 0xfbe6e2, 0x3b221e,
            ),
            (
                "green",
                LIGHT.green,
                DARK.green,
                0x4f9a6c,
                0x6fb287,
                0xe3f1e8,
                0x1d3125,
            ),
        ];
        for (name, l, d, ls, ds, lsoft, dsoft) in rows {
            assert_eq!(hex(l.strong), ls, "light {name}");
            assert_eq!(hex(d.strong), ds, "dark {name}");
            assert_eq!(hex(l.soft), lsoft, "light {name}-soft");
            assert_eq!(hex(d.soft), dsoft, "dark {name}-soft");
        }
    }

    #[test]
    fn type_scale_has_nine_steps_matching_design_md() {
        let table = [
            (Fs::Xs2, 11.0, 16.0),
            (Fs::Xs, 12.0, 18.0),
            (Fs::Sm, 13.0, 20.0),
            (Fs::Md, 14.0, 22.0),
            (Fs::Lg, 15.0, 22.0),
            (Fs::Xl, 18.0, 26.0),
            (Fs::Xl2, 28.0, 34.0),
            (Fs::Xl3, 32.0, 40.0),
            (Fs::Xl4, 40.0, 44.0),
        ];
        assert_eq!(Fs::ALL.len(), 9);
        for (step, size, line) in table {
            assert_eq!(step.size(), size);
            assert_eq!(step.line_height(), line);
        }
        let sizes: Vec<f32> = Fs::ALL.iter().map(|s| s.size()).collect();
        assert!(sizes.windows(2).all(|w| w[0] < w[1]), "steps increase");
    }

    #[test]
    fn only_three_weights() {
        let values: Vec<u16> = Weight::ALL.iter().map(|w| w.value()).collect();
        assert_eq!(values, vec![400, 500, 600]);
    }

    #[test]
    fn letter_spacing_is_em_based() {
        assert_eq!(Fs::Md.letter_spacing(), 0.0);
        assert_eq!(Fs::Xs2.letter_spacing(), 0.0);
        assert!((Fs::Xl3.letter_spacing() - (-0.64)).abs() < 1e-6);
    }

    #[test]
    fn spacing_sizes_and_radius_match_design_md() {
        assert_eq!(SP, [4.0, 8.0, 12.0, 16.0, 20.0, 24.0, 32.0]);
        assert_eq!((H_BTN, H_CTL, H_PILL, H_PILL_SM), (40.0, 32.0, 24.0, 20.0));
        assert_eq!((SIDE_W, RAIL_W, TABBAR_H), (234.0, 72.0, 64.0));
        assert_eq!((H_NAV_ITEM, SHEET_HEADER_H), (38.0, 60.0));
        assert_eq!(
            [
                CORNER_2XS,
                CORNER_XS,
                CORNER_SM,
                CORNER_MD,
                CORNER_LG,
                CORNER_XL,
                CORNER_BUBBLE,
                CORNER_TAIL
            ],
            [5.0, 6.0, 8.0, 12.0, 14.0, 24.0, 18.0, 6.0]
        );
        assert_eq!(SQUIRCLE_SMOOTHING, 0.6);
    }

    #[test]
    fn press_values_and_tint() {
        assert_eq!(
            (PRESS_SCALE, PRESS_IN_MS, PRESS_HOLD_MS, PRESS_OUT_MS),
            (0.975, 180, 120, 200)
        );
        // 7 % and 10 % of 255, rounded.
        assert_eq!(
            EmberTheme::new(Scheme::Light).press_tint().to_argb(),
            0x12_000000
        );
        assert_eq!(
            EmberTheme::new(Scheme::Dark).press_tint().to_argb(),
            0x1a_000000
        );
    }

    #[test]
    fn preference_selects_the_palette() {
        assert_eq!(ThemePref::default(), ThemePref::System);
        // System follows the OS in both directions.
        assert_eq!(
            EmberTheme::select(ThemePref::System, Scheme::Light).scheme,
            Scheme::Light
        );
        assert_eq!(
            EmberTheme::select(ThemePref::System, Scheme::Dark).scheme,
            Scheme::Dark
        );
        // An explicit choice ignores the OS.
        assert_eq!(
            EmberTheme::select(ThemePref::Light, Scheme::Dark).scheme,
            Scheme::Light
        );
        assert_eq!(
            EmberTheme::select(ThemePref::Dark, Scheme::Light).scheme,
            Scheme::Dark
        );
        assert_eq!(
            EmberTheme::select(ThemePref::Dark, Scheme::Light)
                .palette
                .bg,
            DARK.bg
        );
    }

    #[test]
    fn preference_cycles_system_light_dark() {
        assert_eq!(ThemePref::System.next(), ThemePref::Light);
        assert_eq!(ThemePref::Light.next(), ThemePref::Dark);
        assert_eq!(ThemePref::Dark.next(), ThemePref::System);
        let labels: Vec<_> = ThemePref::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(labels, ["시스템", "라이트", "다크"]);
    }

    #[test]
    fn preference_serializes_as_lowercase() {
        assert_eq!(serde_json::to_string(&ThemePref::Dark).unwrap(), "\"dark\"");
        assert_eq!(
            serde_json::from_str::<ThemePref>("\"system\"").unwrap(),
            ThemePref::System
        );
    }
}
