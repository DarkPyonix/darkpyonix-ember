//! compose-rust version of the drawn team characters (#220 phase 4): [`Character`] and the
//! [`StylePicker`] of the project settings sheet.
//!
//! The drawing itself (`svg`, styles, states, seeds) is pure code in `ui::characters` and is
//! used as it is; only the two components are rewritten.

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::collections::HashMap;

use compose_rust::ui as cu;
use compose_rust::ui::{Modifier, Role};
use compose_rust::{Alignment, AssetKind, TextAlign, TextOverflow, TypeRole};

pub use crate::ui::characters::{
    crown_shown, family_of, svg, CharSpec, CharState, CharStyle, Family,
};

use super::components::{Label, Txt};
use super::theme::*;
use crate::ui::coexist::shared_ui;

thread_local! {
    /// Drawn documents by their text. The toolkit takes `'static` bytes, so each distinct
    /// character (style, name, state, size, scheme) is leaked once and reused.
    static DOCS: RefCell<HashMap<String, &'static [u8]>> = RefCell::new(HashMap::new());
}

fn doc_bytes(doc: String) -> &'static [u8] {
    DOCS.with(|cache| {
        *cache
            .borrow_mut()
            .entry(doc.clone())
            .or_insert_with(|| Box::leak(doc.into_bytes().into_boxed_slice()))
    })
}

/// A drawn character. `project` picks the project's style from prefs and decides the crown
/// (never in 기본 프로젝트); an empty project means the default style.
pub fn Character(
    agent: &str,
    leader: bool,
    size: f32,
    name: &str,
    project: &str,
    state: CharState,
    role: Option<&str>,
) {
    let theme = ember_theme();
    // The server's value first, this device's choice only as a fallback (FR-L26).
    let (server, server_overrides) = crate::services::try_services()
        .map(|s| {
            s.client.read(|st| {
                st.project(project)
                    .map(|p| (p.character_style.clone(), Some(p.character_overrides.clone())))
                    .unwrap_or((None, None))
            })
        })
        .unwrap_or((None, None));
    let prefs = shared_ui().prefs.get();
    let style = crate::ui::project::effective_style(server.as_deref(), prefs.saved_character_style(project));
    let overrides = crate::ui::characters::effective_character_overrides(
        server_overrides.as_ref(),
        prefs.saved_character_overrides(project),
    );
    let base = CharSpec {
        style,
        name: name.to_string(),
        agent: agent.to_string(),
        state,
        lead: crown_shown(project, leader),
        role: role.map(str::to_string),
        dark: theme.scheme == Scheme::Dark,
        pin: None,
    };
    let spec = crate::ui::characters::apply_character_override(base, project, overrides.get(name));
    draw(&spec, size);
}

pub fn draw(spec: &CharSpec, size: f32) {
    // The root fills the image box (SVG root sizes are pixels, so a dp number drew small on
    // Android, compose-rust#677); the modifier sets the box.
    let doc = svg(spec).replacen("width=\"100\" height=\"100\"", crate::ui::shell::FILL_BOX, 1);
    let id = compose_rust::asset(AssetKind::Svg, doc_bytes(doc));
    cu::Image(id).modifier(Modifier::new().size(size, size));
}

/// The gap between style cards (`.cs-grid` gap `--sp2`).
pub const STYLE_CARD_GAP: f32 = 8.0;
/// A style card's padding (`.cs-card` padding `--sp3`).
pub const STYLE_CARD_PAD: f32 = 12.0;
/// The character box of a card (`.cs-art` 56 px) and the character in it (48 px).
pub const STYLE_CARD_ART: (f32, f32) = (56.0, 48.0);
/// The gap between the art, the name and the description (`.cs-card` gap `--sp1`).
pub const STYLE_CARD_INNER_GAP: f32 = 4.0;
/// The description's lines: two at 12/18, so every card of a row is one height.
pub const STYLE_CARD_DESC_LINES: u32 = 2;
/// One card's height: padding, art, name (13/20), two description lines (12/18), padding.
pub const STYLE_CARD_H: f32 = STYLE_CARD_PAD
    + STYLE_CARD_ART.0
    + STYLE_CARD_INNER_GAP
    + 20.0
    + STYLE_CARD_INNER_GAP
    + 18.0 * STYLE_CARD_DESC_LINES as f32
    + STYLE_CARD_PAD;

/// The picker's columns: three, two at a window 560 dp wide or narrower (`.cs-grid` and its
/// `@media (max-width:560px)`). A window that has not reported its width counts as wide.
pub fn style_picker_columns(window_width_dp: f32) -> usize {
    if window_width_dp > 0.0 && window_width_dp <= 560.0 {
        2
    } else {
        3
    }
}

/// The picker's height for `columns`: its rows of cards and the gaps between them.
pub fn style_picker_height(columns: usize) -> f32 {
    let rows = CharStyle::ALL.len().div_ceil(columns.max(1));
    rows as f32 * STYLE_CARD_H + rows.saturating_sub(1) as f32 * STYLE_CARD_GAP
}

/// The 팀 캐릭터 picker of the 새 프로젝트 and project settings sheets: the mockup's six
/// `.cs-card`s in a grid of three columns (two at 560 dp and below), FR-L26.1. The whole card is
/// a radio button; a pick goes to `on_pick`, and the sheet keeps it in its draft and saves it
/// with 저장 or 만들기 (FR-L26).
pub fn StylePicker(seed_name: &str, current: CharStyle, on_pick: impl FnMut(CharStyle) + Clone + 'static) {
    let columns = style_picker_columns(crate::ui::window_size::current_window_size().width_dp);
    StyleGrid(seed_name, current, columns, on_pick);
}

