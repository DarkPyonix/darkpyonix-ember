//! The bundled app fonts: Inter for Latin, then Pretendard for Hangul, then the system fonts;
//! JetBrains Mono for code and paths (the `Mono` role, `docs/DESIGN.md` "Type").
//!
//! All three are SIL OFL 1.1 and ship beside their license texts in `assets/fonts/`. They are
//! bundled so Windows and Linux render like the macOS mockup; Apple SD Gothic Neo cannot be
//! redistributed. Only the three weights `ui::theme::Weight` can name (400, 500, 600) are
//! included for the prose faces, because a face no text asks for is only download size. Code
//! uses 400, 500 (types, `editor.css` `.t-ty`) and 400 italic (comments, `.t-com`).

use std::sync::OnceLock;

use compose_rust::schema::OverlayTokens;
use compose_rust::{asset, AssetKind, ColorRole, ColorScheme, DesignSystem, FontStack, Palette, Theme, TypeRole};

/// One bundled face: its file name under `assets/fonts/` and its bytes.
pub struct Face {
    pub file: &'static str,
    pub bytes: &'static [u8],
}

macro_rules! face {
    ($file:literal) => {
        Face { file: $file, bytes: include_bytes!(concat!("../../assets/fonts/", $file)) }
    };
}

/// Inter 4.1 static faces (rsms/inter release v4.1, `extras/otf`).
pub static INTER: [Face; 3] =
    [face!("Inter-Regular.otf"), face!("Inter-Medium.otf"), face!("Inter-SemiBold.otf")];

/// Pretendard 1.3.9 static faces (orioncactus/pretendard release v1.3.9, `public/static`).
pub static PRETENDARD: [Face; 3] = [
    face!("Pretendard-Regular.otf"),
    face!("Pretendard-Medium.otf"),
    face!("Pretendard-SemiBold.otf"),
];

/// JetBrains Mono 2.304 static faces (JetBrains/JetBrainsMono release v2.304, `fonts/ttf`).
pub static JETBRAINS_MONO: [Face; 3] = [
    face!("JetBrainsMono-Regular.ttf"),
    face!("JetBrainsMono-Medium.ttf"),
    face!("JetBrainsMono-Italic.ttf"),
];

/// The license file that sits beside each family's faces.
pub const LICENSES: [&str; 3] = ["Inter-OFL.txt", "Pretendard-OFL.txt", "JetBrainsMono-OFL.txt"];

fn register(faces: &[Face]) -> Vec<u32> {
    faces.iter().map(|f| asset(AssetKind::Font, f.bytes)).collect()
}

/// Registers the faces and the stack once and returns the stack's font id.
///
/// Registrations ride the batch and are re-sent to a new Host, so calling this while the
/// theme is built is early enough. The id is cached because the theme is built once per launch
/// but the call costs nothing to repeat.
pub fn app_font() -> u32 {
    static STACK: OnceLock<u32> = OnceLock::new();
    *STACK.get_or_init(|| {
        FontStack::new().family(register(&INTER)).family(register(&PRETENDARD)).register()
    })
}

/// Registers JetBrains Mono, with Pretendard behind it for Hangul in comments and strings,
/// once, and returns the stack's font id for the `Mono` role.
pub fn mono_font() -> u32 {
    static STACK: OnceLock<u32> = OnceLock::new();
    *STACK.get_or_init(|| {
        let _ = app_font();
        FontStack::new().family(register(&JETBRAINS_MONO)).family(register(&PRETENDARD)).register()
    })
}

// Overlay surfaces and ink follow the same tokens as Ember's own components.
use crate::ui::cr::theme::{DARK, LIGHT};

