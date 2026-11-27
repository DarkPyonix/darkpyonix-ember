//! The bundled app fonts: Inter for Latin, then Pretendard for Hangul, then the system fonts;
//! JetBrains Mono for code and paths (the `Mono` role, `docs/DESIGN.md` "Type").
//!
//! All three are SIL OFL 1.1 and ship beside their license texts in `assets/fonts/`. They are
//! bundled so Windows and Linux render like the macOS mockup; Apple SD Gothic Neo cannot be
//! redistributed. Only the three weights `ui::theme::Weight` can name (400, 500, 600) are
//! included for the prose faces, because a face no text asks for is only download size. Code
//! uses 400, 500 (types, `editor.css` `.t-ty`) and 400 italic (comments, `.t-com`).
//!
//! The launcher registers only Inter and Pretendard at start (NFR-L1, #1082). JetBrains Mono
//! (824,600 bytes) is registered the first time a text asks for the `Mono` role
//! ([`text_role`]), and the running theme gains it in the same batch, because most launcher
//! screens draw no code. The editor window is a code view and registers it with its theme
//! ([`editor_theme`]).
//!
//! The Inter faces are subset to the code points Pretendard does not already draw (NFR-L1,
//! #1082): Greek, Cyrillic and the symbols both families carry come from Pretendard, whose
//! Latin is drawn from Inter. That keeps 1,236,500 of the release's 1,867,500 bytes. The
//! modification is recorded beside the license in `assets/fonts/Inter-SUBSET.txt`.

use std::cell::Cell;
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

