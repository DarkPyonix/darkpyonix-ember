//! Design tokens (NFR-L3, `docs/DESIGN.md` section 2) and the System / Light / Dark choice
//! (FR-L28).
//!
//! Every colour, size, space and radius the shell and the shared components draw comes from
//! here, so a screen never copies a literal. The values are the mockup's `style.css` tokens.
//!
//! compose-rust version of `ui::theme` (#220 phase 1). The Dioxus original stays until the last
//! screen has moved; the token values here must equal it (the tests below pin both).
//!
//! compose-rust can register an app palette per colour role (`compose_rust::Palette`), but
//! the Ember tokens (surface-2, hover, sel, the soft accents) have no role, so the tokens stay
//! plain constants passed as `Paint::Literal`.
//! TODO(compose-rust#374): bundled Inter + Pretendard need a font stack per role; the theme
//! only takes one asset per `TypeRole` (`Theme::with_font`), so the fonts of #216 land after.
//! Layered card and sheet shadow tokens follow the mockup (#214).

use compose_rust::{Color, Paint};
use compose_rust::ui::{Modifier, Shadow};

// `Scheme` and `ThemePref` are plain enums that `SharedUi` and `Prefs` already hold, so the
// Dioxus module's definitions are the single source until phase 8 moves them here.
pub use crate::ui::theme::{Scheme, ThemePref};

/// The scheme the OS reports.
///
/// TODO(compose-rust#97): the toolkit resolves `FollowSystem` inside the renderer and exposes no
/// hook to read the result, so System draws the light palette until one exists. An explicit
/// Light or Dark choice is exact.
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

    /// `--shadow`: cards and the composer.
    pub fn shadow_sm(&self) -> Vec<Shadow> {
        match self.scheme {
            Scheme::Light => SHADOW_SM_LIGHT.to_vec(),
            Scheme::Dark => SHADOW_SM_DARK.to_vec(),
        }
    }

    /// The floating tab bar's shadow.
    pub fn shadow_md(&self) -> Vec<Shadow> {
        match self.scheme {
            Scheme::Light => SHADOW_MD_LIGHT.to_vec(),
            Scheme::Dark => SHADOW_MD_DARK.to_vec(),
        }
    }

    /// `--shadow-lg`: composer, popovers and toasts.
    pub fn shadow_lg(&self) -> Vec<Shadow> {
        match self.scheme {
            Scheme::Light => SHADOW_LG_LIGHT.to_vec(),
            Scheme::Dark => SHADOW_LG_DARK.to_vec(),
        }
    }
}

