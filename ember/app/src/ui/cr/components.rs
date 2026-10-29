//! compose-rust version of `ui::components` (#220 phase 1): pick chip, pill, card, sheet header,
//! buttons. Same names and style functions as the Dioxus module; the components are plain
//! functions that call compose-rust widgets, so a screen composes them inside a `#[composable]`.
//!
//! All compose-rust calls live in this file so the API changes coming in compose-rust (#285
//! `Copy` state handles, #286 `#[Composable]`, #289 and #290 call macros) touch one place.
//!
//! Gaps in the pinned compose-rust, each with its issue:
//! - #372: no container click, so nothing here is clickable except `Button`;
//! - #373: `Button` has no `max_lines`, so the width estimate `label_min_width` stays (#208);
//! - #374: `Text` and `TextField` have no `font` or `word_break`, so no bundled font and no
//!   Korean word-keep yet (#216, #208);
//! - #375: no `Modifier.shadow`, so one `elevation` stands in for the layered shadow (#214).

#![allow(non_snake_case)]

use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{
    Alignment, ButtonVariant, Color, ColorRole, FontRef, FontRefs, GenericFamily, IconRole,
    KeyEvent, Paint, ShapeRole, TextAlign, TextOverflow, TypeRole,
};

use super::theme::*;
use super::theme::Palette;

// ---------------------------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------------------------

/// The font stack for prose (`--font`): Inter, then Apple SD Gothic Neo and Noto Sans KR for
/// Hangul, then the system UI face. The toolkit resolves installed families by name; it has no
/// way to bundle a font file yet, so Inter applies where it is installed.
/// TODO(compose-rust#374): `Text` has no `font` property yet, so nothing calls this; wire it
/// into `Label` and `Txt` (and bundle Inter and Pretendard, #216) when it lands.
#[allow(dead_code)]
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
        TypeRole::Headline => (Fs::Xl3, Weight::Regular, false),
        TypeRole::Title => (Fs::Xl, Weight::Semibold, false),
        TypeRole::Subtitle => (Fs::Lg, Weight::Medium, false),
        TypeRole::BodyStrong => (Fs::Md, Weight::Medium, false),
        TypeRole::Caption => (Fs::Xs, Weight::Regular, false),
        TypeRole::Label => (Fs::Sm, Weight::Medium, false),
        TypeRole::Mono => (Fs::Xs, Weight::Regular, true),
        _ => (Fs::Md, Weight::Regular, false),
    }
}

fn with_weight(m: Modifier, weight: Option<f32>) -> Modifier {
    match weight {
        Some(w) => m.weight(w),
        None => m,
    }
}

fn with_height(m: Modifier, height: Option<f32>) -> Modifier {
    match height {
        Some(h) => m.height(h),
        None => m,
    }
}

// ---------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------

/// Text on the type scale: one of the nine sizes and one of the three weights.
pub fn Label(text: &str, fs: Fs, weight: Weight, color: Option<Paint>, max_lines: Option<u32>) {
    let theme = ember_theme();
    let color = color.unwrap_or_else(|| paint(theme.palette.text));
    let mut t = cu::Text(text)
        .font_size(fs.size())
        .line_height(fs.line_height())
        .font_weight(weight.value())
        .letter_spacing(fs.letter_spacing())
        .color(color);
    if let Some(n) = max_lines {
        t = t.max_lines(n);
    }
    drop(t);
}

/// Text for screens that still name a toolkit type role: the role maps to a step of the scale,
/// and a role colour maps to its token in the current scheme.
pub fn Txt(
    text: &str,
    type_role: TypeRole,
    color: Option<Paint>,
    max_lines: Option<u32>,
    overflow: Option<TextOverflow>,
    weight: Option<f32>,
    text_align: Option<TextAlign>,
) {
    let theme = ember_theme();
    let (fs, font_weight, mono) = type_role_style(type_role);
    let color = match color {
        Some(c) => resolve_paint(c, theme.palette),
        None if type_role == TypeRole::Caption => paint(theme.palette.muted),
        None => paint(theme.palette.text),
    };
    let mut t = cu::Text(text);
    if mono {
        // The theme's monospace face; the bundled font stack lands with compose-rust#374.
        t = t.type_role(TypeRole::Mono);
    }
    let mut t = t
        .font_size(fs.size())
        .line_height(fs.line_height())
        .font_weight(font_weight.value())
        .letter_spacing(fs.letter_spacing())
        .color(color)
        .modifier(with_weight(Modifier::new(), weight));
    if let Some(n) = max_lines {
        t = t.max_lines(n);
    }
    if let Some(o) = overflow {
        t = t.overflow(o);
    }
    if let Some(a) = text_align {
        t = t.text_align(a);
    }
    drop(t);
}

