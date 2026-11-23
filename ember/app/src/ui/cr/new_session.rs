//! compose-rust version of `ui::new_session` (#220 phase 3, #1068): the new session dialog
//! (FR-L8, FR-L15, FR-L16, FR-L17): project, folder, harness and model (`FR-L15`), permission
//! mode (`FR-L16`), and computer (`GET /computers`), defaulting to the last-used combination
//! for the project. Accounts come from the harness profile (`FR-L17`), never a per-session
//! choice.
//!
//! v0.0.7: #374 fixed (`TextField::text`); workaround kept until checked on a display (a
//! controlled value must not move the caret while typing), so remembered text (folder) is
//! shown as the placeholder and an untouched field means "use it".

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember_with_key, MutableState};
use compose_rust::ui::Modifier;
use compose_rust::{ColorRole, Paint, SpaceRole, TypeRole};

use super::components::{
    AppButton, ButtonKind, EmberField, EmberSheet, FieldOptions, Label, NamedSelect, Select,
    SheetFooter, Txt,
};
use super::notice;
use super::theme::*;
use crate::model::{self, index_of};
use crate::prefs::LastUsed;
use crate::server::{AgentDefaults, CreateSession};
use crate::services::{run_shared, services};
use crate::ui::coexist::shared_ui;
use crate::ui::home::{
    default_option, harness_block, mode_after_harness_change, model_catalog, modes_for,
    needs_approve_all, Harness, ModelOption, PermMode, Selection,
};

/// Width of the option popups, a little under the 560 px sheet's field.
const POPUP_WIDTH: f32 = 440.0;

/// The local computer's id in the server's registry (`ember/server/src/computers`).
const LOCAL_COMPUTER: &str = "local";

#[derive(Debug, Clone, PartialEq)]
struct Choice {
    /// `None` = the default ("this server").
    value: Option<String>,
    label: String,
}

/// The dialog, composed while `SharedUi::new_session` is `Some`.
pub fn NewSessionDialog() {
    // The form is mounted only while the dialog is open, so every opening starts from the
    // remembered combination with empty text fields.
    if let Some(preset) = shared_ui().new_session.get() {
        NewSessionForm(&preset);
    }
}

/// Whether Start is enabled: not already starting, and a project, a folder and an agent chosen.
pub fn can_start(busy: bool, project: &str, cwd: &str, agent: &str) -> bool {
    !busy && !project.is_empty() && !cwd.is_empty() && !agent.is_empty()
}

/// Selectable catalog rows for the new-session picker (FR-L15): excludes harnesses marked
/// unavailable (such as Gemini) or uninstalled when `GET /agents` has answered, while keeping
/// the currently selected harness visible. Falls back to Claude Code and Codex if nothing is
/// reported installed yet.
pub fn available_models(agents: &[(String, bool)], current: Harness) -> Vec<ModelOption> {
    let mut allowed: Vec<Harness> = Harness::ALL
        .iter()
        .copied()
        .filter(|h| harness_block(*h, agents, &[]).is_none())
        .collect();
    if allowed.is_empty() {
        allowed.extend([Harness::ClaudeCode, Harness::Codex]);
    }
    model_catalog()
        .into_iter()
        .filter(|o| {
            allowed.contains(&o.harness)
                || (o.harness == current && current != Harness::Gemini)
        })
        .collect()
}

/// Resolve the initial harness, model, effort, permission mode and computer from a project's
/// `LastUsed` entry, falling back to `AgentDefaults` (FR-L8, FR-L15, FR-L16).
pub fn resolve_initial_selection(
    remembered: Option<&LastUsed>,
    defaults: &AgentDefaults,
) -> (ModelOption, PermMode, Option<String>) {
    let mut sel = Selection::initial_with_defaults(defaults);
    if sel.option.harness == Harness::Gemini {
        sel = Selection::initial();
    }
    let Some(last) = remembered else {
        return (sel.option, sel.mode, None);
    };
    if let Some(h) = Harness::from_kind(&last.agent).filter(|h| *h != Harness::Gemini) {
        sel = sel.with_harness(h);
    }
    let harness = sel.option.harness;
    let cat = model_catalog();
    if let Some(model_id) = last.model.as_deref().filter(|m| !m.is_empty()) {
        let want_effort = last.effort.as_deref().or(sel.option.effort);
        if let Some(found) = cat
            .iter()
            .find(|o| o.harness == harness && o.model == model_id && o.effort == want_effort)
            .or_else(|| cat.iter().find(|o| o.harness == harness && o.model == model_id))
        {
            sel = sel.with_option(*found);
        }
    } else if let Some(effort) = last.effort.as_deref() {
        if let Some(found) = cat
            .iter()
            .find(|o| o.harness == harness && o.model == sel.option.model && o.effort == Some(effort))
        {
            sel = sel.with_option(*found);
        }
    }
    if let Some(mode) = last.mode.as_deref().and_then(PermMode::from_key) {
        sel = sel.with_mode(mode);
    }
    (sel.option, sel.mode, last.computer.clone())
}

