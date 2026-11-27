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

use super::components::{resolve_paint, Label, Txt};
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

/// A parsed top-level Markdown block for conversation message rendering (FR-L5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlock {
    Paragraph(String),
    Heading { level: u8, text: String },
    Code { lang: Option<String>, code: String },
    ListItem { marker: String, text: String },
    Blockquote(String),
}

struct FenceState<'a> {
    ch: u8,
    len: usize,
    lang: Option<String>,
    lines: Vec<&'a str>,
}

fn parse_fence_open(line: &str) -> Option<(u8, usize, Option<String>)> {
    let trimmed = line.trim_start();
    let first = *trimmed.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let len = trimmed.bytes().take_while(|&b| b == first).count();
    if len < 3 {
        return None;
    }
    let info = trimmed[len..].trim();
    if first == b'`' && info.contains('`') {
        return None;
    }
    let lang = if info.is_empty() { None } else { Some(info.to_string()) };
    Some((first, len, lang))
}

fn is_closing_fence(line: &str, ch: u8, min_len: usize) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= min_len && trimmed.bytes().all(|b| b == ch)
}

fn parse_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        let after = &trimmed[hashes..];
        if let Some(rest) = after.strip_prefix(' ').or_else(|| after.strip_prefix('\t')) {
            let text = rest.trim();
            if !text.is_empty() {
                return Some((hashes as u8, text.to_string()));
            }
        }
    }
    None
}

fn parse_list_item(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let text = rest.trim();
            if !text.is_empty() {
                return Some(("•".to_string(), text.to_string()));
            }
        }
    }
    let digits_len = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_len > 0 {
        let after_digits = &trimmed[digits_len..];
        if let Some(rest) = after_digits.strip_prefix(". ") {
            let text = rest.trim();
            if !text.is_empty() {
                return Some((format!("{}.", &trimmed[..digits_len]), text.to_string()));
            }
        }
    }
    None
}

fn parse_blockquote(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("> ") {
        Some(rest)
    } else if trimmed == ">" {
        Some("")
    } else {
        None
    }
}

/// Parse conversation text into top-level Markdown blocks (FR-L5).
///
/// Plain text without block-level syntax yields a single [`MdBlock::Paragraph`], and unclosed
/// code fences are preserved as [`MdBlock::Code`] so streaming replies render cleanly.
pub fn parse_markdown_blocks(text: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut para_lines: Vec<&str> = Vec::new();
    let mut quote_lines: Vec<&str> = Vec::new();
    let mut fence: Option<FenceState<'_>> = None;

    let flush_para = |blocks: &mut Vec<MdBlock>, para_lines: &mut Vec<&str>| {
        if !para_lines.is_empty() {
            blocks.push(MdBlock::Paragraph(para_lines.join("\n")));
            para_lines.clear();
        }
    };
    let flush_quote = |blocks: &mut Vec<MdBlock>, quote_lines: &mut Vec<&str>| {
        if !quote_lines.is_empty() {
            blocks.push(MdBlock::Blockquote(quote_lines.join("\n")));
            quote_lines.clear();
        }
    };

    for line in text.lines() {
        if let Some(ref mut state) = fence {
            if !is_closing_fence(line, state.ch, state.len) {
                state.lines.push(line);
                continue;
            }
        }
        if let Some(done) = fence.take() {
            blocks.push(MdBlock::Code { lang: done.lang, code: done.lines.join("\n") });
            continue;
        }

        if line.trim().is_empty() {
            flush_para(&mut blocks, &mut para_lines);
            flush_quote(&mut blocks, &mut quote_lines);
            continue;
        }

        if let Some((ch, len, lang)) = parse_fence_open(line) {
            flush_para(&mut blocks, &mut para_lines);
            flush_quote(&mut blocks, &mut quote_lines);
            fence = Some(FenceState { ch, len, lang, lines: Vec::new() });
            continue;
        }

        if let Some((level, heading)) = parse_heading(line) {
            flush_para(&mut blocks, &mut para_lines);
            flush_quote(&mut blocks, &mut quote_lines);
            blocks.push(MdBlock::Heading { level, text: heading });
            continue;
        }

        if let Some(quote) = parse_blockquote(line) {
            flush_para(&mut blocks, &mut para_lines);
            quote_lines.push(quote);
            continue;
        }

        if let Some((marker, item)) = parse_list_item(line) {
            flush_para(&mut blocks, &mut para_lines);
            flush_quote(&mut blocks, &mut quote_lines);
            blocks.push(MdBlock::ListItem { marker, text: item });
            continue;
        }

        flush_quote(&mut blocks, &mut quote_lines);
        para_lines.push(line);
    }

    if let Some(done) = fence {
        blocks.push(MdBlock::Code { lang: done.lang, code: done.lines.join("\n") });
    }
    flush_para(&mut blocks, &mut para_lines);
    flush_quote(&mut blocks, &mut quote_lines);
    blocks
}

