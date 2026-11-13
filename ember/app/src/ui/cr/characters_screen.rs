//! The team character screen (FR-L22, INTENT D29, #489), laid out as the mockup
//! `characters.html`: the six styles as numbered sections of nine cells, the team preview with
//! its style switch, and the editor of one member (a stage and a form).
//!
//! It is not a sidebar entry (D26, #503): it opens from the project settings sheet and has a
//! back button (the mockup reaches it only from its design index). What is real: the project's
//! style (read from the server, saved with the project API), the members of the team preview
//! (the project's sessions), and the editor's per-member overrides (name, harness, role, style,
//! state and silhouette), saved through `PATCH /api/projects/{name}` (`character_overrides`,
//! #489). What is a preview: the sample team of a project without members.

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
    apply_character_override, build_character_override, characters_project, editor_save_needed, effective_character_overrides,
    harness_label, state_label, style_apply_needed, supports_pin, team_cols, team_preview, updated_character_overrides,
    with_topic, EDITOR_HARNESSES, EDITOR_ROLES, SILHOUETTES,
};
use crate::ui::coexist::shared_ui;
use crate::ui::project::{effective_style, patch_error_message};

/// Sends the style to the project (FR-L26).
fn apply_style(project: String, style: CharStyle, saving: MutableState<bool>, message: MutableState<Option<String>>) {
    saving.set(true);
    message.set(None);
    let patch = ember_client::wire::ProjectPatch { character_style: Some(style.id().to_string()), ..Default::default() };
    let sent = patch.clone();
    let ui = shared_ui().clone();
    let for_prefs = project.clone();
    run_shared(
        async move { services().patch_project(&project, &patch).await.map(|_| ()) },
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

/// Sends per-member character overrides to the project (`PATCH /api/projects/{name}`, FR-L22, #489).
fn save_character_override(
    project: String,
    member_name: String,
    override_entry: Option<ember_client::wire::CharacterOverride>,
    current_overrides: std::collections::BTreeMap<String, ember_client::wire::CharacterOverride>,
    saving: MutableState<bool>,
    message: MutableState<Option<String>>,
) {
    saving.set(true);
    message.set(None);
    let patch_map = updated_character_overrides(&current_overrides, &member_name, override_entry.clone());
    let prefs_map: std::collections::BTreeMap<String, ember_client::wire::CharacterOverride> = patch_map
        .iter()
        .filter_map(|(k, v)| v.clone().map(|o| (k.clone(), o)))
        .collect();
    let patch = ember_client::wire::ProjectPatch {
        character_overrides: Some(patch_map),
        ..Default::default()
    };
    let sent = patch.clone();
    let ui = shared_ui().clone();
    let for_prefs = project.clone();
    let reset = override_entry.is_none();
    run_shared(
        async move { services().patch_project(&project, &patch).await.map(|_| ()) },
        move |r| {
            saving.set(false);
            match r {
                Ok(()) => {
                    ui.update_prefs(|pr| pr.set_character_overrides(&for_prefs, prefs_map));
                    let text = if reset {
                        format!("{member_name} 팀원의 캐릭터 설정을 초기화했습니다")
                    } else {
                        format!("{member_name} 팀원의 캐릭터 설정을 저장했습니다")
                    };
                    message.set(Some(text));
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

fn Section(title: &str, note: &str, preview: bool, content: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
            Label(title, Fs::Xl, Weight::Semibold, None, Some(1));
            if preview {
                Pill("미리보기", PillTone::Grey, true);
            }
        });
        Label(note, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
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

/// One catalog cell (`.cell`): the character on a card with its caption.
fn Cell(spec: &CharSpec, caption: &str) {
    let p = ember_theme().palette;
    cu::Column()
        .alignment(Alignment::TopCenter)
        .spacing(6.0)
        .modifier(Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_LG).padding(SP[2]))
        .content(|| {
            draw(spec, 64.0);
            Label(caption, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
        });
}

/// One numbered style section of the catalog (mockup `#cands`): the five states, the three
/// harnesses and the leader crown, nine cells.
fn StyleSection(index: usize, style: CharStyle, cols: usize) {
    let p = ember_theme().palette;
    cu::Column().spacing(6.0).modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).content(|| {
            Label(&format!("{}. {}", index + 1, style.label()), Fs::Xl, Weight::Semibold, None, Some(1));
            Pill(style.id(), PillTone::Grey, true);
        });
        Label(style.blurb(), Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
        let mut cells: Vec<(CharSpec, &str)> = CharState::ALL
            .iter()
            .map(|&s| (spec(style, "ember-리더", "claude", "리뷰어", s, false, None), state_label(s)))
            .collect();
        for (i, (id, _)) in EDITOR_HARNESSES.iter().enumerate() {
            cells.push((spec(style, "core-구현", id, ["테스터", "디자인", "문서"][i], CharState::Idle, false, None), harness_label(id)));
        }
        cells.push((spec(style, "ember-리더", "claude", "리더", CharState::Done, true, None), "리더 왕관"));
        Grid(&cells, cols, SP[2], |(s, label)| Cell(s, label));
    });
}

/// Where a review capture starts the screen (`EMBER_CR_SCROLL` here, read in debug builds only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStart {
    /// The whole screen from the top.
    Top,
    /// The team preview first: the head and the style catalog are left out.
    Team,
    /// The editor first: the head, the catalog and the team preview are left out.
    Editor,
}

/// The start from `EMBER_CR_SCROLL=<team|editor>` and `EMBER_CR_CHARACTER=<index>`: the toolkit
/// could not scroll a column from code before v0.0.5 (v0.0.7: #385 fixed with
/// `ScrollState::scroll_to`; workaround kept until checked on a display, because the capture
/// script relies on it), so the blocks above are left out,
/// as `EMBER_CR_SCROLL` does on the home and files screens. Picking a member alone starts at the
/// editor, so its 팀원 설정 저장 and 초기화 buttons are in view.
pub fn capture_start(scroll: Option<&str>, character: Option<&str>) -> CaptureStart {
    match scroll.map(str::trim) {
        Some("team") => CaptureStart::Team,
        Some("editor") => CaptureStart::Editor,
        _ if capture_member_index(character).is_some() => CaptureStart::Editor,
        _ => CaptureStart::Top,
    }
}

/// The team preview index from `EMBER_CR_CHARACTER`, if it is a number.
pub fn capture_member_index(character: Option<&str>) -> Option<usize> {
    character?.trim().parse::<usize>().ok()
}

/// The member the editor opens with for a capture: the `EMBER_CR_CHARACTER`-th cell of the team
/// preview (0 is the first), or `None` when the index is missing or past the team.
pub fn capture_member(character: Option<&str>, team: &[crate::ui::characters::TeamCell]) -> Option<String> {
    team.get(capture_member_index(character)?).map(|c| c.name.clone())
}

/// Review captures only: release builds ignore both variables.
fn capture_env(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(name).ok()
    } else {
        None
    }
}

pub fn CharactersScreen() {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let width = compose_rust::runtime::current_window_size().width_dp;
    let compact = width <= 560.0;
    let wide = width > 1000.0;
    let cols = team_cols(width);
    let asked = ui.characters.get().unwrap_or_default();
    let svc = crate::services::try_services();
    let names: Vec<String> = svc.as_ref().map(|s| s.client.read(|st| st.projects().into_iter().map(|v| v.name).collect())).unwrap_or_default();
    let project = characters_project(&asked, ui.selected_project.get().as_deref(), &names);
    let project_name = project.clone().unwrap_or_default();
    let (server, server_overrides) = svc
        .as_ref()
        .map(|s| {
            s.client.read(|st| {
                st.project(&project_name)
                    .map(|pr| (pr.character_style.clone(), Some(pr.character_overrides.clone())))
                    .unwrap_or((None, None))
            })
        })
        .unwrap_or((None, None));
    let prefs = ui.prefs.get();
    let current = effective_style(server.as_deref(), prefs.saved_character_style(&project_name));
    let current_overrides = effective_character_overrides(
        server_overrides.as_ref(),
        prefs.saved_character_overrides(&project_name),
    );
    // The project's real members, for the team preview (sessions of the project).
    let members = svc
        .as_ref()
        .map(|s| {
            s.client.read(|st| {
                crate::ui::project::project_boxes(st)
                    .into_iter()
                    .find(|b| b.id == project_name)
                    .map(|b| b.members)
                    .unwrap_or_default()
            })
        })
        .unwrap_or_default();
    let team = team_preview(&project_name, &members);
    let capture_character = capture_env("EMBER_CR_CHARACTER");
    let start = capture_start(capture_env("EMBER_CR_SCROLL").as_deref(), capture_character.as_deref());

    // The team style starts as the project's own; a switch only previews until it is applied.
    let chosen = remember(|| mutable_state_of(None::<CharStyle>));
    let style = chosen.get().unwrap_or(current);
    let saving = remember(|| mutable_state_of(false));
    let message = remember(|| mutable_state_of(None::<String>));
    let ed_saving = remember(|| mutable_state_of(false));
    let ed_message = remember(|| mutable_state_of(None::<String>));
    let ed_name = remember(|| {
        let initial = capture_member(capture_character.as_deref(), &team.cells)
            .or_else(|| members.first().map(|m| m.name.clone()))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "ember-리더".to_string());
        mutable_state_of(initial)
    });
    let ed_harness = remember(|| mutable_state_of(0usize));
    let ed_role = remember(|| mutable_state_of(0usize));
    let ed_style = remember(|| mutable_state_of(None::<CharStyle>));
    let ed_state = remember(|| mutable_state_of(CharState::Idle));
    let ed_pin = remember(|| mutable_state_of(None::<usize>));
    let ed_loaded_key = remember(|| mutable_state_of(String::new()));
    let trimmed_ed_name = ed_name.get().trim().to_string();
    let load_key = format!("{project_name}|{trimmed_ed_name}");
    if ed_loaded_key.get_untracked() != load_key {
        ed_loaded_key.set(load_key);
        if let Some(ov) = current_overrides.get(&trimmed_ed_name) {
            if let Some(h) = ov.harness.as_deref() {
                if let Some(pos) = EDITOR_HARNESSES.iter().position(|(id, _)| *id == h) {
                    ed_harness.set(pos);
                }
            }
            if let Some(r) = ov.role.as_deref() {
                if let Some(pos) = EDITOR_ROLES.iter().position(|&x| x == r) {
                    ed_role.set(pos);
                }
            }
            ed_style.set(ov.style.as_deref().and_then(CharStyle::from_id));
            if let Some(st) = ov.state.as_deref().and_then(CharState::from_id) {
                ed_state.set(st);
            }
            ed_pin.set(ov.silhouette.filter(|&i| i < SILHOUETTES.len()));
        }
    }

    cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
        cu::Column().spacing(SP[5]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
            if start == CaptureStart::Top {
                // Back to where it was opened from; no sidebar entry (D26).
                cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    let u = ui.clone();
                    AppButton("뒤로", Some(IconRole::Back), ButtonKind::Default, true, true, false, move || u.close_characters());
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    if compact {
                        cu::Spacer().modifier(Modifier::new().width(super::project::PHONE_MENU_ROOM));
                    }
                });
                cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("팀 캐릭터", if compact { Fs::Xl } else { Fs::Xl2 }, Weight::Regular, None, Some(1));
                    Label(LEAD, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                    let sub = match &project {
                        Some(n) => format!("{n} 프로젝트의 스타일은 {}입니다.", current.label()),
                        None => "프로젝트가 아직 없어 기본 스타일로 보여 드립니다.".to_string(),
                    };
                    Label(&sub, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                });

                // 1. The catalog: one numbered section per style.
                for (i, s) in CharStyle::ALL.iter().copied().enumerate() {
                    StyleSection(i, s, cols);
                }
            }

            // 2. The team preview with its style switch and the apply button.
            let team_note = if team.sample {
                "이 프로젝트에 아직 팀원이 없어 예시 팀원 18명을 고른 스타일로 그립니다. 리더에게는 왕관이 붙고, 기본 프로젝트에는 리더가 없습니다".to_string()
            } else {
                format!("{} 프로젝트의 팀원을 고른 스타일로 그립니다. 리더에게는 왕관이 붙고, 기본 프로젝트에는 리더가 없습니다", project_name)
            };
            if start != CaptureStart::Editor {
                Section("팀 미리보기", &team_note, team.sample, || {
                    // `.segx`: six style buttons; a phone puts three on a line.
                    let per_line = if compact { 3 } else { CharStyle::ALL.len() };
                    for line in CharStyle::ALL.chunks(per_line) {
                        cu::Row().spacing(6.0).alignment(Alignment::CenterStart).content(|| {
                            for &s in line {
                                let (c, m) = (chosen.clone(), message.clone());
                                // TODO(compose-rust#473): no pressed state for a screen reader (aria-pressed).
                                let kind = if s == style { ButtonKind::Primary } else { ButtonKind::Default };
                                AppButton(s.label(), None, kind, true, true, false, move || {
                                    c.set(Some(s));
                                    m.set(None);
                                });
                            }
                        });
                    }
                    let needed = style_apply_needed(current, style);
                    cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
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
                            Some(_) if !needed => format!("{} 이미 이 프로젝트의 스타일입니다", with_topic(style.label())),
                            Some(n) => format!("{n}의 현재 스타일은 {}입니다", current.label()),
                        };
                        Label(&note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                    });
                    if let Some(m) = message.get() {
                        Label(&m, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    cu::Column()
                        .spacing(SP[3])
                        .modifier(Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_LG).padding(SP[4]))
                        .content(|| {
                            let ov_map = current_overrides.clone();
                            let proj_id = project_name.clone();
                            Grid(&team.cells, cols, SP[3], |m| {
                                let base = spec(style, &m.name, &m.agent, m.role.as_deref().unwrap_or(""), m.state, m.crown, None);
                                let drawn = apply_character_override(base, &proj_id, ov_map.get(&m.name));
                                draw(&drawn, 64.0);
                                Label(&m.name, Fs::Xs, Weight::Semibold, None, Some(1));
                                Label(harness_label(&drawn.agent), Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
                            });
                            if team.more > 0 {
                                Label(&format!("외 {}명", team.more), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            }
                        });
                });
            }

            // 3. The editor: a stage and a form side by side (one column up to 1000 dp).
            Section("편집기", "이름을 바꾸면 캐릭터가 바뀝니다. 말랑과 키링은 실루엣을 고정할 수 있습니다. 저장한 팀원 설정은 프로젝트에 기록됩니다.", false, || {
                let name = ed_name.get();
                let trimmed_name = name.trim().to_string();
                let shown_name = if trimmed_name.is_empty() { "이름 없음".to_string() } else { trimmed_name.clone() };
                let harness_idx = ed_harness.get().min(EDITOR_HARNESSES.len() - 1);
                let role_idx = ed_role.get().min(EDITOR_ROLES.len() - 1);
                let (agent_id, _) = EDITOR_HARNESSES[harness_idx];
                let role = EDITOR_ROLES[role_idx];
                let ed_v = ed_style.get().unwrap_or(style);
                let pin = if supports_pin(ed_v) { ed_pin.get() } else { None };
                let candidate = build_character_override(harness_idx, role_idx, ed_style.get(), ed_state.get(), ed_pin.get(), style);
                let can_save_ed = project.is_some()
                    && !ed_saving.get()
                    && editor_save_needed(&trimmed_name, &candidate, &current_overrides);
                let has_saved_ed = project.is_some()
                    && !ed_saving.get()
                    && !trimmed_name.is_empty()
                    && current_overrides.contains_key(&trimmed_name);
                let stage = || {
                    cu::Column()
                        .alignment(Alignment::Center)
                        .spacing(SP[3])
                        .modifier(Modifier::new().fill_max_width().height(if compact { 300.0 } else { 320.0 }).background(paint(p.surface_2)).corner_radius(CORNER_MD))
                        .content(|| {
                            draw(&spec(ed_v, &shown_name, agent_id, role, ed_state.get(), role == "리더", pin), if compact { 160.0 } else { 200.0 });
                            cu::Row().spacing(10.0).content(|| {
                                for suffix in ["-2", "-리뷰", "-테스트", "-문서", "-조사"] {
                                    draw(&spec(ed_v, &format!("{shown_name}{suffix}"), agent_id, &suffix[1..], CharState::Idle, false, pin), 44.0);
                                }
                            });
                        });
                };
                let name_field = || {
                    FieldRow("이름", || {
                        let (n, m) = (ed_name.clone(), ed_message.clone());
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            placeholder: "ember-리더".to_string(),
                            text: Some(name.clone()),
                            on_value_change: Some(Box::new(move |v| {
                                n.set(v);
                                m.set(None);
                            })),
                            ..Default::default()
                        });
                    });
                };
                // TODO(compose-rust#473): the Select trigger is a Clickable with no accessible
                // name; the visible label above it names it for sighted users only.
                let harness_field = || {
                    FieldRow("하네스", || {
                        let labels: Vec<String> = EDITOR_HARNESSES.iter().map(|(_, l)| l.to_string()).collect();
                        let at = ed_harness.get().min(labels.len() - 1);
                        let (h, m) = (ed_harness.clone(), ed_message.clone());
                        Select(&labels[at], &labels, at, 260.0, move |i| {
                            h.set(i);
                            m.set(None);
                        });
                    });
                };
                let role_field = || {
                    FieldRow("역할", || {
                        let labels: Vec<String> = EDITOR_ROLES.iter().map(|r| r.to_string()).collect();
                        let at = ed_role.get().min(labels.len() - 1);
                        let (r, m) = (ed_role.clone(), ed_message.clone());
                        Select(&labels[at], &labels, at, 260.0, move |i| {
                            r.set(i);
                            m.set(None);
                        });
                    });
                };
                let style_field = || {
                    FieldRow("스타일", || {
                        let labels: Vec<String> = CharStyle::ALL.iter().map(|s| s.label().to_string()).collect();
                        let at = CharStyle::ALL.iter().position(|&s| s == ed_v).unwrap_or(0);
                        let (v, m) = (ed_style.clone(), ed_message.clone());
                        Select(&labels[at], &labels, at, 260.0, move |i| {
                            v.set(CharStyle::ALL.get(i).copied());
                            m.set(None);
                        });
                    });
                };
                let pin_field = || {
                    FieldRow("실루엣 고정", || {
                        if supports_pin(ed_v) {
                            let mut labels = vec!["자동 (이름이 정합니다)".to_string()];
                            labels.extend(SILHOUETTES.iter().map(|s| s.to_string()));
                            let at = pin.map(|i| i + 1).unwrap_or(0);
                            let (pn, m) = (ed_pin.clone(), ed_message.clone());
                            Select(&labels[at], &labels, at, 260.0, move |i| {
                                pn.set(i.checked_sub(1));
                                m.set(None);
                            });
                        } else {
                            Label(&format!("{} 실루엣을 고정할 수 없습니다", with_topic(ed_v.label())), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                        }
                    });
                };
                let state_field = || {
                    FieldRow("상태", || {
                        let per_line = if compact { 3 } else { CharState::ALL.len() };
                        for line in CharState::ALL.chunks(per_line) {
                            cu::Row().spacing(6.0).content(|| {
                                for &s in line {
                                    let (st, m) = (ed_state.clone(), ed_message.clone());
                                    let kind = if ed_state.get() == s { ButtonKind::Primary } else { ButtonKind::Default };
                                    AppButton(state_label(s), None, kind, true, true, false, move || {
                                        st.set(s);
                                        m.set(None);
                                    });
                                }
                            });
                        }
                    });
                };
                // The form's last full-width row (`.ed-form .full`): the buttons first, the note
                // beside them, or under them on a phone where two buttons fill the line.
                let status_note = match &project {
                    None => "저장할 프로젝트가 없습니다".to_string(),
                    Some(_) if trimmed_name.is_empty() => "이름을 입력하면 팀원 설정을 저장할 수 있습니다".to_string(),
                    Some(_) if !can_save_ed && current_overrides.contains_key(&trimmed_name) => {
                        format!("{trimmed_name} 팀원의 설정이 저장되어 있습니다")
                    }
                    Some(n) => format!("{n} 프로젝트에 {trimmed_name} 팀원 설정을 저장합니다"),
                };
                let actions_row = || {
                    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                        cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                            let (pr, nm, cand, ovs, sv, ms) = (
                                project.clone(),
                                trimmed_name.clone(),
                                candidate.clone(),
                                current_overrides.clone(),
                                ed_saving.clone(),
                                ed_message.clone(),
                            );
                            AppButton(
                                if ed_saving.get() { "저장 중" } else { "팀원 설정 저장" },
                                None,
                                ButtonKind::Primary,
                                true,
                                can_save_ed,
                                false,
                                move || {
                                    if let Some(proj) = pr.clone() {
                                        save_character_override(proj, nm.clone(), Some(cand.clone()), ovs.clone(), sv.clone(), ms.clone());
                                    }
                                },
                            );
                            if has_saved_ed {
                                let (pr, nm, ovs, sv, ms) = (
                                    project.clone(),
                                    trimmed_name.clone(),
                                    current_overrides.clone(),
                                    ed_saving.clone(),
                                    ed_message.clone(),
                                );
                                AppButton(
                                    "초기화",
                                    None,
                                    ButtonKind::Default,
                                    true,
                                    true,
                                    false,
                                    move || {
                                        if let Some(proj) = pr.clone() {
                                            save_character_override(proj, nm.clone(), None, ovs.clone(), sv.clone(), ms.clone());
                                        }
                                    },
                                );
                            }
                            if !compact {
                                Label(&status_note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                            }
                        });
                        if compact {
                            Label(&status_note, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                        }
                        if let Some(m) = ed_message.get() {
                            Label(&m, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                        }
                    });
                };
                let form = || {
                    cu::Column().spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(|| {
                        name_field();
                        if compact {
                            harness_field();
                            role_field();
                            style_field();
                            pin_field();
                        } else {
                            Pair(harness_field, role_field);
                            Pair(style_field, pin_field);
                        }
                        state_field();
                        actions_row();
                    });
                };
                let card = Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_LG).padding(SP[5]);
                if wide {
                    cu::Row().spacing(SP[5]).alignment(Alignment::CenterStart).modifier(card).content(|| {
                        cu::Column().modifier(Modifier::new().weight(1.0)).content(stage);
                        cu::Column().modifier(Modifier::new().weight(1.2)).content(form);
                    });
                } else {
                    cu::Column().spacing(SP[5]).modifier(card).content(|| {
                        stage();
                        form();
                    });
                }
            });
        });
    });
}