/// The request for the server and the combination to remember for the project (FR-L8, FR-L15,
/// FR-L16, FR-L17). Accounts come from the harness profile, so `account` is always `None`.
pub fn build_request(
    project: &str,
    option: ModelOption,
    mode: PermMode,
    cwd: &str,
    title: &str,
    computer: Option<String>,
) -> (CreateSession, LastUsed) {
    let agent = option.harness.kind().to_string();
    let model = Some(option.model.to_string());
    let title = title.trim();
    let body = CreateSession {
        project: project.to_string(),
        agent: agent.clone(),
        cwd: cwd.to_string(),
        model: model.clone(),
        title: (!title.is_empty()).then(|| title.to_string()),
        account: None,
    };
    let remember = LastUsed {
        agent,
        account: None,
        computer,
        model,
        effort: option.effort.map(str::to_string),
        mode: Some(mode.key().to_string()),
        cwd: cwd.to_string(),
    };
    (body, remember)
}

fn muted() -> Option<Paint> {
    Some(Paint::Role(ColorRole::OnSurfaceVariant))
}

fn NewSessionForm(preset: &str) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let key = preset.to_string();

    let projects: Vec<String> = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| {
            s.projects()
                .into_iter()
                .filter(|p| !s.project(&p.name).is_some_and(|pr| pr.archived))
                .map(|p| p.name)
                .collect()
        })
    };

    // Initial selection: the preset project (or the last one used), and what was last used
    // with it, falling back to global agent defaults (FR-L8, FR-L15, FR-L16).
    let initial = remember_with_key(key.clone(), || {
        let prefs = ui.prefs.get_untracked();
        let defaults = ui.live.agent_defaults.get_untracked();
        let proj = if preset.is_empty() {
            prefs
                .last_project
                .clone()
                .filter(|p| projects.contains(p))
                .or_else(|| projects.first().cloned())
                .unwrap_or_default()
        } else {
            preset.to_string()
        };
        let (opt, mode, comp) = resolve_initial_selection(prefs.last_used.get(&proj), &defaults);
        (proj, opt, mode, comp)
    });
    let project = remember_with_key(key.clone(), || mutable_state_of(initial.0.clone()));
    let new_project = remember_with_key(key.clone(), || mutable_state_of(String::new()));
    let option = remember_with_key(key.clone(), || mutable_state_of(initial.1));
    let mode = remember_with_key(key.clone(), || mutable_state_of(initial.2));
    let computer = remember_with_key(key.clone(), || mutable_state_of(initial.3.clone()));
    let cwd = remember_with_key(key.clone(), || mutable_state_of(String::new()));
    let title = remember_with_key(key.clone(), || mutable_state_of(String::new()));
    let busy = remember_with_key(key, || mutable_state_of(false));

    // ---- options ---------------------------------------------------------------------------
    const NEW_PROJECT: &str = "새 프로젝트\u{2026}";
    let mut project_options = projects.clone();
    project_options.push(NEW_PROJECT.to_string());
    let project_now = project.get();
    let creating_project = project_now.is_empty() || !projects.contains(&project_now);
    let project_index = if creating_project {
        project_options.len() - 1
    } else {
        index_of(&project_options, Some(&project_now))
    };
    let effective_project = if creating_project {
        new_project.get().trim().to_string()
    } else {
        project_now
    };

    let agents_live: Vec<(String, bool)> = ui
        .live
        .agents
        .get()
        .iter()
        .map(|a| (a.kind.clone(), a.installed))
        .collect();
    let option_now = option.get();
    let models = available_models(&agents_live, option_now.harness);
    let effective_option = if models.contains(&option_now) {
        option_now
    } else {
        models
            .first()
            .copied()
            .unwrap_or_else(|| default_option(Harness::ClaudeCode))
    };
    let model_labels: Vec<String> = models.iter().map(|o| o.chip_label()).collect();
    let model_index = models
        .iter()
        .position(|o| *o == effective_option)
        .unwrap_or(0);

    let modes = modes_for(effective_option.harness);
    let effective_mode = mode_after_harness_change(effective_option.harness, mode.get());
    let mode_labels: Vec<String> = modes.iter().map(|m| m.label().to_string()).collect();
    let mode_index = modes
        .iter()
        .position(|m| *m == effective_mode)
        .unwrap_or(0);

    let computers: Vec<Choice> = {
        let _ = ui.live.launcher.get();
        let mut out = vec![Choice {
            value: None,
            label: "메인 서버 (기본)".into(),
        }];
        services().client.read(|s| {
            out.extend(
                s.computers()
                    .iter()
                    .filter(|c| c.id != LOCAL_COMPUTER)
                    .map(|c| Choice {
                        value: Some(c.id.clone()),
                        label: format!("{} ({})", c.name, model::reachability_label(c.reachability)),
                    }),
            )
        });
        out
    };
    let computer_now = computer.get();
    let computer_index = computers
        .iter()
        .position(|c| c.value == computer_now)
        .unwrap_or(0);

    // Remembered or derived default for the folder field.
    let remembered = ui.prefs.get().last_used.get(&effective_project).cloned();
    let default_cwd = remembered
        .as_ref()
        .map(|l| l.cwd.clone())
        .filter(|c| !c.is_empty())
        .or_else(|| services().client.read(|s| model::latest_cwd(s, &effective_project)))
        .unwrap_or_default();
    let cwd_text = cwd.get();
    let cwd_value = if cwd_text.trim().is_empty() {
        default_cwd.clone()
    } else {
        cwd_text.trim().to_string()
    };
    let can_start = can_start(
        busy.get(),
        &effective_project,
        &cwd_value,
        effective_option.harness.kind(),
    );

    let close = {
        let (open, busy) = (ui.new_session.clone(), busy.clone());
        move || {
            if !busy.get_untracked() {
                open.set(None);
            }
        }
    };

    let start = {
        let computers = computers.clone();
        let (ui, busy, title) = (ui.clone(), busy.clone(), title.clone());
        let (effective_project, cwd_value) = (effective_project.clone(), cwd_value.clone());
        move || {
            if !can_start {
                return;
            }
            busy.set(true);
            let computer_id = computers.get(computer_index).and_then(|c| c.value.clone());
            let t = title.get_untracked();
            let bypass = needs_approve_all(effective_mode);
            let (body, remember) = build_request(
                &effective_project,
                effective_option,
                effective_mode,
                &cwd_value,
                &t,
                computer_id.clone(),
            );
            let project_name = effective_project.clone();
            let (ui, busy) = (ui.clone(), busy.clone());
            run_shared(
                async move {
                    let s = services();
                    let rec = s.create_session(&body).await?;
                    let mut notice = None;
                    // The demo (#199) has one computer and no node to switch.
                    if let Some(cid) = computer_id.filter(|_| s.demo.is_none()) {
                        // A session starts on the main server; move it before the first message.
                        if let Err(e) = s.server.switch_computer(&rec.id, &cid).await {
                            notice = Some(format!(
                                "메인 서버에서 시작했습니다. 컴퓨터를 바꾸지 못했습니다({e})"
                            ));
                        }
                    }
                    if bypass {
                        if let Err(e) = s.set_approve_all(&rec.id, true).await {
                            let msg = format!("모두 허용을 켜지 못했습니다({e})");
                            notice = Some(match notice {
                                Some(prev) => format!("{prev} / {msg}"),
                                None => msg,
                            });
                        }
                    }
                    Ok::<_, String>((rec, notice))
                },
                move |r| {
                    busy.set(false);
                    match r {
                        Ok((rec, note)) => {
                            ui.update_prefs(|p| {
                                p.last_used.insert(project_name.clone(), remember);
                                p.last_project = Some(project_name);
                            });
                            ui.new_session.set(None);
                            ui.open_conversation(&rec.id);
                            if let Some(n) = note {
                                notice::post(n);
                            }
                        }
                        Err(e) => notice::post(format!("세션을 시작하지 못했습니다: {e}")),
                    }
                },
            );
        }
    };

    // The mockup's sheet: a centered 560 px modal, a 60 px header with the title and a close
    // button, the form scrolling in between, and a footer with cancel and create.
    let body_height = {
        let w = crate::ui::window_size::current_window_size();
        super::components::sheet_body_max_height(w.width_dp, w.height_dp, false, 60.0 + 65.0)
            .min(900.0)
    };
    EmberSheet("새 세션", true, close.clone(), None, || {
        // A long form on a short window scrolls inside the dialog.
        { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().height(body_height), {
            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Md, modifier = Modifier::new().fill_max_width().padding(SP[4]), {
                Field("프로젝트", || {
                    let (options, projects) = (project_options.clone(), projects.clone());
                    let (ui, project, option, mode, computer) = (
                        ui.clone(),
                        project.clone(),
                        option.clone(),
                        mode.clone(),
                        computer.clone(),
                    );
                    Select(
                        &project_options[project_index.min(project_options.len() - 1)],
                        &project_options,
                        project_index,
                        POPUP_WIDTH,
                        move |i| {
                            let p = options.get(i).cloned().unwrap_or_default();
                            let p = if projects.contains(&p) { p } else { String::new() };
                            let remembered = ui.prefs.get_untracked().last_used.get(&p).cloned();
                            let defaults = ui.live.agent_defaults.get_untracked();
                            seed(
                                &p,
                                remembered.as_ref(),
                                &defaults,
                                &project,
                                &option,
                                &mode,
                                &computer,
                            );
                        },
                    );
                });
                if creating_project {
                    Field("프로젝트 이름", || {
                        let v = new_project.clone();
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            placeholder: "my-project".to_string(),
                            on_value_change: Some(Box::new(move |t| v.set(t))),
                            ..Default::default()
                        });
                    });
                }
                Field("컴퓨터의 폴더", || {
                    let v = cwd.clone();
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: if default_cwd.is_empty() {
                            "/path/to/project".to_string()
                        } else {
                            default_cwd.clone()
                        },
                        on_value_change: Some(Box::new(move |t| v.set(t))),
                        ..Default::default()
                    });
                });
                Field("제목 (선택)", || {
                    let v = title.clone();
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: "새 대화".to_string(),
                        on_value_change: Some(Box::new(move |t| v.set(t))),
                        ..Default::default()
                    });
                });
                Field("하네스와 모델", || {
                    let (choices, option, mode) = (models.clone(), option.clone(), mode.clone());
                    NamedSelect(
                        "하네스와 모델",
                        &model_labels[model_index.min(model_labels.len().saturating_sub(1))],
                        &model_labels,
                        model_index,
                        POPUP_WIDTH,
                        move |i| {
                            if let Some(o) = choices.get(i).copied() {
                                option.set(o);
                                mode.set(mode_after_harness_change(o.harness, mode.get_untracked()));
                            }
                        },
                    );
                    Label(
                        "프로필은 하네스를 따릅니다. 계정을 따로 고르지 않습니다",
                        Fs::Xs,
                        Weight::Regular,
                        Some(paint(p.muted)),
                        Some(2),
                    );
                });
                Field("권한 모드", || {
                    let (choices, mode) = (modes.to_vec(), mode.clone());
                    NamedSelect(
                        "권한 모드",
                        &mode_labels[mode_index.min(mode_labels.len().saturating_sub(1))],
                        &mode_labels,
                        mode_index,
                        POPUP_WIDTH,
                        move |i| {
                            if let Some(m) = choices.get(i).copied() {
                                mode.set(m);
                            }
                        },
                    );
                    let desc_color = if effective_mode.is_bypass() {
                        paint(p.red.strong)
                    } else {
                        paint(p.muted)
                    };
                    Label(
                        effective_mode.description(),
                        Fs::Xs,
                        Weight::Regular,
                        Some(desc_color),
                        Some(2),
                    );
                });
                Field("컴퓨터", || {
                    let (choices, computer) = (computers.clone(), computer.clone());
                    let labels: Vec<String> = computers.iter().map(|c| c.label.clone()).collect();
                    Select(
                        &labels[computer_index.min(labels.len() - 1)],
                        &labels,
                        computer_index,
                        POPUP_WIDTH,
                        move |i| computer.set(choices.get(i).and_then(|c| c.value.clone())),
                    );
                });
            }) };
        }) };
        SheetFooter(|| {
            if busy.get() {
                { use ::compose_rust::ui::IndeterminateProgressIndicator; IndeterminateProgressIndicator!(circular = true, modifier = Modifier::new().size(18.0, 18.0)) };
            }
            AppButton("취소", None, ButtonKind::Default, true, !busy.get(), false, close.clone());
            AppButton("만들기", None, ButtonKind::Primary, true, can_start, false, start);
        });
    });
}

