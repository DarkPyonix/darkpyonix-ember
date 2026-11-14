//! compose-rust version of `ui::components` (#220 phase 1): pick chip, pill, card, sheet header,
//! buttons. Same names and style functions as the Dioxus module; the components are plain
//! functions that call compose-rust widgets, so a screen composes them inside a `#[Composable]`.
//!
//! All compose-rust calls live in this file so the API changes coming in compose-rust (#285
//! `Copy` state handles, #286 `#[Composable]`, #289 and #290 call macros) touch one place.
//!
//! Gaps in the pinned compose-rust, each with its issue:
//! - #372: container `on_click` is in compose-rust v0.0.5 (FR-55);
//! - #443 and #444 are fixed in v0.0.8: `Text::overflow_wrap` lets a long token break
//!   ([`WrapLabel`]), and `TextField::word_break` keeps the placeholder's words whole (#208);

#![allow(non_snake_case)]

use compose_rust::ui as cu;
use compose_rust::ui::{Modifier, Role};
use compose_rust::{
    Alignment, ButtonVariant, Color, ColorRole, FontRef, FontRefs, GenericFamily, IconRole,
    KeyEvent, OverflowWrap, Paint, ShapeRole, TextAlign, TextOverflow, TypeRole, WordBreak,
};

use super::theme::*;
use super::theme::Palette;

// ---------------------------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------------------------

/// The font stack for prose (`--font`): Inter, then Apple SD Gothic Neo and Noto Sans KR for
/// Hangul, then the system UI face. The toolkit resolves installed families by name; it has no
/// way to bundle a font file yet, so Inter applies where it is installed.
/// v0.0.7: #374 fixed (`Text::font` and `TextField::font` exist); workaround kept until checked on
/// a display (wiring this stack into `Label` and `Txt` changes every label's face, and Inter and
/// Pretendard are not bundled yet, #216), so nothing calls this yet.
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
/// (compose-rust#372, FR-55). It is not a pointer subscription: a click stays the container's
/// `on_click` even though the renderer sends `on_pointer` events since v0.0.7 (#417), because
/// `on_click` is also what Enter, Space and an assistive press run. Every clickable container in
/// `ui/cr` goes through here; an icon-only one goes through [`NamedClickable`].
pub fn Clickable(
    modifier: Modifier,
    alignment: Alignment,
    mut on_click: impl FnMut() + 'static,
    content: impl FnOnce(),
) {
    cu::Box().modifier(modifier).alignment(alignment).on_click(move || on_click()).content(content);
}

/// A [`Clickable`] whose child is only an icon or a glyph, named for assistive technology with
/// `Modifier::label` (compose-rust v0.0.7, #460, #429). `label` is `&'static str` on purpose:
/// the toolkit keeps every distinct label for the life of the program (at most 4096), so a name
/// built from data (a routine name, a commit subject) takes a `Button`'s `accessible_name`
/// instead.
pub fn NamedClickable(
    label: &'static str,
    modifier: Modifier,
    alignment: Alignment,
    on_click: impl FnMut() + 'static,
    content: impl FnOnce(),
) {
    Clickable(modifier.label(label).role(Role::Button).click_action(true), alignment, on_click, content);
}

/// The label of a copy button. The button puts text the screen already holds on the clipboard
/// with `compose_rust::clipboard::set_text` (compose-rust v0.0.7, #471).
pub const COPY_LABEL: &str = "복사";

/// What a copy button says after it is pressed: `sent` is whether the toolkit took the text.
pub fn copy_notice(sent: bool) -> &'static str {
    if sent { "클립보드에 복사했습니다" } else { "복사하지 못했습니다" }
}

