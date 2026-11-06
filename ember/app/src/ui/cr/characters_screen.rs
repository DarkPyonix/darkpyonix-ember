//! The team character screen (FR-L22, INTENT D29, #489): the catalog of the six styles, the
//! 18-member team preview in the chosen style and the editor of one sample member.
//!
//! It is not a sidebar entry (D26, #503): it opens from the project settings sheet and has a
//! back button. What is real: the project's style (read from the server, saved with the
//! project API). What is a preview: the 18 members and the editor's name, harness, role, state
//! and silhouette, which no storage holds yet (TODO(#489): per-member edits).

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, IconRole};

use super::characters::{draw, CharSpec, CharState, CharStyle};
use super::components::{AppButton, ButtonKind, EmberField, FieldOptions, Label, Pill, PillTone, Select};
use super::theme::*;
use crate::services::{run_shared, services};
use crate::ui::characters::{
    characters_project, harness_label, preview_team, state_label, style_apply_needed, supports_pin, team_cols,
    EDITOR_HARNESSES, EDITOR_ROLES, SILHOUETTES,
};
use crate::ui::coexist::shared_ui;
use crate::ui::project::{effective_style, patch_error_message};

/// Sends the style to the project (the one persisted part of this screen).
fn apply_style(project: String, style: CharStyle, saving: MutableState<bool>, message: MutableState<Option<String>>) {
    saving.set(true);
    message.set(None);
    let patch = ember_client::wire::ProjectPatch { character_style: Some(style.id().to_string()), ..Default::default() };
    let sent = patch.clone();
    let ui = shared_ui().clone();
    let for_prefs = project.clone();
    run_shared(
        async move { services().client.patch_project(&project, &patch).await.map(|_| ()) },
        move |r| {
            saving.set(false);
            match r {
                Ok(()) => {
                    ui.update_prefs(|pr| pr.set_character_style(&for_prefs, style));
                    message.set(Some("프로젝트의 캐릭터 스타일을 저장했습니다".to_string()));
                }
                Err(e) => message.set(Some(patch_error_message(e.status(), e.code(), &sent).to_string())),
            }
        },
    );
}

/// `cols` equal columns; a short last row keeps the cell width.
fn Grid<T>(items: &[T], cols: usize, gap: f32, mut cell: impl FnMut(&T)) {
    let cols = cols.max(1);
    cu::Column().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
        for row in items.chunks(cols) {
            cu::Row().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
                for item in row {
                    cu::Column().alignment(Alignment::TopCenter).modifier(Modifier::new().weight(1.0)).content(|| cell(item));
                }
                for _ in row.len()..cols {
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                }
            });
        }
    });
}

fn Card(content: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Column()
        .spacing(SP[2])
        .modifier(Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_LG).padding(SP[3]))
        .content(content);
}

fn Section(title: &str, note: &str, preview: bool, content: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
            Label(title, Fs::Lg, Weight::Semibold, None, Some(1));
            if preview {
                Pill("미리보기", PillTone::Grey, true);
            }
        });
        Label(note, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
        content();
    });
}