/// WCAG relative luminance of an opaque colour (the alpha byte is ignored).
pub fn luminance(c: Color) -> f64 {
    let lin = |v: u32| {
        let s = v as f64 / 255.0;
        if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    let v = c.to_argb();
    0.2126 * lin((v >> 16) & 0xff) + 0.7152 * lin((v >> 8) & 0xff) + 0.0722 * lin(v & 0xff)
}

/// WCAG contrast ratio of two opaque colours, 1.0 to 21.0 (NFR-L3 contrast checks).
pub fn contrast_ratio(a: Color, b: Color) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
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

/// A page title (`style.css:66` `.head h1`, 32/400; 28 on a phone): book weight, as the mockup.
/// Design review 2026-11-13 B8: the titles were drawn Medium or Semibold and read heavier.
pub const PAGE_TITLE_WEIGHT: Weight = Weight::Regular;
/// The home greeting (`home.css:233` `.greet`, 32/500, `DESIGN.md` 6.1).
pub const GREETING_WEIGHT: Weight = Weight::Medium;

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
/// TODO(compose-rust#294): `Modifier::CornerCurve { smoothing }`; until then corners are plain
/// arcs of the same fixed radius, so only the curve differs and layout is the same.
pub const SQUIRCLE_SMOOTHING: f32 = 0.6;

/// Press: scale 0.975 in 180 ms, held at least 120 ms, back in 200 ms with no overshoot.
/// TODO(GAPS: 누름 피드백): the scale needs `PointerKinds::DOWN/UP`, which the pinned rev lacks
/// on Box, Row and Column. Buttons get the toolkit's own press feedback until then.
pub const PRESS_SCALE: f32 = 0.975;
pub const PRESS_IN_MS: u32 = 180;
pub const PRESS_HOLD_MS: u32 = 120;
pub const PRESS_OUT_MS: u32 = 200;

/// The shadow tokens of the mockup, two layers where it has two. Each is a list in CSS order
/// (the first layer is on top). The toolkit draws its first layer at the bottom.
///
/// `sm` is the mockup's `--shadow` (cards and sheets), `lg` is `--shadow-lg` (composer,
/// popovers and toasts) and `md` is the floating tab bar's shadow, which the mockup writes inline.
/// Colour is black or the warm ink at the alpha the mockup gives, rounded to 8 bits.
pub const SHADOW_SM_LIGHT: [Shadow; 2] = [
    Shadow::new(0.0, 1.0, 2.0, 0.0, Paint::Literal(Color::argb(0x08_1c1b19))),
    Shadow::new(0.0, 1.0, 4.0, 0.0, Paint::Literal(Color::argb(0x08_1c1b19))),
];
pub const SHADOW_SM_DARK: [Shadow; 2] = [
    Shadow::new(0.0, 1.0, 2.0, 0.0, Paint::Literal(Color::argb(0x4d_000000))),
    Shadow::new(0.0, 2.0, 8.0, 0.0, Paint::Literal(Color::argb(0x33_000000))),
];
pub const SHADOW_MD_LIGHT: [Shadow; 2] = [
    Shadow::new(
        0.0,
        10.0,
        30.0,
        0.0,
        Paint::Literal(Color::argb(0x24_1c1b19)),
    ),
    Shadow::new(0.0, 2.0, 6.0, 0.0, Paint::Literal(Color::argb(0x0f_1c1b19))),
];
pub const SHADOW_MD_DARK: [Shadow; 1] = [Shadow::new(
    0.0,
    10.0,
    30.0,
    0.0,
    Paint::Literal(Color::argb(0x80_000000)),
)];
pub const SHADOW_LG_LIGHT: [Shadow; 1] = [Shadow::new(
    0.0,
    10.0,
    32.0,
    0.0,
    Paint::Literal(Color::argb(0x1a_1c1b19)),
)];
pub const SHADOW_LG_DARK: [Shadow; 1] = [Shadow::new(
    0.0,
    10.0,
    32.0,
    0.0,
    Paint::Literal(Color::argb(0x73_000000)),
)];

/// Apply CSS-ordered layers in the toolkit's bottom-to-top drawing order.
pub fn with_shadows(modifier: Modifier, layers: Vec<Shadow>) -> Modifier {
    modifier.shadows(layers.into_iter().rev())
}

// ---------------------------------------------------------------------------------------------
// Theme state
// ---------------------------------------------------------------------------------------------

/// The theme to draw with now. Reading it inside a composable subscribes the caller to the
/// preference, because [`crate::ui::coexist::SharedUi::theme_pref`] is a `MutableState`.
pub fn ember_theme() -> EmberTheme {
    // Unit tests run without installed services; they draw the default (System) theme.
    let pref = crate::services::try_services()
        .map(|s| s.ui.theme_pref.get())
        .unwrap_or_default();
    EmberTheme::select(pref, system_scheme())
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

    #[test]
    fn fr_l28_ember_theme_follows_the_shared_preference() {
        // Without installed services `ember_theme` falls back to the default preference.
        assert_eq!(ember_theme().scheme, Scheme::Light);
        assert_eq!(EmberTheme::select(ThemePref::Dark, system_scheme()).scheme, Scheme::Dark);
        assert_eq!(EmberTheme::select(ThemePref::System, system_scheme()).scheme, Scheme::Light);
    }

    /// The tokens are the mockup's `--shadow` and `--shadow-lg`, light and dark.
    #[test]
    fn nfr_l3_shadow_tokens_match_the_mockup() {
        let light = EmberTheme::new(Scheme::Light);
        let dark = EmberTheme::new(Scheme::Dark);
        let rows = |layers: Vec<Shadow>| -> Vec<(f32, f32, f32, f32, u32)> {
            layers
                .iter()
                .map(|l| match l.paint {
                    Paint::Literal(c) => (l.x, l.y, l.blur, l.spread, c.to_argb()),
                    _ => panic!("shadows are literal colours"),
                })
                .collect()
        };
        assert_eq!(
            rows(light.shadow_sm()),
            [(0.0, 1.0, 2.0, 0.0, 0x08_1c1b19), (0.0, 1.0, 4.0, 0.0, 0x08_1c1b19)]
        );
        assert_eq!(
            rows(dark.shadow_sm()),
            [(0.0, 1.0, 2.0, 0.0, 0x4d_000000), (0.0, 2.0, 8.0, 0.0, 0x33_000000)]
        );
        assert_eq!(
            rows(light.shadow_md()),
            [(0.0, 10.0, 30.0, 0.0, 0x24_1c1b19), (0.0, 2.0, 6.0, 0.0, 0x0f_1c1b19)]
        );
        assert_eq!(rows(dark.shadow_md()), [(0.0, 10.0, 30.0, 0.0, 0x80_000000)]);
        assert_eq!(rows(light.shadow_lg()), [(0.0, 10.0, 32.0, 0.0, 0x1a_1c1b19)]);
        assert_eq!(rows(dark.shadow_lg()), [(0.0, 10.0, 32.0, 0.0, 0x73_000000)]);
        // Modifier::shadow stores layers in insertion order, with the first at the bottom.
        // CSS puts its first layer on top, so the second CSS layer must be inserted first.
        let css = light.shadow_sm();
        let expected = Modifier::new().shadow(css[1]).shadow(css[0]);
        assert_eq!(with_shadows(Modifier::new(), css.clone()), expected);
        assert_ne!(expected, Modifier::new().shadow(css[0]).shadow(css[1]));
    }

    // NFR-L3, #510: dark inputs, dividers and the disabled primary button follow the mockup
    // tokens. The contrast numbers are in the failure messages.

    #[test]
    fn nfr_l3_dark_field_tokens_are_dark() {
        // Fields draw `surface` on a `line` border, the chip frame `surface_2`; the composer
        // and searches are bare on a `surface` card. None may be the light grey of the toolkit.
        for (name, c) in [("surface", DARK.surface), ("surface_2", DARK.surface_2), ("line", DARK.line), ("hover", DARK.hover), ("bg", DARK.bg)] {
            assert!(luminance(c) < 0.03, "dark {name} is not dark: luminance {:.3}", luminance(c));
        }
        assert!(luminance(LIGHT.surface) > 0.8);
    }

    #[test]
    fn nfr_l3_placeholder_reads_on_its_field_in_both_themes() {
        // The mockup sets `.field::placeholder` in `--muted`.
        for (scheme, p) in [("light", &LIGHT), ("dark", &DARK)] {
            for (field, fill) in [("surface", p.surface), ("surface_2", p.surface_2)] {
                let r = contrast_ratio(p.muted, fill);
                assert!(r >= 3.0, "{scheme} muted on {field} is {r:.2}:1");
            }
        }
    }

    #[test]
    fn nfr_l3_disabled_primary_button_reads_in_both_themes() {
        use super::super::components::{button_style_for, ButtonKind};
        for (scheme, p) in [("light", &LIGHT), ("dark", &DARK)] {
            let s = button_style_for(ButtonKind::Primary, false, false, p);
            let r = contrast_ratio(s.text, s.fill);
            assert!(r >= 3.0, "{scheme} disabled primary text on fill is {r:.2}:1");
            // Still a filled button, not the surface it sits on.
            assert!(contrast_ratio(s.fill, p.surface) >= 1.5, "{scheme} disabled fill vanishes");
            // Enabled stays the ink pair; only disabled changed.
            assert_eq!(button_style_for(ButtonKind::Primary, false, true, p).fill, p.ink);
            assert_eq!(button_style_for(ButtonKind::Default, false, false, p).fill, p.surface);
        }
    }

    #[test]
    fn nfr_l3_ide_split_is_a_default_button_not_inverted() {
        use super::super::session::ide_split_style;
        for p in [&LIGHT, &DARK] {
            let s = ide_split_style(p);
            assert_eq!(s.fill, p.surface);
            assert_eq!(s.border, p.line);
            assert_eq!(s.divider, p.line);
            assert!(contrast_ratio(s.ink, s.fill) >= 4.5);
        }
        assert!(luminance(ide_split_style(&DARK).fill) < 0.03);
    }

    #[test]
    fn nfr_l3_contrast_ratio_matches_wcag_reference_values() {
        let white = Color::rgb(0xffffff);
        let black = Color::rgb(0x000000);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-9);
        // #767676 on white is the 4.54:1 reference.
        assert!((contrast_ratio(Color::rgb(0x767676), white) - 4.54).abs() < 0.02);
    }
}
