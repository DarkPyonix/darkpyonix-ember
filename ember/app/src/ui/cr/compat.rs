//! compose-rust version of `ui::compat`: small widgets the screens share (badge, dot, spinner,
//! status indicator, message and code text, banner).
//!
//! compose-rust has `Badge` and a text selection container of its own, but their looks do not
//! match the mockup tokens yet, so these stay Ember widgets built from the same primitives as
//! the Dioxus ones. Each says what would replace it. A split pane is the toolkit's
//! `compose_rust::ui::SplitPane` (compose-rust#27, in the v0.0.11 pin); the fixed two-column
//! stand-in that was here had no caller and is gone (#673).

#![allow(non_snake_case)]

#![allow(non_snake_case)]

use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, Color, ColorRole, Paint, ShapeRole, SpaceRole, TextOverflow, TypeRole};
use ember_client::LauncherStatus;

use super::components::Txt;
use super::theme::*;
use super::theme::Palette;
use crate::model::Tone;

/// `(fill, ink)` of a tone: the strong colour on its soft pair, from the palette.
pub fn tone_colors(tone: Tone, p: &Palette) -> (Color, Color) {
    match tone {
        Tone::Neutral => (p.grey.soft, p.text_2),
        Tone::Accent => (p.blue.soft, p.blue.strong),
        // The orange accent on its soft pair is about 1.8:1 in light; the text is the accent
        // pulled toward the page text colour so a pill reads.
        Tone::Warning => (p.orange.soft, mix(p.orange.strong, p.text, 0.45)),
        Tone::Error => (p.red.soft, p.red.strong),
        Tone::Success => (p.green.soft, p.green.strong),
    }
}

/// `a` moved `t` of the way to `b`, opaque.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let (a, b) = (a.to_argb(), b.to_argb());
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        (x + (y - x) * t).round() as u32
    };
    Color::rgb((ch(16) << 16) | (ch(8) << 8) | ch(0))
}

/// The strong colour of a tone, for dots.
pub fn tone_strong(tone: Tone, p: &Palette) -> Color {
    match tone {
        Tone::Neutral => p.grey.strong,
        Tone::Accent => p.blue.strong,
        Tone::Warning => p.orange.strong,
        Tone::Error => p.red.strong,
        Tone::Success => p.green.strong,
    }
}

/// A small status pill.
pub fn Badge(text: &str, tone: Tone) {
    let theme = ember_theme();
    let (fill, ink) = tone_colors(tone, theme.palette);
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().height(H_PILL).background(paint(fill)).shape_role(ShapeRole::Full), {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
            Txt(text, TypeRole::Label, Some(paint(ink)), Some(1), None, None, None);
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
        }) };
}

/// A dot, e.g. "finished, unread" or a computer's reachability.
pub fn Dot(tone: Tone) {
    let theme = ember_theme();
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .size(8.0, 8.0)
                .background(paint(tone_strong(tone, theme.palette)))
                .shape_role(ShapeRole::Full), {}) };
}

/// An indeterminate circular spinner, 12 px as in the mockup. The ring's stroke scales with its
/// size (compose-rust#377); the mockup's `--c-blue-soft` track has no builder, so the ring has
/// the design system's track.
pub fn Spinner(size: f32) {
    { use ::compose_rust::ui::IndeterminateProgressIndicator; IndeterminateProgressIndicator!(circular = true, modifier = Modifier::new().size(size, size)) };
}

/// A session's status as the launcher shows it (FR-L2): running spinner, waiting-for-approval
/// pill (outranks running), finished-unread dot, finished, failed.
pub fn StatusIndicator(status: LauncherStatus) {
    let label = super::ko::status_label(status);
    { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Xs, alignment = Alignment::CenterStart, content = || match status {
        LauncherStatus::Running => {
            Spinner(12.0);
            Txt(&label, TypeRole::Caption, Some(Paint::Role(ColorRole::Primary)), None, None, None, None);
        }
        LauncherStatus::WaitingForApproval => Badge(&label, Tone::Warning),
        LauncherStatus::FinishedUnread => {
            Dot(Tone::Accent);
            Txt(&label, TypeRole::Caption, Some(Paint::Role(ColorRole::Primary)), None, None, None, None);
        }
        LauncherStatus::Failed => Badge(&label, Tone::Error),
        _ => Txt(&label, TypeRole::Caption, Some(Paint::Role(ColorRole::OnSurfaceVariant)), None, None, None, None),
    }) };
}

fn status_tone(status: LauncherStatus) -> Tone {
    match status {
        LauncherStatus::Running | LauncherStatus::FinishedUnread => Tone::Accent,
        LauncherStatus::WaitingForApproval => Tone::Warning,
        LauncherStatus::Failed => Tone::Error,
        LauncherStatus::Finished => Tone::Success,
        LauncherStatus::Idle | LauncherStatus::Unknown => Tone::Neutral,
    }
}