thread_local! {
    /// The `Mono` stack's font id, once something on this thread has asked for it. Kept per
    /// thread because compose-rust's asset registry is: an id names a font only on the thread
    /// that registered it.
    static MONO: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Registers JetBrains Mono, with Pretendard behind it for Hangul in comments and strings,
/// once per thread, and returns the stack's font id for the `Mono` role.
///
/// Only a code view calls this directly ([`editor_theme`]); launcher text reaches it through
/// [`text_role`], so the launcher does not register the faces until a text needs them
/// (NFR-L1, #1082).
pub fn mono_font() -> u32 {
    if let Some(id) = MONO.get() {
        return id;
    }
    let _ = app_font();
    let id = FontStack::new().family(register(&JETBRAINS_MONO)).family(register(&PRETENDARD)).register();
    MONO.set(Some(id));
    id
}

/// Whether the `Mono` stack has been registered on this thread (NFR-L1, #1082).
pub fn mono_registered() -> bool {
    MONO.get().is_some()
}

/// The role a text is set in, with its font ready: the first `Mono` text registers JetBrains
/// Mono and gives the running theme its `Mono` font (NFR-L1, #1082).
///
/// Every launcher text in the `Mono` role names its role through this. The registration and the
/// new theme ride the batch of the render that asked, and the Renderer applies a whole batch
/// before it draws any of it, so the first mono text is drawn in JetBrains Mono. The check
/// reads the running theme, not [`mono_registered`], so a theme set after the first call
/// without `Mono` gets it back on the next mono text.
///
/// TODO(compose-rust#1086): once a theme role can name a lazily registered font, set it in
/// `app_theme` and remove this helper and its call-site test.
pub fn text_role(role: TypeRole) -> TypeRole {
    if role == TypeRole::Mono && compose_rust::theme::current_theme().font(TypeRole::Mono).is_none() {
        let id = mono_font();
        compose_rust::theme::set_theme(compose_rust::theme::current_theme().with_font(TypeRole::Mono, id));
    }
    role
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
/// its own `--bg` in CSS, gives that role `surface`. The current line is set on the editor
/// itself ([`CODE_EDITOR_COLORS`], FR-L31.28), not here: Cupertino draws it as `Primary` at 8%.
const EDITOR_PALETTE: Palette = APP_PALETTE.with(ColorRole::Background, LIGHT.surface, DARK.surface);

/// The code type of the editor window (FR-L31.27): `editor.css:2` `--code-fs:13px` on a
/// `--code-lh:22px` pitch, in sp for the code editor's `font_size` and `line_height`.
pub const CODE_FONT_SIZE: f32 = 13.0;
pub const CODE_LINE_HEIGHT: f32 = 22.0;
/// The line number column (FR-L31.27): `editor.css:71` `.ed-no` is 56px wide with the number
/// right-aligned `--sp3` (12px) from its right edge. The toolkit draws a number right-aligned
/// in its column, so the column's least width and its side padding carry both values.
pub const CODE_GUTTER_MIN_WIDTH: f32 = 56.0;
pub const CODE_GUTTER_PADDING: f32 = 12.0;
/// The room between the gutter and the text (FR-L31.27). The mockup's text starts 72px in: the
/// 56px number column, then the 4px change marker and its 12px margin (`editor.css:73`), kept
/// on every line. The editor keeps a fold column, as Code-OSS does, which the mockup does not
/// draw; the toolkit lays it out one row (22dp) wide, with its 16dp change column, after the
/// number column and before this inset. With [`CODE_FIXED_GUTTER_COLUMNS`] both columns are
/// kept on every file, so the text starts at the same 94dp (56 + 16 + 22 + 0) in every file:
/// past the mockup's 72px by the fold column (#477).
pub const CODE_TEXT_INSET: f32 = 0.0;
/// Whether the gutter keeps its fold and change columns on a file with no folds and no changes
/// (FR-L31.27, #477): dioxus-compose's `fixed_gutter_columns`, so the text start does not move
/// between files.
pub const CODE_FIXED_GUTTER_COLUMNS: bool = true;
/// The room above the first line and below the last, in dp (FR-L31.27): `editor.css:68`
/// `.ed-code{padding:var(--sp2) 0}`, 8px (`style.css:19` `--sp2:8px`) inside what scrolls, so it
/// scrolls away with the text as the mockup's does. The code editor's `content_padding_dp`.
pub const CODE_CONTENT_PADDING: (f32, f32) = (8.0, 8.0);
/// The OpenType features the code text is set with (FR-L31.27, #477): `liga 0` and `calt 0`, so
/// `->`, `=>` and `!=` are two glyphs a column each, as the mockup draws them. JetBrains Mono
/// joins them by default; the mockup's `.ed-code` (`editor.css:68`) names no feature and its
/// pages load no JetBrains Mono face, so they draw in the fallback face, which has no
/// ligatures. dioxus-compose's `font_feature_settings` on `CodeEditor` (compose-rust#1080).
pub const CODE_FONT_FEATURES: [compose_rust::FontFeature; 2] =
    [compose_rust::FontFeature::NO_LIGATURES, compose_rust::FontFeature::new(b"calt", 0)];

/// The colours of the code editor's own parts (FR-L31.27): line numbers in `muted`
/// (`editor.css:71`), the caret line's number in `text` (`editor.css:72`). A part is a role of
/// the palette, and the palette has no role that is `muted` by name, so the number takes
/// `SyntaxComment`, which this palette sets to `muted` (`.t-com`, `editor.css:77`).
pub const CODE_EDITOR_COLORS: compose_rust::code::CodeEditorColors = compose_rust::code::CodeEditorColors::new()
    .line_number(ColorRole::SyntaxComment)
    .current_line_number(ColorRole::OnSurface)
    // FR-L31.28: the caret line's band is `surface-2` (`editor.css:70` `.ed-ln.cur`), which the
    // palette gives `SurfaceVariant`. A part named here is drawn opaque, not as a wash.
    .current_line(ColorRole::SurfaceVariant)
    // compose-rust#816 (v0.0.11): extend the caret line's `surface-2` band across the line
    // number and fold columns (`editor.css:70,72`).
    .current_line_gutter(ColorRole::SurfaceVariant);

/// The Ember palette's value for a role in one scheme (tests and the capture checks read it).
pub fn palette_color(role: ColorRole, scheme: ColorScheme) -> Option<compose_rust::Color> {
    APP_PALETTE.get(role, scheme)
}

/// The toast's elevation in dp: the mockup's `.toast` casts `--shadow-lg` (`0 10px 32px`).
pub const TOAST_ELEVATION: u16 = 12;

/// The overlay tokens the app sets: the toast's shadow, pill corner (`.toast`
/// `border-radius:var(--pill)`), inverted ink fill and text (`--ink` / `--ink-text`, #873),
/// borderless pill, padding, and 13px text (compose-rust#446, compose-rust#781/v0.0.11).
pub const APP_OVERLAY: OverlayTokens = OverlayTokens::new()
    .toast_elevation(TOAST_ELEVATION)
    .toast_corner_radius(999)
    .toast_container(LIGHT.ink, DARK.ink)
    .toast_content(LIGHT.ink_text, DARK.ink_text)
    .toast_border_width(0)
    .toast_padding(16, 10)
    .toast_type(13, 18);

/// The toolkit theme the app launches with: the prose roles in Inter and Pretendard. `Mono`
/// is JetBrains Mono once a text has asked for it ([`text_role`]); until then the launcher
/// has not registered it and the theme leaves the role unset (NFR-L1, #1082).
pub fn app_theme(scheme: ColorScheme) -> Theme {
    let theme = Theme::unified(DesignSystem::Cupertino)
        .with_palette(&APP_PALETTE)
        .with_overlay_tokens(&APP_OVERLAY)
        .with_color_scheme(scheme)
        .with_default_font(app_font());
    match MONO.get() {
        Some(id) => theme.with_font(TypeRole::Mono, id),
        None => theme,
    }
}

/// The theme of the separate editor window: [`app_theme`] with the editor palette and `Mono`
/// in JetBrains Mono from the start, because the window is a code view.
pub fn editor_theme(scheme: ColorScheme) -> Theme {
    app_theme(scheme).with_palette(&EDITOR_PALETTE).with_font(TypeRole::Mono, mono_font())
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
        let rest = OverlayTokens {
            toast_elevation: 0,
            toast_corner_radius: 0,
            toast_container_light: 0,
            toast_container_dark: 0,
            toast_content_light: 0,
            toast_content_dark: 0,
            toast_border_width: None,
            toast_padding_horizontal: 0,
            toast_padding_vertical: 0,
            toast_font_size: 0,
            toast_line_height: 0,
            ..*overlay
        };
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

    /// The value of the custom property `name` in the mockup's `.ed` rule, in px.
    fn ed_px(name: &str) -> f32 {
        let css = include_str!("../../assets/editor/editor.css");
        let at = css.find(&format!("{name}:")).unwrap_or_else(|| panic!("{name} in editor.css")) + name.len() + 1;
        let end = at + css[at..].find("px").expect("a px value");
        css[at..end].parse().expect("a number")
    }

    /// FR-L31.27 (design review E1, #895): the code type, the row pitch and the line number
    /// column are the mockup's (`editor.css:2`, `:68`, `:71`), and both code editors take them.
    #[test]
    fn fr_l31_27_code_metrics_and_line_numbers_are_the_mockups() {
        assert_eq!(CODE_FONT_SIZE, ed_px("--code-fs"));
        assert_eq!(CODE_LINE_HEIGHT, ed_px("--code-lh"));
        let css = include_str!("../../assets/editor/editor.css");
        assert!(css.contains(".ed-no{width:56px;flex:none;text-align:right;padding-right:var(--sp3);color:var(--muted)"), "the mockup's number column");
        assert_eq!(CODE_GUTTER_MIN_WIDTH, 56.0);
        // `--sp3` is 12px (`style.css:19`).
        assert!(include_str!("../../assets/editor/style.css").contains("--sp3:12px"));
        assert_eq!(CODE_GUTTER_PADDING, 12.0);
        assert!(css.contains(".ed-code{flex:1;overflow:auto;font:var(--code-fs)/var(--code-lh) var(--mono);padding:var(--sp2) 0"), "the mockup's top and bottom padding");
        assert!(include_str!("../../assets/editor/style.css").contains("--sp2:8px"));
        assert_eq!(CODE_CONTENT_PADDING, (8.0, 8.0));
        // Line numbers `muted`, the caret line's number `text`, in both schemes.
        assert_eq!(CODE_EDITOR_COLORS.line_number, Some(ColorRole::SyntaxComment));
        assert_eq!(CODE_EDITOR_COLORS.current_line_number, Some(ColorRole::OnSurface));
        for scheme in [ColorScheme::Light, ColorScheme::Dark] {
            let palette = editor_theme(scheme).palette.expect("editor palette");
            let (muted, text) = if scheme == ColorScheme::Light { (LIGHT.muted, LIGHT.text) } else { (DARK.muted, DARK.text) };
            assert_eq!(palette.get(ColorRole::SyntaxComment, scheme), Some(muted));
            assert_eq!(palette.get(ColorRole::OnSurface, scheme), Some(text));
        }
        // Both code editors (the file's and the diff's working-tree side) are given the values.
        for (file, source) in [("editor_workbench.rs", include_str!("editor_workbench.rs")), ("editor_scm.rs", include_str!("editor_scm.rs"))] {
            for prop in [
                "font_size: Some(crate::ui::fonts::CODE_FONT_SIZE)",
                "line_height: Some(crate::ui::fonts::CODE_LINE_HEIGHT)",
                "gutter_min_width: Some(crate::ui::fonts::CODE_GUTTER_MIN_WIDTH)",
                "gutter_padding: Some(crate::ui::fonts::CODE_GUTTER_PADDING)",
                "text_inset: Some(crate::ui::fonts::CODE_TEXT_INSET)",
                "colors: crate::ui::fonts::CODE_EDITOR_COLORS",
            ] {
                assert!(source.contains(prop), "{file} passes `{prop}` to its CodeEditor");
            }
        }
        // The padding lies inside the file's editor, which scrolls it away with the text, not in
        // a band the page leaves above the editor's box.
        let workbench = include_str!("editor_workbench.rs");
        assert!(
            workbench.contains("content_padding_dp: Some(crate::ui::fonts::CODE_CONTENT_PADDING)"),
            "the file's CodeEditor takes the mockup's padding"
        );
        assert!(!workbench.contains("margin-top:{crate::ui::fonts::"), "no band above the editor's box");
    }

    /// FR-L31.27 (#477): the editor keeps the fold column, as Code-OSS does, and the gutter keeps
    /// its fold and change columns on every file, so the text starts at the same column in each.
    #[test]
    fn fr_l31_27_the_text_starts_at_a_fixed_column() {
        assert!(std::hint::black_box(CODE_FIXED_GUTTER_COLUMNS), "the gutter keeps its fold and change columns");
        assert_eq!(CODE_TEXT_INSET, 0.0);
        let workbench = include_str!("editor_workbench.rs");
        assert!(
            workbench.contains("fixed_gutter_columns: crate::ui::fonts::CODE_FIXED_GUTTER_COLUMNS"),
            "the file's CodeEditor keeps its gutter columns on every file"
        );
        // Folding stays: the editor is still given the document's folds.
        assert!(workbench.contains("folds: crate::ui::editor::folds_from("), "the file's CodeEditor still folds");
    }

    /// FR-L31.27 (#477): the code text is set without ligatures, `liga 0` and `calt 0`, so `->`
    /// is two glyphs as the mockup draws it, and the code editor node carries both records
    /// (compose-rust#1080).
    #[test]
    fn fr_l31_27_the_code_editor_draws_no_ligatures() {
        use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
        use compose_rust::schema::{PropertyKind, WidgetKind};
        let named: Vec<([u8; 4], u32)> = CODE_FONT_FEATURES.iter().map(|f| (f.tag, f.value)).collect();
        assert_eq!(named, [(*b"liga", 0), (*b"calt", 0)]);
        // The mockup's code area names no feature of its own.
        let css = include_str!("../../assets/editor/editor.css");
        assert!(!css.contains("font-feature-settings") && !css.contains("font-variant-ligatures"));

        fn unligated() -> dioxus_compose::prelude::Element {
            use dioxus_compose::prelude::*;
            rsx! { CodeEditor { text: "a -> b => c != d", font_feature_settings: CODE_FONT_FEATURES } }
        }
        let mut host = dioxus_compose::Host::new(unligated);
        let batch = decode_batch(host.rebuild().unwrap()).unwrap();
        let editor = batch
            .iter()
            .find_map(|mutation| match mutation {
                Mutation::Create { node_id, widget: WidgetKind::CodeEditor } => Some(*node_id),
                _ => None,
            })
            .expect("a code editor node");
        let features = batch
            .iter()
            .find_map(|mutation| match mutation {
                Mutation::SetProp { node_id, property: PropertyKind::FontFeatureSettings, value: PropertyValue::Bytes(bytes) }
                    if *node_id == editor =>
                {
                    Some(bytes.to_vec())
                }
                _ => None,
            })
            .expect("the code editor carries font feature settings");
        assert_eq!(features, b"liga\0\0\0\0calt\0\0\0\0".to_vec());

        // Both code editors (the file's and the diff's working-tree side) are given them.
        for (file, source) in [("editor_workbench.rs", include_str!("editor_workbench.rs")), ("editor_scm.rs", include_str!("editor_scm.rs"))] {
            assert!(
                source.contains("font_feature_settings: crate::ui::fonts::CODE_FONT_FEATURES"),
                "{file} passes the code font features to its CodeEditor"
            );
        }
    }

    /// FR-L31.28 (design review E2, #895): the caret line's band is the mockup's `surface-2`
    /// (`editor.css:70`; `style.css:3` `#f7f6f2`, `:28` `#252523`), not the toolkit's accent wash,
    /// and extends across the gutter (`current_line_gutter`, compose-rust#816).
    #[test]
    fn fr_l31_28_the_current_line_is_surface_2() {
        assert!(include_str!("../../assets/editor/editor.css").contains(".ed-ln.cur{background:var(--surface-2)}"));
        let role = CODE_EDITOR_COLORS.current_line.expect("the editor names its current line colour");
        let gutter_role = CODE_EDITOR_COLORS
            .current_line_gutter
            .expect("the editor extends the current line band across the gutter");
        assert_eq!(gutter_role, role);
        for (scheme, rgb) in [(ColorScheme::Light, 0xf7f6f2), (ColorScheme::Dark, 0x252523)] {
            let colour = editor_theme(scheme).palette.expect("editor palette").get(role, scheme).expect("the role has a value");
            assert_eq!(colour.to_argb() & 0x00ff_ffff, rgb, "{scheme:?}");
        }
    }

    /// Runs `f` on a thread of its own, so the thread-local `Mono` registration starts unset.
    fn on_fresh_thread(f: impl FnOnce() + Send + 'static) {
        std::thread::spawn(f).join().expect("the test thread");
    }

    /// Design review 2026-11-13 C6: `Mono` is the bundled JetBrains Mono stack, not a system face,
    /// in the editor window from the start and in the launcher once a mono text has asked.
    #[test]
    fn design_c6_mono_role_is_the_bundled_jetbrains_mono() {
        on_fresh_thread(|| {
            let editor = editor_theme(ColorScheme::Dark);
            let mono = editor.font(TypeRole::Mono).expect("Mono has a font");
            assert_ne!(mono, 0);
            assert_eq!(mono, mono_font());
            assert_ne!(Some(mono), editor.font(TypeRole::Body));
            // Registered now, so a launcher theme built afterwards carries it too.
            assert_eq!(app_theme(ColorScheme::Light).font(TypeRole::Mono), Some(mono));
        });
    }

    /// NFR-L1 (#1082): the launcher starts without JetBrains Mono; its theme leaves `Mono`
    /// unset and nothing has registered the faces.
    #[test]
    fn nfr_l1_startup_theme_leaves_mono_unregistered() {
        on_fresh_thread(|| {
            let theme = app_theme(ColorScheme::Light);
            assert!(!mono_registered(), "building the launcher theme registers no mono face");
            assert_eq!(theme.font(TypeRole::Mono), None);
            assert_eq!(theme.font(TypeRole::Body), Some(app_font()));
            assert_eq!(theme.font(TypeRole::Title), Some(app_font()));
            // Prose roles never ask for mono.
            for role in [TypeRole::Body, TypeRole::Title, TypeRole::Label, TypeRole::Caption] {
                assert_eq!(text_role(role), role);
            }
            assert!(!mono_registered());
        });
    }

    /// NFR-L1 (#1082): the first `Mono` text registers the stack and gives the running theme its
    /// `Mono` font; the prose font is untouched and a second call changes nothing.
    #[test]
    fn nfr_l1_mono_registers_on_first_need() {
        on_fresh_thread(|| {
            compose_rust::theme::set_theme(app_theme(ColorScheme::Light));
            assert_eq!(compose_rust::theme::current_theme().font(TypeRole::Mono), None);
            assert_eq!(text_role(TypeRole::Mono), TypeRole::Mono);
            assert!(mono_registered());
            let running = compose_rust::theme::current_theme();
            assert_eq!(running.font(TypeRole::Mono), Some(mono_font()));
            assert_eq!(running.font(TypeRole::Body), Some(app_font()));
            assert_eq!(running.palette.and_then(|p| p.get(ColorRole::Surface, ColorScheme::Light)), Some(LIGHT.surface));
            text_role(TypeRole::Mono);
            assert_eq!(compose_rust::theme::current_theme(), running);
        });
    }

    /// NFR-L1 (#1082): every launcher text in the `Mono` role names it through `text_role`, so
    /// none is drawn before the stack is registered.
    #[test]
    fn nfr_l1_launcher_mono_text_goes_through_text_role() {
        let sources = [
            ("components.rs", include_str!("cr/components.rs")),
            ("vcs.rs", include_str!("cr/vcs.rs")),
        ];
        for (file, source) in sources {
            let production = source.split("#[cfg(test)]").next().unwrap();
            assert!(!production.contains("type_role = TypeRole::Mono"), "{file} names Mono without text_role");
            assert!(!production.contains("Some(TypeRole::Mono) } else"), "{file} names Mono without text_role");
        }
        let components = include_str!("cr/components.rs").split("#[cfg(test)]").next().unwrap();
        assert!(components.contains("type_role = o.type_role.map(crate::ui::fonts::text_role)"), "fields resolve their role");
        assert_eq!(components.matches("Some(crate::ui::fonts::text_role(TypeRole::Mono))").count(), 2, "Txt and the name measure");
        assert_eq!(components.matches("type_role = crate::ui::fonts::text_role(TypeRole::Mono)").count(), 2, "MonoLabel and MonoLine");
    }

    /// #873: the toast is the mockup's inverted ink pill (dark on light theme, light on dark),
    /// borderless, 13px text.
    #[test]
    fn issue_873_toast_uses_inverted_ink_pill() {
        assert_eq!(APP_OVERLAY.toast_container_light, LIGHT.ink.to_argb());
        assert_eq!(APP_OVERLAY.toast_container_dark, DARK.ink.to_argb());
        assert_eq!(APP_OVERLAY.toast_content_light, LIGHT.ink_text.to_argb());
        assert_eq!(APP_OVERLAY.toast_content_dark, DARK.ink_text.to_argb());
        assert_eq!(APP_OVERLAY.toast_border_width, Some(0));
        assert_eq!(APP_OVERLAY.toast_padding_horizontal, 16);
        assert_eq!(APP_OVERLAY.toast_padding_vertical, 10);
        assert_eq!(APP_OVERLAY.toast_font_size, 13);
        assert_eq!(APP_OVERLAY.toast_line_height, 18);
    }
}
