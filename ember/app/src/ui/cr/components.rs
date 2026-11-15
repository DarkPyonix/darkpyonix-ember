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
    Alignment, ButtonVariant, Color, ColorRole, IconRole,
    KeyEvent, OverflowWrap, Paint, ShapeRole, TextAlign, TextOverflow, TypeRole, WordBreak,
};

use super::theme::*;
use super::theme::Palette;

// ---------------------------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------------------------

// The prose and mono font stacks are the toolkit theme's: `ui::fonts::app_theme` registers the
// bundled Inter, Pretendard and JetBrains Mono faces and sets them for every `TypeRole`
// (compose-rust#374, Ember #216), so `Label` and `Txt` name no font of their own.

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

/// The press feedback every pressable sends (FR-L34.1, #250): the mockup's calm press, a scale
/// to 0.975 and a tint at 0.07 in light and 0.10 in dark (`core.js` `pressStart`, `style.css`
/// `.ripple-clip`). The toolkit's own defaults are the same numbers today
/// (`ModifierChain.kt` `PRESS_SCALE`, `PRESS_TINT_LIGHT`, `PRESS_TINT_DARK`); Ember sends them
/// so the values are the mockup's whatever the toolkit's defaults become, and so the dark tint
/// follows Ember's scheme. The renderer draws the tint and the keyboard focus ring in the shape
/// the same node carries (`pressedShape`), so a pressable puts its `corner_radius`,
/// `corner_radii` or `shape_role` on the node that takes the click (FR-L34.2, #251).
pub fn pressable(modifier: Modifier) -> Modifier {
    modifier.press_feedback(PRESS_SCALE, ember_theme().press_alpha())
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
    cu::Box().modifier(pressable(modifier)).alignment(alignment).on_click(move || on_click()).content(content);
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

/// A [`NamedClickable`] that cannot act now: the same painted box, named and a disabled button
/// to assistive technology, with no click handler and so no press feedback.
pub fn NamedDisabled(label: &'static str, modifier: Modifier, alignment: Alignment, content: impl FnOnce()) {
    cu::Box().modifier(modifier.label(label).role(Role::Button).disabled(true)).alignment(alignment).content(content);
}

// TODO(compose-rust#831): a container
// takes its name from `Modifier::label` only, which keeps every distinct name for the life of the
// program and drops names past 4096 (`compose-rust/src/ui/mod.rs`, `MAX_LABELS`), and a `Button`
// with content insets its children by the design system's 16 dp with no way to change it
// (`HostButton.kt`, `labelPadding`). So a painted box whose name is built from data (a path, a
// commit subject, a profile name) keeps a clear `Button` over it for the name, the focus and the
// click. When a container takes an owned name, move every caller onto [`Clickable`] with that
// name and delete this.
/// The control of a painted box whose accessible name is built from data: a clear toolkit
/// `Button` exactly over its `Box` parent. It takes the parent's bounds without changing them
/// (`Modifier::match_parent_size`, compose-rust#431), so the box keeps the size of what it
/// paints. `expanded` says whether the menu it opens is open (FR-L10.4). A name that is one of a
/// fixed set never comes here: it goes on the painted box itself through [`NamedClickable`].
pub fn DataNamedOverlay(name: &str, enabled: bool, expanded: Option<bool>, on_click: impl FnMut() + 'static) {
    let m = Modifier::new().match_parent_size();
    let m = match expanded {
        Some(open) => m.expanded(open),
        None => m,
    };
    cu::Button(name)
        .accessible_name(name)
        .enabled(enabled)
        .variant(ButtonVariant::Text)
        .color(Paint::Literal(Color::argb(0)))
        .modifier(m)
        .on_click(on_click);
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

// ---------------------------------------------------------------------------------------------
// Anchored popover menus (FR-L10.4, #888)
// ---------------------------------------------------------------------------------------------

/// The check on a menu's current row (`I("check")`).
pub const ICON_CHECK_SVG: &str = include_str!("../../../assets/nav/check.svg");

/// The mockup's two option popovers. `Mode` is the permission picker (`.mp-pop.mp-mode`,
/// `core.js` `mdDraw`); `Drop` is the session top bar's IDE and web page menus (`.sdrop`,
/// `session.css` 219-227).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopKind {
    Mode,
    Drop,
}

/// The measures of a [`PopKind`], all in dp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopMetrics {
    /// The box's width before the window caps it.
    pub width: f32,
    /// The box's own padding (`.sdrop{padding:6px}`; the mode box has none).
    pub box_pad: f32,
    /// The head's top, side and bottom padding.
    pub head_pad: (f32, f32, f32),
    /// The list's top, side and bottom padding (`.mp-body{padding:4px 8px 8px}`).
    pub list_pad: (f32, f32, f32),
    /// A row's vertical and horizontal padding.
    pub row_pad: (f32, f32),
    /// The gap between a row's icon, text and check.
    pub row_gap: f32,
    /// The lowest a row is (`.md-row{min-height:56px}`; a drop row has no minimum).
    pub row_min_h: f32,
    pub row_radius: f32,
    /// The gap between the control and the box.
    pub offset: f32,
}

pub fn pop_metrics(kind: PopKind) -> PopMetrics {
    match kind {
        // `.mp-pop.mp-mode{width:340px}`, `.md-head{padding:12px 16px 4px}`,
        // `.md-row{padding:8px 12px;gap:12px;border-radius:var(--r-sm)}`, 8 below the control.
        PopKind::Mode => PopMetrics {
            width: 340.0,
            box_pad: 0.0,
            head_pad: (12.0, 16.0, 4.0),
            list_pad: (4.0, 8.0, 8.0),
            row_pad: (8.0, 12.0),
            row_gap: 12.0,
            row_min_h: 56.0,
            row_radius: CORNER_SM,
            offset: 8.0,
        },
        // `.sdrop{width:360px;padding:6px;top:calc(100% + 6px)}`, `.sdrop-h{padding:8px 10px}`,
        // `.sdrop-i{padding:8px 10px;gap:10px;border-radius:var(--r)}`. A row has no minimum in
        // the CSS, but its title (`b`, 14 at the body's 1.5 line height, 21) over the 12
        // `.small` line (18) with 8 above and below is 55 high (review r8: ours drew 50).
        PopKind::Drop => PopMetrics {
            width: 360.0,
            box_pad: 6.0,
            head_pad: (8.0, 10.0, 8.0),
            list_pad: (0.0, 0.0, 0.0),
            row_pad: (8.0, 10.0),
            row_gap: 10.0,
            row_min_h: DROP_ROW_MIN_H,
            row_radius: CORNER_MD,
            offset: 6.0,
        },
    }
}

/// The fill of the current row: `sel` in the mode picker (`.md-row.on`), `hover` in a drop menu
/// (`.sdrop-i.on`).
pub fn pop_chosen_fill(kind: PopKind, p: &Palette) -> Color {
    match kind {
        PopKind::Mode => p.sel,
        PopKind::Drop => p.hover,
    }
}

/// The height of a drop menu row with a title and a description line (`.sdrop-i`: 8 + 21 + 18 + 8).
pub const DROP_ROW_MIN_H: f32 = 55.0;

/// How far the toolkit keeps a popover from the window's edges: `popoverWindowMargin`, the
/// theme's `SpaceRole.Sm` (compose-rust `renderer/desktop/src/renderer/HostPopover.kt`
/// `popoverWindowMargin`, `shiftInside`).
pub const TOOLKIT_POPOVER_MARGIN: f32 = 8.0;

/// The phone drop panel's side gap (`.sdrop{left:12px;right:12px}` at 560 and below).
pub const DROP_PHONE_SIDE: f32 = 12.0;

/// The sideways shift a drop menu takes at 560 and below, so the panel sits 12 from each side.
///
/// The toolkit places a popover by its anchor and then slides it inside the window, never
/// nearer than its own 8 dp margin (`HostPopover.kt` `shiftInside`: `x.coerceAtMost(window -
/// margin - width).coerceAtLeast(margin)`), and the Host cannot name a margin or a coordinate.
/// The panel is the window less 24 wide and lined up with the end of a control that ends at
/// least 16 from the window's right edge (every drop menu's control does), so the toolkit puts
/// its start at 8. `Modifier::offset` then draws it 4 further right, at 12..window-12, without
/// moving where the toolkit placed it. The 4 dp strip at the right of the drawn box is outside
/// the popup's own bounds, so a press there counts as a press outside (it is inside the box's 6
/// dp padding, where nothing is pressable). Wider windows need none: the panel stays on the
/// control's end, well inside the margin.
pub fn pop_phone_shift(kind: PopKind, window_width_dp: f32) -> f32 {
    let phone = window_width_dp > 0.0 && window_width_dp <= 560.0;
    if kind == PopKind::Drop && phone { DROP_PHONE_SIDE - TOOLKIT_POPOVER_MARGIN } else { 0.0 }
}

/// How a menu is shown at a window width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopPresentation {
    Popover,
    Sheet,
}

/// At 560 and below the mode picker is a bottom sheet (`style.css` `@media (max-width:560px)
/// .mp-pop`); a drop menu stays a panel under the bar (`session.css` `.sdrop{position:fixed;
/// left:12px;right:12px}`). A window that has not reported its size (0) is not a phone.
pub fn pop_presentation(kind: PopKind, window_width_dp: f32) -> PopPresentation {
    let phone = window_width_dp > 0.0 && window_width_dp <= 560.0;
    if phone && kind == PopKind::Mode { PopPresentation::Sheet } else { PopPresentation::Popover }
}

/// The box's width: the kind's, never more than the window less 12 at each side
/// (`.sdrop{max-width:calc(100vw - 24px)}`), and a drop menu takes all of that at 560 and below.
pub fn pop_width(kind: PopKind, window_width_dp: f32) -> f32 {
    let want = pop_metrics(kind).width;
    if window_width_dp <= 0.0 {
        return want;
    }
    let room = (window_width_dp - 24.0).max(0.0);
    if kind == PopKind::Drop && window_width_dp <= 560.0 { room } else { want.min(room) }
}

/// One option of a [`PopoverMenu`]: an icon (in `tint`, else `text-2`, red on a danger row),
/// the title, a description, a pill after the title, and whether it is the current one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PopRow {
    pub icon: Option<&'static str>,
    pub tint: Option<Color>,
    pub title: String,
    pub description: Option<String>,
    pub badge: Option<(String, PillTone)>,
    pub chosen: bool,
    pub danger: bool,
    /// The title is an address, set in the mono face at 12 (`.sdrop-i .mono`).
    pub mono: bool,
}

/// What a [`PopoverMenu`] shows.
pub struct PopMenu<'a> {
    /// The box's name for assistive technology (the mockup's `aria-label`); also the sheet's
    /// title when there is no head. `&'static str` for the reason [`NamedClickable`] gives.
    pub name: &'static str,
    pub kind: PopKind,
    /// The head line; empty draws none.
    pub head: &'a str,
    pub sub: Option<&'a str>,
    pub rows: &'a [PopRow],
    /// The edge of the control the box lines up with.
    pub align: compose_rust::PopoverAlign,
}

/// An option menu hanging from the control that opens it (FR-L10.4, #888): the toolkit
/// `Popover` (compose-rust v0.0.10) with `anchor` as its first child, below the control and
/// lined up with `menu.align`. Escape and a press outside call `on_dismiss`; the renderer moves
/// the box inside the window and gives focus back to the control when it closes
/// (`HostPopover.kt` `PopoverContent`). A row tap calls `on_dismiss` and then `on_choose(i)`.
/// `footer` is drawn under a line below the rows (`.sdrop-f`). At 560 and below a mode menu is a
/// bottom sheet with the same rows ([`pop_presentation`]).
/// Not built: arrow keys between rows and focus on the current row when the menu opens
/// (TODO(compose-rust#810): a `Popover` moves no focus between its rows; compose-rust#504 did
/// it for the toolkit `Menu` only. Add the keys when #810 is in the pin).
pub fn PopoverMenu(
    open: bool,
    menu: PopMenu<'_>,
    on_dismiss: impl FnMut() + Clone + 'static,
    on_choose: impl FnMut(usize) + Clone + 'static,
    anchor: impl FnOnce(),
    footer: Option<&mut dyn FnMut()>,
) {
    PopoverMenuWith(open, menu, PopExtra::default(), on_dismiss, on_choose, anchor, footer);
}

/// What a caller may ask of a [`PopoverMenu`] beyond its rows. The Home composer's pickers use
/// these (#573, #574, #849, review r8); the default asks for nothing, which is [`PopoverMenu`].
#[derive(Clone, Default)]
pub struct PopExtra {
    /// The gap between the control and the box when it is not the kind's own: a chip inside a
    /// composer opens its menu 8 from the composer, not from the chip (`core.js` `place()`).
    pub offset: Option<f32>,
    /// A row tap only reports the row and the caller closes the menu. The permission picker
    /// needs it: the first tap on 바이패스 arms the row and the menu stays open (FR-L16).
    pub keep_open: bool,
    /// At 560 and below the sheet has the menu's own head (`.md-head`, 12/600 `muted`) and no
    /// title bar or close button, as the mockup's `.mp-pop` bottom sheet.
    pub plain_sheet: bool,
    /// The pointer came over row `i`.
    pub on_hover: Option<std::rc::Rc<dyn Fn(usize)>>,
    /// The weight the control takes in its row. A weight is read from the row's direct child,
    /// which is the popover node, and the renderer hands it to the anchor (`HostPopover.kt`
    /// `parentModifier`), so a control that shares a row by weight says it here.
    pub anchor_weight: Option<f32>,
}