/// The code colours of `editor.css:77` (`.t-kw` c-orange, `.t-str` c-green, `.t-com` muted,
/// `.t-fn` c-blue, `.t-ty` text-2) and the diff colours of `editor.css:74` and `vcs.css`
/// (`.ed-gm.add` c-green, `.ed-gm.mod` c-blue, `.vcs-ln.add` c-green-soft, `.vcs-ln.del`
/// c-red-soft). The mockup has five code classes; a role it has no class for takes the class
/// nearest in meaning (macros and tags read as keywords, escapes as strings, attributes,
/// numbers and constants as functions) or the plain text colour (variables, properties,
/// operators, punctuation). The word-level diff backgrounds (`Diff*Emphasis`) have no mockup
/// value and stay the toolkit's. [provisional, design review 2026-11-13 A1/A9/C2]
const APP_PALETTE: Palette = Palette::new()
    .with(ColorRole::Background, LIGHT.bg, DARK.bg)
    .with(ColorRole::Surface, LIGHT.surface, DARK.surface)
    .with(ColorRole::SurfaceContainer, LIGHT.surface, DARK.surface)
    .with(ColorRole::SurfaceVariant, LIGHT.surface_2, DARK.surface_2)
    .with(ColorRole::OnSurface, LIGHT.text, DARK.text)
    .with(ColorRole::OnSurfaceVariant, LIGHT.text_2, DARK.text_2)
    .with(ColorRole::OutlineVariant, LIGHT.line, DARK.line)
    .with(ColorRole::SyntaxKeyword, LIGHT.orange.strong, DARK.orange.strong)
    .with(ColorRole::SyntaxMacro, LIGHT.orange.strong, DARK.orange.strong)
    .with(ColorRole::SyntaxTag, LIGHT.orange.strong, DARK.orange.strong)
    .with(ColorRole::SyntaxString, LIGHT.green.strong, DARK.green.strong)
    .with(ColorRole::SyntaxEscape, LIGHT.green.strong, DARK.green.strong)
    .with(ColorRole::SyntaxComment, LIGHT.muted, DARK.muted)
    .with(ColorRole::SyntaxFunction, LIGHT.blue.strong, DARK.blue.strong)
    .with(ColorRole::SyntaxAttribute, LIGHT.blue.strong, DARK.blue.strong)
    .with(ColorRole::SyntaxNumber, LIGHT.blue.strong, DARK.blue.strong)
    .with(ColorRole::SyntaxConstant, LIGHT.blue.strong, DARK.blue.strong)
    .with(ColorRole::SyntaxType, LIGHT.text_2, DARK.text_2)
    .with(ColorRole::SyntaxVariable, LIGHT.text, DARK.text)
    .with(ColorRole::SyntaxProperty, LIGHT.text, DARK.text)
    .with(ColorRole::SyntaxOperator, LIGHT.text, DARK.text)
    .with(ColorRole::SyntaxPunctuation, LIGHT.text, DARK.text)
    .with(ColorRole::DiffAdded, LIGHT.green.strong, DARK.green.strong)
    .with(ColorRole::DiffModified, LIGHT.blue.strong, DARK.blue.strong)
    .with(ColorRole::DiffRemoved, LIGHT.red.strong, DARK.red.strong)
    .with(ColorRole::DiffAddedContainer, LIGHT.green.soft, DARK.green.soft)
    .with(ColorRole::DiffRemovedContainer, LIGHT.red.soft, DARK.red.soft);

/// The editor window's palette: the app palette with the code panel as `surface`
/// (`editor.css:52` `.ed-main` and `:71` `.ed-no`). Cupertino's code editor fills its panel
/// and gutter with `Background`, so the separate editor process (FR-L31), whose page paints
/// its own `--bg` in CSS, gives that role `surface`. The current line stays the toolkit's
/// (Cupertino draws it as `Primary` at 8%; there is no role for it, the mockup's is
/// `surface-2`, `editor.css:70`), a toolkit gap reported with the design review.
const EDITOR_PALETTE: Palette = APP_PALETTE.with(ColorRole::Background, LIGHT.surface, DARK.surface);

/// The Ember palette's value for a role in one scheme (tests and the capture checks read it).
pub fn palette_color(role: ColorRole, scheme: ColorScheme) -> Option<compose_rust::Color> {
    APP_PALETTE.get(role, scheme)
}

/// The toast's elevation in dp: the mockup's `.toast` casts `--shadow-lg` (`0 10px 32px`).
pub const TOAST_ELEVATION: u16 = 12;

/// The overlay tokens the app sets: only the toast's shadow and its pill corner (`.toast`
/// `border-radius:var(--pill)`). A message is not a node and takes no Modifier, so its shadow
/// is an overlay token (compose-rust#446, v0.0.9); every other token is 0, the design system's.
pub const APP_OVERLAY: OverlayTokens =
    OverlayTokens::new().toast_elevation(TOAST_ELEVATION).toast_corner_radius(999);

/// The toolkit theme the app launches with: the prose roles in Inter and Pretendard, `Mono`
/// in JetBrains Mono.
pub fn app_theme(scheme: ColorScheme) -> Theme {
    Theme::unified(DesignSystem::Cupertino)
        .with_palette(&APP_PALETTE)
        .with_overlay_tokens(&APP_OVERLAY)
        .with_color_scheme(scheme)
        .with_default_font(app_font())
        .with_font(TypeRole::Mono, mono_font())
}