fn StyleGrid(seed_name: &str, current: CharStyle, columns: usize, on_pick: impl FnMut(CharStyle) + Clone + 'static) {
    cu::Column().spacing(STYLE_CARD_GAP).modifier(Modifier::new().fill_max_width()).content(|| {
        for row in CharStyle::ALL.chunks(columns.max(1)) {
            cu::Row().spacing(STYLE_CARD_GAP).modifier(Modifier::new().fill_max_width()).content(|| {
                for &style in row {
                    let mut pick = on_pick.clone();
                    StyleCard(seed_name, style, style == current, move || pick(style));
                }
                // A short last row keeps the cards at the grid's column width.
                for _ in row.len()..columns.max(1) {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                }
            });
        }
    });
}

/// One `.cs-card`: the character, the style name and its description, chosen or not.
fn StyleCard(seed_name: &str, style: CharStyle, chosen: bool, on_click: impl FnMut() + 'static) {
    let theme = ember_theme();
    let p = theme.palette;
    let dark = theme.scheme == Scheme::Dark;
    let (fill, line) = super::components::card_colors(chosen, p);
    let card = Modifier::new()
        .weight(1.0)
        .height(STYLE_CARD_H)
        .background(paint(fill))
        .border(1.0, paint(line))
        .corner_radius(CORNER_MD)
        .role(Role::RadioButton)
        .checked(chosen)
        .label(style.label());
    super::components::Clickable(card, Alignment::TopCenter, on_click, || {
        cu::Column()
            .alignment(Alignment::TopCenter)
            .spacing(STYLE_CARD_INNER_GAP)
            .modifier(Modifier::new().fill_max_width().padding(STYLE_CARD_PAD))
            .content(|| {
                cu::Box()
                    .alignment(Alignment::Center)
                    .modifier(Modifier::new().size(STYLE_CARD_ART.0, STYLE_CARD_ART.0))
                    .content(|| {
                        draw(
                            &CharSpec {
                                style,
                                name: seed_name.to_string(),
                                agent: "claude".into(),
                                state: CharState::Idle,
                                lead: false,
                                role: Some("리뷰어".into()),
                                dark,
                                pin: None,
                            },
                            STYLE_CARD_ART.1,
                        );
                    });
                Label(style.label(), Fs::Sm, Weight::Semibold, None, Some(1));
                // `.cs-s`: 12/18 muted, centred like the card (`text-align:center`).
                Txt(
                    style.blurb(),
                    TypeRole::Caption,
                    Some(paint(p.muted)),
                    Some(STYLE_CARD_DESC_LINES),
                    Some(TextOverflow::Ellipsis),
                    None,
                    Some(TextAlign::Center),
                );
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::{Host, Recomposer};

    #[test]
    fn fr_l24_documents_are_cached_by_text() {
        let a = doc_bytes("<svg>a</svg>".to_string());
        let b = doc_bytes("<svg>a</svg>".to_string());
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn fr_l26_1_picker_is_three_columns_and_two_at_560_and_below() {
        assert_eq!(style_picker_columns(1440.0), 3);
        assert_eq!(style_picker_columns(561.0), 3);
        assert_eq!(style_picker_columns(560.0), 2);
        assert_eq!(style_picker_columns(390.0), 2);
        assert_eq!(style_picker_columns(0.0), 3, "an unreported window counts as wide");
        // 12 + 56 + 4 + 20 + 4 + 2 x 18 + 12.
        assert_eq!(STYLE_CARD_H, 144.0);
        assert_eq!(style_picker_height(3), 2.0 * 144.0 + 8.0);
        assert_eq!(style_picker_height(2), 3.0 * 144.0 + 16.0);
    }

    #[test]
    fn fr_l26_1_each_style_is_one_bordered_clickable_card() {
        use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
        use compose_rust::PropertyKind;
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| StyleGrid("ember", CharStyle::Crew, 3, |_| {}))));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let clicks = batch
            .iter()
            .filter(|m| matches!(m, Mutation::SetProp { property: PropertyKind::OnClick, value: PropertyValue::Integer(_), .. }))
            .count();
        assert_eq!(clicks, CharStyle::ALL.len(), "the card is the control, no inner button");
        let mods: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { modifier, .. } => Some(format!("{modifier:?}")),
                _ => None,
            })
            .collect();
        assert_eq!(mods.iter().filter(|m| *m == "Height(144.0)").count(), CharStyle::ALL.len(), "{mods:?}");
        assert!(mods.iter().filter(|m| m.starts_with("Border")).count() >= CharStyle::ALL.len(), "{mods:?}");
        assert!(mods.iter().any(|m| m == "Padding(12.0)"), "{mods:?}");
        assert!(mods.iter().any(|m| m.starts_with("Size")), "the 56 px art box: {mods:?}");
        let buttons = batch
            .iter()
            .filter(|m| matches!(m, Mutation::Create { widget, .. } if format!("{widget:?}") == "Button"))
            .count();
        assert_eq!(buttons, 0, "no AppButton inside the card");
        let src = include_str!("characters.rs");
        let card = &src[src.find("fn StyleCard(").unwrap()..];
        assert!(card.contains(".role(Role::RadioButton)") && card.contains(".checked(chosen)"));
        assert!(card.contains("card_colors(chosen, p)"));
    }

    #[test]
    fn fr_l24_the_style_picker_lists_every_style_label() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| StylePicker("ember", CharStyle::Crew, |_| {}))));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let text: String = batch.iter().map(|m| format!("{m:?}")).collect();
        for style in CharStyle::ALL {
            assert!(text.contains(style.label()), "missing {}", style.label());
        }
    }
}