// ---------------------------------------------------------------------------------------------
// Text field
// ---------------------------------------------------------------------------------------------

/// Options of [`EmberField`]; `Default` is the plain single-line field.
#[derive(Default)]
pub struct FieldOptions {
    pub weight: Option<f32>,
    pub fill_max_width: bool,
    pub placeholder: String,
    pub multiline: bool,
    /// `None` keeps the field enabled.
    pub enabled: Option<bool>,
    /// The field inside the composer or a row: no fill, no border.
    pub bare: bool,
    pub height: Option<f32>,
    pub corner_radius: Option<f32>,
    pub background: Option<Paint>,
    pub border_width: Option<f32>,
    pub border_color: Option<Paint>,
    pub type_role: Option<TypeRole>,
    pub on_value_change: Option<Box<dyn FnMut(String)>>,
    pub on_submit: Option<Box<dyn FnMut(String)>>,
    pub on_focus_lost: Option<Box<dyn FnMut()>>,
    pub on_key_down: Option<Box<dyn FnMut(KeyEvent)>>,
}

/// A text field drawn with the tokens instead of the toolkit's filled style, which stays light
/// in the dark scheme. Not `bare`: a `surface` field on a `line` border with the
/// `corner-sm` radius and the 40 px control height (style.css `.field`).
pub fn EmberField(o: FieldOptions) {
    let theme = ember_theme();
    let p = theme.palette;
    let (fill, border_w) = if o.bare { (Color::argb(0), 0.0) } else { (p.surface, 1.0) };
    let modifier = with_height(with_weight(Modifier::new(), o.weight), o.height.or(if o.multiline || o.bare { None } else { Some(H_BTN) }))
        .fill_max_width_if(o.fill_max_width)
        .background(o.background.unwrap_or_else(|| paint(fill)))
        .border(
            o.border_width.unwrap_or(border_w),
            o.border_color.unwrap_or_else(|| paint(p.line)),
        )
        .corner_radius(o.corner_radius.unwrap_or(CORNER_SM));
    // TODO(compose-rust#376): the renderer paints the design system's field frame over these
    // modifiers, so only the height shows today; the fill, border, radius and 12 px padding
    // appear once the node's own are honored. 14 px text needs #374.
    let mut f = cu::TextField()
        .modifier(modifier)
        .placeholder(o.placeholder)
        .multiline(o.multiline)
        .enabled(o.enabled.unwrap_or(true));
    if let Some(r) = o.type_role {
        f = f.type_role(r);
    }
    if let Some(h) = o.on_value_change {
        f = f.on_value_change(h);
    }
    if let Some(h) = o.on_submit {
        f = f.on_submit(h);
    }
    if let Some(h) = o.on_focus_lost {
        f = f.on_focus_lost(h);
    }
    if let Some(h) = o.on_key_down {
        f = f.on_key_down(h);
    }
    drop(f);
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
    match tone {
        PillTone::Blue => (p.blue.strong, p.blue.soft),
        PillTone::Orange => (p.orange.strong, p.orange.soft),
        PillTone::Red => (p.red.strong, p.red.soft),
        PillTone::Green => (p.green.strong, p.green.soft),
        // style.css: `.pill.sky` is blue text on the sky fill, the default grey pill is
        // `--text-2` on the grey fill; the sky and grey strong colours are not text colours.
        PillTone::Sky => (p.blue.strong, p.sky.soft),
        PillTone::Grey => (p.text_2, p.grey.soft),
    }
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
pub fn Pill(text: &str, tone: PillTone, small: bool) {
    let theme = ember_theme();
    let (fg, bg) = pill_colors(tone, theme.palette);
    let (height, fs, weight) = pill_metrics(small);
    // A Row carries the fill so the height is the pill's own (24 or 20), not its content's.
    cu::Row()
        .alignment(Alignment::CenterStart)
        .modifier(Modifier::new().height(height).background(paint(bg)).shape_role(ShapeRole::Full))
        .content(|| {
            cu::Spacer().modifier(Modifier::new().width(SP[1]));
            Label(text, fs, weight, Some(paint(fg)), Some(1));
            cu::Spacer().modifier(Modifier::new().width(SP[1]));
        });
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

/// Workaround until compose-rust#373 (`Button` `max_lines` / `overflow`): Button and Chip labels
/// are measured too narrow, so a short Korean label such as "보내기" wraps to two lines. The
/// button gets a width that fits its label: an estimate of the label's advance (a wide glyph is
/// one em, a narrow one about 0.55 em), the 24 dp side padding of the toolkit button on both sides (measured: 16 dp left only
/// one glyph and an ellipsis), and room for the icon. Remove this once #373 is in.
pub fn label_min_width(text: &str, font_px: f32, has_icon: bool) -> f32 {
    let advance: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 0.55 } else { 1.0 })
        .sum::<f32>()
        * font_px;
    let icon = if has_icon { 20.0 + 8.0 } else { 0.0 };
    advance + icon + 2.0 * 24.0 + 4.0
}