/// The theme of the separate editor window: [`app_theme`] with the editor palette.
pub fn editor_theme(scheme: ColorScheme) -> Theme {
    app_theme(scheme).with_palette(&EDITOR_PALETTE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr_l3_app_theme_is_unified() {
        let theme = app_theme(ColorScheme::Light);
        assert!(!theme.adaptive);
        assert_eq!(theme.design_system, DesignSystem::Cupertino);
        assert_eq!(theme.color_scheme, ColorScheme::Light);
        let palette = theme.palette.expect("app palette");
        assert_eq!(palette.get(ColorRole::Surface, ColorScheme::Light), Some(LIGHT.surface));
        assert_eq!(palette.get(ColorRole::Surface, ColorScheme::Dark), Some(DARK.surface));
        assert_eq!(palette.get(ColorRole::OnSurface, ColorScheme::Light), Some(LIGHT.text));
        assert_eq!(palette.get(ColorRole::OnSurface, ColorScheme::Dark), Some(DARK.text));
    }

    #[test]
    fn fr_l9_app_theme_gives_the_toast_its_shadow_and_pill_corner() {
        // compose-rust#446: the toast's shadow and corner are overlay tokens; the rest stay 0.
        let overlay = app_theme(ColorScheme::Dark).overlay.expect("overlay tokens");
        assert_eq!(overlay.toast_elevation, TOAST_ELEVATION);
        assert_eq!(overlay.toast_corner_radius, 999);
        let rest = OverlayTokens { toast_elevation: 0, toast_corner_radius: 0, ..*overlay };
        assert_eq!(rest, OverlayTokens::new());
        assert!(editor_theme(ColorScheme::Light).overlay.is_some());
    }

    fn hex(role: ColorRole, scheme: ColorScheme) -> u32 {
        let c = palette_color(role, scheme).unwrap_or_else(|| panic!("{role:?} has no {scheme:?} value"));
        c.to_argb() & 0x00ff_ffff
    }

    /// Design review 2026-11-13 A1, C2: the code colours are the mockup's tokens
    /// (`editor.css:77`, `style.css:3-7` light and `:28-31` dark).
    #[test]
    fn design_a1_syntax_roles_are_the_mockup_tokens() {
        use ColorScheme::{Dark, Light};
        let rows = [
            (ColorRole::SyntaxKeyword, 0xe49b55, 0xe6a76a),
            (ColorRole::SyntaxString, 0x4f9a6c, 0x6fb287),
            (ColorRole::SyntaxComment, 0x8f8b83, 0x8b877f),
            (ColorRole::SyntaxFunction, 0x3f74d4, 0x6b95e0),
            (ColorRole::SyntaxType, 0x56534d, 0xc3bfb7),
            (ColorRole::SyntaxVariable, 0x1c1b19, 0xf0eee9),
            (ColorRole::SyntaxPunctuation, 0x1c1b19, 0xf0eee9),
        ];
        for (role, light, dark) in rows {
            assert_eq!(hex(role, Light), light, "{role:?} light");
            assert_eq!(hex(role, Dark), dark, "{role:?} dark");
        }
    }

    /// Design review 2026-11-13 A9: change markers `c-green` added and `c-blue` modified
    /// (`editor.css:74`), removed `c-red`, line backgrounds the soft accents (`vcs.css`).
    #[test]
    fn design_a9_diff_roles_are_the_mockup_tokens() {
        use ColorScheme::{Dark, Light};
        let rows = [
            (ColorRole::DiffAdded, 0x4f9a6c, 0x6fb287),
            (ColorRole::DiffModified, 0x3f74d4, 0x6b95e0),
            (ColorRole::DiffRemoved, 0xd65a4a, 0xe07264),
            (ColorRole::DiffAddedContainer, 0xe3f1e8, 0x1d3125),
            (ColorRole::DiffRemovedContainer, 0xfbe6e2, 0x3b221e),
        ];
        for (role, light, dark) in rows {
            assert_eq!(hex(role, Light), light, "{role:?} light");
            assert_eq!(hex(role, Dark), dark, "{role:?} dark");
        }
    }

    /// Design review 2026-11-13 A1: the editor window's code panel is `surface`
    /// (`#ffffff` / `#1f1f1d`), while the main window keeps `bg` for `Background`.
    #[test]
    fn design_a1_editor_code_panel_is_surface() {
        for scheme in [ColorScheme::Light, ColorScheme::Dark] {
            let editor = editor_theme(scheme).palette.expect("editor palette");
            let app = app_theme(scheme).palette.expect("app palette");
            let (surface, bg) = if scheme == ColorScheme::Light { (LIGHT.surface, LIGHT.bg) } else { (DARK.surface, DARK.bg) };
            assert_eq!(editor.get(ColorRole::Background, scheme), Some(surface));
            assert_eq!(app.get(ColorRole::Background, scheme), Some(bg));
            assert_eq!(editor.get(ColorRole::SyntaxKeyword, scheme), app.get(ColorRole::SyntaxKeyword, scheme));
        }
    }

    /// Design review 2026-11-13 C6: `Mono` is the bundled JetBrains Mono stack, not a system face.
    #[test]
    fn design_c6_mono_role_is_the_bundled_jetbrains_mono() {
        for theme in [app_theme(ColorScheme::Light), editor_theme(ColorScheme::Dark)] {
            let mono = theme.font(TypeRole::Mono).expect("Mono has a font");
            assert_ne!(mono, 0);
            assert_eq!(mono, mono_font());
            assert_ne!(Some(mono), theme.font(TypeRole::Body));
        }
    }
}
