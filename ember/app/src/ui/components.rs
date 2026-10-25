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

use super::theme::*;

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
    rsx! {
        Button {
            text,
            icon,
            enabled,
            fill_max_width,
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
    rsx! {
        Button {
            text,
            icon,
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
    children: Element,
) -> Element {
    let theme = use_ember_theme();
    let (fill, border) = card_colors(selected, theme.palette);
    rsx! {
        Card {
            fill_max_width,
            weight,
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
