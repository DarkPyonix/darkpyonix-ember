//! Local stand-ins for widgets dioxus-compose does not have yet.
//!
//! Every screen uses these instead of the stand-in's building blocks directly, so when the
//! real widget lands only this file changes. Each says which upcoming widget replaces it.

use dioxus_compose::prelude::*;

use ember_client::LauncherStatus;

use crate::model::{status_label, Tone};
use crate::ui::components::Txt;
use crate::ui::theme::*;
use crate::ui::theme::Palette;

/// `(fill, ink)` of a tone: the strong colour on its soft pair, from the palette.
pub fn tone_colors(tone: Tone, p: &Palette) -> (Color, Color) {
    match tone {
        Tone::Neutral => (p.grey.soft, p.text_2),
        Tone::Accent => (p.blue.soft, p.blue.strong),
        Tone::Warning => (p.orange.soft, p.orange.strong),
        Tone::Error => (p.red.soft, p.red.strong),
        Tone::Success => (p.green.soft, p.green.strong),
    }
}

/// A small status pill.
///
/// TODO(dioxus-compose M9, ~10-08): replace with the `Chip`/badge widget.
#[component]
pub fn Badge(#[props(into)] text: String, tone: Tone) -> Element {
    let theme = use_ember_theme();
    let (fill, ink) = tone_colors(tone, theme.palette);
    rsx! {
        dioxus_compose::Box {
            background: paint(fill),
            shape_role: ShapeRole::Full,
            height: H_PILL,
            padding: 0.0,
            alignment: Alignment::Center,
            Row {
                alignment: Alignment::CenterStart,
                Spacer { width: 10.0 }
                Txt { text, type_role: TypeRole::Label, color: paint(ink), max_lines: 1 }
                Spacer { width: 10.0 }
            }
        }
    }
}

/// A dot, e.g. "finished, unread" or a computer's reachability.
///
/// TODO(dioxus-compose M9, ~10-08): replace with the badge widget's dot form.
#[component]
pub fn Dot(tone: Tone) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let fill = match tone {
        Tone::Neutral => p.grey.strong,
        Tone::Accent => p.blue.strong,
        Tone::Warning => p.orange.strong,
        Tone::Error => p.red.strong,
        Tone::Success => p.green.strong,
    };
    rsx! {
        dioxus_compose::Box {
            width: 8.0,
            height: 8.0,
            shape_role: ShapeRole::Full,
            background: paint(fill),
        }
    }
}

/// An indeterminate circular spinner.
#[component]
pub fn Spinner(#[props(default = 14.0)] size: f32) -> Element {
    rsx! {
        ProgressIndicator { determinate: false, circular: true, width: size, height: size }
    }
}

/// A session's status as the launcher shows it (FR-L2): running spinner, waiting-for-approval
/// pill (outranks running), finished-unread dot, finished, failed.
#[component]
pub fn StatusIndicator(status: LauncherStatus) -> Element {
    let label = status_label(status);
    rsx! {
        Row {
            space_role: SpaceRole::Xs,
            alignment: Alignment::CenterStart,
            match status {
                LauncherStatus::Running => rsx! {
                    Spinner {}
                    Txt { text: label, type_role: TypeRole::Caption, color: Paint::Role(ColorRole::Primary) }
                },
                LauncherStatus::WaitingForApproval => rsx! {
                    Badge { text: label, tone: Tone::Warning }
                },
                LauncherStatus::FinishedUnread => rsx! {
                    Dot { tone: Tone::Accent }
                    Txt { text: "Unread", type_role: TypeRole::Caption, color: Paint::Role(ColorRole::Primary) }
                },
                LauncherStatus::Failed => rsx! {
                    Badge { text: label, tone: Tone::Error }
                },
                _ => rsx! {
                    Txt { text: label, type_role: TypeRole::Caption, color: Paint::Role(ColorRole::OnSurfaceVariant) }
                },
            }
        }
    }
}

/// Conversation text.
///
/// TODO(dioxus-compose-markdown, ~10-14): render with the `Markdown` component (and its
/// `MarkdownStream` for streaming replies). Plain text until then.
/// TODO(dioxus-compose M9, ~10-08): wrap in the text selection container so text can be
/// selected and copied.
#[component]
pub fn MessageText(#[props(into)] text: String, #[props(default)] ink: Option<ColorRole>) -> Element {
    rsx! {
        Txt {
            text,
            type_role: TypeRole::Body,
            color: ink.map(Paint::Role),
        }
    }
}

/// Code or tool output in a monospace face.
///
/// TODO(dioxus-compose-markdown, ~10-14): syntax highlighting. TODO(dioxus-compose M9,
/// ~10-08): horizontal scroll for long lines and text selection; long lines wrap until then.
/// TODO(code editor widget, FR-38): read-only editor view for large outputs.
#[component]
pub fn CodeText(#[props(into)] text: String, #[props(default)] max_lines: Option<u32>, #[props(default)] error: bool) -> Element {
    let theme = use_ember_theme();
    rsx! {
        dioxus_compose::Box {
            fill_max_width: true,
            background: paint(theme.palette.surface_2),
            corner_radius: CORNER_SM,
            padding: SP[1],
            Txt {
                text,
                type_role: TypeRole::Mono,
                max_lines,
                overflow: max_lines.map(|_| TextOverflow::Ellipsis),
                color: Paint::Role(if error { ColorRole::Error } else { ColorRole::OnSurfaceVariant }),
            }
        }
    }
}

/// Two panes side by side, the first taking `start_weight` of the width.
///
/// TODO(dioxus-compose, ~10-14): replace with the split pane widget (draggable divider).
#[component]
pub fn SplitPane(start: Element, end: Element, #[props(default = 0.45)] start_weight: f32) -> Element {
    rsx! {
        Row {
            fill_max_width: true,
            fill_max_height: true,
            space_role: SpaceRole::Md,
            Column { weight: start_weight, fill_max_height: true, {start} }
            Divider { vertical: true }
            Column { weight: 1.0 - start_weight, fill_max_height: true, {end} }
        }
    }
}

/// A banner across the top of a screen.
#[component]
pub fn Banner(#[props(into)] text: String, tone: Tone) -> Element {
    let theme = use_ember_theme();
    let (fill, ink) = tone_colors(tone, theme.palette);
    rsx! {
        dioxus_compose::Box {
            fill_max_width: true,
            background: paint(fill),
            corner_radius: CORNER_MD,
            padding: SP[2],
            Txt { text, type_role: TypeRole::Label, color: paint(ink) }
        }
    }
}
