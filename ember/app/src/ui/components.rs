//! Shared component tokens (`docs/DESIGN.md` section 4) and `Label` for the separate editor window (`FR-L31`).
//!
//! Each reads the tokens in [`super::theme`]. The style decisions (which colour, which height)
//! are plain functions so they are unit-tested without a renderer.

use dioxus_compose::prelude::*;
use dioxus_compose::{FontRef, FontRefs, GenericFamily};

use super::theme::*;
use super::theme::Palette;

// ---------------------------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------------------------

/// The font stack for prose (`--font`): Inter, then Apple SD Gothic Neo and Noto Sans KR for
/// Hangul, then the system UI face.
pub fn font_refs(mono: bool) -> FontRefs {
    if mono {
        FontRefs::new([
            FontRef::System("JetBrains Mono".into()),
            FontRef::System("SF Mono".into()),
            FontRef::System("Menlo".into()),
            FontRef::Generic(GenericFamily::Monospace),
        ])
    } else {
        FontRefs::new([
            FontRef::System("Inter".into()),
            FontRef::System("Apple SD Gothic Neo".into()),
            FontRef::System("Noto Sans KR".into()),
            FontRef::Generic(GenericFamily::SystemUi),
        ])
    }
}

/// The app palette colour standing in for a toolkit colour role, so a screen that still names
/// a role draws the mockup's tokens in both schemes.
pub fn role_color(role: ColorRole, p: &Palette) -> Color {
    match role {
        ColorRole::Primary => p.blue.strong,
        ColorRole::OnPrimary => p.ink_text,
        ColorRole::PrimaryContainer => p.blue.soft,
        ColorRole::OnPrimaryContainer => p.blue.strong,
        ColorRole::Secondary => p.green.strong,
        ColorRole::SecondaryContainer => p.green.soft,
        ColorRole::OnSecondaryContainer => p.green.strong,
        ColorRole::Tertiary => p.orange.strong,
        ColorRole::TertiaryContainer => p.orange.soft,
        ColorRole::OnTertiaryContainer => p.orange.strong,
        ColorRole::Error => p.red.strong,
        ColorRole::OnError => p.ink_text,
        ColorRole::SurfaceVariant | ColorRole::SurfaceContainer => p.surface_2,
        ColorRole::OnSurfaceVariant => p.text_2,
        ColorRole::Outline | ColorRole::OutlineVariant => p.line,
        ColorRole::Background => p.bg,
        ColorRole::Surface => p.surface,
        _ => p.text,
    }
}

/// A paint resolved against the palette: roles become tokens, literals pass through.
pub fn resolve_paint(paint: Paint, p: &Palette) -> Paint {
    match paint {
        Paint::Role(role) => Paint::Literal(role_color(role, p)),
        other => other,
    }
}

/// The type step, weight and face a toolkit type role stands for (400/500/600, never bold).
pub fn type_role_style(role: TypeRole) -> (Fs, Weight, bool) {
    match role {
        TypeRole::Headline => (Fs::Xl2, Weight::Semibold, false),
        TypeRole::Title => (Fs::Xl, Weight::Semibold, false),
        TypeRole::Subtitle => (Fs::Lg, Weight::Medium, false),
        TypeRole::BodyStrong => (Fs::Md, Weight::Medium, false),
        TypeRole::Caption => (Fs::Xs, Weight::Regular, false),
        TypeRole::Label => (Fs::Sm, Weight::Medium, false),
        TypeRole::Mono => (Fs::Sm, Weight::Regular, true),
        _ => (Fs::Md, Weight::Regular, false),
    }
}

// ---------------------------------------------------------------------------------------------
// Text (used by `ui/editor.rs`, FR-L31)
// ---------------------------------------------------------------------------------------------