/// Conversation text rendered as structured Markdown blocks (FR-L5).
///
/// The text sits in a `SelectionContainer` so it can be selected and copied (compose-rust#5).
/// Inline formatting and syntax highlighting remain future work.
pub fn MessageText(text: &str, ink: Option<ColorRole>) {
    let blocks = parse_markdown_blocks(text);
    let paint_ink = ink.map(Paint::Role);
    { use ::compose_rust::ui::SelectionContainer; SelectionContainer!({
        match blocks.as_slice() {
            [] => {
                Txt(text, TypeRole::Body, paint_ink, None, None, None, None);
            }
            [MdBlock::Paragraph(body)] => {
                Txt(body, TypeRole::Body, paint_ink, None, None, None, None);
            }
            _ => {
                let p = ember_theme().palette;
                let heading_color = paint_ink.map(|c| resolve_paint(c, p));
                { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                    for block in &blocks {
                        match block {
                            MdBlock::Paragraph(body) => {
                                Txt(body, TypeRole::Body, paint_ink, None, None, None, None);
                            }
                            MdBlock::Heading { level, text: heading } => {
                                let fs = match level {
                                    1 => Fs::Lg,
                                    2 => Fs::Md,
                                    _ => Fs::Sm,
                                };
                                Label(heading, fs, Weight::Semibold, heading_color, None);
                            }
                            MdBlock::Code { lang, code } => {
                                if let Some(lang) = lang.as_deref().filter(|l| !l.is_empty()) {
                                    { use ::compose_rust::ui::Column; Column!(spacing = 2.0, modifier = Modifier::new().fill_max_width(), {
                                        Label(lang, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                                        CodeText(code, None, false);
                                    }) };
                                } else {
                                    CodeText(code, None, false);
                                }
                            }
                            MdBlock::ListItem { marker, text: item } => {
                                { use ::compose_rust::ui::Row; Row!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                                    Txt(marker, TypeRole::Body, paint_ink, None, None, None, None);
                                    Txt(item, TypeRole::Body, paint_ink, None, None, Some(1.0), None);
                                }) };
                            }
                            MdBlock::Blockquote(quote) => {
                                { use ::compose_rust::ui::Row; Row!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new()
                                        .width(3.0)
                                        .min_height(18.0)
                                        .background(paint(p.muted))
                                        .shape_role(ShapeRole::Full)) };
                                    Txt(quote, TypeRole::Body, Some(paint(p.muted)), None, None, Some(1.0), None);
                                }) };
                            }
                        }
                    }
                }) };
            }
        }
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

    #[test]
    fn fr_l5_parse_markdown_plain_paragraph() {
        assert_eq!(
            parse_markdown_blocks("hello world"),
            vec![MdBlock::Paragraph("hello world".to_string())]
        );
        assert_eq!(
            parse_markdown_blocks("line one\nline two"),
            vec![MdBlock::Paragraph("line one\nline two".to_string())]
        );
        assert_eq!(
            parse_markdown_blocks("first para\n\nsecond para"),
            vec![
                MdBlock::Paragraph("first para".to_string()),
                MdBlock::Paragraph("second para".to_string()),
            ]
        );
        assert_eq!(parse_markdown_blocks(""), vec![]);
    }

    #[test]
    fn fr_l5_parse_markdown_headings() {
        let md = "# Top\n## Mid\n### Sub\n###### Deep\n#1078 not a heading";
        assert_eq!(
            parse_markdown_blocks(md),
            vec![
                MdBlock::Heading { level: 1, text: "Top".to_string() },
                MdBlock::Heading { level: 2, text: "Mid".to_string() },
                MdBlock::Heading { level: 3, text: "Sub".to_string() },
                MdBlock::Heading { level: 6, text: "Deep".to_string() },
                MdBlock::Paragraph("#1078 not a heading".to_string()),
            ]
        );
    }

    #[test]
    fn fr_l5_parse_markdown_fenced_code_blocks() {
        let closed = "```rust\nfn main() {}\n```\n~~~sh\necho hi\n~~~";
        assert_eq!(
            parse_markdown_blocks(closed),
            vec![
                MdBlock::Code { lang: Some("rust".to_string()), code: "fn main() {}".to_string() },
                MdBlock::Code { lang: Some("sh".to_string()), code: "echo hi".to_string() },
            ]
        );

        // Streaming reply with an unclosed code fence still yields a Code block.
        let streaming = "```python\nx = 1\ny = 2";
        assert_eq!(
            parse_markdown_blocks(streaming),
            vec![MdBlock::Code {
                lang: Some("python".to_string()),
                code: "x = 1\ny = 2".to_string(),
            }]
        );
    }

    #[test]
    fn fr_l5_parse_markdown_lists_and_blockquotes() {
        let md = "- alpha\n* beta\n+ gamma\n1. first\n2. second\n> quoted one\n> quoted two";
        assert_eq!(
            parse_markdown_blocks(md),
            vec![
                MdBlock::ListItem { marker: "•".to_string(), text: "alpha".to_string() },
                MdBlock::ListItem { marker: "•".to_string(), text: "beta".to_string() },
                MdBlock::ListItem { marker: "•".to_string(), text: "gamma".to_string() },
                MdBlock::ListItem { marker: "1.".to_string(), text: "first".to_string() },
                MdBlock::ListItem { marker: "2.".to_string(), text: "second".to_string() },
                MdBlock::Blockquote("quoted one\nquoted two".to_string()),
            ]
        );
    }

    #[test]
    fn fr_l5_parse_markdown_mixed_document() {
        let md = "# Summary\n\nIntro line.\n\n```rust\nlet n = 42;\n```\n\n- check build\n- ship fix\n\n> verified on ci";
        assert_eq!(
            parse_markdown_blocks(md),
            vec![
                MdBlock::Heading { level: 1, text: "Summary".to_string() },
                MdBlock::Paragraph("Intro line.".to_string()),
                MdBlock::Code { lang: Some("rust".to_string()), code: "let n = 42;".to_string() },
                MdBlock::ListItem { marker: "•".to_string(), text: "check build".to_string() },
                MdBlock::ListItem { marker: "•".to_string(), text: "ship fix".to_string() },
                MdBlock::Blockquote("verified on ci".to_string()),
            ]
        );
    }

    #[test]
    fn fr_l5_message_text_renders_mixed_markdown_blocks() {
        const MD: &str = "# Plan\n\n```rust\nfn ok() {}\n```\n\n- step one\n\n> note here";
        let mut host = Host::new(|| Box::new(Recomposer::new(|| MessageText(MD, None))));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let has_widget = |pick: fn(&compose_rust::WidgetKind) -> bool| {
            batch.iter().any(|m| matches!(m, Mutation::Create { widget, .. } if pick(widget)))
        };
        let has_text = |s: &str| {
            batch.iter().any(|m| {
                matches!(m, Mutation::SetProp { value: PropertyValue::String(t), .. } if *t == s)
            })
        };
        assert!(has_widget(|w| matches!(w, compose_rust::WidgetKind::SelectionContainer)), "{batch:?}");
        assert!(has_widget(|w| matches!(w, compose_rust::WidgetKind::ScrollRow)), "{batch:?}");
        assert!(has_text("Plan"), "{batch:?}");
        assert!(has_text("rust"), "{batch:?}");
        assert!(has_text("fn ok() {}"), "{batch:?}");
        assert!(has_text("•"), "{batch:?}");
        assert!(has_text("step one"), "{batch:?}");
        assert!(has_text("note here"), "{batch:?}");
    }
}