fn FieldRow(label: &str, content: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Column().spacing(4.0).modifier(Modifier::new().fill_max_width()).content(|| {
        Label(label, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
        content();
    });
}

fn spec(style: CharStyle, name: &str, agent: &str, role: &str, state: CharState, lead: bool, pin: Option<usize>) -> CharSpec {
    CharSpec {
        style,
        name: name.to_string(),
        agent: agent.to_string(),
        state,
        lead,
        role: Some(role.to_string()),
        dark: ember_theme().scheme == Scheme::Dark,
        pin,
    }
}

pub fn CharactersScreen() {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let width = compose_rust::runtime::current_window_size().width_dp;
    let compact = width <= 560.0;
    let asked = ui.characters.get().unwrap_or_default();
    let svc = crate::services::try_services();
    let names: Vec<String> = svc.as_ref().map(|s| s.client.read(|st| st.projects().into_iter().map(|v| v.name).collect())).unwrap_or_default();
    let project = characters_project(&asked, ui.selected_project.get().as_deref(), &names);
    let project_name = project.clone().unwrap_or_default();
    let server = svc.as_ref().and_then(|s| s.client.read(|st| st.project(&project_name).and_then(|pr| pr.character_style.clone())));
    let current = effective_style(server.as_deref(), ui.prefs.get().saved_character_style(&project_name));

    // The chosen style starts as the project's own; a pick only previews until it is applied.
    let chosen = remember(|| mutable_state_of(None::<CharStyle>));
    let style = chosen.get().unwrap_or(current);
    let saving = remember(|| mutable_state_of(false));
    let message = remember(|| mutable_state_of(None::<String>));
    let ed_name = remember(|| mutable_state_of("ember-리더".to_string()));
    let ed_harness = remember(|| mutable_state_of(0usize));
    let ed_role = remember(|| mutable_state_of(0usize));
    let ed_state = remember(|| mutable_state_of(CharState::Idle));
    let ed_pin = remember(|| mutable_state_of(None::<usize>));

    cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
        cu::Column().spacing(SP[4]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
            // Back to where it was opened from; no sidebar entry (D26).
            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                let u = ui.clone();
                AppButton("뒤로", Some(IconRole::Back), ButtonKind::Default, true, true, false, move || u.close_characters());
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                if compact {
                    cu::Spacer().modifier(Modifier::new().width(super::project::PHONE_MENU_ROOM));
                }
            });
            cu::Column().spacing(4.0).modifier(Modifier::new().fill_max_width()).content(|| {
                Label("팀 캐릭터", if compact { Fs::Xl2 } else { Fs::Xl3 }, Weight::Medium, None, Some(1));
                let sub = match &project {
                    Some(n) => format!("{n} 프로젝트의 팀원은 모두 고른 스타일로 보입니다. 모양과 색은 팀원 이름과 하네스로 정해집니다."),
                    None => "프로젝트가 아직 없어 기본 스타일로 보여 드립니다.".to_string(),
                };
                Label(&sub, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            });

            // 1. The catalog.
            Section("스타일 여섯 가지", "하나를 누르면 아래 미리보기가 바뀝니다. 프로젝트에 쓰려면 적용하세요", false, || {
                let cols = if width > 1000.0 { 3 } else { 2 };
                Grid(&CharStyle::ALL, cols, SP[2], |&s| {
                    Card(|| {
                        cu::Column().alignment(Alignment::TopCenter).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                            draw(&spec(s, "ember-리더", "claude", "리뷰어", CharState::Idle, false, None), 64.0);
                            Label(s.label(), Fs::Md, Weight::Semibold, None, Some(1));
                            Label(s.blurb(), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(3));
                            let c = chosen.clone();
                            let kind = if s == style { ButtonKind::Primary } else { ButtonKind::Default };
                            let text = if s == current { "현재 스타일" } else if s == style { "선택됨" } else { "보기" };
                            let m = message.clone();
                            AppButton(text, None, kind, true, true, false, move || {
                                c.set(Some(s));
                                m.set(None);
                            });
                        });
                    });
                });
                let needed = style_apply_needed(current, style);
                cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    let (pr, sv, ms) = (project.clone(), saving.clone(), message.clone());
                    AppButton(
                        if saving.get() { "저장 중" } else { "이 프로젝트에 적용" },
                        None,
                        ButtonKind::Primary,
                        true,
                        needed && project.is_some() && !saving.get(),
                        false,
                        move || {
                            if let Some(n) = pr.clone() {
                                apply_style(n, style, sv.clone(), ms.clone());
                            }
                        },
                    );
                    let note = match &project {
                        None => "적용할 프로젝트가 없습니다".to_string(),
                        Some(n) => format!("{n}의 현재 스타일은 {}입니다", current.label()),
                    };
                    Label(&note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                });
                if let Some(m) = message.get() {
                    Label(&m, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                }
            });

            // 2. The chosen style across states and harnesses.
            Section(&format!("{}의 상태와 하네스", style.label()), "상태는 정지한 자세로 보입니다. 리더에게는 왕관이 붙습니다", true, || {
                let mut cells: Vec<(CharSpec, &str)> = CharState::ALL
                    .iter()
                    .map(|&s| (spec(style, "ember-리더", "claude", "리뷰어", s, false, None), state_label(s)))
                    .collect();
                for (i, (id, _)) in EDITOR_HARNESSES.iter().enumerate() {
                    cells.push((spec(style, "core-구현", id, ["테스터", "디자인", "문서"][i], CharState::Idle, false, None), harness_label(id)));
                }
                cells.push((spec(style, "ember-리더", "claude", "리더", CharState::Done, true, None), "리더 왕관"));
                Card(|| {
                    Grid(&cells, team_cols(width), SP[2], |(s, label)| {
                        draw(s, 56.0);
                        Label(label, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                });
            });

            // 3. The team preview.
            Section(&format!("팀 미리보기 ({})", style.label()), "예시 팀원 18명을 고른 스타일로 그립니다. 이 프로젝트의 실제 팀원이 아닙니다. 기본 프로젝트에는 리더가 없습니다", true, || {
                let team = preview_team();
                Card(|| {
                    Grid(&team, team_cols(width), SP[3], |m| {
                        draw(&spec(style, &m.name(), m.agent, m.role, m.state, m.crown(), None), 56.0);
                        Label(&m.name(), Fs::Xs, Weight::Semibold, None, Some(1));
                        Label(harness_label(m.agent), Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                });
            });

            // 4. The editor.
            Section("편집기", "이름을 바꾸면 캐릭터가 바뀝니다. 팀원별 수정은 아직 저장되지 않고, 이 화면에서만 보입니다", true, || {
                let name = ed_name.get();
                let shown_name = if name.trim().is_empty() { "이름 없음".to_string() } else { name.trim().to_string() };
                let (agent_id, _) = EDITOR_HARNESSES[ed_harness.get().min(EDITOR_HARNESSES.len() - 1)];
                let role = EDITOR_ROLES[ed_role.get().min(EDITOR_ROLES.len() - 1)];
                let pin = if supports_pin(style) { ed_pin.get() } else { None };
                Card(|| {
                    cu::Column().alignment(Alignment::TopCenter).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                        draw(&spec(style, &shown_name, agent_id, role, ed_state.get(), role == "리더", pin), 160.0);
                        cu::Row().spacing(SP[1]).content(|| {
                            for suffix in ["-2", "-리뷰", "-테스트", "-문서", "-조사"] {
                                draw(&spec(style, &format!("{shown_name}{suffix}"), agent_id, &suffix[1..], CharState::Idle, false, pin), 36.0);
                            }
                        });
                    });
                    FieldRow("이름", || {
                        let n = ed_name.clone();
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            placeholder: "ember-리더".to_string(),
                            text: Some(name.clone()),
                            on_value_change: Some(Box::new(move |v| n.set(v))),
                            ..Default::default()
                        });
                    });
                    FieldRow("하네스", || {
                        let labels: Vec<String> = EDITOR_HARNESSES.iter().map(|(_, l)| l.to_string()).collect();
                        let at = ed_harness.get().min(labels.len() - 1);
                        let h = ed_harness.clone();
                        Select(&labels[at], &labels, at, 260.0, move |i| h.set(i));
                    });
                    FieldRow("역할", || {
                        let labels: Vec<String> = EDITOR_ROLES.iter().map(|r| r.to_string()).collect();
                        let at = ed_role.get().min(labels.len() - 1);
                        let r = ed_role.clone();
                        Select(&labels[at], &labels, at, 260.0, move |i| r.set(i));
                    });
                    FieldRow("상태", || {
                        cu::Row().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
                            for s in CharState::ALL {
                                let st = ed_state.clone();
                                let kind = if ed_state.get() == s { ButtonKind::Primary } else { ButtonKind::Default };
                                AppButton(state_label(s), None, kind, true, true, false, move || st.set(s));
                            }
                        });
                    });
                    FieldRow("실루엣 고정", || {
                        if supports_pin(style) {
                            let mut labels = vec!["자동 (이름이 정합니다)".to_string()];
                            labels.extend(SILHOUETTES.iter().map(|s| s.to_string()));
                            let at = pin.map(|i| i + 1).unwrap_or(0);
                            let pn = ed_pin.clone();
                            Select(&labels[at], &labels, at, 260.0, move |i| pn.set(i.checked_sub(1)));
                        } else {
                            Label(&format!("{}은 실루엣을 고정할 수 없습니다. 말랑과 키링에서 고를 수 있습니다", style.label()), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                        }
                    });
                });
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::{Host, Recomposer};

    fn rendered() -> String {
        let mut host = Host::new(|| Box::new(Recomposer::new(CharactersScreen)));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    #[test]
    fn fr_l22_the_characters_screen_lists_the_catalog_and_marks_previews() {
        let _guard = crate::services::test_guard();
        let text = rendered();
        for s in CharStyle::ALL {
            assert!(text.contains(s.label()), "missing {}", s.label());
        }
        assert!(text.contains("팀 캐릭터"));
        assert!(text.contains("미리보기"));
        assert!(text.contains("편집기"));
        assert!(text.contains("뒤로"));
    }

    #[test]
    fn fr_l22_opening_and_closing_the_screen_is_not_a_menu() {
        let ui = crate::ui::coexist::SharedUi::new(crate::prefs::Prefs::default());
        let menu = ui.menu.get_untracked();
        ui.open_characters("ember");
        assert_eq!(ui.characters.get_untracked().as_deref(), Some("ember"));
        assert_eq!(ui.menu.get_untracked(), menu);
        ui.close_characters();
        assert_eq!(ui.characters.get_untracked(), None);
    }
}