/// Text on the type scale: one of the nine sizes and one of the three weights.
#[component]
pub fn Label(
    #[props(into)] text: String,
    #[props(default = Fs::Md)] fs: Fs,
    #[props(default = Weight::Regular)] weight: Weight,
    #[props(default)] color: Option<Paint>,
    #[props(default)] max_lines: Option<u32>,
) -> Element {
    let theme = use_ember_theme();
    let color = color.unwrap_or_else(|| paint(theme.palette.text));
    rsx! {
        Text {
            text,
            font_size: fs.size(),
            line_height: fs.line_height(),
            font_weight: weight.value(),
            letter_spacing: fs.letter_spacing(),
            color,
            max_lines,
            font: font_refs(false),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pill
// ---------------------------------------------------------------------------------------------

/// The colour families a pill can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillTone {
    Blue,
    Orange,
    Red,
    Green,
    Sky,
    Grey,
}

/// `(text, fill)`: the strong colour as text on its soft pair.
pub fn pill_colors(tone: PillTone, p: &Palette) -> (Color, Color) {
    let a = match tone {
        PillTone::Blue => p.blue,
        PillTone::Orange => p.orange,
        PillTone::Red => p.red,
        PillTone::Green => p.green,
        PillTone::Sky => p.sky,
        PillTone::Grey => p.grey,
    };
    (a.strong, a.soft)
}

/// `(height, font step, weight)`: 24 px with 12/500, or 20 px with 11/500 when small.
pub fn pill_metrics(small: bool) -> (f32, Fs, Weight) {
    if small {
        (H_PILL_SM, Fs::Xs2, Weight::Medium)
    } else {
        (H_PILL, Fs::Xs, Weight::Medium)
    }
}

// ---------------------------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// 40 px pill, 1 px `line` border on `surface`.
    Default,
    /// `ink` fill.
    Primary,
    /// `c-red` text with a red-tinted border.
    Destructive,
}

/// What a button looks like, resolved from the tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonStyle {
    pub height: f32,
    pub fill: Color,
    pub text: Color,
    pub border: Color,
    pub fs: Fs,
}

pub fn button_style(kind: ButtonKind, small: bool, p: &Palette) -> ButtonStyle {
    let (height, fs) = if small {
        (H_CTL, Fs::Sm)
    } else {
        (H_BTN, Fs::Md)
    };
    match kind {
        ButtonKind::Default => ButtonStyle {
            height,
            fill: p.surface,
            text: p.text,
            border: p.line,
            fs,
        },
        ButtonKind::Primary => ButtonStyle {
            height,
            fill: p.ink,
            text: p.ink_text,
            border: p.ink,
            fs,
        },
        ButtonKind::Destructive => ButtonStyle {
            height,
            fill: p.surface,
            text: p.red.strong,
            border: p.red.soft,
            fs,
        },
    }
}

pub fn label_min_width(text: &str, font_px: f32, has_icon: bool) -> f32 {
    let advance: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 0.55 } else { 1.0 })
        .sum::<f32>()
        * font_px;
    let icon = if has_icon { 20.0 + 8.0 } else { 0.0 };
    advance + icon + 2.0 * 16.0
}

// ---------------------------------------------------------------------------------------------
// Pick chip & Card
// ---------------------------------------------------------------------------------------------

/// `(height, fill)`: 32 px, no border, `surface-2` fill; the selected chip takes `sel`.
pub fn pickchip_style(selected: bool, p: &Palette) -> (f32, Color) {
    (H_CTL, if selected { p.sel } else { p.surface_2 })
}