// A Box with a pointer-up subscription would give the mockup's label sizes and no width
// estimate, but it has no keyboard focus or activation, no button role for screen readers and
// no hover or press feedback (only the toolkit `Button` and `Chip` carry those), so every
// control here is the toolkit's. The differences from the mockup (label size, side padding,
// the 16 px close glyph in the small icon button) are compose-rust#373 and #372.

/// A pill button. Text is a call to action, so it ends in "하세요" or is a plain noun label.
pub fn AppButton(
    text: &str,
    icon: Option<IconRole>,
    kind: ButtonKind,
    small: bool,
    enabled: bool,
    fill_max_width: bool,
    on_click: impl FnMut() + 'static,
) {
    let theme = ember_theme();
    let s = button_style(kind, small, theme.palette);
    // TODO(compose-rust#373): the toolkit label size and padding stand in for the mockup's,
    // and `label_min_width` keeps Korean labels on one line until `max_lines` exists.
    let width = if fill_max_width || text.is_empty() {
        None
    } else {
        Some(label_min_width(text, s.fs.size(), icon.is_some()))
    };
    let mut b = cu::Button(text)
        .enabled(enabled)
        .variant(ButtonVariant::Text)
        .color(paint(s.text))
        .modifier(
            Modifier::new()
                .fill_max_width_if(fill_max_width)
                .width_if(width)
                .height(s.height)
                .background(paint(s.fill))
                .border(1.0, paint(s.border))
                .shape_role(ShapeRole::Full),
        )
        .on_click(on_click);
    if let Some(i) = icon {
        b = b.icon(i);
    }
    drop(b);
}