/// [`PopoverMenu`] with the options of [`PopExtra`].
pub fn PopoverMenuWith(
    open: bool,
    menu: PopMenu<'_>,
    extra: PopExtra,
    on_dismiss: impl FnMut() + Clone + 'static,
    on_choose: impl FnMut(usize) + Clone + 'static,
    anchor: impl FnOnce(),
    footer: Option<&mut dyn FnMut()>,
) {
    let theme = ember_theme();
    let p = theme.palette;
    let window = crate::ui::window_size::current_window_size();
    let m = pop_metrics(menu.kind);
    match pop_presentation(menu.kind, window.width_dp) {
        PopPresentation::Sheet => {
            // One node, so the sheet is not a separate child that the caller's row spaces.
            cu::Box().content(|| {
                anchor();
                let title = if extra.plain_sheet { "" } else if menu.head.is_empty() { menu.name } else { menu.head };
                EmberSheet(title, open, on_dismiss.clone(), None, || {
                    if extra.plain_sheet {
                        PopPlainSheetBody(&menu, &m, &extra, on_dismiss, on_choose);
                        return;
                    }
                    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[1])).content(|| {
                        if let Some(sub) = menu.sub {
                            cu::Row().modifier(Modifier::new().fill_max_width().padding(SP[1])).content(|| {
                                Label(sub, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            });
                        }
                        PopRows(&menu, &m, &extra, on_dismiss, on_choose);
                        if let Some(f) = footer {
                            PopFooter(f);
                        }
                    });
                });
            });
        }
        PopPresentation::Popover => {
            let width = pop_width(menu.kind, window.width_dp);
            let shift = pop_phone_shift(menu.kind, window.width_dp);
            let placed = if shift != 0.0 { Modifier::new().offset(shift, 0.0) } else { Modifier::new() };
            cu::Popover(open)
                .placement(compose_rust::PopoverPlacement::Bottom)
                .align(menu.align)
                .offset(extra.offset.unwrap_or(m.offset))
                .on_dismiss(on_dismiss.clone())
                .modifier(pop_anchor_weight(extra.anchor_weight,
                    // `elevation(0)`: the box casts its own shadow, so the toolkit's menu
                    // elevation is turned off. Without it the renderer casts a second shadow
                    // from the inner surface, which shows through the box as a grey band.
                    with_shadows(placed, theme.shadow_lg())
                        .elevation(0.0)
                        .width(width)
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_LG)
                        .padding(m.box_pad)
                        .role(Role::Menu)
                        .label(menu.name),
                ))
                .anchor(anchor)
                .content(|| {
                    cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
                        if !menu.head.is_empty() {
                            PopHead(menu.kind, menu.head, menu.sub, &m);
                        }
                        cu::Spacer().modifier(Modifier::new().height(m.list_pad.0));
                        cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
                            cu::Spacer().modifier(Modifier::new().width(m.list_pad.1));
                            cu::Column().modifier(Modifier::new().width(0.0).weight(1.0)).content(|| {
                                PopRows(&menu, &m, &extra, on_dismiss, on_choose);
                            });
                            cu::Spacer().modifier(Modifier::new().width(m.list_pad.1));
                        });
                        cu::Spacer().modifier(Modifier::new().height(m.list_pad.2));
                        if let Some(f) = footer {
                            PopFooter(f);
                        }
                    });
                });
        }
    }
}

/// The head: `.md-head` (12 `muted` 600) for the mode picker, `.sdrop-h` (a 14/600 title over a
/// 12 `muted` subtitle) for a drop menu.
fn PopHead(kind: PopKind, head: &str, sub: Option<&str>, m: &PopMetrics) {
    let p = ember_theme().palette;
    cu::Spacer().modifier(Modifier::new().height(m.head_pad.0));
    cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Spacer().modifier(Modifier::new().width(m.head_pad.1));
        cu::Column().modifier(Modifier::new().width(0.0).weight(1.0)).content(|| match kind {
            PopKind::Mode => Label(head, Fs::Xs, Weight::Semibold, Some(paint(p.muted)), Some(1)),
            PopKind::Drop => {
                Label(head, Fs::Md, Weight::Semibold, Some(paint(p.text)), Some(1));
                if let Some(sub) = sub {
                    Label(sub, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
            }
        });
        cu::Spacer().modifier(Modifier::new().width(m.head_pad.1));
    });
    cu::Spacer().modifier(Modifier::new().height(m.head_pad.2));
}

fn PopRows(
    menu: &PopMenu<'_>,
    m: &PopMetrics,
    extra: &PopExtra,
    on_dismiss: impl FnMut() + Clone + 'static,
    on_choose: impl FnMut(usize) + Clone + 'static,
) {
    for (i, r) in menu.rows.iter().enumerate() {
        let (mut dismiss, mut choose) = (on_dismiss.clone(), on_choose.clone());
        let keep_open = extra.keep_open;
        let row = || {
            PopRowView(r, menu.kind, m, move || {
                if !keep_open {
                    dismiss();
                }
                choose(i);
            })
        };
        match extra.on_hover.clone() {
            // The row's own box is a button; the pointer subscription sits on a box around it,
            // which hears the pointer come over the row and takes nothing from the press.
            Some(hover) => cu::Box()
                .modifier(Modifier::new().fill_max_width())
                .on_pointer(compose_rust::PointerKinds::ENTER, move |_| hover(i))
                .content(row),
            None => row(),
        }
    }
}

/// `modifier` with the weight of [`PopExtra::anchor_weight`]. The weight is read by the row
/// the control is in and handed to the anchor; the floating box itself takes none.
pub fn pop_anchor_weight(weight: Option<f32>, modifier: Modifier) -> Modifier {
    match weight {
        Some(w) => modifier.weight(w),
        None => modifier,
    }
}

/// The body of a [`PopExtra::plain_sheet`]: the head and the padded rows of the popover box,
/// with no title bar above it. The column itself says nothing to assistive technology: named
/// as a menu, the macOS tree turned it into one static text (`value=menu`) and dropped every
/// row under it at 390 (review st, A FAIL 1), so the rows sit straight in the sheet as the old
/// option sheet's did, each a named menu item that says whether it is the current one.
fn PopPlainSheetBody(
    menu: &PopMenu<'_>,
    m: &PopMetrics,
    extra: &PopExtra,
    on_dismiss: impl FnMut() + Clone + 'static,
    on_choose: impl FnMut(usize) + Clone + 'static,
) {
    cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
        if !menu.head.is_empty() {
            PopHead(menu.kind, menu.head, menu.sub, m);
        }
        cu::Spacer().modifier(Modifier::new().height(m.list_pad.0));
        cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Spacer().modifier(Modifier::new().width(m.list_pad.1));
            cu::Column().modifier(Modifier::new().width(0.0).weight(1.0)).content(|| {
                PopRows(menu, m, extra, on_dismiss, on_choose);
            });
            cu::Spacer().modifier(Modifier::new().width(m.list_pad.1));
        });
        cu::Spacer().modifier(Modifier::new().height(m.list_pad.2));
    });
}

/// One row (`.md-row`, `.sdrop-i`): a menu item to assistive technology, named by its text,
/// that says whether it is the current one.
fn PopRowView(r: &PopRow, kind: PopKind, m: &PopMetrics, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let fill = if r.chosen { paint(pop_chosen_fill(kind, p)) } else { Paint::Literal(Color::argb(0)) };
    let title_c = if r.danger { p.red.strong } else { p.text };
    let icon_c = r.tint.unwrap_or(if r.danger { p.red.strong } else { p.text_2 });
    Clickable(
        Modifier::new()
            .fill_max_width()
            .background(fill)
            .corner_radius(m.row_radius)
            .role(Role::MenuItem)
            .selected(r.chosen)
            .click_action(true),
        Alignment::CenterStart,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                // The side padding, and the row's minimum height.
                cu::Spacer().modifier(Modifier::new().width(m.row_pad.1).height(m.row_min_h));
                if let Some(svg) = r.icon {
                    super::shell::NavIcon(svg, icon_c.to_argb(), 16.0);
                    cu::Spacer().modifier(Modifier::new().width(m.row_gap));
                }
                // Zero wide and weighted, so a long description wraps instead of widening the row.
                cu::Column().spacing(2.0).modifier(Modifier::new().width(0.0).weight(1.0)).content(|| {
                    cu::Spacer().modifier(Modifier::new().height(m.row_pad.0 - 2.0));
                    cu::Row().spacing(6.0).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                        // With a badge the title is zero wide and weighted, so the badge keeps its
                        // own width at the row's end (the session model menu's one row squeezed
                        // 준비 중 to 준비... before), and the title wraps to a second line rather
                        // than being cut (review st, A FAIL 2: "실행 중인 세션의 모델은 바꿀 수..."
                        // in the 340 wide menu).
                        let title_mod = if r.badge.is_some() { Modifier::new().width(0.0).weight(1.0) } else { Modifier::new() };
                        cu::Box().modifier(title_mod).content(|| {
                            if r.mono {
                                MonoLabel(&r.title, Fs::Xs, Weight::Regular, Some(paint(title_c)));
                            } else {
                                Label(&r.title, Fs::Md, Weight::Semibold, Some(paint(title_c)), Some(pop_title_lines(r)));
                            }
                        });
                        if let Some((text, tone)) = &r.badge {
                            Pill(text, *tone, true);
                        }
                    });
                    if let Some(d) = &r.description {
                        Label(d, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                    }
                    cu::Spacer().modifier(Modifier::new().height(m.row_pad.0 - 2.0));
                });
                if r.chosen {
                    cu::Spacer().modifier(Modifier::new().width(m.row_gap));
                    super::shell::NavIcon(ICON_CHECK_SVG, p.text.to_argb(), 16.0);
                }
                cu::Spacer().modifier(Modifier::new().width(m.row_pad.1));
            });
        },
    );
}

/// How many lines a row's title may take: two beside a badge, where the title has only what the
/// badge leaves, one otherwise.
pub fn pop_title_lines(r: &PopRow) -> u32 {
    if r.badge.is_some() { 2 } else { 1 }
}

/// `.sdrop-f`: 4 above, a line, then the caller's controls padded 10.
fn PopFooter(footer: &mut dyn FnMut()) {
    let p = ember_theme().palette;
    cu::Spacer().modifier(Modifier::new().height(4.0));
    cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width().padding(10.0)).content(|| footer());
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
    scale_text(text, fs, weight, color, max_lines, false, None);
}

/// A [`Label`] centred on its own lines, for a name under a round tile whose second line would
/// otherwise start at the left (the files share sheet's receivers, `.fl-share-tile`
/// `text-align:center`).
pub fn CentredLabel(text: &str, fs: Fs, weight: Weight, color: Option<Paint>, max_lines: Option<u32>) {
    scale_text(text, fs, weight, color, max_lines, false, Some(TextAlign::Center));
}

/// A [`Label`] whose token too long for its line breaks between its letters, the mockup's
/// `word-break:keep-all; overflow-wrap:anywhere` (compose-rust#443, v0.0.8). Such a text is as
/// narrow as its widest letter in a weighted row, so use it where the text should wrap rather
/// than widen its column.
pub fn WrapLabel(text: &str, fs: Fs, weight: Weight, color: Option<Paint>, max_lines: Option<u32>) {
    scale_text(text, fs, weight, color, max_lines, true, None);
}

fn scale_text(
    text: &str,
    fs: Fs,
    weight: Weight,
    color: Option<Paint>,
    max_lines: Option<u32>,
    wrap_anywhere: bool,
    align: Option<TextAlign>,
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
    if let Some(a) = align {
        t = t.text_align(a);
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
        // The theme's monospace face: the bundled JetBrains Mono stack (`ui::fonts::mono_font`).
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
    /// A single-line field whose hint can be a long value (a remembered path or model): Ember
    /// draws the hint itself on one line with an ellipsis, under the field while it is empty,
    /// because the toolkit's placeholder always wraps (FR-L10.1, #878). Ignored when multiline.
    pub one_line_hint: bool,
    pub height: Option<f32>,
    pub corner_radius: Option<f32>,
    pub background: Option<Paint>,
    pub border_width: Option<f32>,
    pub border_color: Option<Paint>,
    pub type_role: Option<TypeRole>,
    /// The line pitch in dp when it is not the type step's own: the vcs textareas reset it to
    /// the font's normal one (`.field{font:...}` without a line height).
    pub line_height: Option<f32>,
    pub on_value_change: Option<Box<dyn FnMut(String)>>,
    pub on_submit: Option<Box<dyn FnMut(String)>>,
    pub on_focus_gained: Option<Box<dyn FnMut()>>,
    pub on_focus_lost: Option<Box<dyn FnMut()>>,
    pub on_key_down: Option<Box<dyn FnMut(KeyEvent)>>,
    /// Binds the field to a focus handle, so `handle.request()` puts the caret in it
    /// (compose-rust#410, `TextField::focus_handle`).
    pub focus: Option<cu::FocusHandle>,
    /// The field's name for assistive technology when the placeholder is an example rather than
    /// a name (the branch menu's `feat/새-브랜치` is "새 브랜치 이름"). `&'static str` for the
    /// reason [`NamedClickable`] gives.
    pub label: Option<&'static str>,
}

/// The type step of a field's text and placeholder when no role is named (style.css `.field`).
pub const FIELD_FONT: Fs = Fs::Md;

/// The scale step a field's text and placeholder are set in: [`FIELD_FONT`], or the step Ember
/// gives the named role ([`type_role_style`], `Label` 13 px), never the toolkit's own ladder.
pub fn field_fs(role: Option<TypeRole>) -> Fs {
    role.map_or(FIELD_FONT, |r| type_role_style(r).0)
}

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
    use compose_rust::runtime::{mutable_state_of, remember};
    let mut o = o;
    let theme = ember_theme();
    let p = theme.palette;
    let multiline = o.multiline;
    let height = o.height.or(if multiline { None } else { Some(H_BTN) });
    // `.field:focus{border-color:var(--ink)}` (FR-L10.2, #878): the frame is this row, so the
    // field's focus events redraw its border; the caller's own handlers still run.
    let focused = remember(|| mutable_state_of(false));
    let (gained, lost) = (focused.clone(), focused.clone());
    let (mut user_gained, mut user_lost) = (o.on_focus_gained.take(), o.on_focus_lost.take());
    o.on_focus_gained = Some(Box::new(move || {
        gained.set(true);
        if let Some(f) = user_gained.as_mut() {
            f();
        }
    }));
    o.on_focus_lost = Some(Box::new(move || {
        lost.set(false);
        if let Some(f) = user_lost.as_mut() {
            f();
        }
    }));
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
    let border_color = if focused.get() { Some(paint(p.ink)) } else { o.border_color.take() };
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
    // A multiline field clips its text to itself: the toolkit's bare field clips only to a
    // corner the node names (`HostTextField.kt`), so a line scrolled up showed in the top inset
    // (review r5, field-typed-1440; FR-L10.2).
    let inner = if multiline && o.height.is_some() {
        Modifier::new().weight(1.0).fill_max_height().clip(true)
    } else if multiline {
        Modifier::new().weight(1.0).clip(true)
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
            if o.one_line_hint && !multiline {
                OneLineHintField(o);
            } else {
                FieldNode(o, inner);
            }
            if side > 0.0 {
                cu::Spacer().modifier(Modifier::new().width(side));
            }
        });
}

