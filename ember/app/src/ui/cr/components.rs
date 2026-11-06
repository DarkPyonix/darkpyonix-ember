//! compose-rust version of `ui::components` (#220 phase 1): pick chip, pill, card, sheet header,
//! buttons. Same names and style functions as the Dioxus module; the components are plain
//! functions that call compose-rust widgets, so a screen composes them inside a `#[composable]`.
//!
//! All compose-rust calls live in this file so the API changes coming in compose-rust (#285
//! `Copy` state handles, #286 `#[Composable]`, #289 and #290 call macros) touch one place.
//!
//! Gaps in the pinned compose-rust, each with its issue:
//! - #372: container `on_click` is in compose-rust v0.0.5 (FR-55);
//! - #443: `Text` has no `overflow_wrap`, so a long unbreakable token clips (#208);
//! - #444: `TextField` has no `word_break`, so the placeholder can break inside a word (#208);

#![allow(non_snake_case)]

use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{
    Alignment, ButtonVariant, Color, ColorRole, FontRef, FontRefs, GenericFamily, IconRole,
    KeyEvent, Paint, ShapeRole, TextAlign, TextOverflow, TypeRole, WordBreak,
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

/// A container that reacts to a click.
///
/// This is the container's own `on_click`, which the renderer turns into a button: keyboard
/// focus, Enter and Space, a button role for screen readers, hover and press state
/// (compose-rust#372, FR-55). It is not a pointer subscription: the renderer of v0.0.4 accepts
/// `on_pointer` but never sends the Host a `Pointer` event, so a handler written that way never
/// runs (the sidebar rows did nothing). Every clickable container in `ui/cr` goes through here.
pub fn Clickable(
    modifier: Modifier,
    alignment: Alignment,
    mut on_click: impl FnMut() + 'static,
    content: impl FnOnce(),
) {
    cu::Box().modifier(modifier).alignment(alignment).on_click(move || on_click()).content(content);
}

/// One row of a popup menu: 38 px high, left-aligned `--text`, the chosen row on `--sel`.
/// The toolkit `Button` as a menu entry centres its label in link colour and stretches the
/// popup to the window, so rows are Ember's own until `Menu` entries can be styled.
/// TODO(compose-rust#372): no keyboard focus or role, as for every `Clickable`.
pub fn MenuRow(label: &str, width: f32, chosen: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let bg = if chosen { paint(p.sel) } else { Paint::Literal(Color::argb(0)) };
    Clickable(
        Modifier::new().width(width).height(38.0).background(bg).corner_radius(CORNER_SM),
        Alignment::CenterStart,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                Label(label, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
            });
        },
    );
}

/// A small modal list of options: opaque `--surface`, 1 px `--line`, the `corner-md` radius.
/// The toolkit `Menu` hangs its popup from the anchor's bottom-start corner and grows upward
/// over the anchor, with no way to place it below, so option lists are a modal instead.
/// TODO(compose-rust#383): an anchored popup placed below or beside its anchor.
pub fn OptionSheet(
    open: bool,
    options: &[String],
    chosen: Option<usize>,
    width: f32,
    on_dismiss: impl FnMut() + Clone + 'static,
    on_choose: impl FnMut(usize) + Clone + 'static,
) {
    let theme = ember_theme();
    let p = theme.palette;
    cu::Dialog(open)
        .modifier(
            with_shadows(Modifier::new(), theme.shadow_lg())
                .width(width + 2.0 * SP[0])
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .corner_radius(CORNER_MD)
                .padding(SP[0]),
        )
        .on_dismiss(on_dismiss.clone())
        .content(|| {
            cu::Column().content(|| {
                for (i, label) in options.iter().enumerate() {
                    let (mut dismiss, mut choose) = (on_dismiss.clone(), on_choose.clone());
                    MenuRow(label, width, chosen == Some(i), move || {
                        dismiss();
                        choose(i);
                    });
                }
            });
        });
}