/// A 40 px (or 32 px) circle with a 1 px border and a glyph.
pub fn IconButton(icon: IconRole, small: bool, on_click: impl FnMut() + 'static) {
    let theme = ember_theme();
    let size = if small { H_CTL } else { H_BTN };
    let ring = Modifier::new()
        .size(size, size)
        .background(paint(theme.palette.surface))
        .border(1.0, paint(theme.palette.line))
        .shape_role(ShapeRole::Full);
    // TODO(compose-rust#372): the mockup's 1.5 px line icons (18 px, 16 px small) are SVG
    // assets that need a clickable container with button semantics; the toolkit glyph is
    // tinted `--text-2` until then, and the small button shows it small.
    cu::Button("")
        .icon(icon)
        .color(paint(theme.palette.text_2))
        .variant(ButtonVariant::Text)
        .modifier(ring)
        .on_click(on_click);
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
pub fn PickChip(
    text: &str,
    icon: Option<IconRole>,
    selected: bool,
    icon_only: bool,
    on_click: impl FnMut() + 'static,
) {
    let theme = ember_theme();
    let (height, fill) = pickchip_style(selected, theme.palette);
    let text = if icon_only && icon.is_some() { "" } else { text };
    // TODO(compose-rust#373): label size and padding are the toolkit's, see AppButton.
    let width = if text.is_empty() {
        None
    } else {
        Some(label_min_width(text, Fs::Sm.size(), icon.is_some()))
    };
    let mut b = cu::Button(text)
        .variant(ButtonVariant::Text)
        .color(paint(theme.palette.text_2))
        .modifier(
            Modifier::new()
                .width_if(width)
                .height(height)
                .background(paint(fill))
                .shape_role(ShapeRole::Full),
        )
        .on_click(on_click);
    if let Some(i) = icon {
        b = b.icon(i);
    }
    drop(b);
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
/// TODO(compose-rust#378): corner radii render at about 0.55x the dp value on macOS, so the
/// card, inset and field corners look tighter than the mockup until that is fixed.
/// TODO(compose-rust#375): one elevation stands in for the layered shadow (#214); plain arc
/// corners instead of the squircle.
pub fn EmberCard(
    selected: bool,
    fill_max_width: bool,
    weight: Option<f32>,
    padding: Option<f32>,
    border: Option<Color>,
    content: impl FnOnce(),
) {
    let theme = ember_theme();
    let (fill, line) = card_colors(selected, theme.palette);
    let border = border.unwrap_or(line);
    let m = with_weight(Modifier::new(), weight)
        .fill_max_width_if(fill_max_width)
        .background(paint(fill))
        .border(1.0, paint(border))
        .corner_radius(CORNER_LG)
        .elevation(ELEVATION_CARD);
    // A Box rather than `Card`, whose design-system shape ignores `corner_radius`.
    // TODO(compose-rust): the mockup's 16/20 padding is asymmetric; `Modifier.padding` is
    // uniform, so 16 is used on all sides unless the caller passes one.
    // The 4 dp spacers raise the uniform 16 to the mockup's 20 on the sides (`--pad-card`).
    let side = if padding.is_some() { 0.0 } else { PAD_CARD_H - PAD_CARD_V };
    cu::Box().modifier(m.padding(padding.unwrap_or(PAD_CARD_V))).content(|| {
        cu::Row().content(|| {
            cu::Spacer().modifier(Modifier::new().width(side));
            cu::Column().content(content);
            cu::Spacer().modifier(Modifier::new().width(side));
        });
    });
}

/// The inset panel one level below a card: `surface-2`, `corner-md`.
pub fn InsetPanel(fill_max_width: bool, content: impl FnOnce()) {
    let theme = ember_theme();
    // TODO(compose-rust): the mockup's 12/16 padding is asymmetric; 12 on all sides for now.
    cu::Box()
        .modifier(
            Modifier::new()
                .fill_max_width_if(fill_max_width)
                .background(paint(theme.palette.surface_2))
                .corner_radius(CORNER_MD)
                .padding(PAD_ROW_V),
        )
        .content(|| {
            // 4 dp more on each side makes the mockup's 12 16 (`.inset`).
            cu::Row().content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[0]));
                cu::Column().content(content);
                cu::Spacer().modifier(Modifier::new().width(SP[0]));
            });
        });
}

// ---------------------------------------------------------------------------------------------
// Sheet header
// ---------------------------------------------------------------------------------------------