/// One row of a popup menu: 38 px high, left-aligned `--text`, the chosen row on `--sel`.
/// The toolkit `Button` as a menu entry centres its label in link colour and stretches the
/// popup to the window, so rows are Ember's own until `Menu` entries can be styled. The row is a
/// menu item to assistive technology and says which one is chosen (compose-rust v0.0.7, #372,
/// #429); its visible label is its name.
pub fn MenuRow(label: &str, width: f32, chosen: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let bg = if chosen { paint(p.sel) } else { Paint::Literal(Color::argb(0)) };
    Clickable(
        Modifier::new()
            .width(width)
            .height(38.0)
            .background(bg)
            .corner_radius(CORNER_SM)
            .role(Role::MenuItem)
            .selected(chosen),
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
/// v0.0.7: #383 fixed (`Select`, `Dropdown::closed`, `MenuItem` and `Popover` open below the
/// anchor); workaround kept until checked on a display (the toolkit popup must match the
/// mockup's surface, border and row height before the modal goes).
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
/// v0.0.7: #383 fixed (`Select` and `Dropdown::closed(true)` draw a closed field with a
/// below-anchor popup); workaround kept until checked on a display (the closed field must match
/// the mockup's 40 px field in both schemes).
pub fn Select(
    shown: &str,
    options: &[String],
    chosen: usize,
    popup_width: f32,
    on_choose: impl FnMut(usize) + Clone + 'static,
) {
    select_field(None, shown, options, chosen, popup_width, on_choose);
}

/// A [`Select`] named for assistive technology by the label beside it (`FieldRow`'s text), so a
/// screen reader says which field it is and not only the chosen option. The trigger is a dropdown
/// list to the platform (`Role::DropdownList`, which compose-rust#473 hands to the platform in
/// v0.0.9). `name` is `&'static str` for the reason [`NamedClickable`] gives.
pub fn NamedSelect(
    name: &'static str,
    shown: &str,
    options: &[String],
    chosen: usize,
    popup_width: f32,
    on_choose: impl FnMut(usize) + Clone + 'static,
) {
    select_field(Some(name), shown, options, chosen, popup_width, on_choose);
}

fn select_field(
    name: Option<&'static str>,
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
    let trigger = Modifier::new()
        .fill_max_width()
        .height(H_BTN)
        .background(paint(p.surface))
        .border(1.0, paint(p.line))
        .corner_radius(CORNER_SM)
        .role(Role::DropdownList)
        .expanded(open.get())
        .click_action(true);
    let trigger = match name {
        Some(n) => trigger.label(n),
        None => trigger,
    };
    // One node, so the popup's `Dialog` is not a separate child that a parent's spacing counts.
    cu::Box().modifier(Modifier::new().fill_max_width()).content(|| {
    Clickable(
        trigger,
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
///
/// It breaks Korean only at spaces (`WordBreak::KeepAll`) and keeps room for its longest word in
/// a weighted row (compose-rust#427), so a token wider than its container still clips. Text that
/// can hold a path, a URL or a long name uses [`WrapLabel`].
pub fn Label(text: &str, fs: Fs, weight: Weight, color: Option<Paint>, max_lines: Option<u32>) {
    scale_text(text, fs, weight, color, max_lines, false);
}

/// A [`Label`] whose token too long for its line breaks between its letters, the mockup's
/// `word-break:keep-all; overflow-wrap:anywhere` (compose-rust#443, v0.0.8). Such a text is as
/// narrow as its widest letter in a weighted row, so use it where the text should wrap rather
/// than widen its column.
pub fn WrapLabel(text: &str, fs: Fs, weight: Weight, color: Option<Paint>, max_lines: Option<u32>) {
    scale_text(text, fs, weight, color, max_lines, true);
}

fn scale_text(
    text: &str,
    fs: Fs,
    weight: Weight,
    color: Option<Paint>,
    max_lines: Option<u32>,
    wrap_anywhere: bool,
) {
    let theme = ember_theme();
    let color = color.unwrap_or_else(|| paint(theme.palette.text));
    let mut t = cu::Text(text)
        .word_break(WordBreak::KeepAll)
        .font_size(fs.size())
        .line_height(fs.line_height())
        .font_weight(weight.value())
        .letter_spacing(fs.letter_spacing())
        .color(color);
    if wrap_anywhere {
        t = t.overflow_wrap(OverflowWrap::Anywhere);
    }
    if let Some(n) = max_lines {
        t = t.max_lines(n).overflow(TextOverflow::Ellipsis);
    }
    drop(t);
}

/// A short mono string at a scale size (the one-time code): selectable text, never broken.
pub fn MonoLabel(text: &str, fs: Fs, weight: Weight, color: Option<Paint>) {
    let theme = ember_theme();
    let color = color.unwrap_or_else(|| paint(theme.palette.text));
    drop(
        cu::Text(text)
            .type_role(TypeRole::Mono)
            .font_size(fs.size())
            .line_height(fs.line_height())
            .font_weight(weight.value())
            .color(color),
    );
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
        // The theme's monospace face. v0.0.7: #374 fixed (`Text::font`); the bundled font stack
        // waits for the fonts themselves (#216), see `font_refs`.
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
    /// The field inside the composer or a row: no fill, no border, and no toolkit frame
    /// (`TextField::bare`, compose-rust v0.0.7). `background`, `border_*` and `corner_radius`
    /// are not sent for it: the container draws the frame.
    pub bare: bool,
    /// The room inside a `bare` field, `(horizontal, vertical)` in dp; `None` is none. The
    /// toolkit's bare field has no padding of its own, so this is the mockup's inset (the
    /// textarea's 2 px, `.inputbar input` 14 px). A vertical inset is taken from both sides and
    /// also counts towards the horizontal one. A framed field defaults to [`FIELD_INSET`]
    /// ([`FIELD_INSET_MULTILINE`] when multiline) and takes this in its place.
    pub inset: Option<(f32, f32)>,
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

/// The type step of a field's text and placeholder when no role is named (style.css `.field`).
pub const FIELD_FONT: Fs = Fs::Md;

/// The text inset of a framed single-line field, `(horizontal, vertical)`: style.css `.field`
/// padding 0 12 (FR-L10.2, #878).
pub const FIELD_INSET: (f32, f32) = (12.0, 0.0);
/// The same for a multiline field: `textarea.field` padding 10 12.
pub const FIELD_INSET_MULTILINE: (f32, f32) = (12.0, 10.0);

/// A text field drawn with the tokens instead of the toolkit's filled style, which stays light
/// in the dark scheme. Not `bare`: a `surface` field on a `line` border with the
/// `corner-sm` radius and the 40 px control height (style.css `.field`), its text 12 px from
/// the frame. `bare`: see [`BareField`].
///
/// The frame is Ember's own row and the field inside it is bare (FR-L10.2, #878): the toolkit's
/// framed field puts its text 8 px in, and a per-side inner padding would also have to keep a
/// single line centred in the 40 px height, which `TextField` cannot do yet.
/// TODO(compose-rust#771): move the frame and the 12 px inset onto the field once it takes
/// per-side content padding; until then a click on the 12 px strip does not focus the field.
pub fn EmberField(o: FieldOptions) {
    if o.bare {
        BareField(o);
        return;
    }
    let mut o = o;
    let theme = ember_theme();
    let p = theme.palette;
    let multiline = o.multiline;
    let height = o.height.or(if multiline { None } else { Some(H_BTN) });
    // compose-rust v0.0.5 (#376) honours a TextField's own Background, Border and Shape in place
    // of the design system's frame, which stays light in the dark scheme (#510). The frame row
    // names the token fill and line instead.
    let frame = with_height(with_weight(Modifier::new(), o.weight), height)
        .fill_max_width_if(o.fill_max_width)
        .background(o.background.take().unwrap_or_else(|| paint(p.surface)))
        .corner_radius(o.corner_radius.take().unwrap_or(CORNER_SM));
    // A zero-width border is not sent: the desktop renderer passes it to Compose's
    // `Modifier.border(0.dp)`, which is `Dp.Hairline` and draws a 1 px line (FR-L10.2, #844).
    let border_w = o.border_width.take().unwrap_or(1.0);
    let border_color = o.border_color.take();
    let frame = if border_w > 0.0 { frame.border(border_w, border_color.unwrap_or_else(|| paint(p.line))) } else { frame };
    let inset = o.inset.take().unwrap_or(if multiline { FIELD_INSET_MULTILINE } else { FIELD_INSET });
    let inner = FieldOptions { bare: true, inset: Some(inset), height, weight: None, fill_max_width: true, ..o };
    cu::Row()
        .alignment(if multiline { Alignment::TopStart } else { Alignment::CenterStart })
        .modifier(frame)
        .content(move || BareField(inner));
}

/// A field inside a container that draws the frame (FR-L10.2, #844).
///
/// The toolkit's bare field (compose-rust v0.0.7 `HostTextField.kt`, `Bare`) draws no frame,
/// has no inner padding and sets its text at the top of whatever height it is given
/// (`contentAlignment = TopStart`). So the height, weight, width and inset the caller asked for
/// go on a Row around it, which centres a single line vertically, and the field itself takes
/// only the row's remaining width (and its height, when multiline). No background, border or
/// shape is sent: a zero-width border is drawn as a 1 px hairline, the frame that was still
/// visible inside the composers.
fn BareField(o: FieldOptions) {
    let (ix, iy) = o.inset.unwrap_or((0.0, 0.0));
    let (ix, iy) = (ix.max(0.0), iy.max(0.0));
    let side = (ix - iy).max(0.0);
    let multiline = o.multiline;
    let mut wrap = with_height(with_weight(Modifier::new(), o.weight), o.height).fill_max_width_if(o.fill_max_width);
    if iy > 0.0 {
        wrap = wrap.padding(iy);
    }
    let inner = if multiline && o.height.is_some() {
        Modifier::new().weight(1.0).fill_max_height()
    } else {
        Modifier::new().weight(1.0)
    };
    cu::Row()
        .alignment(if multiline { Alignment::TopStart } else { Alignment::CenterStart })
        .modifier(wrap)
        .content(move || {
            if side > 0.0 {
                cu::Spacer().modifier(Modifier::new().width(side));
            }
            FieldNode(o, inner);
            if side > 0.0 {
                cu::Spacer().modifier(Modifier::new().width(side));
            }
        });
}

/// The toolkit field with every option but the frame, which `modifier` carries.
fn FieldNode(o: FieldOptions, modifier: Modifier) {
    // compose-rust#444 (v0.0.8): the placeholder and the typed text break Korean only at spaces,
    // and a long token such as a path or a URL breaks inside itself rather than clipping.
    let mut f = cu::TextField()
        .modifier(modifier)
        .word_break(WordBreak::KeepAll)
        .overflow_wrap(OverflowWrap::Anywhere)
        .placeholder(o.placeholder)
        .multiline(o.multiline)
        .enabled(o.enabled.unwrap_or(true));
    if let Some(t) = o.text {
        f = f.text(t);
    }
    // FR-L10.2 (#844): 14 px text and placeholder (style.css `.field`) unless the caller names a
    // role; an explicit `font_size` would override the role's size, so it is sent only without one.
    match o.type_role {
        Some(r) => f = f.type_role(r),
        None => f = f.font_size(FIELD_FONT.size()),
    }
    // The node's transparent fill alone still left the toolkit's frame drawn inside the
    // composer (H2, SE10, CH1); `bare` drops it.
    if o.bare {
        f = f.bare();
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
/// toolkit indicator, whose colours are the design system's (compose-rust#377 made only the
/// ring's stroke scale; the indicator still takes no colour). Hoisted from the
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
    /// The label's step on the type scale: 14 (`Fs::Md`), or 13 (`Fs::Sm`) when small.
    pub fs: Fs,
    /// The label's weight: 500 for every kind (style.css `.btn`, FR-L10.2).
    pub weight: Weight,
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
            weight: Weight::Medium,
        },
        ButtonKind::Primary => ButtonStyle {
            height,
            fill: p.ink,
            text: p.ink_text,
            border: p.ink,
            fs,
            weight: Weight::Medium,
        },
        ButtonKind::Destructive => ButtonStyle {
            height,
            fill: p.surface,
            text: p.red.strong,
            border: p.red.soft,
            fs,
            weight: Weight::Medium,
        },
    }
}

/// `button_style` for a button that may be disabled. A disabled primary button keeps its
/// filled shape, in `muted` with the `ink_text` on it: the toolkit's own disabled fade turned it
/// into grey text on a grey fill, 1.5:1 in the dark scheme (NFR-L3, #510). The other kinds keep
/// their style; the toolkit fades them.
pub fn button_style_for(kind: ButtonKind, small: bool, enabled: bool, p: &Palette) -> ButtonStyle {
    let s = button_style(kind, small, p);
    if kind == ButtonKind::Primary && !enabled {
        ButtonStyle { fill: p.muted, border: p.muted, text: p.ink_text, ..s }
    } else {
        s
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
    let s = button_style_for(kind, small, enabled, theme.palette);
    let mut b = cu::Button(text)
        .enabled(enabled)
        .variant(ButtonVariant::Text)
        .color(paint(s.text))
        // FR-L10.2 (#844): the mockup's `.btn` 14/500 (`.btn.sm` 13/500), not the toolkit's
        // label style (about 17/600). compose-rust v0.0.7 sends both only when they are set.
        .font_size(s.fs.size())
        .font_weight(s.weight.value())
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

/// A pill button that is one choice of several: the chosen one is `Primary`, the others
/// `Default`. To assistive technology a `Toggle` says it is pressed (`Modifier::selected`, the
/// mockup's `aria-pressed`) and a `Radio` is a radio button that is checked or not; compose-rust
/// v0.0.9 hands both to the platform (#473).
pub fn ChoiceButton(
    text: &str,
    choice: Choice,
    chosen: bool,
    small: bool,
    enabled: bool,
    on_click: impl FnMut() + 'static,
) {
    let theme = ember_theme();
    let kind = if chosen { ButtonKind::Primary } else { ButtonKind::Default };
    let s = button_style_for(kind, small, enabled, theme.palette);
    let state = match choice {
        Choice::Toggle => Modifier::new().selected(chosen),
        Choice::Radio => Modifier::new().role(Role::RadioButton).checked(chosen),
    };
    drop(
        cu::Button(text)
            .enabled(enabled)
            .variant(ButtonVariant::Text)
            .color(paint(s.text))
            // The same 14/500 (13/500 small) label as AppButton (FR-L10.2, #844).
            .font_size(s.fs.size())
            .font_weight(s.weight.value())
            .modifier(
                state
                    .height(s.height)
                    .background(paint(s.fill))
                    .border(1.0, paint(s.border))
                    .shape_role(ShapeRole::Full),
            )
            .on_click(on_click),
    );
}

/// How a [`ChoiceButton`] tells assistive technology that it is the chosen one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// A pressed toggle (`aria-pressed`), such as the character style switch.
    Toggle,
    /// A checked radio button (`role=radio`), such as the fan mode control.
    Radio,
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
    // v0.0.7: #372 fixed (container `on_click`), and `Button::content` (#425) can hold the
    // mockup's 1.5 px SVG line icons; workaround kept until checked on a display (moving every
    // caller to an SVG and a Korean `accessible_name` changes this signature). The toolkit glyph
    // is tinted `--text-2`, and the small button shows it small.
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

/// A card: `surface`, 1 px `line`, the `corner-lg` radius, the card shadow (corner radii render
/// at their dp value since compose-rust#378).
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

/// Whether a (non-monitor) sheet is a bottom sheet at this width, which is as tall as its
/// content up to [`SHEET_BOTTOM_MAX_FRACTION`] and scrolls taller content itself (FR-L32.1).
pub(crate) fn sheet_wraps_content(window_width_dp: f32) -> bool {
    sheet_presentation(window_width_dp, false) == SheetPresentation::Bottom
}

/// The sheet header and the separator under it (`SheetHeader` and `cu::Separator`), FR-L32.1.
pub(crate) const SHEET_HEADER_CHROME_H: f32 = SHEET_HEADER_H + 1.0;

/// [`SheetFooter`]: its 1 dp line, 12 above the buttons, the 32 dp buttons and 20 below (FR-L32.1).
pub(crate) const SHEET_FOOTER_H: f32 = 1.0 + SP[2] + H_CTL + SP[4];

/// The tallest a bottom sheet grows, as a share of the window (`.sheet` `max-height:85vh`).
pub(crate) const SHEET_BOTTOM_MAX_FRACTION: f32 = 0.85;

/// The tallest a sheet may be (`.sheet` in the mockup): 85 percent of the window for a bottom
/// sheet, the whole window for the full-screen monitor, and the window less 32 dp for a dialog.
pub(crate) fn sheet_max_height(window_width_dp: f32, window_height_dp: f32, monitor: bool) -> f32 {
    let h = window_height_dp.max(0.0);
    match sheet_presentation(window_width_dp, monitor) {
        SheetPresentation::Bottom => h * SHEET_BOTTOM_MAX_FRACTION,
        SheetPresentation::FullScreen => h,
        SheetPresentation::Dialog => (h - 32.0).max(0.0),
    }
}

/// The body height used while the window height is unknown (not yet reported).
///
/// v0.0.7: #467 and #479 fixed (the window opts in with `with_report_size(true)` in
/// `shell::window_chrome`, and a 390x844 window sends its first size); workaround kept until
/// checked on a display (the first composition still reads 0x0 before the first report, measured
/// 2026-11-06, and the 85 percent formula at 390 needs a real press on each sheet).
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
/// Its accessible name is "닫기" (compose-rust v0.0.7, #460).
/// v0.0.7: #476 fixed (containers take `on_key_down`, and `Batch::request_focus` exists);
/// workaround kept until checked on a display (FR-L32, #502: that Escape reaches the modal's
/// `on_dismiss`). Ember passes the same close callback (guarded where a guard exists) to the
/// modal's `on_dismiss`, so Escape closes every sheet once the renderer delivers it.
pub fn CloseButton(on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    NamedClickable(
        super::a11y::CLOSE,
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
/// v0.0.7: #294 fixed (`Modifier::corner_curve`); workaround kept until checked on a display
/// (the `corner-xl` squircle must be compared with the mockup in both schemes); plain arcs for now.
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
    // The mockup's `.modal` scrim, rgba(0, 0, 0, 0.3), set on every presentation (compose-rust#384,
    // v0.0.10 `ModalScope::scrim`). The layered card shadow (#214) replaces the Material tint ring that `elevation` painted.
    // Explicit zero padding suppresses the renderer dialog's theme padding, which otherwise
    // adds space around the frame and prevents a full-screen sheet.
    let modifier = with_shadows(Modifier::new().padding(0.0), theme.shadow_sm()).background(paint(theme.palette.surface));
    let dismiss = on_dismiss.clone();
    let body = move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
            SheetHeader(title, dismiss, actions);
            cu::Separator(Some(paint(theme.palette.line)));
            content();
        });
    };
    let window = crate::ui::window_size::current_window_size();
    let presentation = sheet_presentation(window.width_dp, monitor);
    if presentation == SheetPresentation::Bottom {
        // compose-rust#445 (v0.0.9): the sheet is as tall as its content up to 85 percent of the
        // window, and taller content scrolls inside it, so a short sheet wraps its content.
        cu::Sheet(open)
            .modifier(modifier.fill_max_width().corner_radii([CORNER_XL, CORNER_XL, 0.0, 0.0]))
            .max_height_fraction(SHEET_BOTTOM_MAX_FRACTION)
            .scrim(Paint::Literal(MODAL_SCRIM))
            .on_dismiss(on_dismiss)
            .content(body);
    } else if presentation == SheetPresentation::FullScreen {
        cu::Dialog(open)
            .modifier(modifier.width(window.width_dp).height(window.height_dp))
            .scrim(Paint::Literal(MODAL_SCRIM))
            .on_dismiss(on_dismiss)
            .content(body);
    } else {
        cu::Dialog(open)
            .modifier(modifier.corner_radius(CORNER_XL).width(sheet_width(window.width_dp, width)))
            .scrim(Paint::Literal(MODAL_SCRIM))
            .on_dismiss(on_dismiss)
            .content(body);
    }
}

/// The mockup's modal scrim (`style.css` `.modal`, `rgba(0,0,0,.3)`): black at alpha 0x4D (77 of 255).
pub const MODAL_SCRIM: Color = Color::argb(0x4D00_0000);

/// A filter chip (`.chip`): 32 px, `--ink` when selected, `--surface-2` otherwise, 12 px medium.
/// Its label is its name, and it says whether it is selected (compose-rust v0.0.7, #429).
pub fn FilterChip(text: &str, selected: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (bg, ink) = if selected { (p.ink, p.ink_text) } else { (p.surface_2, p.text_2) };
    Clickable(
        Modifier::new().height(H_CTL).background(paint(bg)).shape_role(ShapeRole::Full).selected(selected),
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

// ---------------------------------------------------------------------------------------------
// SearchField (style.css `.searchbox`)
// ---------------------------------------------------------------------------------------------

/// The compact search height of the session sidebar (`.sside .st-q`).
pub const SEARCH_FIELD_H_COMPACT: f32 = 36.0;
/// The leading magnifier's size (`.searchbox svg`).
pub const SEARCH_ICON_SIZE: f32 = 18.0;

/// The pill radius of a search field of `height` (`.searchbox` is 999, capped at half the height).
pub fn search_field_radius(height: f32) -> f32 {
    (height / 2.0).min(PILL)
}

/// The leading padding before the magnifier: 16 at the 40 px height, 12 at the compact one.
pub fn search_field_padding(height: f32) -> f32 {
    if height < H_BTN { 12.0 } else { 16.0 }
}

/// The visible gap between the magnifier and the typed text (`.searchbox` gap 8).
pub const SEARCH_FIELD_GAP: f32 = 8.0;
/// The leading inset the field adds before its text. The v0.0.6 framed field had about 12 px,
/// taken back with a -4 px offset (design review 2026-11-13 C4). Since #844 the field is the
/// toolkit's bare one (compose-rust v0.0.7), which has none, so the -4 px offset drew the
/// placeholder over the magnifier (design review round 2); the 8 px gap is now a spacer.
pub const SEARCH_FIELD_INSET: f32 = 0.0;

/// The visible icon-to-text gap: a spacer, the field's own inset and an offset, which together
/// always come to [`SEARCH_FIELD_GAP`].
pub fn search_field_gap() -> f32 {
    search_field_spacer() + search_field_offset() + SEARCH_FIELD_INSET
}

/// The spacer between the magnifier and the field, for an inset smaller than the gap.
pub fn search_field_spacer() -> f32 {
    (SEARCH_FIELD_GAP - SEARCH_FIELD_INSET).max(0.0)
}

/// The negative x offset an inset larger than the gap would need; 0 since the field is bare
/// (#844), so [`SearchField`] no longer sends one.
pub fn search_field_offset() -> f32 {
    (SEARCH_FIELD_GAP - SEARCH_FIELD_INSET).min(0.0)
}

/// A search box: white fill, 1 px border, pill radius, a leading magnifier and a bare field that
/// takes the rest of the row. Width intent: it fills its column (`fill_max_width`); the mockup's
/// 280/220 px is for the standalone box, which a caller gets by constraining the parent. The
/// compact (36 px) field uses the `Label` role (Fs::Sm, 13 px) like the mockup's session field. The field is uncontrolled and keeps its hint `placeholder`.
/// v0.0.7: #376 and #374 fixed (`TextField` takes the node's background, border and shape, and
/// `text`); workaround kept until checked on a display (the bare field inside Ember's own pill
/// must be compared with the mockup before the frame moves onto the field).
pub fn SearchField(placeholder: &str, height: f32, on_change: impl FnMut(String) + 'static) {
    let p = ember_theme().palette;
    let placeholder = placeholder.to_string();
    let on_change = std::rc::Rc::new(std::cell::RefCell::new(Some(on_change)));
    cu::Row()
        .alignment(Alignment::CenterStart)
        .modifier(
            Modifier::new()
                .fill_max_width()
                .height(height)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .shape_role(ShapeRole::Full),
        )
        .content(move || {
            cu::Spacer().modifier(Modifier::new().width(search_field_padding(height)));
            super::shell::NavIcon(super::shell::ICON_SEARCH_SVG, p.muted.to_argb(), SEARCH_ICON_SIZE);
            if search_field_spacer() > 0.0 {
                cu::Spacer().modifier(Modifier::new().width(search_field_spacer()));
            }
            // The field takes the row's remaining width by weight, so it never overruns it, and
            // the bare field's own row centres the placeholder in the box's height (#844).
            EmberField(FieldOptions {
                bare: true,
                type_role: Some(TypeRole::Label), // one 13 dp size for every search field (mockup fs-sm)
                height: Some(height),
                weight: Some(1.0),
                placeholder: placeholder.clone(),
                on_value_change: on_change.borrow_mut().take().map(|f| Box::new(f) as Box<dyn FnMut(String)>),
                ..Default::default()
            });
            // `.searchbox` padding 0 16: typed text stops short of the round end.
            cu::Spacer().modifier(Modifier::new().width(search_field_padding(height)));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l10_1_search_field_height_radius_and_icon_size() {
        assert_eq!(H_BTN, 40.0);
        assert_eq!(SEARCH_FIELD_H_COMPACT, 36.0);
        assert_eq!(SEARCH_ICON_SIZE, 18.0);
        assert_eq!(search_field_radius(40.0), 20.0);
        assert_eq!(search_field_radius(36.0), 18.0);
    }

    #[test]
    fn fr_l10_1_search_field_padding_is_16_and_12_when_compact() {
        assert_eq!(search_field_padding(H_BTN), 16.0);
        assert_eq!(search_field_padding(SEARCH_FIELD_H_COMPACT), 12.0);
    }

    #[test]
    fn fr_l10_1_search_field_visible_gap_is_8() {
        // Design review 2026-11-13 C4: 8 px after the magnifier. The bare field (#844) has no
        // inset, so the gap is all spacer and nothing is pulled over the magnifier.
        assert_eq!(search_field_gap(), 8.0);
        assert_eq!(SEARCH_FIELD_INSET, 0.0);
        assert_eq!(search_field_offset(), 0.0);
        assert_eq!(search_field_spacer(), 8.0);
    }

    /// One node of a composition: its widget, its parent, and its modifiers and props as
    /// `"Height(40.0)"` and `"Bare=Bool(true)"`.
    struct EmittedNode {
        id: u32,
        widget: String,
        parent: Option<u32>,
        mods: Vec<String>,
        props: Vec<String>,
    }

    fn emitted_nodes(content: fn()) -> Vec<EmittedNode> {
        use compose_rust::protocol::{decode_batch, Mutation};
        use compose_rust::{Host, Recomposer};
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut nodes: Vec<EmittedNode> = Vec::new();
        for m in batch.iter() {
            match m {
                Mutation::Create { node_id, widget } => nodes.push(EmittedNode {
                    id: *node_id,
                    widget: format!("{widget:?}"),
                    parent: None,
                    mods: Vec::new(),
                    props: Vec::new(),
                }),
                Mutation::Insert { parent_id, node_id, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.parent = Some(*parent_id);
                    }
                }
                Mutation::SetModifier { node_id, modifier, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.mods.push(format!("{modifier:?}"));
                    }
                }
                Mutation::SetProp { node_id, property, value } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.props.push(format!("{property:?}={value:?}"));
                    }
                }
                _ => {}
            }
        }
        nodes
    }

    fn the<'a>(nodes: &'a [EmittedNode], widget: &str) -> &'a EmittedNode {
        nodes.iter().find(|n| n.widget == widget).unwrap_or_else(|| panic!("no {widget} node"))
    }

    fn parent_of<'a>(nodes: &'a [EmittedNode], child: &EmittedNode) -> &'a EmittedNode {
        nodes.iter().find(|n| Some(n.id) == child.parent).expect("parent node")
    }

    /// The props a bare `Row` with this alignment sends, to compare a wrapper's with.
    fn row_alignment(content: fn()) -> Vec<String> {
        let nodes = emitted_nodes(content);
        let row = nodes.iter().rev().find(|n| n.widget == "Row").expect("the Row");
        assert!(!row.props.is_empty(), "the alignment is sent as a prop");
        row.props.clone()
    }

    fn has_props(node: &EmittedNode, props: &[String]) -> bool {
        props.iter().all(|p| node.props.contains(p))
    }

    #[test]
    fn fr_l10_2_bare_field_sends_no_border_background_or_shape() {
        // Compose draws `border(0.dp)` as a 1 px hairline, the frame still seen inside the home,
        // conversation and channel composers (design review round 2).
        let nodes = emitted_nodes(|| {
            EmberField(FieldOptions { bare: true, weight: Some(1.0), height: Some(40.0), ..Default::default() })
        });
        let field = the(&nodes, "TextField");
        assert!(field.props.iter().any(|p| p == "Bare=Bool(true)"), "{:?}", field.props);
        for n in &nodes {
            assert!(
                !n.mods.iter().any(|m| m.starts_with("Border") || m.starts_with("Background") || m.starts_with("Shape")),
                "{} {:?}",
                n.widget,
                n.mods
            );
        }
    }

    #[test]
    fn fr_l10_2_framed_field_never_sends_a_zero_border() {
        let nodes = emitted_nodes(|| EmberField(FieldOptions { border_width: Some(0.0), ..Default::default() }));
        for n in &nodes {
            assert!(!n.mods.iter().any(|m| m.starts_with("Border")), "{} {:?}", n.widget, n.mods);
        }
        let field = the(&nodes, "TextField");
        let frame = parent_of(&nodes, parent_of(&nodes, field));
        assert!(frame.mods.iter().any(|m| m.starts_with("Background")), "{:?}", frame.mods);
    }

    #[test]
    fn fr_l10_2_framed_field_draws_the_frame_and_insets_its_text_12() {
        // style.css `.field`: 40 px, surface fill, 1 px line, corner-sm, padding 0 12 (#878: the
        // toolkit's framed field put the text 8 px in).
        let nodes = emitted_nodes(|| EmberField(FieldOptions { fill_max_width: true, ..Default::default() }));
        let field = the(&nodes, "TextField");
        assert!(field.props.iter().any(|p| p == "Bare=Bool(true)"), "{:?}", field.props);
        for m in ["Border", "Background", "Shape", "Height"] {
            assert!(!field.mods.iter().any(|x| x.starts_with(m)), "the field itself sends no {m}: {:?}", field.mods);
        }
        let inset_row = parent_of(&nodes, field);
        assert_eq!(inset_row.widget, "Row");
        assert!(inset_row.mods.iter().any(|m| m == "Height(40.0)"), "{:?}", inset_row.mods);
        let spacers: Vec<_> = nodes.iter().filter(|n| n.widget == "Spacer" && n.parent == Some(inset_row.id)).collect();
        assert_eq!(spacers.len(), 2, "12 px before and after the text");
        assert!(spacers.iter().all(|s| s.mods.iter().any(|m| m == "Width(12.0)")));
        let frame = parent_of(&nodes, inset_row);
        assert_eq!(frame.widget, "Row");
        assert!(frame.mods.iter().any(|m| m == "Height(40.0)"), "{:?}", frame.mods);
        assert!(frame.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", frame.mods);
        assert!(frame.mods.iter().any(|m| m.starts_with("Background")), "{:?}", frame.mods);
        assert!(frame.mods.iter().any(|m| m.starts_with("Border")), "{:?}", frame.mods);
        assert!(frame.mods.iter().any(|m| m.starts_with("Shape")), "{:?}", frame.mods);
        let centre = row_alignment(|| cu::Row().alignment(Alignment::CenterStart).content(|| {}));
        assert!(has_props(frame, &centre), "{:?} has {centre:?}", frame.props);
        assert_eq!(FIELD_INSET, (12.0, 0.0));
    }

    #[test]
    fn fr_l10_2_framed_multiline_field_insets_10_and_12() {
        // style.css `textarea.field` padding 10 12: 10 all round, plus 2 more at each side.
        let nodes = emitted_nodes(|| {
            EmberField(FieldOptions { fill_max_width: true, multiline: true, height: Some(120.0), ..Default::default() })
        });
        let field = the(&nodes, "TextField");
        assert!(field.mods.iter().any(|m| m == "FillMaxHeight"), "{:?}", field.mods);
        let inset_row = parent_of(&nodes, field);
        assert!(inset_row.mods.iter().any(|m| m == "Padding(10.0)"), "{:?}", inset_row.mods);
        let spacers: Vec<_> = nodes.iter().filter(|n| n.widget == "Spacer" && n.parent == Some(inset_row.id)).collect();
        assert!(spacers.len() == 2 && spacers.iter().all(|s| s.mods.iter().any(|m| m == "Width(2.0)")));
        let frame = parent_of(&nodes, inset_row);
        assert!(frame.mods.iter().any(|m| m == "Height(120.0)"), "{:?}", frame.mods);
        let top = row_alignment(|| cu::Row().alignment(Alignment::TopStart).content(|| {}));
        assert!(has_props(frame, &top), "{:?} has {top:?}", frame.props);
        assert_eq!(FIELD_INSET_MULTILINE, (12.0, 10.0));
    }

    #[test]
    fn fr_l10_2_bare_single_line_field_is_centred_in_the_callers_height() {
        // HostTextField.kt sets a bare field's text at the top of the height it is given, so the
        // height and weight go on a row that centres it.
        let nodes = emitted_nodes(|| {
            EmberField(FieldOptions { bare: true, weight: Some(1.0), height: Some(40.0), ..Default::default() })
        });
        let field = the(&nodes, "TextField");
        assert!(!field.mods.iter().any(|m| m.starts_with("Height")), "{:?}", field.mods);
        assert!(field.mods.iter().any(|m| m == "Weight(1.0)"), "{:?}", field.mods);
        let row = parent_of(&nodes, field);
        assert_eq!(row.widget, "Row");
        assert!(row.mods.iter().any(|m| m == "Height(40.0)"), "{:?}", row.mods);
        assert!(row.mods.iter().any(|m| m == "Weight(1.0)"), "{:?}", row.mods);
        let centre = row_alignment(|| cu::Row().alignment(Alignment::CenterStart).content(|| {}));
        assert!(has_props(row, &centre), "{:?} has {centre:?}", row.props);
        // No inset was asked for: no spacer beside the field.
        assert!(!nodes.iter().any(|n| n.widget == "Spacer"));
    }

    #[test]
    fn fr_l10_2_bare_field_takes_the_callers_inset() {
        // `.inputbar input` padding 0 14.
        let nodes = emitted_nodes(|| {
            EmberField(FieldOptions { bare: true, fill_max_width: true, inset: Some((14.0, 0.0)), ..Default::default() })
        });
        let field = the(&nodes, "TextField");
        let row = parent_of(&nodes, field);
        assert!(row.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", row.mods);
        let spacers: Vec<_> = nodes.iter().filter(|n| n.widget == "Spacer" && n.parent == Some(row.id)).collect();
        assert_eq!(spacers.len(), 2);
        assert!(spacers.iter().all(|s| s.mods.iter().any(|m| m == "Width(14.0)")));
        // A multiline field fills the row's height inside the vertical inset, text at the top.
        let nodes = emitted_nodes(|| {
            EmberField(FieldOptions {
                bare: true,
                multiline: true,
                fill_max_width: true,
                height: Some(84.0),
                inset: Some((2.0, 2.0)),
                ..Default::default()
            })
        });
        let field = the(&nodes, "TextField");
        assert!(field.mods.iter().any(|m| m == "FillMaxHeight"), "{:?}", field.mods);
        let row = parent_of(&nodes, field);
        assert!(row.mods.iter().any(|m| m == "Height(84.0)"), "{:?}", row.mods);
        assert!(row.mods.iter().any(|m| m == "Padding(2.0)"), "{:?}", row.mods);
        let top = row_alignment(|| cu::Row().alignment(Alignment::TopStart).content(|| {}));
        assert!(has_props(row, &top), "{:?} has {top:?}", row.props);
        assert!(!nodes.iter().any(|n| n.widget == "Spacer"), "the 2 px all round is the padding alone");
    }

    #[test]
    fn fr_l10_2_search_field_centres_its_placeholder_8_after_the_magnifier() {
        let nodes = emitted_nodes(|| SearchField("세션 검색", SEARCH_FIELD_H_COMPACT, |_| {}));
        let field = the(&nodes, "TextField");
        assert!(!field.mods.iter().any(|m| m.starts_with("Height") || m.starts_with("Offset")), "{:?}", field.mods);
        let row = parent_of(&nodes, field);
        assert!(row.mods.iter().any(|m| m == "Height(36.0)"), "{:?}", row.mods);
        let centre = row_alignment(|| cu::Row().alignment(Alignment::CenterStart).content(|| {}));
        assert!(has_props(row, &centre), "{:?} has {centre:?}", row.props);
        assert!(nodes.iter().any(|n| n.widget == "Spacer" && n.mods.iter().any(|m| m == "Width(8.0)")), "the 8 px gap");
        assert!(!nodes.iter().any(|n| n.mods.iter().any(|m| m.starts_with("Offset"))), "nothing is pulled over the magnifier");
    }

    #[test]
    fn fr_l10_2_bare_callers_name_the_mockup_inset() {
        // `.inputbar input` 0 14 (channels.css), the composer textarea's 2 px (home.css), and the
        // session composer's 14 px less the 12 px card padding plus the textarea's 2 px.
        let channels = include_str!("channels.rs");
        let at = channels.find("placeholder: \"메시지 입력\"").expect("channel composer field");
        let opts = &channels[channels[..at].rfind("EmberField(FieldOptions {").expect("field")..at];
        assert!(opts.contains("inset: Some((14.0, 0.0))"), "{opts}");
        assert!(include_str!("home.rs").contains("inset: Some((2.0, 2.0))"));
        assert!(include_str!("conversation.rs").contains("inset: Some((4.0, 0.0))"));
    }

    #[test]
    fn fr_l10_1_search_field_call_sites() {
        let sites = [
            ("channels", include_str!("channels.rs"), "채널, 메시지 검색"),
            ("session", include_str!("session.rs"), "세션 검색"),
            ("project", include_str!("project.rs"), "팀원 검색"),
            ("project", include_str!("project.rs"), "멤버 검색"),
            ("routines", include_str!("routines.rs"), "루틴 검색"),
        ];
        for (name, src, ph) in sites {
            assert!(src.contains("SearchField("), "{name} uses SearchField");
            assert!(src.contains(&format!("SearchField(\"{ph}\"")), "{name} keeps the placeholder {ph}");
        }
        let project = include_str!("project.rs");
        assert!(!project.contains("placeholder: \"멤버 검색\""), "the old EmberField member search is gone");
    }

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
        // NFR-L3 (#510): the field names its frame, so the toolkit's light one is not drawn.
        assert!(field.iter().any(|m| m.starts_with("Shape")), "{field:?}");
        assert!(field.iter().any(|m| m.starts_with("Background")), "{field:?}");
        assert!(field.iter().any(|m| m.starts_with("Border")), "{field:?}");
    }

    /// Every `SetProp` of one composition as `"Property=Value"`, e.g. `"FontSize=Float(14.0)"`.
    fn emitted_props(content: fn()) -> Vec<String> {
        use compose_rust::protocol::{decode_batch, Mutation};
        use compose_rust::{Host, Recomposer};
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { property, value, .. } => Some(format!("{property:?}={value:?}")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fr_l10_2_button_style_is_14_500_and_13_500_small() {
        for p in [&LIGHT, &DARK] {
            for kind in [ButtonKind::Default, ButtonKind::Primary, ButtonKind::Destructive] {
                for enabled in [true, false] {
                    let s = button_style_for(kind, false, enabled, p);
                    assert_eq!((s.fs.size(), s.weight.value()), (14.0, 500), "{kind:?}");
                    let s = button_style_for(kind, true, enabled, p);
                    assert_eq!((s.fs.size(), s.weight.value()), (13.0, 500), "{kind:?} small");
                }
            }
        }
    }

    #[test]
    fn fr_l10_2_app_button_sends_its_label_size_and_weight() {
        let big = emitted_props(|| AppButton("저장", None, ButtonKind::Primary, false, true, false, || {}));
        assert!(big.iter().any(|m| m == "FontSize=Float(14.0)"), "{big:?}");
        assert!(big.iter().any(|m| m == "FontWeight=Integer(500)"), "{big:?}");
        let small = emitted_props(|| AppButton("취소", None, ButtonKind::Default, true, false, false, || {}));
        assert!(small.iter().any(|m| m == "FontSize=Float(13.0)"), "{small:?}");
        assert!(small.iter().any(|m| m == "FontWeight=Integer(500)"), "{small:?}");
        // A choice button carries the same label as AppButton.
        let choice = emitted_props(|| ChoiceButton("자동", Choice::Radio, true, false, true, || {}));
        assert!(choice.iter().any(|m| m == "FontSize=Float(14.0)"), "{choice:?}");
        assert!(choice.iter().any(|m| m == "FontWeight=Integer(500)"), "{choice:?}");
        let choice = emitted_props(|| ChoiceButton("수동", Choice::Toggle, false, true, true, || {}));
        assert!(choice.iter().any(|m| m == "FontSize=Float(13.0)"), "{choice:?}");
        assert!(choice.iter().any(|m| m == "FontWeight=Integer(500)"), "{choice:?}");
    }

    #[test]
    fn fr_l10_2_field_text_is_14_and_bare_drops_the_toolkit_frame() {
        let plain = emitted_props(|| EmberField(FieldOptions { ..Default::default() }));
        assert!(plain.iter().any(|m| m == "FontSize=Float(14.0)"), "{plain:?}");
        // #878: a framed field is a bare field inside Ember's own `.field` frame, so the toolkit
        // frame (and its 8 px inset) is never drawn.
        assert!(plain.iter().any(|m| m == "Bare=Bool(true)"), "{plain:?}");
        let bare = emitted_props(|| EmberField(FieldOptions { bare: true, multiline: true, ..Default::default() }));
        assert!(bare.iter().any(|m| m == "Bare=Bool(true)"), "{bare:?}");
        assert!(bare.iter().any(|m| m == "FontSize=Float(14.0)"), "{bare:?}");
        // A named role keeps its own size: no explicit FontSize overrides it.
        let mono = emitted_props(|| EmberField(FieldOptions { type_role: Some(TypeRole::Mono), ..Default::default() }));
        assert!(!mono.iter().any(|m| m.starts_with("FontSize=")), "{mono:?}");
    }

    #[test]
    fn fr_l10_2_composers_inside_a_frame_pass_bare() {
        let channels = include_str!("channels.rs");
        let at = channels.find("placeholder: \"메시지 입력\"").expect("channel composer field");
        let opts = &channels[channels[..at].rfind("EmberField(FieldOptions {").expect("field")..at];
        assert!(opts.contains("bare: true"), "channel composer field is bare: {opts}");
        // The home and conversation composers already pass it (H2, SE10).
        assert!(include_str!("home.rs").contains("bare: true"));
        assert!(include_str!("conversation.rs").contains("bare: true"));
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
    fn fr_l10_choices_and_selects_carry_their_state_for_assistive_technology() {
        // compose-rust#473 (v0.0.9): the platform hears declared roles and states, so the style
        // switch says which style is pressed, the fan modes are radios and a select is named.
        let production = include_str!("components.rs").split("#[cfg(test)]").next().unwrap();
        let choice = production.split("pub fn ChoiceButton(").nth(1).unwrap();
        assert!(choice.contains("Choice::Toggle => Modifier::new().selected(chosen)"));
        assert!(choice.contains("Choice::Radio => Modifier::new().role(Role::RadioButton).checked(chosen)"));
        let select = production.split("fn select_field(").nth(1).unwrap();
        assert!(select.contains(".role(Role::DropdownList)"));
        assert!(select.contains(".expanded(open.get())"));
        assert!(select.contains("Some(n) => trigger.label(n)"));
        let chars = include_str!("characters_screen.rs");
        assert!(chars.contains("ChoiceButton(s.label(), Choice::Toggle, s == style"));
        for name in ["하네스", "역할", "스타일", "실루엣 고정"] {
            assert!(chars.contains(&format!("NamedSelect(\"{name}\", &labels[at]")), "{name}");
        }
        let monitor = include_str!("monitor.rs");
        assert!(monitor.contains("ChoiceButton(mode.label(), Choice::Radio, mode == FanMode::Auto"));
        for src in [chars, monitor] {
            assert!(!src.contains(concat!("TODO(compose-rust#", "473)")));
        }
    }

    #[test]
    fn fr_212_modal_scrim_is_the_mockups() {
        // compose-rust#384 (v0.0.10 `ModalScope::scrim`): every presentation takes the mockup's scrim.
        let src = include_str!("components.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("tests")];
        assert!(!production.contains(concat!("TODO(compose-rust#", "384)")));
        assert_eq!(MODAL_SCRIM, Color::argb(0x4D00_0000));
        let sheet = &production[production.find("pub fn EmberSheetSized").expect("sheet")..];
        let sheet = &sheet[..sheet.find("\n}\n").expect("end")];
        assert_eq!(sheet.matches(".scrim(Paint::Literal(MODAL_SCRIM))").count(), 3, "bottom sheet, full screen and dialog");
    }

    #[test]
    fn fr_l32_bottom_sheet_wraps_its_content_up_to_the_fraction() {
        // compose-rust#445: the bottom sheet takes a maximum height instead of a fixed one.
        let production = include_str!("components.rs").split("#[cfg(test)]").next().unwrap();
        let sized = production.split("pub fn EmberSheetSized(").nth(1).unwrap();
        let bottom = sized.split("if presentation == SheetPresentation::Bottom {").nth(1).unwrap();
        let bottom = bottom.split("} else if presentation == SheetPresentation::FullScreen {").next().unwrap();
        assert!(bottom.contains(".max_height_fraction(SHEET_BOTTOM_MAX_FRACTION)"));
        assert!(!bottom.contains(".height("));
        assert!(!production.contains(concat!("TODO(compose-rust#", "445)")));
        assert_eq!(SHEET_BOTTOM_MAX_FRACTION, 0.85);
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
    fn fr_l32_1_sheet_chrome_heights_match_the_header_and_footer() {
        // SheetHeader is SHEET_HEADER_H high and EmberSheetSized puts a 1 dp separator under it;
        // SheetFooter is a 1 dp line, SP[2], an H_CTL row and SP[4].
        assert_eq!(SHEET_HEADER_CHROME_H, 61.0);
        assert_eq!(SHEET_FOOTER_H, 65.0);
        let production = include_str!("components.rs").split("#[cfg(test)]").next().unwrap();
        let footer = production.split("pub fn SheetFooter(").nth(1).unwrap();
        let footer = &footer[..footer.find("\n}\n").unwrap()];
        assert!(footer.contains("height(1.0)") && footer.contains("height(SP[2])"));
        assert!(footer.contains("height(H_CTL)") && footer.contains("height(SP[4])"));
        assert!(sheet_wraps_content(390.0));
        assert!(!sheet_wraps_content(560.0));
        assert!(!sheet_wraps_content(1440.0));
        // A body sized with both chrome heights leaves the footer inside the sheet.
        for (w, h) in [(390.0, 844.0), (390.0, 640.0), (1440.0, 900.0)] {
            let body = sheet_body_max_height(w, h, false, SHEET_HEADER_CHROME_H + SHEET_FOOTER_H);
            assert!(body + SHEET_HEADER_CHROME_H + SHEET_FOOTER_H <= sheet_max_height(w, h, false) + 0.01, "{w}x{h}");
        }
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

    #[Composable]
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
    fn copy_button_says_whether_the_text_was_copied() {
        // compose-rust#471 (v0.0.7): a copy button writes through the toolkit clipboard.
        assert_eq!(copy_notice(true), "클립보드에 복사했습니다");
        assert_eq!(copy_notice(false), "복사하지 못했습니다");
        assert!(compose_rust::clipboard::set_text("ember").is_ok());
        for file in [include_str!("channel_owner.rs"), include_str!("add_computer.rs")] {
            let prod = file.split("#[cfg(test)]").next().unwrap();
            assert!(prod.contains("compose_rust::clipboard::set_text("));
            assert!(!prod.contains("TODO(compose-rust#471)"));
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
                // AppButton, IconButton, PickChip, and ChoiceButton (FR-L10, compose-rust#473).
                "components.rs" => 4,
                "agents.rs" => 2,
                "files.rs" => 1,
                "home.rs" => 0,
                "launcher.rs" => 1,
                "project.rs" => 1,
                "routines.rs" => 2,
                // FocusOverlay and the StashPill accessible-name overlay (FR-G6, #350).
                "vcs.rs" => 2,
                // The accessible-name overlay of the channel + button (#499); it keeps its
                // visible text and `accessible_name` until #425's content slot is checked.
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
    fn fr_l10_1_fields_keep_words_and_wrap_labels_break_long_tokens() {
        // compose-rust#443 and #444 (v0.0.8): no workaround markers are left, the field keeps
        // Korean words whole and breaks a long token, and WrapLabel lets a token break.
        let production = include_str!("components.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains(concat!("TODO(compose-rust#", "443)")));
        assert!(!production.contains(concat!("TODO(compose-rust#", "444)")));
        // Framed and bare fields both build their TextField in FieldNode (#844).
        let field = production.split("pub fn EmberField(").nth(1).unwrap();
        let field = field.split("\nfn ").next().unwrap();
        assert!(field.contains("BareField(inner)"), "a framed field is a bare one in Ember's frame (#878)");
        let bare = production.split("fn BareField(").nth(1).unwrap();
        assert!(bare.split("\nfn ").next().unwrap().contains("FieldNode(o, inner)"));
        let node = production.split("fn FieldNode(").nth(1).unwrap();
        let node = node.split("\nfn ").next().unwrap().split("\npub fn ").next().unwrap();
        assert!(node.contains(".word_break(WordBreak::KeepAll)"));
        assert!(node.contains(".overflow_wrap(OverflowWrap::Anywhere)"));
        let text = production.split("fn scale_text(").nth(1).unwrap();
        assert!(text.contains("if wrap_anywhere {\n        t = t.overflow_wrap(OverflowWrap::Anywhere);"));
        for screen in [include_str!("monitor.rs"), include_str!("routines.rs")] {
            assert!(!screen.contains(concat!("TODO(compose-rust#", "443)")));
        }
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
    use compose_rust::{Composable, EventPayload, Host, PropertyKind, Recomposer};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CLICKS: AtomicUsize = AtomicUsize::new(0);

    #[Composable]
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