/// A closed select: a 40 px field showing the chosen option and an arrow, opening an
/// [`OptionSheet`]. The toolkit `Dropdown` is a wheel on Apple design systems and a flat list
/// of every option in the tree.
/// TODO(compose-rust#383): a `Dropdown` presentation that does not depend on the design system.
pub fn Select(
    shown: &str,
    options: &[String],
    chosen: usize,
    popup_width: f32,
    on_choose: impl FnMut(usize) + Clone + 'static,
) {
    use compose_rust::runtime::{mutable_state_of, remember};
    let p = ember_theme().palette;
    let open = remember(|| mutable_state_of(false));
    let toggle = open.clone();
    // One node, so the popup's `Dialog` is not a separate child that a parent's spacing counts.
    cu::Box().modifier(Modifier::new().fill_max_width()).content(|| {
    Clickable(
        Modifier::new()
            .fill_max_width()
            .height(H_BTN)
            .background(paint(p.surface))
            .border(1.0, paint(p.line))
            .corner_radius(CORNER_SM),
        Alignment::CenterStart,
        move || toggle.set(!toggle.get_untracked()),
        || {
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                Label(shown, Fs::Md, Weight::Regular, Some(paint(p.text)), Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                super::shell::NavIcon(super::shell::ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 16.0);
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
            });
        },
    );
    let dismiss = open.clone();
    OptionSheet(open.get(), options, Some(chosen), popup_width, move || dismiss.set(false), on_choose);
    });
}

/// The footer every sheet and dialog shares: a line, then actions right-aligned (`.snew-foot`).
pub fn SheetFooter(content: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
    // `.snew-foot`: 12 above, 20 below, 20 at the right, 6 between the buttons.
    cu::Spacer().modifier(Modifier::new().height(SP[2]));
    cu::Row()
        .spacing(6.0)
        .alignment(Alignment::CenterStart)
        .modifier(Modifier::new().fill_max_width().height(H_CTL))
        .content(|| {
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            content();
            // The row's 6 px gap sits before this spacer too.
            cu::Spacer().modifier(Modifier::new().width(SP[4] - 6.0));
        });
    cu::Spacer().modifier(Modifier::new().height(SP[4]));
}

// ---------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------