/// The header every sheet shares: 60 px high, 20 px side padding, the title in `fs-xl` 600 on
/// the left, actions right-aligned, a 32 px close button at the far right.
pub fn SheetHeader(
    title: &str,
    on_close: impl FnMut() + 'static,
    actions: Option<&mut dyn FnMut()>,
) {
    cu::Row()
        .alignment(Alignment::CenterStart)
        .modifier(Modifier::new().fill_max_width().height(SHEET_HEADER_H))
        .content(|| {
            cu::Spacer().modifier(Modifier::new().width(SP[4]));
            Label(title, Fs::Xl, Weight::Semibold, None, Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            if let Some(actions) = actions {
                actions();
            }
            IconButton(IconRole::Close, true, on_close);
            cu::Spacer().modifier(Modifier::new().width(SP[4]));
        });
}

/// A modal with the shared header (#212). Wide windows get a centered dialog over the scrim;
/// a compact window gets the toolkit's bottom sheet, full width.
/// TODO(compose-rust): `corner-xl` squircle corners; plain arcs for now.
pub fn EmberSheet(
    title: &str,
    open: bool,
    on_dismiss: impl FnMut() + Clone + 'static,
    actions: Option<&mut dyn FnMut()>,
    content: impl FnOnce(),
) {
    let theme = ember_theme();
    let modifier = Modifier::new()
        .background(paint(theme.palette.surface))
        .corner_radius(CORNER_XL)
        .elevation(ELEVATION_LARGE);
    let dismiss = on_dismiss.clone();
    let body = move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
            SheetHeader(title, dismiss, actions);
            cu::Separator(Some(paint(theme.palette.line)));
            content();
        });
    };
    if compose_rust::window_size().is_compact() {
        cu::Sheet(open)
            .modifier(modifier.fill_max_width())
            .on_dismiss(on_dismiss.clone())
            .content(body);
    } else {
        cu::Dialog(open).modifier(modifier).on_dismiss(on_dismiss).content(body);
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
        assert_eq!(type_role_style(TypeRole::Headline), (Fs::Xl3, Weight::Regular, false));
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
    fn pill_sky_is_blue_text_and_grey_is_text_2() {
        assert_eq!(pill_colors(PillTone::Sky, &LIGHT), (LIGHT.blue.strong, LIGHT.sky.soft));
        assert_eq!(pill_colors(PillTone::Grey, &DARK), (DARK.text_2, DARK.grey.soft));
    }

    #[test]
    fn pill_sizes() {
        assert_eq!(pill_metrics(false), (24.0, Fs::Xs, Weight::Medium));
        assert_eq!(pill_metrics(true), (20.0, Fs::Xs2, Weight::Medium));
    }

    #[test]
    fn label_min_width_grows_with_the_label_and_the_icon() {
        // Three Hangul glyphs at 14 px, no icon: 42 + 48 padding + 4 slack.
        assert_eq!(label_min_width("보내기", 14.0, false), 94.0);
        assert_eq!(label_min_width("보내기", 14.0, true), 122.0);
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

    fn texts(mutations: &[compose_rust::protocol::Mutation<'_>]) -> Vec<String> {
        use compose_rust::protocol::{Mutation, PropertyValue};
        mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fr_l10_shared_components_compose_through_the_host() {
        use compose_rust::protocol::decode_batch;
        use compose_rust::{Host, Recomposer};
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                EmberCard(false, true, None, Some(16.0), None, || {
                    Pill("진행 중", PillTone::Blue, false);
                    AppButton("보내기", None, ButtonKind::Primary, false, true, false, || {});
                    PickChip("프로젝트", Some(IconRole::Close), false, false, || {});
                });
            }))
        });
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let all = texts(&batch);
        for want in ["진행 중", "보내기", "프로젝트"] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
    }

    /// What the Host actually emits for a widget: every `SetModifier` record, as debug text.
    fn emitted_modifiers(content: fn()) -> Vec<String> {
        use compose_rust::protocol::{decode_batch, Mutation};
        use compose_rust::{Host, Recomposer};
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { modifier, .. } => Some(format!("{modifier:?}")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fr_l10_pill_and_card_send_the_mockup_metrics_to_the_renderer() {
        let pill = emitted_modifiers(|| Pill("진행 중", PillTone::Blue, false));
        assert!(pill.iter().any(|m| m == "Height(24.0)"), "{pill:?}");
        let small = emitted_modifiers(|| Pill("작음", PillTone::Blue, true));
        assert!(small.iter().any(|m| m == "Height(20.0)"), "{small:?}");
        let card = emitted_modifiers(|| EmberCard(false, false, None, None, None, || {}));
        assert!(card.iter().any(|m| m.starts_with("Shape { top_start: 14.0")), "{card:?}");
        let field = emitted_modifiers(|| {
            EmberField(FieldOptions { ..Default::default() });
        });
        assert!(field.iter().any(|m| m == "Height(40.0)"), "{field:?}");
        assert!(field.iter().any(|m| m.starts_with("Shape { top_start: 8.0")), "{field:?}");
    }

    #[test]
    fn fr_l10_body_text_sends_14_over_22_in_text_colour() {
        use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
        use compose_rust::{Host, Recomposer};
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                Txt("본문", TypeRole::Body, None, None, None, None, None)
            }))
        });
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let floats: Vec<f32> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { value: PropertyValue::Float(f), .. } => Some(*f),
                _ => None,
            })
            .collect();
        assert!(floats.contains(&14.0) && floats.contains(&22.0), "{floats:?}");
    }
}