/// The 8 px status dot of a session row (`sdot`).
pub fn StatusDot(status: LauncherStatus) {
    Dot(status_tone(status));
}

/// The Korean status pill of a session row (`pill`).
pub fn StatusPill(status: LauncherStatus) {
    Badge(super::ko::status_label(status), status_tone(status));
}

/// Conversation text.
///
/// TODO(markdown): render with a markdown component (and its stream form for streaming
/// replies) once compose-rust has one. Plain text until then. The text sits in a
/// `SelectionContainer` so it can be selected and copied (compose-rust#5).
pub fn MessageText(text: &str, ink: Option<ColorRole>) {
    { use ::compose_rust::ui::SelectionContainer; SelectionContainer!({
        Txt(text, TypeRole::Body, ink.map(Paint::Role), None, None, None, None);
    }) };
}

/// Code or tool output in a monospace face.
///
/// TODO(markdown): syntax highlighting. TODO(FR-38): read-only editor view for large outputs.
pub fn CodeText(text: &str, max_lines: Option<u32>, error: bool) {
    let theme = ember_theme();
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .fill_max_width()
                .background(paint(theme.palette.surface_2))
                .corner_radius(CORNER_SM)
                .padding(PAD_ROW_V), {
            // `.inset` pads 12 16; the uniform 12 above plus this 4 makes the left edge 16.
            { use ::compose_rust::ui::ScrollRow; ScrollRow!(modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                Txt(
                    text,
                    TypeRole::Mono,
                    Some(Paint::Role(if error { ColorRole::Error } else { ColorRole::OnSurfaceVariant })),
                    max_lines,
                    max_lines.map(|_| TextOverflow::Ellipsis),
                    None,
                    None,
                );
            }) };
        }) };
}

/// A banner across the top of a screen.
pub fn Banner(text: &str, tone: Tone) {
    let theme = ember_theme();
    let (fill, ink) = tone_colors(tone, theme.palette);
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .fill_max_width()
                .background(paint(fill))
                .corner_radius(CORNER_MD)
                .padding(SP[2]), {
            Txt(text, TypeRole::Label, Some(paint(ink)), None, None, None, None);
        }) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    #[test]
    fn tone_colours_pair_strong_ink_with_soft_fill() {
        assert_eq!(tone_colors(Tone::Error, &LIGHT), (LIGHT.red.soft, LIGHT.red.strong));
        assert_eq!(tone_colors(Tone::Neutral, &DARK), (DARK.grey.soft, DARK.text_2));
        assert_eq!(tone_strong(Tone::Success, &DARK), DARK.green.strong);
    }

    #[test]
    fn fr_l2_status_indicator_shows_the_waiting_pill() {
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                StatusIndicator(LauncherStatus::WaitingForApproval);
                CodeText("cargo test", Some(3), false);
                Banner("hi", Tone::Accent);
                Dot(Tone::Neutral);
            }))
        });
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let want = crate::ui::cr::ko::status_label(LauncherStatus::WaitingForApproval);
        let has = |s: &str| {
            batch.iter().any(|m| {
                matches!(m, Mutation::SetProp { value: PropertyValue::String(t), .. } if *t == s)
            })
        };
        assert!(has(&want) && has("cargo test") && has("hi"), "{batch:?}");
    }

    #[test]
    fn no_stand_in_split_pane_is_left() {
        // compose-rust#27 (#673): the toolkit `SplitPane` is in the v0.0.11 pin and nothing called
        // the fixed two-column stand-in, so it is gone. A screen that needs a divider takes
        // `compose_rust::ui::SplitPane`.
        let src = include_str!("compat.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(!production.contains(concat!("fn Split", "Pane(")));
        assert!(!production.contains(concat!("TODO(compose-rust#", "27)")));
    }

    #[test]
    fn message_text_can_be_selected_and_copied() {
        // compose-rust#5: the conversation text sits in a SelectionContainer.
        let mut host = Host::new(|| Box::new(Recomposer::new(|| MessageText("hello", None))));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert!(
            batch.iter().any(|m| matches!(m, Mutation::Create { widget: compose_rust::WidgetKind::SelectionContainer, .. })),
            "{batch:?}"
        );
    }

    #[test]
    fn code_text_scrolls_horizontally_in_a_scroll_row() {
        // #673: long monospace lines scroll sideways inside a toolkit ScrollRow instead of wrapping.
        let mut host = Host::new(|| Box::new(Recomposer::new(|| CodeText("cargo test --locked", None, false))));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert!(
            batch.iter().any(|m| matches!(m, Mutation::Create { widget: compose_rust::WidgetKind::ScrollRow, .. })),
            "{batch:?}"
        );
    }
}
