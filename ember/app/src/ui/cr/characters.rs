//! compose-rust version of the drawn team characters (#220 phase 4): [`Character`] and the
//! [`StylePicker`] of the project settings sheet.
//!
//! The drawing itself (`svg`, styles, states, seeds) is pure code in `ui::characters` and is
//! used as it is; only the two components are rewritten.

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::collections::HashMap;

use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, AssetKind, SpaceRole};

pub use crate::ui::characters::{
    crown_shown, family_of, svg, CharSpec, CharState, CharStyle, Family,
};

use super::components::{AppButton, ButtonKind, Label};
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
    let style = shared_ui().prefs.get().character_style(project);
    let spec = CharSpec {
        style,
        name: name.to_string(),
        agent: agent.to_string(),
        state,
        lead: crown_shown(project, leader),
        role: role.map(str::to_string),
        dark: theme.scheme == Scheme::Dark,
    };
    draw(&spec, size);
}

fn draw(spec: &CharSpec, size: f32) {
    // The renderer draws an SVG at its own width and height, so the pixel size goes in.
    let px = format!("{}", size.round() as i32);
    let doc = svg(spec).replacen("width=\"100\" height=\"100\"", &format!("width=\"{px}\" height=\"{px}\""), 1);
    let id = compose_rust::asset(AssetKind::Svg, doc_bytes(doc));
    cu::Image(id).modifier(Modifier::new().size(size, size));
}

/// The 팀 캐릭터 picker of the project settings sheet: six preview cards, three rows of two, so
/// the Korean names are not clipped. The choice is saved to prefs at once.
/// TODO(server): save it on the project instead.
pub fn StylePicker(project_id: &str, seed_name: &str) {
    let theme = ember_theme();
    let ui = shared_ui().clone();
    let p = theme.palette;
    let current = ui.prefs.get().character_style(project_id);
    let dark = theme.scheme == Scheme::Dark;
    cu::Column().space_role(SpaceRole::Sm).modifier(Modifier::new().fill_max_width()).content(|| {
        for row in CharStyle::ALL.chunks(2) {
            cu::Row().space_role(SpaceRole::Sm).modifier(Modifier::new().fill_max_width()).content(|| {
                for &style in row {
                    cu::Column()
                        .alignment(Alignment::TopCenter)
                        .space_role(SpaceRole::Xs)
                        .modifier(Modifier::new().weight(1.0))
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
                                },
                                56.0,
                            );
                            let (ui, pid) = (ui.clone(), project_id.to_string());
                            let kind = if style == current { ButtonKind::Primary } else { ButtonKind::Default };
                            AppButton(style.label(), None, kind, true, true, false, move || {
                                ui.update_prefs(|pr| pr.set_character_style(&pid, style));
                            });
                            Label(style.blurb(), Fs::Xs2, Weight::Regular, Some(paint(p.text_2)), Some(3));
                        });
                }
            });
        }
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
    fn fr_l24_the_style_picker_lists_every_style_label() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| StylePicker("p1", "ember"))));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let text: String = batch.iter().map(|m| format!("{m:?}")).collect();
        for style in CharStyle::ALL {
            assert!(text.contains(style.label()), "missing {}", style.label());
        }
    }
}