/// Two form fields side by side, equal width (`.ed-form` two columns).
fn Pair(a: impl FnOnce(), b: impl FnOnce()) {
    cu::Row().spacing(SP[3]).alignment(Alignment::TopStart).modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Column().modifier(Modifier::new().weight(1.0)).content(a);
        cu::Column().modifier(Modifier::new().weight(1.0)).content(b);
    });
}

/// The mockup's lead paragraph (`.lead`).
const LEAD: &str = "팀(프로젝트)을 만들 때 캐릭터 스타일 여섯 가지 중 하나를 고릅니다. 고른 스타일은 그 팀의 모든 팀원에게 적용됩니다. 모양과 색은 팀원 이름과 하네스로 정해지므로 같은 팀원은 어디서나 같은 모습입니다. 하네스는 앱 팔레트의 색 계열(Claude 주황, Codex 파랑, Gemini 하늘)을 따릅니다. 상태는 아직 움직임 대신 정지한 자세로 보입니다.";

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
        let svc = crate::services::test_services();
        svc.reset_demo();
        let text = rendered();
        for s in CharStyle::ALL {
            assert!(text.contains(s.label()), "missing {}", s.label());
        }
        assert!(text.contains("팀 캐릭터"));
        assert!(text.contains("팀 미리보기"));
        assert!(text.contains("편집기"));
        assert!(text.contains("팀원 설정 저장"));
        assert!(text.contains("뒤로"));
    }

    #[test]
    fn fr_l22_the_catalog_is_six_numbered_sections_of_nine_cells() {
        let _guard = crate::services::test_guard();
        let text = rendered();
        for (i, s) in CharStyle::ALL.iter().enumerate() {
            let heading = format!("{}. {}", i + 1, s.label());
            assert!(text.contains(&heading), "missing {heading}");
            assert!(text.contains(s.id()), "missing the id pill {}", s.id());
        }
        // Each section ends with the crown cell, so six of them.
        assert_eq!(text.matches("리더 왕관").count(), 6);
        for want in ["팀 미리보기", "이 프로젝트에 적용", "이름", "하네스", "역할", "스타일", "실루엣 고정", "상태"] {
            assert!(text.contains(want), "missing {want}");
        }
    }

    #[test]
    fn fr_l22_team_cells_draw_the_project_members_or_the_labelled_sample() {
        let _guard = crate::services::test_guard();
        let svc = crate::services::try_services().expect("test services");
        let ui = shared_ui();
        let names: Vec<String> = svc.client.read(|st| st.projects().into_iter().map(|v| v.name).collect());
        let asked = ui.characters.get_untracked().unwrap_or_default();
        let project = characters_project(&asked, ui.selected_project.get_untracked().as_deref(), &names).unwrap_or_default();
        let members = svc.client.read(|st| {
            crate::ui::project::project_boxes(st).into_iter().find(|b| b.id == project).map(|b| b.members).unwrap_or_default()
        });
        let text = rendered();
        if members.is_empty() {
            assert!(text.contains("예시 팀원 18명"), "a project without members says the team is a sample");
        } else {
            assert!(text.contains("프로젝트의 팀원을 고른 스타일로 그립니다"));
            assert!(text.contains(&members[0].name), "missing the member {}", members[0].name);
            assert!(!text.contains("예시 팀원 18명"));
        }
    }

    fn texts_of<'a>(batch: &'a [compose_rust::protocol::Mutation<'a>]) -> Vec<(u32, &'a str)> {
        use compose_rust::protocol::{Mutation, PropertyValue};
        use compose_rust::PropertyKind;
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => Some((*node_id, *s)),
                Mutation::SetText { node_id, text, .. } => Some((*node_id, *text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fr_l22_every_button_on_the_screen_has_a_name() {
        use compose_rust::protocol::Mutation;
        use compose_rust::WidgetKind;
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(CharactersScreen)));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let texts = texts_of(&batch);
        let buttons: Vec<u32> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::Create { node_id, widget: WidgetKind::Button } => Some(*node_id),
                _ => None,
            })
            .collect();
        assert!(!buttons.is_empty());
        for b in buttons {
            let name = texts.iter().find(|(n, _)| *n == b).map(|(_, t)| *t).unwrap_or("");
            assert!(!name.trim().is_empty(), "button {b} has no name");
        }
    }

    /// The latest text and click handler of every node across batches.
    #[derive(Default)]
    struct Nodes {
        text: std::collections::HashMap<u32, String>,
        click: std::collections::HashMap<u32, u64>,
    }

    impl Nodes {
        fn take(&mut self, batch: &[compose_rust::protocol::Mutation<'_>]) {
            use compose_rust::protocol::{Mutation, PropertyValue};
            use compose_rust::PropertyKind;
            for (n, t) in texts_of(batch) {
                self.text.insert(n, t.to_string());
            }
            for m in batch {
                if let Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } = m {
                    self.click.insert(*node_id, *h as u64);
                }
            }
        }

        fn button(&self, name: &str) -> (u32, u64) {
            self.click
                .iter()
                .find(|(n, _)| self.text.get(n).is_some_and(|t| t == name))
                .map(|(n, h)| (*n, *h))
                .unwrap_or_else(|| panic!("no {name} button with a click handler"))
        }
    }

    #[test]
    fn fr_l22_the_team_style_switch_redraws_the_editor_style() {
        use compose_rust::protocol::HostEvent;
        use compose_rust::EventPayload;
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(CharactersScreen)));
        let mut nodes = Nodes::default();
        nodes.take(&compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        // Two clicks, so the second one changes the style whatever the project's style is.
        let mut last = Vec::new();
        for name in ["크루", "포켓"] {
            let (node_id, handler_id) = nodes.button(name);
            let (bytes, _) = host.dispatch(HostEvent { node_id, handler_id, payload: EventPayload::Clicked }).expect("dispatch");
            let batch = compose_rust::protocol::decode_batch(bytes).expect("decode");
            last = texts_of(&batch).into_iter().map(|(n, t)| (n, t.to_string())).collect::<Vec<_>>();
            nodes.take(&batch);
            last.retain(|(n, _)| *n != node_id);
        }
        // The editor's style select follows the team style until it is picked on its own.
        assert!(last.iter().any(|(_, t)| t == "포켓"), "the editor style did not change to 포켓: {last:?}");
    }

    #[test]
    fn fr_l22_capture_hook_parses_scroll_and_character() {
        assert_eq!(capture_start(None, None), CaptureStart::Top);
        assert_eq!(capture_start(Some("team"), None), CaptureStart::Team);
        assert_eq!(capture_start(Some("editor"), None), CaptureStart::Editor);
        assert_eq!(capture_start(Some("junk"), None), CaptureStart::Top);
        // A member alone starts at the editor; an explicit start wins.
        assert_eq!(capture_start(None, Some("2")), CaptureStart::Editor);
        assert_eq!(capture_start(Some("team"), Some("2")), CaptureStart::Team);
        assert_eq!(capture_start(None, Some("x")), CaptureStart::Top);
        assert_eq!(capture_member_index(Some(" 3 ")), Some(3));
        assert_eq!(capture_member_index(Some("-1")), None);
        let team = team_preview("ember", &[]);
        assert_eq!(capture_member(Some("1"), &team.cells), Some(team.cells[1].name.clone()));
        assert_eq!(capture_member(Some("999"), &team.cells), None);
        assert_eq!(capture_member(None, &team.cells), None);
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

    #[test]
    fn fr_l22_editor_save_button_persists_character_override_to_project() {
        use compose_rust::protocol::HostEvent;
        use compose_rust::EventPayload;
        let _guard = crate::services::test_guard();
        let svc = crate::services::test_services();
        svc.reset_demo();
        shared_ui().open_characters("demo-project");

        let mut host = Host::new(|| Box::new(Recomposer::new(CharactersScreen)));
        let mut nodes = Nodes::default();
        nodes.take(&compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode"));

        let (save_id, save_handler) = nodes.button("팀원 설정 저장");
        host.dispatch(HostEvent {
            node_id: save_id,
            handler_id: save_handler,
            payload: EventPayload::Clicked,
        })
        .expect("dispatch click on 팀원 설정 저장");

        // Wait briefly for the async run_shared task on test_services().rt to apply the patch.
        for _ in 0..40 {
            let has_override = svc
                .client
                .read(|st| st.project("demo-project").is_some_and(|p| !p.character_overrides.is_empty()));
            if has_override {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let stored = svc
            .client
            .read(|st| st.project("demo-project").map(|p| p.character_overrides.clone()))
            .unwrap_or_default();
        assert!(
            !stored.is_empty(),
            "clicking '팀원 설정 저장' must persist character_overrides on the project: {stored:?}"
        );
        shared_ui().close_characters();
    }
}