/// `(fill, border)`: `surface` on a `line` border; a selected card takes `sel` and a 1 px
/// `ink` border, never a 2 px one.
pub fn card_colors(selected: bool, p: &Palette) -> (Color, Color) {
    if selected {
        (p.sel, p.ink)
    } else {
        (p.surface, p.line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_roles_never_use_bold() {
        for role in [
            TypeRole::Headline,
            TypeRole::Title,
            TypeRole::Subtitle,
            TypeRole::BodyStrong,
            TypeRole::Caption,
            TypeRole::Label,
            TypeRole::Mono,
            TypeRole::Body,
        ] {
            let (_, w, _) = type_role_style(role);
            assert!(w.value() <= 600);
        }
        assert_eq!(type_role_style(TypeRole::Headline).0, Fs::Xl2);
        assert_eq!(type_role_style(TypeRole::Caption).0, Fs::Xs);
        assert!(type_role_style(TypeRole::Mono).2);
    }

    #[test]
    fn role_colours_follow_the_scheme() {
        assert_eq!(role_color(ColorRole::OnSurfaceVariant, &LIGHT), LIGHT.text_2);
        assert_eq!(role_color(ColorRole::OnSurfaceVariant, &DARK), DARK.text_2);
        assert_eq!(role_color(ColorRole::Error, &DARK), DARK.red.strong);
        assert_eq!(role_color(ColorRole::SurfaceContainer, &DARK), DARK.surface_2);
        assert_ne!(
            role_color(ColorRole::SurfaceContainer, &LIGHT),
            role_color(ColorRole::SurfaceContainer, &DARK)
        );
    }

    #[test]
    fn radii_and_heights_are_the_mockups() {
        assert_eq!((CORNER_LG, CORNER_XL, H_PILL), (14.0, 24.0, 24.0));
        assert_eq!((CORNER_BUBBLE, CORNER_MD), (18.0, 12.0));
    }

    #[test]
    fn font_stack_names_inter_then_korean() {
        let refs = font_refs(false).refs();
        assert_eq!(refs[0], FontRef::System("Inter".into()));
        assert_eq!(refs[1], FontRef::System("Apple SD Gothic Neo".into()));
        assert!(font_refs(true).refs().contains(&FontRef::Generic(GenericFamily::Monospace)));
    }

    #[test]
    fn pill_is_strong_text_on_soft_fill() {
        let (fg, bg) = pill_colors(PillTone::Green, &LIGHT);
        assert_eq!((fg, bg), (LIGHT.green.strong, LIGHT.green.soft));
        let (fg, bg) = pill_colors(PillTone::Red, &DARK);
        assert_eq!((fg, bg), (DARK.red.strong, DARK.red.soft));
    }

    #[test]
    fn pill_sizes() {
        assert_eq!(pill_metrics(false), (24.0, Fs::Xs, Weight::Medium));
        assert_eq!(pill_metrics(true), (20.0, Fs::Xs2, Weight::Medium));
    }

    #[test]
    fn label_min_width_grows_with_the_label_and_the_icon() {
        // Three Hangul glyphs at 14 px, no icon: 42 + 32 padding.
        assert_eq!(label_min_width("보내기", 14.0, false), 74.0);
        assert_eq!(label_min_width("보내기", 14.0, true), 102.0);
        assert!(label_min_width("명령마다 확인", 13.0, false) > label_min_width("확인", 13.0, false));
        // Latin letters are narrower than Hangul.
        assert!(label_min_width("abc", 14.0, false) < label_min_width("가나다", 14.0, false));
    }

    #[test]
    fn button_sizes_and_kinds() {
        let d = button_style(ButtonKind::Default, false, &LIGHT);
        assert_eq!((d.height, d.fs), (40.0, Fs::Md));
        assert_eq!((d.fill, d.border), (LIGHT.surface, LIGHT.line));
        let small = button_style(ButtonKind::Default, true, &LIGHT);
        assert_eq!((small.height, small.fs), (32.0, Fs::Sm));
        let p = button_style(ButtonKind::Primary, false, &DARK);
        assert_eq!((p.fill, p.text), (DARK.ink, DARK.ink_text));
        let x = button_style(ButtonKind::Destructive, false, &LIGHT);
        assert_eq!((x.text, x.border), (LIGHT.red.strong, LIGHT.red.soft));
    }

    #[test]
    fn pickchip_has_no_border_and_a_selected_fill() {
        assert_eq!(pickchip_style(false, &LIGHT), (32.0, LIGHT.surface_2));
        assert_eq!(pickchip_style(true, &DARK), (32.0, DARK.sel));
    }

    #[test]
    fn card_selected_uses_sel_and_ink_border() {
        assert_eq!(card_colors(false, &LIGHT), (LIGHT.surface, LIGHT.line));
        assert_eq!(card_colors(true, &LIGHT), (LIGHT.sel, LIGHT.ink));
    }

    #[test]
    fn sheet_header_geometry() {
        assert_eq!(SHEET_HEADER_H, 60.0);
        assert_eq!(SP[4], 20.0);
    }
}
