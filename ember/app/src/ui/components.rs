//! Shared components (`docs/DESIGN.md` section 4): pick chip, pill, card, sheet header, buttons.
//!
//! Each reads the tokens in [`super::theme`]. The style decisions (which colour, which height)
//! are plain functions so they are unit-tested without a renderer; the components only hand
//! them to dioxus-compose.
//!
//! Workarounds for what the pinned dioxus-compose cannot draw yet, from
//! `.scratch/design-mockup/GAPS-WORKLIST.md` section 3: plain arc corners instead of the
//! squircle, one `elevation` instead of the two-layer shadow, no press scale (the toolkit's own
//! press feedback stays), no backdrop blur.

use dioxus_compose::prelude::*;
use dioxus_compose::{FontRef, FontRefs, GenericFamily};

use super::theme::*;
use super::theme::Palette;

// ---------------------------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------------------------

/// The font stack for prose (`--font`): Inter, then Apple SD Gothic Neo and Noto Sans KR for
/// Hangul, then the system UI face. The toolkit resolves installed families by name; it has no
/// way to bundle a font file yet, so Inter applies where it is installed
/// (`.scratch/issues/custom-font.md`).
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
// Text
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

/// Text for screens that still name a toolkit type role: the role maps to a step of the scale,
/// and a role colour maps to its token in the current scheme.
#[component]
pub fn Txt(
    #[props(into)] text: String,
    #[props(default = TypeRole::Body)] type_role: TypeRole,
    #[props(default)] color: Option<Paint>,
    #[props(default)] max_lines: Option<u32>,
    #[props(default)] overflow: Option<TextOverflow>,
) -> Element {
    let theme = use_ember_theme();
    let (fs, weight, mono) = type_role_style(type_role);
    let color = match color {
        Some(c) => resolve_paint(c, theme.palette),
        None => paint(theme.palette.text),
    };
    rsx! {
        Text {
            text,
            font_size: fs.size(),
            line_height: fs.line_height(),
            font_weight: weight.value(),
            letter_spacing: fs.letter_spacing(),
            color,
            max_lines,
            overflow,
            font: font_refs(mono),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Text field
// ---------------------------------------------------------------------------------------------

/// A text field drawn with the tokens instead of the toolkit's filled style, which stays light
/// in the dark scheme. `bare` is the field inside the composer or a row (no fill, no border);
/// otherwise it is a `surface-2` field on a `line` border with the `corner-md` radius.
#[component]
pub fn EmberField(
    #[props(default)] weight: Option<f32>,
    #[props(default)] fill_max_width: bool,
    #[props(into, default)] placeholder: String,
    #[props(default)] multiline: bool,
    #[props(default = true)] enabled: bool,
    #[props(default)] bare: bool,
    #[props(default)] height: Option<f32>,
    #[props(default)] corner_radius: Option<f32>,
    #[props(default)] background: Option<Paint>,
    #[props(default)] border_width: Option<f32>,
    #[props(default)] border_color: Option<Paint>,
    #[props(default)] type_role: Option<TypeRole>,
    #[props(default)] on_value_change: EventHandler<String>,
    #[props(default)] on_submit: EventHandler<String>,
    #[props(default)] on_focus_lost: EventHandler<()>,
    #[props(default)] on_key_down: EventHandler<KeyEvent>,
) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let (fill, border_w) = if bare {
        (Color::argb(0), 0.0)
    } else {
        (p.surface_2, 1.0)
    };
    rsx! {
        TextField {
            weight,
            fill_max_width,
            placeholder,
            multiline,
            enabled,
            type_role,
            height,
            background: background.unwrap_or_else(|| paint(fill)),
            border_width: border_width.unwrap_or(border_w),
            border_color: border_color.unwrap_or_else(|| paint(p.line)),
            corner_radius: corner_radius.unwrap_or(CORNER_MD),
            on_value_change: move |v: String| on_value_change.call(v),
            on_submit: move |v: String| on_submit.call(v),
            on_focus_lost: move |()| on_focus_lost.call(()),
            on_key_down: move |k: KeyEvent| on_key_down.call(k),
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

/// A status or count pill. Pills carry status and counts, never actions.
#[component]
pub fn Pill(#[props(into)] text: String, tone: PillTone, #[props(default)] small: bool) -> Element {
    let theme = use_ember_theme();
    let (fg, bg) = pill_colors(tone, theme.palette);
    let (height, fs, weight) = pill_metrics(small);
    rsx! {
        dioxus_compose::Box {
            height,
            background: paint(bg),
            shape_role: ShapeRole::Full,
            alignment: Alignment::Center,
            Row {
                padding: 0.0,
                alignment: Alignment::CenterStart,
                Spacer { width: 10.0 }
                Label { text, fs, weight, color: paint(fg), max_lines: 1 }
                Spacer { width: 10.0 }
            }
        }
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

/// Workaround for compose-rust #328 (Button and Chip labels are measured too narrow, so a
/// short Korean label such as "보내기" wraps to two lines). The toolkit's Button has no
/// `max_lines`, so the button gets a width that fits its label: an estimate of the label's
/// advance (a wide glyph is one em, a narrow one about 0.55 em), the 16 dp side padding on both
/// sides, and room for the icon. Remove this once #328 is fixed upstream.
pub fn label_min_width(text: &str, font_px: f32, has_icon: bool) -> f32 {
    let advance: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 0.55 } else { 1.0 })
        .sum::<f32>()
        * font_px;
    let icon = if has_icon { 20.0 + 8.0 } else { 0.0 };
    advance + icon + 2.0 * 16.0
}

/// A pill button. Text is a call to action, so it ends in "하세요" or is a plain noun label.
#[component]
pub fn AppButton(
    #[props(into)] text: String,
    #[props(default)] icon: Option<IconRole>,
    #[props(default = ButtonKind::Default)] kind: ButtonKind,
    #[props(default)] small: bool,
    #[props(default = true)] enabled: bool,
    #[props(default)] fill_max_width: bool,
    #[props(default)] on_click: EventHandler<()>,
) -> Element {
    let theme = use_ember_theme();
    let s = button_style(kind, small, theme.palette);
    let width = if fill_max_width || text.is_empty() {
        None
    } else {
        Some(label_min_width(&text, s.fs.size(), icon.is_some()))
    };
    rsx! {
        Button {
            text,
            icon,
            enabled,
            fill_max_width,
            // Workaround for compose-rust #328: no soft wrap, see `label_min_width`.
            width,
            height: s.height,
            variant: ButtonVariant::Text,
            background: paint(s.fill),
            color: paint(s.text),
            border_width: 1.0,
            border_color: paint(s.border),
            shape_role: ShapeRole::Full,
            on_click: move |()| on_click.call(()),
        }
    }
}

/// A 40 px (or 32 px) circle with a 1 px border and a glyph.
#[component]
pub fn IconButton(
    icon: IconRole,
    #[props(default)] small: bool,
    #[props(default)] on_click: EventHandler<()>,
) -> Element {
    let theme = use_ember_theme();
    let size = if small { H_CTL } else { H_BTN };
    rsx! {
        Button {
            text: "",
            icon,
            width: size,
            height: size,
            variant: ButtonVariant::Text,
            background: paint(theme.palette.surface),
            border_width: 1.0,
            border_color: paint(theme.palette.line),
            shape_role: ShapeRole::Full,
            on_click: move |()| on_click.call(()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pick chip
// ---------------------------------------------------------------------------------------------

/// `(height, fill)`: 32 px, no border, `surface-2` fill; the selected chip takes `sel`.
pub fn pickchip_style(selected: bool, p: &Palette) -> (f32, Color) {
    (H_CTL, if selected { p.sel } else { p.surface_2 })
}

/// The composer's choice chip (`.pickchip`): project, harness/model, computer, mode.
/// With `icon_only` (390 px and below) it drops its text and keeps the glyph.
#[component]
pub fn PickChip(
    #[props(into)] text: String,
    #[props(default)] icon: Option<IconRole>,
    #[props(default)] selected: bool,
    #[props(default)] icon_only: bool,
    #[props(default)] on_click: EventHandler<()>,
) -> Element {
    let theme = use_ember_theme();
    let (height, fill) = pickchip_style(selected, theme.palette);
    let text = if icon_only && icon.is_some() {
        String::new()
    } else {
        text
    };
    // Workaround for compose-rust #328: no soft wrap, see `label_min_width`.
    let width = if text.is_empty() {
        None
    } else {
        Some(label_min_width(&text, Fs::Sm.size(), icon.is_some()))
    };
    rsx! {
        Button {
            text,
            icon,
            width,
            height,
            variant: ButtonVariant::Text,
            background: paint(fill),
            color: paint(theme.palette.text),
            shape_role: ShapeRole::Full,
            on_click: move |()| on_click.call(()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Card
// ---------------------------------------------------------------------------------------------

/// `(fill, border)`: `surface` on a `line` border; a selected card takes `sel` and a 1 px
/// `ink` border, never a 2 px one.
pub fn card_colors(selected: bool, p: &Palette) -> (Color, Color) {
    if selected {
        (p.sel, p.ink)
    } else {
        (p.surface, p.line)
    }
}

/// A card: `surface`, 1 px `line`, the `corner-lg` radius, the card shadow.
/// TODO(GAPS: 스퀘어클, 그림자 2겹): plain arc corners and one elevation for now.
#[component]
pub fn EmberCard(
    #[props(default)] selected: bool,
    #[props(default)] fill_max_width: bool,
    #[props(default)] weight: Option<f32>,
    #[props(default)] padding: Option<f32>,
    #[props(default)] border: Option<Color>,
    children: Element,
) -> Element {
    let theme = use_ember_theme();
    let (fill, line) = card_colors(selected, theme.palette);
    let border = border.unwrap_or(line);
    rsx! {
        Card {
            fill_max_width,
            weight,
            padding,
            background: paint(fill),
            border_width: 1.0,
            border_color: paint(border),
            corner_radius: CORNER_LG,
            elevation: ELEVATION_CARD,
            {children}
        }
    }
}

/// The inset panel one level below a card: `surface-2`, `corner-md`.
#[component]
pub fn InsetPanel(#[props(default)] fill_max_width: bool, children: Element) -> Element {
    let theme = use_ember_theme();
    rsx! {
        Surface {
            fill_max_width,
            background: paint(theme.palette.surface_2),
            corner_radius: CORNER_MD,
            padding: PAD_ROW_V,
            {children}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Sheet header
// ---------------------------------------------------------------------------------------------

/// The header every sheet shares: 60 px high, 20 px side padding, the title in `fs-xl` 600 on
/// the left, actions right-aligned, a 32 px close button at the far right.
#[component]
pub fn SheetHeader(
    #[props(into)] title: String,
    #[props(default)] on_close: EventHandler<()>,
    #[props(default)] actions: Option<Element>,
) -> Element {
    rsx! {
        Row {
            fill_max_width: true,
            height: SHEET_HEADER_H,
            alignment: Alignment::CenterStart,
            Spacer { width: SP[4] }
            Label { text: title, fs: Fs::Xl, weight: Weight::Semibold, max_lines: 1 }
            Spacer { weight: 1.0 }
            if let Some(actions) = actions {
                {actions}
            }
            IconButton { icon: IconRole::Close, small: true, on_click: move |()| on_close.call(()) }
            Spacer { width: SP[4] }
        }
    }
}

/// A modal sheet with the shared header. At 560 px and below it is a full-width bottom sheet;
/// the toolkit's `Sheet` is that already.
/// TODO(GAPS: 스퀘어클): `corner-xl` squircle top corners; plain arcs for now.
#[component]
pub fn EmberSheet(
    #[props(into)] title: String,
    open: bool,
    #[props(default)] on_dismiss: EventHandler<()>,
    #[props(default)] actions: Option<Element>,
    children: Element,
) -> Element {
    let theme = use_ember_theme();
    rsx! {
        Sheet {
            open,
            fill_max_width: true,
            background: paint(theme.palette.surface),
            corner_radius: CORNER_XL,
            elevation: ELEVATION_LARGE,
            on_dismiss: move |()| on_dismiss.call(()),
            Column {
                fill_max_width: true,
                SheetHeader { title, actions, on_close: move |()| on_dismiss.call(()) }
                Separator { color: paint(theme.palette.line) }
                {children}
            }
        }
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