/// Text on the type scale: one of the nine sizes and one of the three weights.
pub fn Label(text: &str, fs: Fs, weight: Weight, color: Option<Paint>, max_lines: Option<u32>) {
    let theme = ember_theme();
    let color = color.unwrap_or_else(|| paint(theme.palette.text));
    // TODO(compose-rust#443): Text has no overflow_wrap builder in v0.0.5, so a single
    // word wider than its container can still be clipped by the renderer.
    let mut t = cu::Text(text)
        .word_break(WordBreak::KeepAll)
        .font_size(fs.size())
        .line_height(fs.line_height())
        .font_weight(weight.value())
        .letter_spacing(fs.letter_spacing())
        .color(color);
    if let Some(n) = max_lines {
        t = t.max_lines(n).overflow(TextOverflow::Ellipsis);
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
    let mut t = cu::Text(text).word_break(WordBreak::KeepAll);
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
        t = t.max_lines(n).overflow(TextOverflow::Ellipsis);
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
    /// Text the field starts with (selectable, copyable); `None` starts empty.
    pub text: Option<String>,
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
    // The renderer paints the design system's own field frame over these, so a frame drawn here
    // only shows at the corners as a second box. The height is honored; the rest is left to the
    // toolkit until #376, and only a caller that asks for a frame gets one.
    let asked = o.background.is_some() || o.border_width.is_some() || o.border_color.is_some() || o.corner_radius.is_some();
    let mut modifier = with_height(with_weight(Modifier::new(), o.weight), o.height.or(if o.multiline || o.bare { None } else { Some(H_BTN) }))
        .fill_max_width_if(o.fill_max_width);
    if asked {
        modifier = modifier
            .background(o.background.unwrap_or_else(|| paint(fill)))
            .border(o.border_width.unwrap_or(border_w), o.border_color.unwrap_or_else(|| paint(p.line)))
            .corner_radius(o.corner_radius.unwrap_or(CORNER_SM));
    }
    // TODO(compose-rust#376): the renderer paints the design system's field frame over these
    // modifiers, so only the height shows today; the fill, border, radius and 12 px padding
    // appear once the node's own are honored. 14 px text needs #374.
    // TODO(compose-rust#444): TextField has no word_break or overflow builder; its
    // multiline placeholder can break Korean words inside a line.
    let mut f = cu::TextField()
        .modifier(modifier)
        .placeholder(o.placeholder)
        .multiline(o.multiline)
        .enabled(o.enabled.unwrap_or(true));
    if let Some(t) = o.text {
        f = f.text(t);
    }
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

/// The Dioxus component's tone enum, for the shared `reach_pill` and `harness_tone` helpers.
impl From<crate::ui::components::PillTone> for PillTone {
    fn from(t: crate::ui::components::PillTone) -> Self {
        use crate::ui::components::PillTone as D;
        match t {
            D::Blue => PillTone::Blue,
            D::Orange => PillTone::Orange,
            D::Red => PillTone::Red,
            D::Green => PillTone::Green,
            D::Sky => PillTone::Sky,
            D::Grey => PillTone::Grey,
        }
    }
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

/// A single-line bar: `pct` (0 to 100) of the track in `fill`. Drawn here rather than with the
/// toolkit indicator, whose colours are the design system's (compose-rust#377). Hoisted from the
/// files screen's `Bar` for the status cards (FR-L18, #349).
pub fn TrackBar(pct: f32, height: f32, fill: Color) {
    let p = ember_theme().palette;
    let pct = pct.clamp(0.0, 100.0);
    cu::Row()
        .modifier(Modifier::new().fill_max_width().height(height).background(paint(p.track)).shape_role(ShapeRole::Full))
        .content(move || {
            if pct > 0.0 {
                cu::Spacer().modifier(Modifier::new().weight(pct).height(height).background(paint(fill)).shape_role(ShapeRole::Full));
            }
            cu::Spacer().modifier(Modifier::new().weight((100.0 - pct) + 0.001));
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
    let mut b = cu::Button(text)
        .enabled(enabled)
        .variant(ButtonVariant::Text)
        .color(paint(s.text))
        .modifier(
            Modifier::new()
                .fill_max_width_if(fill_max_width)
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
    let mut b = cu::Button(text)
        .variant(ButtonVariant::Text)
        .color(paint(theme.palette.text_2))
        .modifier(
            Modifier::new()
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
pub fn EmberCard(
    selected: bool,
    fill_max_width: bool,
    weight: Option<f32>,
    padding: Option<f32>,
    border: Option<Color>,
    content: impl FnOnce(),
) {
    EmberCardSized(selected, fill_max_width, false, weight, padding, border, content);
}

/// `EmberCard` that can also fill the height its parent gives it. In a `Row` with
/// `equal_height(true)` a child with `fill_max_height` takes the tallest sibling's height
/// (FR-L18, compose-rust#389).
pub fn EmberCardSized(
    selected: bool,
    fill_max_width: bool,
    fill_max_height: bool,
    weight: Option<f32>,
    padding: Option<f32>,
    border: Option<Color>,
    content: impl FnOnce(),
) {
    let theme = ember_theme();
    let (fill, line) = card_colors(selected, theme.palette);
    let border = border.unwrap_or(line);
    let mut m = with_shadows(with_weight(Modifier::new(), weight), theme.shadow_sm())
        .fill_max_width_if(fill_max_width);
    if fill_max_height {
        m = m.fill_max_height();
    }
    let m = m
        .background(paint(fill))
        .border(1.0, paint(border))
        .corner_radius(CORNER_LG);
    // A Box rather than `Card`, whose design-system shape ignores `corner_radius`.
    // TODO(compose-rust): the mockup's 16/20 padding is asymmetric; `Modifier.padding` is
    // uniform, so 16 is used on all sides unless the caller passes one.
    // The 4 dp spacers raise the uniform 16 to the mockup's 20 on the sides (`--pad-card`).
    let side = if padding.is_some() { 0.0 } else { PAD_CARD_H - PAD_CARD_V };
    cu::Box().modifier(m.padding(padding.unwrap_or(PAD_CARD_V))).content(|| {
        // The row fills the card, so the card does not shrink to its content (FR-L18).
        cu::Row().modifier(Modifier::new().fill_max_width_if(fill_max_width || weight.is_some())).content(|| {
            cu::Spacer().modifier(Modifier::new().width(side));
            cu::Column().modifier(Modifier::new().weight(1.0)).content(content);
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
            // The Row and the Column take the panel's width (a Column with no weight shrinks to
            // its widest word, which broke the routine prompt into one word per line, #492).
            cu::Row().modifier(Modifier::new().fill_max_width_if(fill_max_width)).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[0]));
                if fill_max_width {
                    cu::Column().modifier(Modifier::new().weight(1.0)).content(content);
                } else {
                    cu::Column().content(content);
                }
                cu::Spacer().modifier(Modifier::new().width(SP[0]));
            });
        });
}

// ---------------------------------------------------------------------------------------------
// Sheet header
// ---------------------------------------------------------------------------------------------

/// The default maximum width of an Ember sheet (760 dp, #212).
pub const SHEET_WIDTH_DEFAULT: f32 = 760.0;

/// The maximum width of the computer monitor sheet (1360 dp, #212, FR-L21).
pub const SHEET_WIDTH_MONITOR: f32 = 1360.0;

/// Width of a centered sheet, capped by the window width.
pub fn sheet_width(window_width_dp: f32, max_width: f32) -> f32 {
    if window_width_dp <= 0.0 {
        max_width
    } else {
        window_width_dp.min(max_width)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SheetPresentation {
    Bottom,
    FullScreen,
    Dialog,
}

fn sheet_presentation(window_width_dp: f32, monitor: bool) -> SheetPresentation {
    if monitor && window_width_dp > 0.0 && window_width_dp <= 560.0 {
        SheetPresentation::FullScreen
    } else if window_width_dp > 0.0 && window_width_dp < 560.0 {
        SheetPresentation::Bottom
    } else {
        SheetPresentation::Dialog
    }
}

/// The tallest a sheet may be (`.sheet` in the mockup): 85 percent of the window for a bottom
/// sheet, the whole window for the full-screen monitor, and the window less 32 dp for a dialog.
pub(crate) fn sheet_max_height(window_width_dp: f32, window_height_dp: f32, monitor: bool) -> f32 {
    let h = window_height_dp.max(0.0);
    match sheet_presentation(window_width_dp, monitor) {
        SheetPresentation::Bottom => h * 0.85,
        SheetPresentation::FullScreen => h,
        SheetPresentation::Dialog => (h - 32.0).max(0.0),
    }
}

/// The body height used while the window height is unknown (not yet reported).
///
/// TODO(compose-rust#467, compose-rust#479): at 390x844 the implicit window never reports its dp
/// size (`current_window_size()` reads 0x0 with class Compact, measured on v0.0.6 with the shell
/// and with a sheet opened by a real press), so every sheet body is this constant. Remove it and
/// check the 85 percent formula at 390 once the implicit window can opt in to `report_size`.
const SHEET_BODY_UNKNOWN_H: f32 = 240.0;

/// The one formula for a sheet body's height: what is left of the sheet's maximum height after
/// the header (and any footer), never negative. Every sheet body sizes itself with this.
pub(crate) fn sheet_body_max_height(window_width_dp: f32, window_height_dp: f32, monitor: bool, chrome_h: f32) -> f32 {
    // A window that has not reported its height yet (0) is not a zero-high window: keep the body
    // visible instead of collapsing it.
    if window_height_dp <= 0.0 {
        return SHEET_BODY_UNKNOWN_H;
    }
    (sheet_max_height(window_width_dp, window_height_dp, monitor) - chrome_h).max(0.0)
}

/// The 32 px round close button of a sheet header, with the mockup's line icon.
/// TODO(compose-rust#372): a pointer-up subscription; the toolkit's modal calls on_dismiss for Escape and the scrim (not yet checked on a display).
/// TODO(compose-rust#476, FR-L32, #502): Escape does not close a sheet on a display. The Rust API
/// of v0.0.6 cannot receive the key: `Key` has only `Enter`, only `TextField` has `on_key_down`,
/// and there is no request-focus call, so a sheet cannot take focus when it opens. Ember passes
/// the same close callback (guarded where a guard exists) to the modal's `on_dismiss`, so Escape
/// closes every sheet as soon as the renderer delivers it.
pub fn CloseButton(on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    Clickable(
        Modifier::new()
            .size(H_CTL, H_CTL)
            .background(paint(p.surface))
            .border(1.0, paint(p.line))
            .shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        || super::shell::NavIcon(super::shell::ICON_CLOSE_SVG, p.text_2.to_argb(), 16.0),
    );
}

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
            cu::Box().modifier(Modifier::new().weight(1.0)).content(|| {
                Label(title, Fs::Xl, Weight::Semibold, None, Some(1));
            });
            cu::Spacer().modifier(Modifier::new().width(SP[2]));
            if let Some(actions) = actions {
                actions();
            }
            CloseButton(on_close);
            cu::Spacer().modifier(Modifier::new().width(SP[4]));
        });
}

/// A responsive modal with the shared header and a scrim (#212).
/// TODO(compose-rust#294): `corner-xl` squircle corners; plain arcs for now.
pub fn EmberSheet(
    title: &str,
    open: bool,
    on_dismiss: impl FnMut() + Clone + 'static,
    actions: Option<&mut dyn FnMut()>,
    content: impl FnOnce(),
) {
    EmberSheetSized(title, open, SHEET_WIDTH_DEFAULT, false, on_dismiss, actions, content);
}

/// [`EmberSheet`] with the dialog's width (`min(760px, 100%)` for most sheets, `min(1360px,
/// 100%)` for the computer monitor). Without a width the dialog takes the whole window,
/// because its content asks for the full width.
pub fn EmberSheetSized(
    title: &str,
    open: bool,
    width: f32,
    monitor: bool,
    on_dismiss: impl FnMut() + Clone + 'static,
    actions: Option<&mut dyn FnMut()>,
    content: impl FnOnce(),
) {
    let theme = ember_theme();
    // TODO(compose-rust#384): expose modal scrim color; the mockup uses rgba(0, 0, 0, 0.3).
    // The layered card shadow (#214) replaces the Material tint ring that `elevation` painted.
    let modifier = with_shadows(Modifier::new(), theme.shadow_sm()).background(paint(theme.palette.surface));
    let dismiss = on_dismiss.clone();
    let body = move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
            SheetHeader(title, dismiss, actions);
            cu::Separator(Some(paint(theme.palette.line)));
            content();
        });
    };
    let window = compose_rust::runtime::current_window_size();
    let presentation = sheet_presentation(window.width_dp, monitor);
    if presentation == SheetPresentation::Bottom {
        // TODO(compose-rust#445): expose a maximum height constraint so short sheets can wrap
        // their content instead of occupying the full 85 percent height.
        cu::Sheet(open)
            .modifier(modifier.fill_max_width().height(sheet_max_height(window.width_dp, window.height_dp, monitor))
                .corner_radii([CORNER_XL, CORNER_XL, 0.0, 0.0]))
            .on_dismiss(on_dismiss)
            .content(body);
    } else if presentation == SheetPresentation::FullScreen {
        cu::Dialog(open)
            .modifier(modifier.width(window.width_dp).height(window.height_dp))
            .on_dismiss(on_dismiss)
            .content(body);
    } else {
        cu::Dialog(open)
            .modifier(modifier.corner_radius(CORNER_XL).width(sheet_width(window.width_dp, width)))
            .on_dismiss(on_dismiss)
            .content(body);
    }
}

/// A filter chip (`.chip`): 32 px, `--ink` when selected, `--surface-2` otherwise, 12 px medium.
/// TODO(compose-rust#372): a pointer-up `Clickable`.
pub fn FilterChip(text: &str, selected: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (bg, ink) = if selected { (p.ink, p.ink_text) } else { (p.surface_2, p.text_2) };
    Clickable(
        Modifier::new().height(H_CTL).background(paint(bg)).shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                Label(text, Fs::Sm, Weight::Medium, Some(paint(ink)), Some(1));
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
            });
        },
    );
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
        assert!(!field.iter().any(|m| m.starts_with("Shape")), "{field:?}");
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

    #[test]
    fn fr_l32_sheet_width_for_each_sheet() {
        assert_eq!(SHEET_WIDTH_DEFAULT, 760.0);
        assert_eq!(SHEET_WIDTH_MONITOR, 1360.0);
        assert_eq!(sheet_width(1440.0, SHEET_WIDTH_DEFAULT), 760.0);
        assert_eq!(sheet_width(1440.0, SHEET_WIDTH_MONITOR), 1360.0);
        assert_eq!(sheet_width(600.0, SHEET_WIDTH_MONITOR), 600.0);
        assert_eq!(sheet_width(0.0, SHEET_WIDTH_DEFAULT), 760.0);
        // These call sites use EmberSheet, which selects SHEET_WIDTH_DEFAULT.
        for (source, call) in [
            (include_str!("settings.rs"), "EmberSheet(\"설정\""),
            (include_str!("new_session.rs"), "EmberSheet(\"새 세션\""),
            (include_str!("project.rs"), "EmberSheet(\"새 프로젝트\""),
            (include_str!("project.rs"), "EmberSheet(&format!"),
            (include_str!("routines.rs"), "EmberSheet(title"),
            (include_str!("files.rs"), "EmberSheet(\"파일 보내기\""),
        ] {
            assert!(source.contains(call), "missing {call}");
        }
        assert!(include_str!("monitor.rs").contains("EmberSheetSized(&title, true, SHEET_WIDTH_MONITOR"));
    }

    #[test]
    fn fr_l32_compact_sheet_presentation_and_height() {
        assert_eq!(sheet_presentation(390.0, false), SheetPresentation::Bottom);
        assert_eq!(sheet_presentation(390.0, true), SheetPresentation::FullScreen);
        assert_eq!(sheet_presentation(560.0, true), SheetPresentation::FullScreen);
        assert_eq!(sheet_presentation(561.0, true), SheetPresentation::Dialog);
        assert_eq!(sheet_presentation(600.0, false), SheetPresentation::Dialog);
        assert_eq!(sheet_presentation(559.0, false), SheetPresentation::Bottom);
        assert_eq!(sheet_presentation(560.0, false), SheetPresentation::Dialog);
        assert_eq!(sheet_max_height(390.0, 1000.0, false), 850.0);
    }

    #[test]
    fn fr_l32_sheet_body_height_is_one_formula() {
        // 85 percent of the window for the sheet, minus the 60 dp header.
        assert_eq!(sheet_body_max_height(390.0, 844.0, false, 60.0), 844.0 * 0.85 - 60.0);
        assert_eq!(sheet_body_max_height(390.0, 700.0, false, 60.0), 700.0 * 0.85 - 60.0);
        assert_eq!(sheet_body_max_height(390.0, 390.0, false, 60.0), 390.0 * 0.85 - 60.0);
        // With a 65 dp footer as well.
        assert_eq!(sheet_body_max_height(390.0, 844.0, false, 125.0), 844.0 * 0.85 - 125.0);
        // The real call sites at 390x844 and 390x640 keep a body well above 300 (header 60, footer 65).
        for h in [844.0, 640.0] {
            assert!(sheet_body_max_height(390.0, h, false, 60.0).min(900.0) > 300.0, "project {h}");
            assert!(sheet_body_max_height(390.0, h, false, 125.0).min(600.0) > 300.0, "routines {h}");
            assert!(sheet_body_max_height(390.0, h, false, 125.0).min(900.0) > 300.0, "new session {h}");
            assert!(sheet_body_max_height(390.0, h, true, 60.0).min(1200.0) > 300.0, "monitor {h}");
        }
        // A window that has not reported its height does not collapse the body.
        assert_eq!(sheet_body_max_height(390.0, 0.0, false, 60.0), 240.0);
        // Never negative.
        assert_eq!(sheet_body_max_height(390.0, 50.0, false, 125.0), 0.0);
        // The monitor fills the screen at 390; a dialog keeps 32 dp of margin.
        assert_eq!(sheet_body_max_height(390.0, 844.0, true, 60.0), 784.0);
        assert_eq!(sheet_body_max_height(900.0, 900.0, false, 60.0), 808.0);
    }

    #[test]
    fn fr_l32_every_modal_wires_dismiss_and_settings_keeps_its_guard() {
        // Escape, the scrim and the close button share one callback, so a modal without
        // on_dismiss could never close on Escape, and a guarded sheet must pass its guard.
        for (name, src) in [
            ("components.rs", include_str!("components.rs")),
            ("session.rs", include_str!("session.rs")),
            ("vcs.rs", include_str!("vcs.rs")),
        ] {
            let lines: Vec<&str> = src.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                let t = l.trim_start();
                if (t.starts_with("cu::Dialog(") || t.starts_with("cu::Sheet(")) && !t.starts_with("//") {
                    let near = lines[i..(i + 24).min(lines.len())].join("\n");
                    assert!(near.contains(".on_dismiss("), "{name}:{} modal without on_dismiss", i + 1);
                }
            }
        }
        assert!(include_str!("project.rs").contains("EmberSheet(&format!(\"{} 설정\", b.name), true, request_close,"));
    }

    static DISMISSES: AtomicUsize = AtomicUsize::new(0);

    #[composable]
    fn host_sheet() {
        EmberSheet("설정", true, || { DISMISSES.fetch_add(1, Ordering::SeqCst); }, None, || {});
    }

    #[test]
    fn fr_l32_renderer_dismiss_reaches_sheet_callback() {
        let _guard = crate::services::test_guard();
        DISMISSES.store(0, Ordering::SeqCst);
        let mut host = Host::new(|| Box::new(Recomposer::new(host_sheet)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let (node_id, handler_id) = batch.iter().find_map(|m| match m {
            Mutation::SetProp { node_id, property: PropertyKind::OnDismiss, value: PropertyValue::Integer(h) } => Some((*node_id, *h as u64)),
            _ => None,
        }).expect("modal dismiss handler");
        // Escape and scrim are interpreted by the renderer, which sends the same
        // dismissal event for both. Host cannot synthesize those physical inputs.
        for expected in 1..=2 {
            host.dispatch(HostEvent { node_id, handler_id, payload: EventPayload::Clicked }).expect("dispatch dismiss");
            assert_eq!(DISMISSES.load(Ordering::SeqCst), expected);
        }
    }

    #[test]
    fn fr_l10_1_chips_and_pills_emit_single_line_max_lines() {
        use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
        use compose_rust::{Host, PropertyKind, Recomposer};
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                Pill("브랜치", PillTone::Blue, false);
                FilterChip("전체", false, || {});
                MenuRow("새 세션", 120.0, false, || {});
                SheetHeader("IDE 열기", || {}, None);
                Label("보내기", Fs::Md, Weight::Medium, None, Some(1));
            }))
        });
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        for label in ["브랜치", "전체", "새 세션", "IDE 열기", "보내기"] {
            let mut target_nodes = Vec::new();
            for m in &batch {
                if let Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } = m {
                    if *s == label {
                        target_nodes.push(*node_id);
                    }
                }
            }
            let mut found = false;
            for nid in target_nodes {
                for m in &batch {
                    if let Mutation::SetProp { node_id, property: PropertyKind::MaxLines, value: PropertyValue::Integer(1) } = m {
                        if *node_id == nid {
                            found = true;
                            break;
                        }
                    }
                }
            }
            assert!(found, "expected text '{label}' to emit MaxLines(1)");
        }
    }

    #[test]
    fn fr_l10_1_cr_screens_use_safe_label_builders() {
        use std::fs;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui/cr");
        for entry in fs::read_dir(dir).expect("cr sources") {
            let path = entry.expect("source entry").path();
            if path.extension().and_then(|s| s.to_str()) != Some("rs") { continue; }
            let name = path.file_name().unwrap().to_str().unwrap();
            let source = fs::read_to_string(&path).expect("source text");
            let production = source.split("#[cfg(test)]").next().unwrap();
            assert!(!production.contains("WordBreak::BreakAll"), "{name} enables word breaking");
            if name != "components.rs" && name != "vcs.rs" {
                assert!(!production.contains("cu::Text("), "{name} bypasses the shared text builder");
            }
            // Existing direct buttons are explicitly counted until their specialized controls
            // move into shared components. Any additional bypass must be reviewed.
            let allowed_buttons = match name {
                "components.rs" => 3,
                "agents.rs" => 2,
                "files.rs" => 1,
                "home.rs" => 2,
                "launcher.rs" => 1,
                "project.rs" => 1,
                "routines.rs" => 2,
                // FocusOverlay and the StashPill accessible-name overlay (FR-G6, #350).
                "vcs.rs" => 2,
                // The accessible-name overlay of the channel + button (#499, compose-rust#429).
                "new_channel.rs" => 1,
                _ => 0,
            };
            assert_eq!(production.matches("cu::Button(").count(), allowed_buttons, "{name} has an unreviewed direct button");
            // Paragraphs and descriptions can use multiple lines. Button and Chip retain
            // their toolkit one-line defaults.
        }
        let source = fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui/cr/components.rs")).unwrap();
        assert!(source.split("#[cfg(test)]").next().unwrap().matches(".word_break(WordBreak::KeepAll)").count() >= 1);
    }

    #[test]
    fn fr_l10_1_card_content_has_available_width() {
        let source = include_str!("components.rs");
        let card = source.split("pub fn EmberCard(").nth(1).unwrap()
            .split("pub fn InsetPanel(").next().unwrap();
        assert!(card.contains("cu::Column().modifier(Modifier::new().weight(1.0)).content(content)"));
    }

    #[test]
    fn fr_l10_1_monitor_descriptions_wrap_and_diff_lines_ellipsize() {
        let monitor = include_str!("monitor.rs");
        // Descriptions (status line, empty card text) may use up to three lines; labels use one.
        assert!(monitor.contains("Label(&status_line(feed, online), Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(3))"));
        assert!(monitor.contains("Label(empty, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(3))"));
        assert!(monitor.contains(r#"Label("할당된 프로젝트가 없습니다", Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(3))"#));
        let vcs = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!vcs.contains("TextOverflow::Clip"));
        assert!(vcs.contains(".word_break(compose_rust::WordBreak::KeepAll)"));
    }

    // ---- FR-L10: a navigation row is a control the renderer can click -----------------------

    use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
    use compose_rust::{composable, EventPayload, Host, PropertyKind, Recomposer};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CLICKS: AtomicUsize = AtomicUsize::new(0);

    #[composable]
    fn host_clickables() {
        use crate::ui::shell::{NavBadge, NavMenu};
        use crate::ui::cr::shell::{NavRow, RailItem, ToolRow};
        cu::Column().content(|| {
            NavRow(NavMenu::Projects, false, NavBadge::None, || { CLICKS.fetch_add(1, Ordering::SeqCst); });
            RailItem(NavMenu::Projects, false, NavBadge::None, || { CLICKS.fetch_add(1, Ordering::SeqCst); });
            ToolRow(crate::ui::shell::ICON_GEAR_SVG, "설정", false, || { CLICKS.fetch_add(1, Ordering::SeqCst); });
            MenuRow("항목", 200.0, false, || { CLICKS.fetch_add(1, Ordering::SeqCst); });
            Clickable(Modifier::new().size(40.0, 40.0), Alignment::Center, || { CLICKS.fetch_add(1, Ordering::SeqCst); }, || {});
        });
    }

    /// The renderer acts on `OnClick` (it makes the node a button) and on nothing else a
    /// container could carry: v0.0.4 ignored `OnPointer`, so rows built on it were dead. The
    /// test asks the Host for the handler of each row as the renderer does, and clicks it.
    #[test]
    fn fr_l10_every_clickable_row_has_a_click_handler_that_runs_when_the_renderer_clicks_it() {
        let _guard = crate::services::test_guard();
        CLICKS.store(0, Ordering::SeqCst);
        let mut host = Host::new(|| Box::new(Recomposer::new(host_clickables)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let clickable: Vec<(u32, u64)> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                    Some((*node_id, *h as u64))
                }
                _ => None,
            })
            .collect();
        assert_eq!(clickable.len(), 5, "each row and the bare Clickable carry OnClick: {clickable:?}");
        for (i, (node_id, handler_id)) in clickable.into_iter().enumerate() {
            host.dispatch(HostEvent { node_id, handler_id, payload: EventPayload::Clicked }).expect("dispatch");
            assert_eq!(CLICKS.load(Ordering::SeqCst), i + 1, "the click on row {i} reached its closure");
        }
    }
}