/// Apply a project's remembered combination (or global defaults) to the selection states.
fn seed(
    project_name: &str,
    remembered: Option<&LastUsed>,
    defaults: &AgentDefaults,
    project: &MutableState<String>,
    option: &MutableState<ModelOption>,
    mode: &MutableState<PermMode>,
    computer: &MutableState<Option<String>>,
) {
    project.set(project_name.to_string());
    let (next_option, next_mode, next_computer) = resolve_initial_selection(remembered, defaults);
    option.set(next_option);
    mode.set(next_mode);
    computer.set(next_computer);
}

/// A labelled form row.
fn Field(label: &str, content: impl FnOnce()) {
    { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
        Txt(label, TypeRole::Label, muted(), None, None, None, None);
        content();
    }) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    #[test]
    fn fr_l8_start_needs_a_project_a_folder_and_an_agent() {
        assert!(can_start(false, "ember", "/work", "codex"));
        assert!(!can_start(true, "ember", "/work", "codex"));
        assert!(!can_start(false, "", "/work", "codex"));
        assert!(!can_start(false, "ember", "", "codex"));
        assert!(!can_start(false, "ember", "/work", ""));
    }

    #[test]
    fn fr_l8_the_request_carries_the_choices_and_the_remembered_combination() {
        let opt = model_catalog()
            .into_iter()
            .find(|o| {
                o.harness == Harness::Codex
                    && o.model == "gpt-5-codex"
                    && o.effort == Some("High")
            })
            .expect("codex gpt-5-codex High");
        let (body, remember) = build_request(
            "ember",
            opt,
            PermMode::Edits,
            "/work",
            "  Fix it  ",
            Some("laptop".into()),
        );
        assert_eq!(body.project, "ember");
        assert_eq!(body.agent, "codex");
        assert_eq!(body.title.as_deref(), Some("Fix it"));
        assert_eq!(body.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(body.account, None);
        assert_eq!(remember.agent, "codex");
        assert_eq!(remember.account, None);
        assert_eq!(remember.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(remember.effort.as_deref(), Some("High"));
        assert_eq!(remember.mode.as_deref(), Some("edits"));
        assert_eq!(remember.computer.as_deref(), Some("laptop"));
        assert_eq!(remember.cwd, "/work");

        let default_opt = default_option(Harness::ClaudeCode);
        let (blank, rem_blank) = build_request("p", default_opt, PermMode::Ask, "/w", "   ", None);
        assert_eq!(blank.title, None);
        assert_eq!(blank.account, None);
        assert_eq!(rem_blank.account, None);
        assert_eq!(rem_blank.mode.as_deref(), Some("ask"));
    }

    #[test]
    fn fr_l8_resolve_initial_selection_restores_last_used_and_respects_defaults() {
        let defaults = AgentDefaults {
            agent: Some("codex".into()),
            model: Some("gpt-5-mini".into()),
            effort: Some("Low".into()),
            mode: Some("edits".into()),
        };

        // Without last_used, falls back to global agent defaults.
        let (opt, mode, comp) = resolve_initial_selection(None, &defaults);
        assert_eq!(opt.harness, Harness::Codex);
        assert_eq!(opt.model, "gpt-5-mini");
        assert_eq!(opt.effort, Some("Low"));
        assert_eq!(mode, PermMode::Edits);
        assert_eq!(comp, None);

        // With last_used, restores agent, model, effort, mode and computer.
        let last = LastUsed {
            agent: "claude-code".into(),
            account: Some("legacy-acc".into()),
            computer: Some("mac-mini".into()),
            model: Some("sonnet".into()),
            effort: Some("Medium".into()),
            mode: Some("plan".into()),
            cwd: "/proj".into(),
        };
        let (opt, mode, comp) = resolve_initial_selection(Some(&last), &defaults);
        assert_eq!(opt.harness, Harness::ClaudeCode);
        assert_eq!(opt.model, "sonnet");
        assert_eq!(opt.effort, Some("Medium"));
        assert_eq!(mode, PermMode::Plan);
        assert_eq!(comp.as_deref(), Some("mac-mini"));

        // Mode unsupported by the harness (Plan on Codex) falls back to Ask.
        let codex_plan = LastUsed {
            agent: "codex".into(),
            model: Some("gpt-5".into()),
            effort: Some("High".into()),
            mode: Some("plan".into()),
            ..Default::default()
        };
        let (opt, mode, _) = resolve_initial_selection(Some(&codex_plan), &AgentDefaults::default());
        assert_eq!(opt.harness, Harness::Codex);
        assert_eq!(mode, PermMode::Ask);

        // Unavailable harness (Gemini) falls back to Claude Code.
        let gemini_defaults = AgentDefaults {
            agent: Some("gemini".into()),
            ..Default::default()
        };
        let (opt, _, _) = resolve_initial_selection(None, &gemini_defaults);
        assert_eq!(opt.harness, Harness::ClaudeCode);
    }

    #[test]
    fn fr_l8_available_models_excludes_gemini_and_falls_back_when_none_installed() {
        let all = available_models(&[], Harness::ClaudeCode);
        assert!(all.iter().all(|o| o.harness != Harness::Gemini));
        assert!(all.iter().any(|o| o.harness == Harness::ClaudeCode));
        assert!(all.iter().any(|o| o.harness == Harness::Codex));

        let codex_only = available_models(
            &[
                ("claude-code".into(), false),
                ("codex".into(), true),
            ],
            Harness::Codex,
        );
        assert!(codex_only.iter().all(|o| o.harness == Harness::Codex));

        let none_installed = available_models(
            &[
                ("claude-code".into(), false),
                ("codex".into(), false),
            ],
            Harness::ClaudeCode,
        );
        assert!(none_installed.iter().any(|o| o.harness == Harness::ClaudeCode));
        assert!(none_installed.iter().any(|o| o.harness == Harness::Codex));
        assert!(none_installed.iter().all(|o| o.harness != Harness::Gemini));
    }

    #[test]
    fn fr_l8_seed_updates_project_option_mode_and_computer() {
        let project = mutable_state_of(String::new());
        let option = mutable_state_of(default_option(Harness::ClaudeCode));
        let mode = mutable_state_of(PermMode::Ask);
        let computer = mutable_state_of(None::<String>);
        let defaults = AgentDefaults {
            agent: Some("codex".into()),
            model: Some("gpt-5".into()),
            effort: Some("High".into()),
            mode: Some("edits".into()),
        };

        let remembered = LastUsed {
            agent: "claude-code".into(),
            account: None,
            computer: Some("studio".into()),
            model: Some("haiku".into()),
            effort: Some("Low".into()),
            mode: Some("plan".into()),
            cwd: "/a".into(),
        };
        seed(
            "alpha",
            Some(&remembered),
            &defaults,
            &project,
            &option,
            &mode,
            &computer,
        );
        assert_eq!(project.get_untracked(), "alpha");
        assert_eq!(option.get_untracked().harness, Harness::ClaudeCode);
        assert_eq!(option.get_untracked().model, "haiku");
        assert_eq!(option.get_untracked().effort, Some("Low"));
        assert_eq!(mode.get_untracked(), PermMode::Plan);
        assert_eq!(computer.get_untracked().as_deref(), Some("studio"));

        seed("beta", None, &defaults, &project, &option, &mode, &computer);
        assert_eq!(project.get_untracked(), "beta");
        assert_eq!(option.get_untracked().harness, Harness::Codex);
        assert_eq!(option.get_untracked().model, "gpt-5");
        assert_eq!(option.get_untracked().effort, Some("High"));
        assert_eq!(mode.get_untracked(), PermMode::Edits);
        assert_eq!(computer.get_untracked(), None);
    }

    #[test]
    fn fr_l8_the_dialog_is_composed_only_while_it_is_open() {
        let _guard = crate::services::test_guard();
        let ui = shared_ui();
        let texts = |batch: &[Mutation<'_>]| -> Vec<String> {
            batch
                .iter()
                .filter_map(|m| match m {
                    Mutation::SetProp {
                        value: PropertyValue::String(s),
                        ..
                    } => Some(s.to_string()),
                    _ => None,
                })
                .collect()
        };
        let mut host = Host::new(|| Box::new(Recomposer::new(NewSessionDialog)));
        let closed = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert!(!texts(&closed).iter().any(|t| t == "새 세션"));
        ui.open_new_session(None);
        let open = decode_batch(host.render_frame(16_000_000).expect("render")).expect("decode");
        let all = texts(&open);
        for want in ["새 세션", "프로젝트", "하네스와 모델", "권한 모드", "컴퓨터", "만들기"] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
        assert!(
            !all.iter().any(|t| t == "계정"),
            "per-session account field must not be composed: {all:?}"
        );
        ui.close_new_session();
    }
}