/// A single-line field with its hint drawn by Ember: a one-line `muted` label that ends in an
/// ellipsis, under the field, while the field is empty (FR-L10.1, #878). The toolkit draws its
/// placeholder with no line limit, so a long path wrapped onto a second line the 40 px field
/// clipped. TODO(compose-rust#786, after v0.0.10): a one-line placeholder with an ellipsis on
/// `TextField` lets the toolkit draw it again; this function goes then.
fn OneLineHintField(o: FieldOptions) {
    use compose_rust::runtime::{mutable_state_of, remember};
    let mut o = o;
    let p = ember_theme().palette;
    let starts_empty = o.text.as_deref().map_or(true, str::is_empty);
    let empty = remember(|| mutable_state_of(starts_empty));
    let hint = std::mem::take(&mut o.placeholder);
    let mut user = o.on_value_change.take();
    let typed = empty.clone();
    o.on_value_change = Some(Box::new(move |t: String| {
        typed.set(t.is_empty());
        if let Some(f) = user.as_mut() {
            f(t);
        }
    }));
    let show = empty.get();
    // The desktop renderer gives a weighted child at least its narrowest width, and a one-line
    // label reports its whole line there: the hint took 708 of the 696 px share (r5 captures,
    // 1440 and 390), ran under the trailing 12 px inset to the frame and showed no ellipsis.
    // A zero width makes the column's narrowest width 0, so it takes its share; the box inside
    // fills it and clips what is left.
    cu::Column().modifier(Modifier::new().weight(1.0).width(0.0)).content(move || {
        cu::Box()
            .alignment(Alignment::CenterStart)
            .modifier(Modifier::new().fill_max_width().clip(true))
            .content(move || {
                if show && !hint.is_empty() {
                    // `WrapLabel` so a path with no space ellipsises inside the box instead of
                    // asking for its whole width.
                    WrapLabel(&hint, FIELD_FONT, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
                FieldNode(o, Modifier::new().fill_max_width());
            });
    });
}

/// The toolkit field with every option but the frame, which `modifier` carries.
fn FieldNode(o: FieldOptions, modifier: Modifier) {
    // compose-rust#444 (v0.0.8): the placeholder and the typed text break Korean only at spaces,
    // and a long token such as a path or a URL breaks inside itself rather than clipping.
    let modifier = match o.label {
        Some(name) => modifier.label(name),
        None => modifier,
    };
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
    // FR-L10.2 (#844): the text and the placeholder take Ember's scale step, 14 px
    // (style.css `.field`) or the named role's step ([`field_fs`]). The role alone is not
    // enough: the toolkit sizes it from the Cupertino ladder (`Label` 15 sp at -0.24, `Body` 17
    // at -0.41), so the 13 px compact search drew 15 and a 14 px field kept Body's -0.41
    // tracking. Size, line height and tracking are always sent; the role stays for its face.
    let fs = field_fs(o.type_role);
    if let Some(r) = o.type_role {
        f = f.type_role(r);
    }
    f = f.font_size(fs.size()).line_height(o.line_height.unwrap_or(fs.line_height())).letter_spacing(fs.letter_spacing());
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
    if let Some(h) = o.on_focus_gained {
        f = f.on_focus_gained(h);
    }
    if let Some(h) = o.on_focus_lost {
        f = f.on_focus_lost(h);
    }
    if let Some(h) = o.on_key_down {
        f = f.on_key_down(h);
    }
    if let Some(h) = &o.focus {
        f = f.focus_handle(h);
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

/// The fixed width of the name in [`NameThenPill`]: its own width (one dp more, since the
/// toolkit reports sizes rounded to whole dp) when the name, the gap and the pill (one dp more,
/// for the same rounding) all fit in the room. `None` when they do not fit, or until the
/// renderer has measured all three: the name then takes what the pill leaves.
///
/// The name never gets a fixed width that leaves the pill less than its own: a row measures its
/// unweighted children in order and the last one gets what is left, so a pill behind a name one
/// gap too wide was cut, reported its cut width, and the name grew by that much on the next
/// pass until the pill was an empty blob (review r8 fix round, item 5).
pub fn name_then_pill_width(room: f32, natural: f32, pill: f32, gap: f32) -> Option<f32> {
    if room <= 0.0 || natural <= 0.0 || pill <= 0.0 {
        return None;
    }
    let own = natural + 1.0;
    (own + gap + pill + 1.0 <= room).then_some(own)
}

/// A one-line name with a pill right after it, taking the rest of a row (`weight(1)`): a short
/// name keeps its own width and the pill follows it, a long one is cut with an ellipsis so the
/// pill and whatever follows the row stay on screen (CSS `flex:0 1 auto;min-width:0` on the
/// name and `flex:none` on the pill; `.detail .topbar` in the mockup). The pill never shrinks.
///
/// TODO(compose-rust#778, after v0.0.10): the measuring below goes when the pin has a weight that
/// leaves a child its own size.
///
/// The toolkit has no weight that leaves a child its own size (Compose `weight(fill = false)`)
/// and no maximum width: a weighted child fills its share, and unweighted ones are measured
/// first, in order, each with what the ones before it left (compose-rust
/// `renderer/desktop/src/renderer/RenderNode.kt` `WeightedStackPolicy`). So the row measures:
/// the room, the pill and the name's own width (a copy in clear ink, left out of the
/// accessibility tree) are reported by the renderer (`observe_size_every_dp`), and
/// [`name_then_pill_width`] picks one of two rows. When everything fits, the name has its own
/// width and a weighted spacer after the pill takes the rest. When it does not (or before the
/// first report), the name is the weighted child, so the unweighted pill is measured first at
/// its own width and the name is cut to the remainder. The gap is a spacer of its own, not row
/// spacing, so the trailing spacer costs no second gap.
pub fn NameThenPill(name: &str, fs: Fs, weight: Weight, color: Option<Paint>, gap: f32, pill: impl FnOnce()) {
    let room = compose_rust::runtime::remember_node_size();
    let natural = compose_rust::runtime::remember_node_size();
    let pill_size = compose_rust::runtime::remember_node_size();
    let width = name_then_pill_width(
        room.measured().width_dp,
        natural.measured().width_dp,
        pill_size.measured().width_dp,
        gap,
    );
    cu::Box()
        .alignment(Alignment::CenterStart)
        .modifier(Modifier::new().weight(1.0).width(0.0).observe_size_every_dp(room.token()))
        .content(|| {
            cu::Row().content(|| {
                cu::Box().modifier(Modifier::new().observe_size_every_dp(natural.token())).content(|| {
                    // The measuring copy: the name's own font, clear ink, out of the
                    // accessibility tree (a `hidden` container still exposes its text).
                    drop(
                        cu::Text(name)
                            .word_break(WordBreak::KeepAll)
                            .font_size(fs.size())
                            .line_height(fs.line_height())
                            .font_weight(weight.value())
                            .letter_spacing(fs.letter_spacing())
                            .color(Paint::Literal(Color::argb(0)))
                            .max_lines(1)
                            .modifier(Modifier::new().hidden(true)),
                    );
                });
            });
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                let name_box = match width {
                    Some(w) => Modifier::new().width(w),
                    None => Modifier::new().weight(1.0).width(0.0),
                };
                cu::Box().alignment(Alignment::CenterStart).modifier(name_box).content(|| {
                    Label(name, fs, weight, color, Some(1));
                });
                cu::Spacer().modifier(Modifier::new().width(gap));
                // No weight and no width: the pill is as wide as its content, always.
                cu::Box().modifier(Modifier::new().observe_size_every_dp(pill_size.token())).content(pill);
                if width.is_some() {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                }
            });
        });
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
        .modifier(pressable(
            Modifier::new()
                .fill_max_width_if(fill_max_width)
                .height(s.height)
                .background(paint(s.fill))
                .border(1.0, paint(s.border))
                .shape_role(ShapeRole::Full),
        ))
        .on_click(on_click);
    // With an icon the renderer draws the text beside the glyph and the button itself had no
    // name (review st, E: the branch menu's 만들기 was `AXButton desc=` with a separate text).
    if let Some(i) = icon {
        b = b.icon(i).accessible_name(text);
    }
    drop(b);
}

/// An [`AppButton`] that opens a menu: the same pill, and it says whether the menu is open
/// (`Modifier::expanded`, FR-L10.4). Its text is its accessible name.
pub fn AppMenuButton(
    text: &str,
    kind: ButtonKind,
    small: bool,
    enabled: bool,
    fill_max_width: bool,
    expanded: bool,
    on_click: impl FnMut() + 'static,
) {
    let theme = ember_theme();
    let s = button_style_for(kind, small, enabled, theme.palette);
    drop(
        cu::Button(text)
            .enabled(enabled)
            .variant(ButtonVariant::Text)
            .color(paint(s.text))
            .font_size(s.fs.size())
            .font_weight(s.weight.value())
            .modifier(pressable(
                Modifier::new()
                    .fill_max_width_if(fill_max_width)
                    .height(s.height)
                    .background(paint(s.fill))
                    .border(1.0, paint(s.border))
                    .shape_role(ShapeRole::Full)
                    .expanded(expanded),
            ))
            .on_click(on_click),
    );
}

/// A pill that is one choice of several, the mockup's `.seg button`: the chosen one is `ink`,
/// the others `surface` on a `line` border ([`choice_style`]). To assistive technology a
/// `Toggle` says it is pressed (`Modifier::selected`, the mockup's `aria-pressed`) and a `Radio`
/// is a radio button that is checked or not; compose-rust v0.0.9 hands both to the platform
/// (#473), and the visible label is its name, as a [`MenuRow`]'s is.
///
/// It is Ember's [`Clickable`] rather than the toolkit `Button`, because `.seg button` pads its
/// label 12 px each side and the toolkit button pads it by the design system's 16 with no way to
/// change it (review r5, FR-L10.2). A disabled choice takes no click and is drawn at 45 percent
/// (`vcs.css` `.btn[disabled]{opacity:.45}`); a chosen one keeps [`button_style_for`]'s muted fill.
pub fn ChoiceButton(
    text: &str,
    choice: Choice,
    chosen: bool,
    small: bool,
    enabled: bool,
    on_click: impl FnMut() + 'static,
) {
    let theme = ember_theme();
    let s = choice_style(chosen, small, enabled, theme.palette);
    let state = match choice {
        Choice::Toggle => Modifier::new().selected(chosen),
        Choice::Radio => Modifier::new().role(Role::RadioButton).checked(chosen),
    };
    let frame = state
        .height(s.height)
        .background(paint(s.fill))
        .border(1.0, paint(s.border))
        .shape_role(ShapeRole::Full);
    // The same 14/500 (13/500 small) label as AppButton (FR-L10.2, #844), on one line.
    let label = move || {
        cu::Row().alignment(Alignment::CenterStart).content(|| {
            cu::Spacer().modifier(Modifier::new().width(CHOICE_PAD_X));
            Label(text, s.fs, s.weight, Some(paint(s.text)), Some(1));
            cu::Spacer().modifier(Modifier::new().width(CHOICE_PAD_X));
        });
    };
    if enabled {
        Clickable(frame, Alignment::Center, on_click, label);
    } else {
        let frame = frame.disabled(true);
        let frame = if chosen { frame } else { frame.alpha(CHOICE_DISABLED_ALPHA) };
        cu::Box().modifier(frame).alignment(Alignment::Center).content(label);
    }
}

/// A choice's label inset at each end (`.seg button` padding 0 `--sp3`).
pub const CHOICE_PAD_X: f32 = 12.0;

/// How a disabled, unchosen choice is faded (`.btn[disabled]{opacity:.45}`).
pub const CHOICE_DISABLED_ALPHA: f32 = 0.45;

/// A group of [`ChoiceButton`]s, the mockup's `.seg`: 8 px apart along a line and between lines,
/// and a choice that does not fit starts the next line (`flex-wrap:wrap`, buttons `flex:none`)
/// instead of being squeezed (settings-sheet-390, review r5). The toolkit's `FlowRow` measures
/// each choice at its own width.
pub fn ChoiceRow(content: impl FnOnce()) {
    cu::FlowRow()
        .horizontal_spacing(SP[1])
        .vertical_spacing(SP[1])
        .modifier(Modifier::new().fill_max_width())
        .content(content);
}

/// A [`ChoiceRow`] as wide as its choices, for a group at the end of a setting row: a full-width
/// one took the whole row and drew its choices over the setting's title and hint
/// (agents-models, review r6). It still starts a new line when the choices do not fit.
pub fn ChoiceRowFit(content: impl FnOnce()) {
    cu::FlowRow().horizontal_spacing(SP[1]).vertical_spacing(SP[1]).content(content);
}

/// A [`ChoiceButton`]'s look, the mockup's `.seg button` (FR-L10.2, #878): the chosen one is
/// `Primary` (`ink`), the others `Default` (`surface` on a 1 px `line` border) with `text-2`
/// text, both with the 14/500 (13/500 small) label.
pub fn choice_style(chosen: bool, small: bool, enabled: bool, p: &Palette) -> ButtonStyle {
    if chosen {
        button_style_for(ButtonKind::Primary, small, enabled, p)
    } else {
        ButtonStyle { text: p.text_2, ..button_style_for(ButtonKind::Default, small, enabled, p) }
    }
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
        .modifier(pressable(ring))
        .on_click(on_click);
}

/// The phone's form of a primary button: a 40 px `ink` circle with the toolkit glyph in
/// `ink_text` and no border (the mockup hides a primary button's label at 560 px and below,
/// `.lbl{display:none}`). The files screen's 보내기 and the project screen's 새 팀원 use it; each
/// built the same button by hand, outside [`pressable`] (FR-L34.5, #250).
pub fn InkIconButton(icon: IconRole, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    cu::Button("")
        .icon(icon)
        .variant(ButtonVariant::Text)
        .color(paint(p.ink_text))
        .modifier(pressable(Modifier::new().size(H_BTN, H_BTN).background(paint(p.ink)).shape_role(ShapeRole::Full)))
        .on_click(on_click);
}

/// The corner of [`BackButton`]: the mockup's `.icon-btn.sm.backbtn` is `border-radius:50%`
/// with `corner-shape:squircle`, which renders as a rounded square (the 390 room and
/// conversation renders show its straight edges); the toolkit draws plain arcs until the
/// squircle lands (#673), so a 10 px arc stands in for it on the 32 px square.
pub const BACK_BTN_RADIUS: f32 = 10.0;

/// The back button of a phone detail view (`.icon-btn.sm.backbtn`): a 32 px rounded square on
/// `surface` with a 1 px `line` border and the 18 px `back` arrow in `text-2`, named 뒤로
/// (review SE13, CH8: the app drew a 40 px circle in the conversation and a "목록" text button
/// in a room).
pub fn BackButton(on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    NamedClickable(
        super::a11y::BACK,
        Modifier::new()
            .size(H_CTL, H_CTL)
            .background(paint(p.surface))
            .border(1.0, paint(p.line))
            .corner_radius(BACK_BTN_RADIUS),
        Alignment::Center,
        on_click,
        || super::shell::NavIcon(super::shell::ICON_BACK_SVG, p.text_2.to_argb(), 18.0),
    );
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
        .modifier(pressable(
            Modifier::new()
                .height(height)
                .background(paint(fill))
                .shape_role(ShapeRole::Full),
        ))
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
    // TODO(compose-rust, unfiled: Modifier has no per-side padding): the mockup's 16/20 padding
    // is asymmetric and `Modifier::padding` is uniform, so 16 is used on all sides unless the
    // caller passes one. The side spacers go when the toolkit takes a padding per side.
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
    InsetPanelSized(fill_max_width, false, content);
}

/// [`InsetPanel`] that can also fill the height its parent gives it, with its content centred
/// in that height (`.ag-acc{align-items:center}`): a tile in an `equal_height` row is as tall as
/// the tallest tile of the row, as a CSS grid stretches its cells (FR-L20a, #886).
pub fn InsetPanelSized(fill_max_width: bool, fill_max_height: bool, content: impl FnOnce()) {
    let theme = ember_theme();
    let mut m = Modifier::new().fill_max_width_if(fill_max_width);
    if fill_max_height {
        m = m.fill_max_height();
    }
    // TODO(compose-rust, unfiled: Modifier has no per-side padding): the mockup's 12/16 padding
    // is asymmetric; 12 on all sides and the 4 dp spacers below until the toolkit takes one per side.
    let panel = cu::Box().modifier(m.background(paint(theme.palette.surface_2)).corner_radius(CORNER_MD).padding(PAD_ROW_V));
    let panel = if fill_max_height { panel.alignment(Alignment::CenterStart) } else { panel };
    panel
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

/// Whether a sheet is the whole window at this width (FR-L32.1, FR-L33): the one boundary every
/// full-screen geometry asks, so a sheet's own width, height and strip agree with its presentation.
pub(crate) fn sheet_is_full_screen(window_width_dp: f32, monitor: bool) -> bool {
    sheet_presentation(window_width_dp, monitor) == SheetPresentation::FullScreen
}

/// The strip a full-screen sheet leaves above its header for the window's own buttons (FR-L33):
/// where the sheet is the whole window (the monitor presentation at 560 and below) it starts at
/// the window's top edge, under the macOS traffic lights, so it reserves the height they cover,
/// as the bottom-tab shell does above every screen (`safe_area::title_strip_height`). A bottom
/// sheet or a dialog sits clear of them and reserves nothing, and so does a zero report (macOS
/// full screen, FR-L33.2). [`EmberSheetSized`] sends it before the shared header; a sheet with no
/// shared title draws its own header and reserves the same strip through it (`vcs_sheet_top_inset`).
pub(crate) fn full_screen_sheet_top_inset(window_width_dp: f32, monitor: bool, area: crate::ui::safe_area::SafeArea) -> f32 {
    if sheet_is_full_screen(window_width_dp, monitor) {
        crate::ui::safe_area::title_strip_height(area, true)
    } else {
        0.0
    }
}

/// The sheet header and the separator under it (`SheetHeader` and `cu::Separator`), FR-L32.1.
pub(crate) const SHEET_HEADER_CHROME_H: f32 = SHEET_HEADER_H + 1.0;

/// [`SheetFooter`]: its 1 dp line, 12 above the buttons, the 32 dp buttons and 20 below (FR-L32.1).
pub(crate) const SHEET_FOOTER_H: f32 = 1.0 + SP[2] + H_CTL + SP[4];

/// The tallest a bottom sheet grows, as a share of the window (`.sheet` `max-height:85vh`).
pub(crate) const SHEET_BOTTOM_MAX_FRACTION: f32 = 0.85;

/// The tallest a sheet may be (`.sheet` in the mockup): 85 percent of the window for a bottom
/// sheet, the whole window for the full-screen monitor, and for a dialog the window less 32 dp,
/// or less the traffic lights' strip above and below where there is one ([`dialog_max_height`]).
pub(crate) fn sheet_max_height(window_width_dp: f32, window_height_dp: f32, monitor: bool) -> f32 {
    let h = window_height_dp.max(0.0);
    match sheet_presentation(window_width_dp, monitor) {
        SheetPresentation::Bottom => h * SHEET_BOTTOM_MAX_FRACTION,
        SheetPresentation::FullScreen => h,
        SheetPresentation::Dialog => dialog_max_height(h, crate::ui::safe_area::safe_area()),
    }
}

/// The tallest a centred dialog may be (FR-L33): the window less 16 dp above and below, and,
/// where the window's own buttons cover a top strip (the macOS traffic lights), less that strip
/// above and below, so the dialog's top edge and its header stay below the strip (review sf: the
/// monitor dialog at 561 and 1440 put "이 컴퓨터" under the lights). A zero report changes nothing.
pub(crate) fn dialog_max_height(window_height_dp: f32, area: crate::ui::safe_area::SafeArea) -> f32 {
    let margin = crate::ui::safe_area::title_strip_height(area, true).max(16.0);
    (window_height_dp.max(0.0) - 2.0 * margin).max(0.0)
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
    // `.sheet.card` (FR-L32, #583): the 1 px `line` border is what separates a dark sheet from
    // the dimmed page. The full-screen monitor has none (`border:0` at 560 px and below).
    let framed = modifier.border(1.0, paint(theme.palette.line));
    let dismiss = on_dismiss.clone();
    let window = crate::ui::window_size::current_window_size();
    let presentation = sheet_presentation(window.width_dp, monitor);
    let top_inset = full_screen_sheet_top_inset(window.width_dp, monitor, crate::ui::safe_area::safe_area());
    let strip_bg = theme.palette.surface;
    // An empty title draws no header: the share sheet is the mockup's `.fl-share`, which has
    // none and puts its close button in its first card (FR-L27.7).
    let body = move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
            if !title.is_empty() {
                if top_inset > 0.0 {
                    // FR-L33: the full-screen sheet's header starts below the traffic lights
                    // (review ba D: "이 컴퓨터" sat under them at 390).
                    cu::Spacer().modifier(Modifier::new().fill_max_width().height(top_inset).background(paint(strip_bg)));
                }
                SheetHeader(title, dismiss, actions);
                cu::Separator(Some(paint(theme.palette.line)));
            }
            content();
        });
    };
    if presentation == SheetPresentation::Bottom {
        // compose-rust#445 (v0.0.9): the sheet is as tall as its content up to 85 percent of the
        // window, and taller content scrolls inside it, so a short sheet wraps its content.
        cu::Sheet(open)
            .modifier(framed.fill_max_width().corner_radii([CORNER_XL, CORNER_XL, 0.0, 0.0]))
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
            .modifier(framed.corner_radius(CORNER_XL).width(sheet_width(window.width_dp, width)))
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

/// The type role of a search field's text: `Label` (13 px) at the compact 36 px height, none
/// (the 14 px field size, [`FIELD_FONT`]) at the 40 px `.searchbox` height (FR-L10.2, #844).
pub fn search_field_role(height: f32) -> Option<TypeRole> {
    if height < H_BTN { Some(TypeRole::Label) } else { None }
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
/// compact (36 px) field uses the `Label` role (Fs::Sm, 13 px) like the mockup's session field; the
/// 40 px one the 14 px field text, as `.searchbox input` inherits the body size. The field is uncontrolled and keeps its hint `placeholder`.
/// v0.0.7: #376 and #374 fixed (`TextField` takes the node's background, border and shape, and
/// `text`); workaround kept until checked on a display (the bare field inside Ember's own pill
/// must be compared with the mockup before the frame moves onto the field).
pub fn SearchField(placeholder: &str, height: f32, on_change: impl FnMut(String) + 'static) {
    SearchFieldFocus(placeholder, height, None, on_change);
}

/// [`SearchField`] bound to a focus handle: a search row that opens on a tap asks for the focus
/// through it, so the user types at once (compose-rust#410).
pub fn SearchFieldFocus(placeholder: &str, height: f32, focus: Option<cu::FocusHandle>, on_change: impl FnMut(String) + 'static) {
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
                // `.searchbox input` inherits the 14 px body text; only the 36 px session field
                // (`.sside .st-q input`) is 13 (#844: every search placeholder drew 13).
                type_role: search_field_role(height),
                height: Some(height),
                weight: Some(1.0),
                placeholder: placeholder.clone(),
                on_value_change: on_change.borrow_mut().take().map(|f| Box::new(f) as Box<dyn FnMut(String)>),
                focus: focus.clone(),
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
    fn fr_l10_2_search_text_is_14_and_13_when_compact() {
        assert_eq!(search_field_role(H_BTN), None);
        assert_eq!(search_field_role(SEARCH_FIELD_H_COMPACT), Some(TypeRole::Label));
        let wide = emitted_props(|| SearchField("팀원 검색", H_BTN, |_| {}));
        assert!(wide.iter().any(|m| m == "FontSize=Float(14.0)"), "{wide:?}");
        let compact = emitted_props(|| SearchField("세션 검색", SEARCH_FIELD_H_COMPACT, |_| {}));
        assert!(!compact.iter().any(|m| m == "FontSize=Float(14.0)"), "{compact:?}");
        // Review r5: without an explicit size the compact `Label` field drew Cupertino's 15 sp,
        // wider than the 14 px ones; it sends 13 itself.
        assert!(compact.iter().any(|m| m == "FontSize=Float(13.0)"), "{compact:?}");
        for props in [&wide, &compact] {
            assert!(props.iter().any(|m| m == "LetterSpacing=Float(0.0)"), "{props:?}");
        }
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
        /// The place among its parent's children (`Insert::index`).
        index: u32,
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
                    index: 0,
                    mods: Vec::new(),
                    props: Vec::new(),
                }),
                Mutation::Insert { parent_id, node_id, index } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.parent = Some(*parent_id);
                        n.index = *index;
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

    /// The `Border` modifiers of one batch, as debug text.
    fn borders(batch: &[compose_rust::protocol::Mutation<'_>]) -> Vec<String> {
        use compose_rust::protocol::Mutation;
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { modifier, .. } if format!("{modifier:?}").starts_with("Border") => Some(format!("{modifier:?}")),
                _ => None,
            })
            .collect()
    }

    /// The node and handler of the first `property` handler in `batch`.
    fn handler_of(batch: &[compose_rust::protocol::Mutation<'_>], property: compose_rust::PropertyKind) -> (u32, u64) {
        use compose_rust::protocol::{Mutation, PropertyValue};
        batch
            .iter()
            .find_map(|m| match m {
                Mutation::SetProp { node_id, property: p, value: PropertyValue::Integer(h) } if *p == property => Some((*node_id, *h as u64)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {property:?} handler"))
    }

    static FOCUS_LOST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn fr_l10_2_focused_framed_field_turns_its_line_ink_and_back() {
        // Review r5 (field-typed-1440): the field held the caret and its frame stayed #ebe9e4;
        // style.css `.field:focus{border-color:var(--ink)}`.
        use compose_rust::protocol::{decode_batch, HostEvent};
        use compose_rust::{EventPayload, Host, PropertyKind, Recomposer};
        use std::sync::atomic::Ordering;
        let _guard = crate::services::test_guard();
        FOCUS_LOST.store(0, Ordering::SeqCst);
        // What the frame sends for an ink line and for the resting line, to compare with.
        let ink = emitted_modifiers(|| {
            EmberField(FieldOptions { border_color: Some(paint(ember_theme().palette.ink)), ..Default::default() })
        });
        let ink: Vec<String> = ink.into_iter().filter(|m| m.starts_with("Border")).collect();
        let line = emitted_modifiers(|| {
            EmberField(FieldOptions { border_color: Some(paint(ember_theme().palette.line)), ..Default::default() })
        });
        let line: Vec<String> = line.into_iter().filter(|m| m.starts_with("Border")).collect();
        assert_eq!((ink.len(), line.len()), (1, 1), "{ink:?} {line:?}");
        assert_ne!(ink, line);

        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                EmberField(FieldOptions {
                    fill_max_width: true,
                    on_focus_lost: Some(Box::new(|| {
                        FOCUS_LOST.fetch_add(1, Ordering::SeqCst);
                    })),
                    ..Default::default()
                })
            }))
        });
        let first = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert_eq!(borders(&first), line, "the resting frame is the line colour");
        let (field, gained) = handler_of(&first, PropertyKind::OnFocusGained);
        let (lost_node, lost) = handler_of(&first, PropertyKind::OnFocusLost);
        assert_eq!(field, lost_node, "both focus events are the text field's");

        let (bytes, _) = host.dispatch(HostEvent { node_id: field, handler_id: gained, payload: EventPayload::FocusGained }).expect("focus");
        let focused = decode_batch(bytes).expect("decode");
        assert_eq!(borders(&focused), ink, "focus redraws the frame in ink");

        let (bytes, _) = host.dispatch(HostEvent { node_id: field, handler_id: lost, payload: EventPayload::FocusLost }).expect("blur");
        let blurred = decode_batch(bytes).expect("decode");
        assert_eq!(borders(&blurred), line, "losing focus puts the line back");
        assert_eq!(FOCUS_LOST.load(Ordering::SeqCst), 1, "the caller's own on_focus_lost still runs");
    }

    #[test]
    fn fr_l10_2_multiline_field_clips_its_scrolled_text() {
        // field-typed-1440 (review r5): with four lines in a 64 px field the bottom of line 2
        // showed in the 10 px top inset; the toolkit's bare field clips only to a named corner.
        let nodes = emitted_nodes(|| {
            EmberField(FieldOptions { fill_max_width: true, multiline: true, height: Some(64.0), ..Default::default() })
        });
        let field = the(&nodes, "TextField");
        assert!(field.mods.iter().any(|m| m == "Clip(true)"), "{:?}", field.mods);
        // A single-line field keeps its own (one line never scrolls up into the inset).
        let single = emitted_nodes(|| EmberField(FieldOptions { fill_max_width: true, ..Default::default() }));
        assert!(!the(&single, "TextField").mods.iter().any(|m| m == "Clip(true)"));
    }

    #[test]
    fn fr_l10_2_focused_framed_field_leaves_a_bare_field_unframed() {
        // A bare field sends no border, focused or not: its container draws the frame.
        let nodes = emitted_nodes(|| EmberField(FieldOptions { bare: true, ..Default::default() }));
        for n in &nodes {
            assert!(!n.mods.iter().any(|m| m.starts_with("Border")), "{} {:?}", n.widget, n.mods);
        }
    }

    #[test]
    fn fr_l10_1_one_line_hint_is_an_ellipsised_label_under_an_empty_field() {
        // #878: the 새 세션 folder hint (a long path) wrapped onto a clipped second line.
        let nodes = emitted_nodes(|| {
            EmberField(FieldOptions {
                fill_max_width: true,
                one_line_hint: true,
                placeholder: "/Users/someone/work/a-very-long-folder/with/many/parts".to_string(),
                ..Default::default()
            })
        });
        let field = the(&nodes, "TextField");
        assert!(
            !field.props.iter().any(|p| p.starts_with("Placeholder=") && p.contains("/Users")),
            "the toolkit placeholder is not sent: {:?}",
            field.props
        );
        let hint = nodes
            .iter()
            .find(|n| n.widget == "Text" && n.props.iter().any(|p| p.contains("/Users/someone")))
            .expect("the hint label");
        assert!(hint.props.iter().any(|p| p == "MaxLines=Integer(1)"), "{:?}", hint.props);
        assert!(hint.props.iter().any(|p| p.starts_with("Overflow=")), "{:?}", hint.props);
        // The hint and the field share one box, the hint first so the field is on top.
        let stack = parent_of(&nodes, field);
        assert_eq!(stack.widget, "Box");
        assert!(stack.mods.iter().any(|m| m.starts_with("Clip")), "{:?}", stack.mods);
        assert_eq!(hint.parent, Some(stack.id));
        // The share of the row is taken by a column (#878: a weighted Box overran the inset).
        let share = parent_of(&nodes, stack);
        assert_eq!(share.widget, "Column");
        assert!(share.mods.iter().any(|m| m == "Weight(1.0)"), "{:?}", share.mods);
        assert!(share.mods.iter().any(|m| m == "Width(0.0)"), "no narrowest width: {:?}", share.mods);
        assert!(hint.id < field.id, "the hint is under the field");
        // A field that starts with text shows no hint.
        let nodes = emitted_nodes(|| {
            EmberField(FieldOptions { one_line_hint: true, placeholder: "힌트".into(), text: Some("값".into()), ..Default::default() })
        });
        assert!(!nodes.iter().any(|n| n.widget == "Text" && n.props.iter().any(|p| p.contains("힌트"))));
    }

    #[test]
    fn fr_l10_1_new_session_long_hints_stay_on_one_line() {
        let src = include_str!("new_session.rs");
        for ph in ["\"/path/to/project\"", "\"에이전트 기본값\""] {
            let at = src.find(ph).unwrap_or_else(|| panic!("{ph}"));
            let opts = &src[src[..at].rfind("EmberField(FieldOptions {").expect("field")..at];
            assert!(opts.contains("one_line_hint: true"), "{ph}: {opts}");
        }
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
        // session composer's 14 px less its 12 px card padding (#844: the text sat 16 px in at
        // 1440 and 390; `.composer` padding is 14 at every width).
        let channels = include_str!("channels.rs");
        let at = channels.find("placeholder: \"메시지 입력\"").expect("channel composer field");
        let opts = &channels[channels[..at].rfind("EmberField(FieldOptions {").expect("field")..at];
        assert!(opts.contains("inset: Some((14.0, 0.0))"), "{opts}");
        assert!(include_str!("home.rs").contains("inset: Some((2.0, 2.0))"));
        assert!(include_str!("conversation.rs").contains("inset: Some((2.0, 0.0))"));
        assert!(!include_str!("conversation.rs").contains("inset: Some((4.0, 0.0))"));
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
    fn the_font_stack_is_the_themes_and_no_screen_helper_names_one() {
        // compose-rust#374, Ember #216 (#673): the unused stand-in stack is gone; the theme
        // carries the bundled faces (`ui::fonts`, tested there).
        let production = include_str!("components.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains(&["fn font", "_refs"].concat()));
        assert!(!production.contains(&["Apple SD", " Gothic Neo"].concat()), "no system face is named");
        let theme = crate::ui::fonts::app_theme(compose_rust::ColorScheme::Light);
        assert_eq!(theme.design_system, compose_rust::DesignSystem::Cupertino);
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
    fn fr_l10_2_choice_is_the_seg_button() {
        for p in [&LIGHT, &DARK] {
            let off = choice_style(false, true, true, p);
            assert_eq!((off.height, off.fill, off.border, off.text), (32.0, p.surface, p.line, p.text_2));
            assert_eq!((off.fs.size(), off.weight.value()), (13.0, 500));
            let on = choice_style(true, true, true, p);
            assert_eq!((on.fill, on.border, on.text), (p.ink, p.ink, p.ink_text));
        }
        let production = include_str!("components.rs").split("#[cfg(test)]").next().unwrap();
        let choice = production.split("pub fn ChoiceButton(").nth(1).unwrap();
        assert!(choice.contains("choice_style(chosen, small, enabled, theme.palette)"));
        assert!(choice.contains(".border(1.0, paint(s.border))"));
    }

    #[test]
    fn fr_l10_2_choice_label_sits_12_from_each_end_of_its_pill() {
        // Review r5: the toolkit button put the label 16 px in; `.seg button` padding 0 12.
        let nodes = emitted_nodes(|| ChoiceButton("JetBrains", Choice::Radio, false, true, true, || {}));
        assert!(!nodes.iter().any(|n| n.widget == "Button"), "the toolkit button pads by 16");
        let text = the(&nodes, "Text");
        assert!(text.props.iter().any(|p| p == "FontSize=Float(13.0)"), "{:?}", text.props);
        assert!(text.props.iter().any(|p| p == "FontWeight=Integer(500)"), "{:?}", text.props);
        assert!(text.props.iter().any(|p| p == "MaxLines=Integer(1)"), "{:?}", text.props);
        let row = parent_of(&nodes, text);
        let spacers: Vec<_> = nodes.iter().filter(|n| n.widget == "Spacer" && n.parent == Some(row.id)).collect();
        assert_eq!(spacers.len(), 2, "one spacer each side of the label");
        assert!(spacers.iter().all(|s| s.mods.iter().any(|m| m == "Width(12.0)")), "{:?}", spacers.iter().map(|s| &s.mods).collect::<Vec<_>>());
        let pill = parent_of(&nodes, row);
        assert!(pill.props.iter().any(|p| p.starts_with("OnClick=")), "the pill is the control: {:?}", pill.props);
        assert!(pill.mods.iter().any(|m| m == "Height(32.0)"), "{:?}", pill.mods);
        assert!(pill.mods.iter().any(|m| m.starts_with("Border")), "{:?}", pill.mods);
        assert!(pill.mods.iter().any(|m| m.starts_with("Background")), "{:?}", pill.mods);
        assert!(pill.mods.iter().any(|m| m.starts_with("ShapeRole") || m.starts_with("Shape")), "{:?}", pill.mods);
        assert_eq!(CHOICE_PAD_X, 12.0);
        // A disabled choice is not clickable and is faded.
        let off = emitted_nodes(|| ChoiceButton("수동", Choice::Radio, false, true, false, || {}));
        assert!(!off.iter().any(|n| n.props.iter().any(|p| p.starts_with("OnClick="))), "a disabled choice takes no click");
        assert!(off.iter().any(|n| n.mods.iter().any(|m| m == "Alpha(0.45)")), "faded to 45 percent");
    }

    #[test]
    fn fr_l10_2_choice_row_wraps_8_apart() {
        // `.seg{display:flex;gap:var(--sp2);flex-wrap:wrap}`: settings-sheet-390 squeezed the
        // fourth 기본 IDE button to 57 px instead of starting a second line.
        let nodes = emitted_nodes(|| {
            ChoiceRow(|| {
                for label in ["Ember 에디터", "VS Code", "JetBrains Gateway", "매번 선택"] {
                    ChoiceButton(label, Choice::Radio, false, true, true, || {});
                }
            })
        });
        let flow = the(&nodes, "FlowRow");
        for prop in ["HorizontalSpacing", "VerticalSpacing"] {
            assert!(flow.props.iter().any(|p| p.starts_with(&format!("{prop}=")) && p.contains("8.0")), "{prop}: {:?}", flow.props);
        }
        assert!(flow.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", flow.mods);
        let pills = nodes.iter().filter(|n| n.parent == Some(flow.id)).count();
        assert_eq!(pills, 4, "each choice is a child of the wrapping row");
    }

    /// The node that takes the click, as the renderer finds it: the one with `OnClick`.
    fn clicked(nodes: &[EmittedNode]) -> &EmittedNode {
        nodes.iter().find(|n| n.props.iter().any(|p| p.starts_with("OnClick="))).expect("a node with OnClick")
    }

    /// Every shared pressable, composed alone: containers through `Clickable` and the four
    /// toolkit `Button`s.
    fn shared_pressables() -> Vec<(&'static str, fn())> {
        let cases: [(&'static str, fn()); 12] = [
            ("Clickable", || Clickable(Modifier::new().size(40.0, 40.0), Alignment::Center, || {}, || {})),
            ("NamedClickable", || {
                NamedClickable(crate::ui::cr::a11y::CLOSE, Modifier::new().size(40.0, 40.0), Alignment::Center, || {}, || {})
            }),
            ("MenuRow", || MenuRow("항목", 200.0, false, || {})),
            ("ChoiceButton", || ChoiceButton("선택", Choice::Toggle, true, false, true, || {})),
            ("FilterChip", || FilterChip("전체", false, || {})),
            ("BackButton", || BackButton(|| {})),
            ("CloseButton", || CloseButton(|| {})),
            ("AppButton", || AppButton("저장", None, ButtonKind::Default, false, true, false, || {})),
            ("AppButton primary small", || AppButton("저장", None, ButtonKind::Primary, true, true, false, || {})),
            ("IconButton", || IconButton(IconRole::Close, false, || {})),
            ("InkIconButton", || InkIconButton(IconRole::Add, || {})),
            ("PickChip", || PickChip("프로젝트", Some(IconRole::Close), false, false, || {})),
        ];
        cases.to_vec()
    }

    #[test]
    fn fr_l34_1_every_shared_pressable_sends_the_mockup_press() {
        // #250. `core.js` pressStart: scale 0.975; `style.css` `.ripple-clip.in`: opacity .07,
        // .1 in dark. Sent on the node that takes the click, which is the one the renderer
        // scales and tints (`ModifierChain.kt` reads the node's last `PressFeedback`).
        let _guard = crate::services::test_guard();
        let alpha = ember_theme().press_alpha();
        assert!(alpha == 0.07 || alpha == 0.10, "{alpha}");
        let want = format!("{:?}", compose_rust::schema::Modifier::PressFeedback { scale: 0.975, tint_alpha: alpha });
        for (name, content) in shared_pressables() {
            let nodes = emitted_nodes(content);
            let node = clicked(&nodes);
            assert!(node.mods.contains(&want), "{name}: {:?}", node.mods);
            assert_eq!(node.mods.iter().filter(|m| m.starts_with("PressFeedback")).count(), 1, "{name}: sent once");
        }
        // No screen sets its own press values: the helper is the one place.
        for (file, src) in [
            ("session.rs", include_str!("session.rs")),
            ("home.rs", include_str!("home.rs")),
            ("project.rs", include_str!("project.rs")),
            ("vcs.rs", include_str!("vcs.rs")),
            ("shell.rs", include_str!("shell.rs")),
            ("channels.rs", include_str!("channels.rs")),
            ("files.rs", include_str!("files.rs")),
        ] {
            assert!(!src.contains(".press_feedback("), "{file} sets press values of its own");
        }
    }

    #[test]
    fn a_data_named_overlay_takes_its_box_and_a_disabled_named_box_has_no_click() {
        // compose-rust#431, #429 (#673). The one clear button that names a painted box from
        // data: the name as text and as accessible name, the box's bounds and no more, and the
        // open state of a menu when it has one. `FillMaxSize` widened a box without a width.
        let _guard = crate::services::test_guard();
        let nodes = emitted_nodes(|| {
            cu::Box().content(|| DataNamedOverlay("feat/673", true, Some(true), || {}));
        });
        let over = the(&nodes, "Button");
        for want in ["Text=String(\"feat/673\")", "AccessibleName=String(\"feat/673\")"] {
            assert!(over.props.iter().any(|p| p == want), "{want}: {:?}", over.props);
        }
        assert!(over.props.iter().any(|p| p.starts_with("OnClick=")), "{:?}", over.props);
        assert!(over.mods.iter().any(|m| m == "MatchParentSize"), "{:?}", over.mods);
        assert!(!over.mods.iter().any(|m| m.starts_with("FillMax")), "{:?}", over.mods);
        let state = |mods: &[String]| -> u32 {
            mods.iter()
                .find_map(|m| m.strip_prefix("Semantics")?.split("state: ").nth(1)?.trim_end_matches([' ', '}']).parse().ok())
                .unwrap_or(0)
        };
        assert_eq!((state(&over.mods) >> compose_rust::schema::SEMANTICS_EXPANDED_SHIFT) & 3, 2, "{:?}", over.mods);
        let plain = emitted_nodes(|| {
            cu::Box().content(|| DataNamedOverlay("feat/673", false, None, || {}));
        });
        let plain = the(&plain, "Button");
        assert!(!plain.mods.iter().any(|m| m.starts_with("Semantics")), "no menu, no state: {:?}", plain.mods);
        assert!(plain.props.iter().any(|p| p == "Enabled=Bool(false)"), "{:?}", plain.props);

        // A named box that cannot act: the name, the button role and the disabled state, and
        // nothing to click or press.
        let nodes = emitted_nodes(|| NamedDisabled(crate::ui::cr::a11y::CLOSE, Modifier::new().size(32.0, 32.0), Alignment::Center, || {}));
        let boxed = the(&nodes, "Box");
        assert!(boxed.props.iter().any(|p| *p == format!("AccessibleName=String({:?})", crate::ui::cr::a11y::CLOSE)), "{:?}", boxed.props);
        assert!(!boxed.props.iter().any(|p| p.starts_with("OnClick=")), "{:?}", boxed.props);
        assert!(!boxed.mods.iter().any(|m| m.starts_with("PressFeedback")), "{:?}", boxed.mods);
        assert!(boxed.mods.iter().any(|m| m.starts_with("Semantics") && m.contains("Button")), "{:?}", boxed.mods);
        assert_eq!((state(&boxed.mods) >> compose_rust::schema::SEMANTICS_DISABLED_SHIFT) & 3, 2, "{:?}", boxed.mods);

        // No screen keeps an overlay function of its own.
        for (file, src) in [("vcs.rs", include_str!("vcs.rs")), ("agents.rs", include_str!("agents.rs")), ("routines.rs", include_str!("routines.rs"))] {
            let production = src.split("#[cfg(test)]").next().unwrap();
            assert!(!production.contains(&["fn Focus", "Overlay"].concat()), "{file}");
        }
    }

    #[test]
    fn fr_l34_5_the_phone_primary_circle_is_one_shared_button_with_its_look_unchanged() {
        // #250: files.rs (보내기) and project.rs (새 팀원) each built this button by hand, so it
        // was outside `pressable`. What is sent is what they sent, plus the press values.
        let _guard = crate::services::test_guard();
        let theme = ember_theme();
        let p = theme.palette;
        assert_eq!(H_BTN, 40.0);
        let nodes = emitted_nodes(|| InkIconButton(IconRole::Send, || {}));
        assert!(!nodes.iter().any(|n| n.widget == "Box"), "one toolkit button and nothing round it");
        let button = the(&nodes, "Button");
        assert!(button.props.iter().any(|x| x.starts_with("OnClick=")), "{:?}", button.props);
        let want = [
            format!("{:?}", compose_rust::schema::Modifier::Width(40.0)),
            format!("{:?}", compose_rust::schema::Modifier::Height(40.0)),
            format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.ink))),
            format!("{:?}", compose_rust::schema::Modifier::PressFeedback { scale: 0.975, tint_alpha: theme.press_alpha() }),
        ];
        for m in &want {
            assert!(button.mods.contains(m), "{m}: {:?}", button.mods);
        }
        assert!(button.mods.iter().any(|m| m.starts_with("ShapeRole(Full")), "{:?}", button.mods);
        assert!(!button.mods.iter().any(|m| m.starts_with("Border")), "no outline: {:?}", button.mods);
        assert_eq!(button.mods.iter().filter(|m| m.starts_with("PressFeedback")).count(), 1, "{:?}", button.mods);
        // The glyph is tinted `ink_text`, not the ringed button's `text-2`.
        let ringed = emitted_nodes(|| IconButton(IconRole::Send, false, || {}));
        assert_ne!(the(&ringed, "Button").props, button.props, "its colour differs from IconButton's");
        // Both screens use it and have no button of their own left.
        for (file, src, call) in [
            ("files.rs", include_str!("files.rs"), "InkIconButton(IconRole::Send, open);"),
            ("project.rs", include_str!("project.rs"), "InkIconButton(IconRole::Add, move || u.open_new_session(project.clone()));"),
        ] {
            let production = src.split("#[cfg(test)]").next().unwrap();
            assert!(production.contains(call), "{file}");
            assert!(!production.contains("cu::Button("), "{file}");
        }
        // The two visible buttons that stay direct send the same press values through the helper.
        for (file, src) in [("launcher.rs", include_str!("launcher.rs")), ("routines.rs", include_str!("routines.rs"))] {
            let production = src.split("#[cfg(test)]").next().unwrap();
            assert_eq!(production.matches(".modifier(pressable(").count(), 1, "{file}");
            assert!(!production.contains(".press_feedback("), "{file}");
        }
    }

    #[test]
    fn fr_l34_2_a_shared_pressable_carries_its_fill_and_shape_on_the_node_that_takes_the_click() {
        // #251. The renderer draws the press tint, the hover layer and the keyboard focus ring
        // in the shape of the node that has the click handler (`RenderNode.kt` `pressedShape`),
        // so the fill and its corner are on that node, never on a child or a wrapper.
        let _guard = crate::services::test_guard();
        for (name, content) in shared_pressables() {
            if name.ends_with("Clickable") {
                continue; // The caller's own modifier; it has no fill here.
            }
            let nodes = emitted_nodes(content);
            let node = clicked(&nodes);
            assert!(node.mods.iter().any(|m| m.starts_with("Background")), "{name}: {:?}", node.mods);
            assert!(node.mods.iter().any(|m| m.starts_with("Shape")), "{name}: {:?}", node.mods);
        }
        // A pill is a pill to the ring too, and the back square keeps its 10 dp corner.
        let chip = emitted_nodes(|| FilterChip("전체", true, || {}));
        assert!(clicked(&chip).mods.iter().any(|m| m.starts_with("ShapeRole(Full")), "{:?}", clicked(&chip).mods);
        let back = emitted_nodes(|| BackButton(|| {}));
        assert!(clicked(&back).mods.iter().any(|m| m.starts_with("Shape { top_start: 10.0")), "{:?}", clicked(&back).mods);
    }

    #[test]
    fn fr_l28_back_button_is_the_32_px_rounded_square_with_the_arrow() {
        // `.icon-btn.sm.backbtn`: 32 px, `surface`, 1 px `line`, squircle corners, an 18 px
        // arrow; review SE13 measured a 40 px circle, CH8 a "목록" text pill.
        let nodes = emitted_nodes(|| BackButton(|| {}));
        assert!(!nodes.iter().any(|n| n.widget == "Text"), "an icon, no visible text");
        assert!(!nodes.iter().any(|n| n.widget == "Button"), "not the toolkit's round icon button");
        let image = the(&nodes, "Image");
        assert!(image.mods.iter().any(|m| m == "Width(18.0)"), "{:?}", image.mods);
        let frame = parent_of(&nodes, image);
        assert!(frame.props.iter().any(|p| p.starts_with("OnClick=")), "the frame is the control: {:?}", frame.props);
        for want in ["Width(32.0)", "Height(32.0)"] {
            assert!(frame.mods.iter().any(|m| m == want), "{want}: {:?}", frame.mods);
        }
        assert!(frame.mods.iter().any(|m| m.starts_with("Shape { top_start: 10.0")), "{:?}", frame.mods);
        assert!(!frame.mods.iter().any(|m| m.starts_with("ShapeRole")), "not a circle: {:?}", frame.mods);
        let p = ember_theme().palette;
        let border = format!("{:?}", compose_rust::schema::Modifier::Border { width: 1.0, paint: paint(p.line) });
        let fill = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.surface)));
        assert!(frame.mods.contains(&border), "{:?}", frame.mods);
        assert!(frame.mods.contains(&fill), "{:?}", frame.mods);
        // The detail views use it: the 싱글 conversation header, the room header, the
        // stand-alone conversation's phone header and the routine detail on a phone (review r7:
        // a bare drawn arrow named 목록).
        for (file, src) in [
            ("session.rs", include_str!("session.rs")),
            ("channels.rs", include_str!("channels.rs")),
            ("conversation.rs", include_str!("conversation.rs")),
            ("routines.rs", include_str!("routines.rs")),
        ] {
            let production = src.split("#[cfg(test)]").next().unwrap();
            assert!(production.contains("BackButton("), "{file}");
            assert!(!production.contains("IconButton(IconRole::Back"), "{file}");
        }
        assert!(!include_str!("channels.rs").contains("AppButton(\"목록\""));
        assert!(!include_str!("routines.rs").contains("FocusOverlay(\"목록\""));
    }

    #[test]
    fn fr_l28_a_detail_top_bar_is_56_high_with_20_sides_so_the_back_square_stays_32() {
        // `.detail{grid-template-rows:56px}`, `.topbar{padding:0 var(--sp5)}`. Review r7: the
        // 싱글 header was 52 high with 12 padding all round, so the back square drew 32 x 28 at
        // x 12; the mockup has 32 x 32 at x 20.
        assert_eq!((TOPBAR_H, TOPBAR_PAD_X), (56.0, 20.0));
        assert!(TOPBAR_H - H_CTL >= 2.0 * SP[1], "room above and below the 32 dp square");
        let session = include_str!("session.rs").split("#[cfg(test)]").next().unwrap();
        let header = session.split("// `.topbar` in `.detail`").nth(1).expect("the 싱글 header").split("ConversationView(").next().unwrap();
        assert!(header.contains(".height(super::theme::TOPBAR_H)"), "{header}");
        assert!(!header.contains(".padding(SP[2])") && !header.contains(".height(52.0)"), "{header}");
        assert_eq!(header.matches("Modifier::new().width(super::theme::TOPBAR_PAD_X)").count(), 2, "both sides");
        // The member name shrinks with an ellipsis so the panel button stays inside the bar,
        // and the status pill follows the name rather than the far end (review r8).
        assert!(header.contains("NameThenPill("), "{header}");
        assert!(header.contains("|| Pill(st_txt, st_tone.into(), true)"), "{header}");
        let routines = include_str!("routines.rs").split("#[cfg(test)]").next().unwrap();
        let icons = routines.split("if layout == DetailHeader::Icons {").nth(1).unwrap().split("} else if layout == DetailHeader::Stacked").next().unwrap();
        assert!(icons.contains("BackButton(") && !icons.contains(".padding(PAD_ROW_V)"), "{icons}");
        assert_eq!(icons.matches("Modifier::new().width(TOPBAR_PAD_X)").count(), 2, "both sides");
    }

    #[test]
    fn fr_l10_2_choice_row_at_the_end_of_a_row_is_as_wide_as_its_choices() {
        // Review r6 (agents-models-1440): a full-width group in SettingRow's trailing slot left
        // the title column a few pixels and drew 낮음 over the hint.
        let nodes = emitted_nodes(|| {
            ChoiceRowFit(|| {
                for label in ["낮음", "보통", "높음"] {
                    ChoiceButton(label, Choice::Radio, false, true, true, || {});
                }
            })
        });
        let flow = the(&nodes, "FlowRow");
        for prop in ["HorizontalSpacing", "VerticalSpacing"] {
            assert!(flow.props.iter().any(|p| p.starts_with(&format!("{prop}=")) && p.contains("8.0")), "{prop}: {:?}", flow.props);
        }
        assert!(!flow.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", flow.mods);
        assert_eq!(nodes.iter().filter(|n| n.parent == Some(flow.id)).count(), 3);
    }

    #[test]
    fn fr_l10_2_choice_groups_wrap_on_settings_and_agents() {
        let settings = include_str!("settings.rs");
        let settings = settings.split("#[cfg(test)]").next().unwrap();
        assert!(settings.matches("ChoiceRow(").count() >= 3, "화면 테마, 에디터 단축키 and 기본 IDE wrap");
        let agents = include_str!("agents.rs");
        let seg = agents.split("fn SegGroup<").nth(1).unwrap();
        let seg = &seg[..seg.find("\n}\n").unwrap()];
        assert!(seg.contains("ChoiceRowFit("), "{seg}");
    }

    #[test]
    fn fr_l10_2_choice_call_sites_are_seg_buttons() {
        let settings = include_str!("settings.rs");
        let settings = settings.split("#[cfg(test)]").next().unwrap();
        for call in [
            "ChoiceButton(pref.label(), Choice::Radio, current_theme == pref, true, true,",
            "ChoiceButton(km.label(), Choice::Radio, current_keymap == km, true, true,",
            "ChoiceButton(label, Choice::Radio, selected, true, true,",
        ] {
            assert!(settings.contains(call), "{call}");
        }
        assert!(!settings.contains("FilterChip("), "the settings choices are not filter chips");
        let agents = include_str!("agents.rs");
        let seg = agents.split("fn SegGroup<").nth(1).unwrap();
        let seg = &seg[..seg.find("\n}\n").unwrap()];
        assert!(seg.contains("ChoiceButton(label, Choice::Radio, selected, true, true,"), "{seg}");
        assert!(seg.contains("ChoiceRowFit(|| {"), "`.seg` gap 8, wrapping: {seg}");
        assert!(!seg.contains("cu::Button("), "{seg}");
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
        // A named role takes Ember's step for it, never the toolkit's Cupertino size.
        let mono = emitted_props(|| EmberField(FieldOptions { type_role: Some(TypeRole::Mono), ..Default::default() }));
        assert!(mono.iter().any(|m| m == "FontSize=Float(12.0)"), "{mono:?}");
    }

    #[test]
    fn fr_l10_2_field_sends_ember_size_line_and_tracking_for_its_placeholder() {
        // Review r5: the toolkit sets the placeholder in the field's own style (HostTextField.kt
        // `textStyle.copy(color = ...)`), whose size and tracking come from the Cupertino ladder
        // unless sent: `Label` drew 15 sp at -0.24 and a 14 px field kept Body's -0.41.
        let cases: [(Option<TypeRole>, f64, f64); 3] =
            [(None, 14.0, 22.0), (Some(TypeRole::Label), 13.0, 20.0), (Some(TypeRole::Mono), 12.0, 18.0)];
        for (role, size, line) in cases {
            for bare in [false, true] {
                let props = field_props(role, bare);
                assert!(props.iter().any(|m| *m == format!("FontSize=Float({size:?})")), "{role:?} {props:?}");
                assert!(props.iter().any(|m| *m == format!("LineHeight=Float({line:?})")), "{role:?} {props:?}");
                assert!(props.iter().any(|m| m == "LetterSpacing=Float(0.0)"), "{role:?} {props:?}");
            }
        }
        assert_eq!(field_fs(None), FIELD_FONT);
        assert_eq!(field_fs(Some(TypeRole::Label)).size(), 13.0);
    }

    fn field_props(role: Option<TypeRole>, bare: bool) -> Vec<String> {
        match (role, bare) {
            (None, false) => emitted_props(|| EmberField(FieldOptions { placeholder: "검색".into(), ..Default::default() })),
            (None, true) => emitted_props(|| EmberField(FieldOptions { bare: true, placeholder: "검색".into(), ..Default::default() })),
            (Some(TypeRole::Label), false) => emitted_props(|| EmberField(FieldOptions { type_role: Some(TypeRole::Label), ..Default::default() })),
            (Some(TypeRole::Label), true) => emitted_props(|| EmberField(FieldOptions { bare: true, type_role: Some(TypeRole::Label), ..Default::default() })),
            (_, false) => emitted_props(|| EmberField(FieldOptions { type_role: Some(TypeRole::Mono), ..Default::default() })),
            (_, true) => emitted_props(|| EmberField(FieldOptions { bare: true, type_role: Some(TypeRole::Mono), ..Default::default() })),
        }
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
    fn fr_l32_a_sheet_is_framed_with_a_line_on_its_surface() {
        // #583: the dark sheet (`#20201d`) met the dimmed page (`#0f0f0e`) with no edge; the
        // mockup's `.sheet.card` has a 1 px `line` border between them.
        let _guard = crate::services::test_guard();
        let p = ember_theme().palette;
        let border = format!("{:?}", compose_rust::schema::Modifier::Border { width: 1.0, paint: paint(p.line) });
        let fill = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.surface)));
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let dialog = emitted_nodes(|| EmberSheet("시트", true, || {}, None, || {}));
        crate::ui::window_size::set_opened_for_test(Some((390.0, 844.0)));
        let bottom = emitted_nodes(|| EmberSheet("시트", true, || {}, None, || {}));
        let full = emitted_nodes(|| EmberSheetSized("시트", true, SHEET_WIDTH_MONITOR, true, || {}, None, || {}));
        crate::ui::window_size::set_opened_for_test(None);
        for (name, node) in [("dialog", the(&dialog, "Dialog")), ("bottom sheet", the(&bottom, "Sheet"))] {
            assert!(node.mods.contains(&border), "{name}: {:?}", node.mods);
            assert!(node.mods.contains(&fill), "{name}: {:?}", node.mods);
            assert_eq!(node.mods.iter().filter(|m| m.starts_with("Border")).count(), 1, "{name}: one 1 px line");
            assert!(node.props.iter().any(|x| x.starts_with("Scrim=")), "{name}: {:?}", node.props);
        }
        assert!(the(&dialog, "Dialog").mods.iter().any(|m| m.starts_with("Shape { top_start: 24.0")), "{:?}", the(&dialog, "Dialog").mods);
        let full = the(&full, "Dialog");
        assert!(full.mods.contains(&fill), "{:?}", full.mods);
        assert!(!full.mods.iter().any(|m| m.starts_with("Border")), "the full screen has no frame: {:?}", full.mods);
        // The scrim stays the mockup's black at 0.3 (measured equal in both renders).
        assert_eq!(MODAL_SCRIM, Color::argb(0x4D00_0000));
    }

    #[test]
    fn fr_l32_sheet_without_a_title_draws_no_header() {
        // files.html `.fl-share` has no `.sheet-h`: the share sheet passes an empty title.
        let _guard = crate::services::test_guard();
        let headed = emitted_nodes(|| EmberSheetSized("공유 시트", true, 400.0, false, || {}, None, || {}));
        assert!(headed.iter().any(|n| n.props.iter().any(|p| p.starts_with("OnClick="))), "the header's close button");
        assert!(headed.iter().any(|n| n.widget == "Separator" || n.widget == "Divider"), "the line under the header");
        let bare = emitted_nodes(|| EmberSheetSized("", true, 400.0, false, || {}, None, || {}));
        assert!(!bare.iter().any(|n| n.props.iter().any(|p| p.starts_with("OnClick="))), "no header close button");
        assert!(!bare.iter().any(|n| n.widget == "Separator" || n.widget == "Divider"), "no header line");
        assert!(bare.iter().any(|n| n.props.iter().any(|p| p.starts_with("OnDismiss="))), "Escape and the scrim still close it");
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
                // AppButton, AppMenuButton, IconButton, InkIconButton and PickChip; ChoiceButton
                // is Ember's clickable pill since review r5 (its 12 px label inset, FR-L10.2).
                // The sixth is DataNamedOverlay, the one clear button that names a painted box
                // from data for every screen (#673).
                "components.rs" => 6,
                // SegGroup moved onto ChoiceButton (#878); the section rows are named
                // containers and the chips named from data use DataNamedOverlay (#673).
                "agents.rs" => 0,
                // The phone's round 보내기 is InkIconButton (FR-L34.5, #250).
                "files.rs" => 0,
                "home.rs" => 0,
                // The row's bare 더 보기 glyph: no shared helper draws a button without a ring.
                "launcher.rs" => 1,
                // The phone's round 새 팀원 is InkIconButton (FR-L34.5, #250).
                "project.rs" => 0,
                // The wide 새 루틴 pill (toolkit label style and a fixed width, which AppButton
                // would change).
                "routines.rs" => 1,
                // Its fixed names are on the painted boxes; the stash pill, the bar selector
                // and the rows named from data use DataNamedOverlay (#673).
                "vcs.rs" => 0,
                // The channel + ring is a named container (compose-rust#429, #673).
                "new_channel.rs" => 0,
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

        // ---- FR-L10.4: option menus hang from their control (#888) -------------------------------

    fn pop_sample_rows() -> Vec<PopRow> {
        vec![
            PopRow { icon: Some(ICON_CHECK_SVG), title: "Ember 에디터".into(), description: Some("네이티브 창으로 엽니다".into()), ..Default::default() },
            PopRow {
                icon: Some(ICON_CHECK_SVG),
                title: "VS Code".into(),
                description: Some("웹 IDE로 엽니다".into()),
                badge: Some(("OSE".into(), PillTone::Sky)),
                chosen: true,
                ..Default::default()
            },
            PopRow { title: "JetBrains Gateway".into(), danger: true, ..Default::default() },
        ]
    }

    static POP_DISMISSED: AtomicUsize = AtomicUsize::new(0);
    static POP_CHOSEN: AtomicUsize = AtomicUsize::new(usize::MAX);

    fn pop_menu(kind: PopKind) {
        let rows = pop_sample_rows();
        PopoverMenu(
            true,
            PopMenu { name: "IDE 고르기", kind, head: "어느 IDE로 열까요", sub: Some("mini:~/ember"), rows: &rows, align: compose_rust::PopoverAlign::End },
            || {
                POP_DISMISSED.fetch_add(1, Ordering::SeqCst);
            },
            |i| POP_CHOSEN.store(i, Ordering::SeqCst),
            || Label("IDE 버튼", Fs::Sm, Weight::Medium, None, Some(1)),
            None,
        );
    }

    fn drop_menu() {
        pop_menu(PopKind::Drop);
    }

    fn mode_menu() {
        pop_menu(PopKind::Mode);
    }

    fn text_node<'a>(nodes: &'a [EmittedNode], text: &str) -> &'a EmittedNode {
        let want = format!("Text=String({text:?})");
        nodes.iter().find(|n| n.props.contains(&want)).unwrap_or_else(|| panic!("no text {text}"))
    }

    fn is_under(nodes: &[EmittedNode], node: &EmittedNode, ancestor: u32) -> bool {
        let mut at = node.parent;
        while let Some(id) = at {
            if id == ancestor {
                return true;
            }
            at = nodes.iter().find(|n| n.id == id).and_then(|n| n.parent);
        }
        false
    }

    #[test]
    fn fr_l10_4_drop_menu_is_a_popover_at_the_controls_end_with_head_rows_pill_and_check() {
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let nodes = emitted_nodes(drop_menu);
        crate::ui::window_size::set_opened_for_test(None);
        let pops: Vec<&EmittedNode> = nodes.iter().filter(|n| n.widget == "Popover").collect();
        assert_eq!(pops.len(), 1, "one popover, no dialog: {:?}", nodes.iter().map(|n| &n.widget).collect::<Vec<_>>());
        assert!(!nodes.iter().any(|n| n.widget == "Dialog" || n.widget == "Sheet"));
        let pop = pops[0];
        // `.sdrop{right:0;top:calc(100% + 6px)}`: below the control, lined up with its end.
        for want in ["Open=Bool(true)", "Placement=Integer(2)", "Align=Integer(3)", "OffsetDp=Float(6.0)"] {
            assert!(pop.props.iter().any(|p| p == want), "{want} in {:?}", pop.props);
        }
        // The box: 360 wide, `corner-lg`, padded 6, a menu to assistive technology.
        assert!(pop.mods.iter().any(|m| m == "Width(360.0)"), "{:?}", pop.mods);
        assert!(pop.mods.iter().any(|m| m == "Padding(6.0)"), "{:?}", pop.mods);
        // Its own shadow only: the toolkit's menu elevation is off, or a second shadow shows
        // through the box as a grey band inside the border.
        assert!(pop.mods.iter().any(|m| m == "Elevation(0.0)"), "{:?}", pop.mods);
        assert!(pop.mods.iter().any(|m| m.starts_with("Shape { top_start: 14.0")), "{:?}", pop.mods);
        assert!(pop.mods.iter().any(|m| m.starts_with("Semantics") && m.contains("Menu")), "{:?}", pop.mods);
        // The control is the popover's first child; everything else is in the box.
        let anchor = nodes.iter().find(|n| n.parent == Some(pop.id) && n.index == 0).expect("anchor child");
        let button = text_node(&nodes, "IDE 버튼");
        assert!(button.id == anchor.id || is_under(&nodes, button, anchor.id), "the control is the anchor");
        for t in ["어느 IDE로 열까요", "mini:~/ember", "Ember 에디터", "네이티브 창으로 엽니다", "VS Code", "OSE", "JetBrains Gateway"] {
            let n = text_node(&nodes, t);
            assert!(is_under(&nodes, n, pop.id), "{t} is inside the popover");
            assert!(n.id != anchor.id && !is_under(&nodes, n, anchor.id), "{t} is not part of the control");
        }
        // One row per option, each a menu item; the current one alone says it is selected and
        // alone carries the check (one more image than the other icon rows).
        let rows: Vec<&EmittedNode> = nodes
            .iter()
            .filter(|n| is_under(&nodes, n, pop.id) && !is_under(&nodes, n, anchor.id) && n.props.iter().any(|p| p.starts_with("OnClick=")))
            .collect();
        assert_eq!(rows.len(), 3, "three rows");
        let sem: Vec<String> = rows.iter().map(|r| r.mods.iter().find(|m| m.starts_with("Semantics")).cloned().expect("row semantics")).collect();
        assert!(sem.iter().all(|m| m.contains("MenuItem")), "{sem:?}");
        assert_ne!(sem[1], sem[0], "the current row says it is selected");
        assert_eq!(sem[0], sem[2], "the others do not");
        let images = |r: &EmittedNode| nodes.iter().filter(|n| n.widget == "Image" && is_under(&nodes, n, r.id)).count();
        assert_eq!((images(rows[0]), images(rows[1]), images(rows[2])), (1, 2, 0), "icon, icon and check, neither");
    }

    #[test]
    fn fr_l10_4_a_row_tap_dismisses_and_reports_its_index() {
        let _guard = crate::services::test_guard();
        POP_DISMISSED.store(0, Ordering::SeqCst);
        POP_CHOSEN.store(usize::MAX, Ordering::SeqCst);
        let mut host = Host::new(|| Box::new(Recomposer::new(drop_menu)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut text_of: std::collections::HashMap<u32, String> = Default::default();
        let mut parent: std::collections::HashMap<u32, u32> = Default::default();
        let mut click: Vec<(u32, u64)> = Vec::new();
        for m in batch.iter() {
            match m {
                Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(t) } => {
                    text_of.insert(*node_id, t.to_string());
                }
                Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                    click.push((*node_id, *h as u64));
                }
                Mutation::Insert { parent_id, node_id, .. } => {
                    parent.insert(*node_id, *parent_id);
                }
                _ => {}
            }
        }
        let mut node = *text_of.iter().find(|(_, t)| t.as_str() == "JetBrains Gateway").expect("row text").0;
        let (node_id, handler_id) = loop {
            if let Some(c) = click.iter().find(|(n, _)| *n == node) {
                break *c;
            }
            node = *parent.get(&node).expect("a clickable ancestor");
        };
        host.dispatch(HostEvent { node_id, handler_id, payload: EventPayload::Clicked }).expect("click");
        assert_eq!(POP_DISMISSED.load(Ordering::SeqCst), 1, "a tap closes the menu");
        assert_eq!(POP_CHOSEN.load(Ordering::SeqCst), 2, "and reports the row");
    }

    #[test]
    fn fr_l10_4_mode_menu_is_340_wide_8_below_and_lined_up_with_the_start_by_default() {
        let m = pop_metrics(PopKind::Mode);
        assert_eq!((m.width, m.box_pad, m.offset), (340.0, 0.0, 8.0));
        assert_eq!((m.head_pad, m.list_pad), ((12.0, 16.0, 4.0), (4.0, 8.0, 8.0)));
        assert_eq!((m.row_pad, m.row_gap, m.row_min_h, m.row_radius), ((8.0, 12.0), 12.0, 56.0, CORNER_SM));
        let d = pop_metrics(PopKind::Drop);
        assert_eq!((d.width, d.box_pad, d.offset), (360.0, 6.0, 6.0));
        assert_eq!((d.head_pad, d.list_pad), ((8.0, 10.0, 8.0), (0.0, 0.0, 0.0)));
        // `.sdrop-i`: 8 + 21 (14 at 1.5) + 18 (`.small`) + 8 = 55 with a description.
        assert_eq!((d.row_pad, d.row_gap, d.row_min_h, d.row_radius), ((8.0, 10.0), 10.0, 55.0, CORNER_MD));
        // `.md-row.on{background:var(--sel)}`, `.sdrop-i.on{background:var(--hover)}`.
        assert_eq!(pop_chosen_fill(PopKind::Mode, &LIGHT), LIGHT.sel);
        assert_eq!(pop_chosen_fill(PopKind::Drop, &DARK), DARK.hover);
    }

    #[test]
    fn fr_l10_4_at_560_and_below_a_mode_menu_is_a_sheet_and_a_drop_menu_spans_the_window() {
        assert_eq!(pop_presentation(PopKind::Mode, 390.0), PopPresentation::Sheet);
        assert_eq!(pop_presentation(PopKind::Mode, 560.0), PopPresentation::Sheet);
        assert_eq!(pop_presentation(PopKind::Mode, 561.0), PopPresentation::Popover);
        // A window that has not reported its size yet is not a phone.
        assert_eq!(pop_presentation(PopKind::Mode, 0.0), PopPresentation::Popover);
        assert_eq!(pop_presentation(PopKind::Drop, 390.0), PopPresentation::Popover);
        assert_eq!(pop_width(PopKind::Drop, 390.0), 366.0);
        assert_eq!(pop_width(PopKind::Drop, 1440.0), 360.0);
        assert_eq!(pop_width(PopKind::Drop, 370.0), 346.0);
        assert_eq!(pop_width(PopKind::Mode, 1440.0), 340.0);
        assert_eq!(pop_width(PopKind::Mode, 0.0), 340.0);
    }

    #[test]
    fn fr_l10_4_mode_menu_at_390_sends_a_bottom_sheet_with_the_rows_and_no_popover() {
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((390.0, 844.0)));
        let nodes = emitted_nodes(mode_menu);
        let drop = emitted_nodes(drop_menu);
        crate::ui::window_size::set_opened_for_test(None);
        assert!(!nodes.iter().any(|n| n.widget == "Popover"));
        let sheet = the(&nodes, "Sheet");
        for t in ["어느 IDE로 열까요", "VS Code", "웹 IDE로 엽니다"] {
            assert!(is_under(&nodes, text_node(&nodes, t), sheet.id), "{t} is in the sheet");
        }
        assert!(!is_under(&nodes, text_node(&nodes, "IDE 버튼"), sheet.id), "the control stays on the page");
        let pop = the(&drop, "Popover");
        assert!(pop.mods.iter().any(|m| m == "Width(366.0)"), "{:?}", pop.mods);
    }

    fn plain_sheet_menu() {
        let rows = pop_sample_rows();
        PopoverMenuWith(
            true,
            PopMenu { name: "IDE 고르기", kind: PopKind::Mode, head: "어느 IDE로 열까요", sub: None, rows: &rows, align: compose_rust::PopoverAlign::Start },
            PopExtra { plain_sheet: true, ..Default::default() },
            || {},
            |_| {},
            || Label("IDE 버튼", Fs::Sm, Weight::Medium, None, Some(1)),
            None,
        );
    }

    #[test]
    fn fr_l10_4_plain_sheet_rows_are_named_menu_items_with_their_selected_state() {
        // Review st, A FAIL 1: at 390 the sheet's column was a named menu, which macOS turned
        // into one static text (`value=menu`) with no rows under it. The rows now sit in the
        // sheet with nothing between them that names itself, each a menu item named by its title.
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((390.0, 844.0)));
        let nodes = emitted_nodes(plain_sheet_menu);
        crate::ui::window_size::set_opened_for_test(None);
        let sheet = the(&nodes, "Sheet");
        let rows: Vec<&EmittedNode> = nodes
            .iter()
            .filter(|n| is_under(&nodes, n, sheet.id) && n.props.iter().any(|p| p.starts_with("OnClick=")))
            .collect();
        assert_eq!(rows.len(), 3, "one pressable row per option");
        let sem = |n: &EmittedNode| n.mods.iter().find(|m| m.starts_with("Semantics")).cloned();
        let row_sem: Vec<String> = rows.iter().map(|r| sem(r).expect("row semantics")).collect();
        assert!(row_sem.iter().all(|m| m.contains("MenuItem")), "{row_sem:?}");
        assert_ne!(row_sem[1], row_sem[0], "the current row says it is selected");
        assert_eq!(row_sem[0], row_sem[2], "the others do not");
        // Each row's name is its title, the only text of the row's title line.
        for (row, title) in rows.iter().zip(["Ember 에디터", "VS Code", "JetBrains Gateway"]) {
            assert!(is_under(&nodes, text_node(&nodes, title), row.id), "{title} names its row");
        }
        // Nothing between the sheet and a row names itself or claims a role: a named node there
        // is what swallowed the rows.
        for row in &rows {
            let mut at = row.parent;
            while let Some(id) = at {
                if id == sheet.id {
                    break;
                }
                let n = nodes.iter().find(|n| n.id == id).expect("ancestor");
                assert!(sem(n).is_none(), "ancestor {} says {:?}", n.widget, sem(n));
                assert!(!n.props.iter().any(|p| p.starts_with("AccessibleName")), "ancestor {} is named: {:?}", n.widget, n.props);
                at = n.parent;
            }
        }
    }

    #[test]
    fn fr_l10_4_a_title_beside_a_badge_takes_two_lines() {
        // Review st, A FAIL 2: the model menu's one row was cut at 1440 and 800.
        let rows = pop_sample_rows();
        assert_eq!(pop_title_lines(&rows[1]), 2, "beside a badge");
        assert_eq!(pop_title_lines(&rows[0]), 1);
        let src = include_str!("components.rs").split("#[cfg(test)]").next().unwrap();
        assert!(src.contains("Some(paint(title_c)), Some(pop_title_lines(r)))"));
    }

    #[test]
    fn fr_l10_4_an_icon_button_is_named_by_its_text_and_a_field_by_its_label() {
        // Review st, E: the branch menu's 만들기 had an empty name and its field was named by
        // its placeholder.
        let nodes = emitted_nodes(|| AppButton("만들기", Some(IconRole::Add), ButtonKind::Default, true, true, false, || {}));
        let button = the(&nodes, "Button");
        assert!(button.props.iter().any(|p| p.starts_with("AccessibleName") && p.contains("만들기")), "{:?}", button.props);
        let field = emitted_nodes(|| EmberField(FieldOptions { placeholder: "feat/새-브랜치".into(), label: Some("새 브랜치 이름"), ..Default::default() }));
        let text_field = the(&field, "TextField");
        let named = text_field.props.iter().chain(text_field.mods.iter()).any(|p| p.contains("새 브랜치 이름"));
        assert!(named, "props {:?} mods {:?}", text_field.props, text_field.mods);
        let vcs = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        assert!(vcs.contains("label: Some(BRANCH_POP_NEW_NAME)") && vcs.contains("BRANCH_POP_NEW_NAME: &str = \"새 브랜치 이름\""));
    }

    #[test]
    fn fr_l28_a_name_keeps_its_width_with_the_pill_after_it_and_a_long_one_gives_way() {
        // Unmeasured: the name takes the room (the pill at the end) until the renderer reports.
        assert_eq!(name_then_pill_width(0.0, 120.0, 60.0, 8.0), None);
        assert_eq!(name_then_pill_width(300.0, 0.0, 60.0, 8.0), None);
        // A short name keeps its own width (plus the dp lost to rounding); the pill follows it.
        assert_eq!(name_then_pill_width(300.0, 120.0, 60.0, 8.0), Some(121.0));
        assert_eq!(name_then_pill_width(300.0, 230.0, 60.0, 8.0), Some(231.0));
        // A long one has no fixed width: the name takes what the pill leaves and is cut with an
        // ellipsis. One dp short of fitting is already too long, so the pill is never the one
        // that gives way (review r8 fix round: the pill read "승인…" beside a name in full).
        assert_eq!(name_then_pill_width(300.0, 231.0, 60.0, 8.0), None);
        assert_eq!(name_then_pill_width(300.0, 400.0, 60.0, 8.0), None);
        assert_eq!(name_then_pill_width(50.0, 400.0, 60.0, 8.0), None);
    }

    fn name_then_pill() {
        NameThenPill("demo-project 세션", Fs::Md, Weight::Semibold, None, 8.0, || Pill("승인 대기", PillTone::Orange, true));
    }

    #[test]
    fn fr_l28_name_then_pill_measures_the_room_the_name_and_the_pill() {
        let _guard = crate::services::test_guard();
        let nodes = emitted_nodes(name_then_pill);
        let observed = nodes.iter().filter(|n| n.mods.iter().any(|m| m.starts_with("ObserveSize") && m.contains("every_dp: true"))).count();
        assert_eq!(observed, 3, "the room, the name's own width and the pill");
        // The measuring copy is invisible and out of the accessibility tree.
        let copies: Vec<&EmittedNode> = nodes.iter().filter(|n| n.props.iter().any(|p| p == "Text=String(\"demo-project 세션\")")).collect();
        assert_eq!(copies.len(), 2);
        assert!(copies[0].mods.iter().any(|m| m.starts_with("Semantics")), "the copy is hidden: {:?}", copies[0].mods);
        // Before a report the visible name fills the room, so a long name never pushes past it.
        let name_box = parent_of(&nodes, copies[1]);
        assert!(name_box.mods.iter().any(|m| m == "Weight(1.0)"), "{:?}", name_box.mods);
    }

    #[test]
    fn fr_l18_a_team_card_row_cuts_the_name_and_keeps_its_buttons() {
        // Review r8 fix round (single-long-390): the member name was an unweighted label, the
        // first child measured, so with a long name 열기 and 종료 got size 0 and the agent text
        // was drawn over the name. The name is now the row's only weighted part.
        let conv = include_str!("conversation.rs").split("#[cfg(test)]").next().unwrap();
        let row = conv.split("for r in rows.iter().cloned() {").nth(1).expect("the team rows").split("\"종료\",").next().unwrap();
        assert!(row.contains("NameThenPill(\n                                    &name_label,"), "{row}");
        assert!(!row.contains("Label(\n                                    &name_label,"), "{row}");
        assert_eq!(row.matches(".weight(").count(), 0, "nothing else in the row is weighted: {row}");
        assert!(row.contains("\"열기\","), "{row}");
    }

    /// What `NameThenPill` sends after the renderer reports the room, the name's own width and
    /// the pill's width, in that order.
    fn name_then_pill_after_reports(sizes: [f32; 3]) -> String {
        use compose_rust::{WindowHeightClass, WindowSizeClass};
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(name_then_pill)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let observed: Vec<u32> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { node_id, modifier, .. } if format!("{modifier:?}").starts_with("ObserveSize") => Some(*node_id),
                _ => None,
            })
            .collect();
        assert_eq!(observed.len(), 3, "the room, the name and the pill");
        let mut sent = Vec::new();
        for (node_id, w) in observed.iter().zip(sizes) {
            let payload = EventPayload::WindowSizeChanged {
                width_dp: w,
                height_dp: 22.0,
                class: WindowSizeClass::Compact,
                height_class: WindowHeightClass::Compact,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            };
            let (bytes, _) = host.dispatch(HostEvent { node_id: *node_id, handler_id: 0, payload }).expect("dispatch");
            sent.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        }
        compose_rust::window::reset_window_size();
        sent.join("\n")
    }

    #[test]
    fn fr_l28_name_then_pill_gives_the_name_its_measured_width() {
        let _guard = crate::services::test_guard();
        // The renderer reports the room (300), the name's own width (120) and the pill (60).
        let sent = name_then_pill_after_reports([300.0, 120.0, 60.0]);
        assert!(sent.contains("Width(121.0)"), "the name keeps its own width: {sent}");
    }

    #[test]
    fn fr_l28_name_then_pill_never_sizes_the_pill() {
        let _guard = crate::services::test_guard();
        // What the toolkit gets for the pill: a box with the size observer and nothing else (no
        // weight, no width), after a gap spacer of its own; the row has no spacing, so the
        // spacer that follows a short name costs no second gap. An unweighted child is measured
        // before the weighted name, so the pill is as wide as its text (review r8 fix round,
        // item 5: the pill read "승인…", then "실…", then nothing, beside a name in full).
        let nodes = emitted_nodes(name_then_pill);
        let pill_text = nodes.iter().find(|n| n.props.iter().any(|p| p == "Text=String(\"승인 대기\")")).expect("the pill text");
        let mut up = parent_of(&nodes, pill_text);
        while !up.mods.iter().any(|m| m.starts_with("ObserveSize")) {
            up = parent_of(&nodes, up);
        }
        let pill_box = up;
        let set = |n: &EmittedNode| n.mods.iter().filter(|m| *m != "Empty").cloned().collect::<Vec<String>>();
        assert_eq!(set(pill_box).len(), 1, "only the size observer: {:?}", pill_box.mods);
        let row = parent_of(&nodes, pill_box);
        assert_eq!(row.widget, "Row");
        assert!(!row.props.iter().chain(row.mods.iter()).any(|p| p.contains("Spac")), "no row spacing: {:?} {:?}", row.props, row.mods);
        let mut kids: Vec<&EmittedNode> = nodes.iter().filter(|n| n.parent == Some(row.id)).collect();
        kids.sort_by_key(|n| n.index);
        assert_eq!(kids.len(), 3, "the name, the gap and the pill");
        assert!(kids[0].mods.iter().any(|m| m == "Weight(1.0)"), "the name takes the rest: {:?}", kids[0].mods);
        assert_eq!((kids[1].widget.as_str(), set(kids[1])), ("Spacer", vec!["Width(8.0)".to_string()]));
        assert_eq!(kids[2].id, pill_box.id);

        // A name too long for the room (400 in 300 beside a pill of 60) gets no fixed width: it
        // stays the weighted child, so nothing is re-sent and the pill keeps its 60.
        let long = name_then_pill_after_reports([300.0, 400.0, 60.0]);
        assert!(!long.contains("Width(") && !long.contains("Weight("), "{long}");
        // Nor does a short name size the pill: the name gets its width, a weighted spacer is
        // added after the pill, and no modifier goes to the pill's box.
        let short = name_then_pill_after_reports([300.0, 120.0, 60.0]);
        let on_pill = format!("node_id: {}, modifier: W", pill_box.id);
        assert!(short.contains("Width(121.0)") && short.contains("Weight(1.0)"), "{short}");
        assert!(!short.contains(&on_pill), "{short}");
    }

    #[test]
    fn fr_l10_4_a_phone_drop_panel_is_drawn_12_from_each_side() {
        // The toolkit keeps a popover 8 from the window's edges and puts a window-less-24 panel
        // on the end of a control that ends 16 or more from the right edge at x 8; the 4 dp
        // shift draws it at 12..378 in a 390 window (`.sdrop{left:12px;right:12px}`, review r8).
        assert_eq!(TOOLKIT_POPOVER_MARGIN + pop_phone_shift(PopKind::Drop, 390.0), 12.0);
        assert_eq!(TOOLKIT_POPOVER_MARGIN + pop_phone_shift(PopKind::Drop, 390.0) + pop_width(PopKind::Drop, 390.0), 390.0 - 12.0);
        assert_eq!(pop_phone_shift(PopKind::Drop, 560.0), 4.0);
        assert_eq!(pop_phone_shift(PopKind::Drop, 561.0), 0.0);
        assert_eq!(pop_phone_shift(PopKind::Drop, 0.0), 0.0, "no size reported yet");
        assert_eq!(pop_phone_shift(PopKind::Mode, 390.0), 0.0, "a mode menu is a sheet there");
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((390.0, 844.0)));
        let phone = emitted_nodes(drop_menu);
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let wide = emitted_nodes(drop_menu);
        crate::ui::window_size::set_opened_for_test(None);
        let pop = the(&phone, "Popover");
        assert!(pop.mods.iter().any(|m| m == "Offset { x: 4.0, y: 0.0 }"), "{:?}", pop.mods);
        let pop = the(&wide, "Popover");
        assert!(!pop.mods.iter().any(|m| m.starts_with("Offset")), "{:?}", pop.mods);
    }

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
