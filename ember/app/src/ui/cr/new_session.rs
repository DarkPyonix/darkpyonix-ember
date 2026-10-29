//! compose-rust version of `ui::new_session` (#220 phase 3): the new session dialog (FR-L8):
//! project, folder, agent (`GET /agents`), account (`GET /accounts`), computer
//! (`GET /computers`) and model, defaulting to the last-used combination for the project.
//!
//! TODO(compose-rust#374): `TextField` is uncontrolled and has no initial value, so remembered
//! text (folder, model) is shown as the placeholder and an untouched field means "use it".

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember_with_key, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{ColorRole, Paint, SpaceRole, TypeRole};

use super::components::{AppButton, ButtonKind, EmberField, EmberSheet, FieldOptions, Select, SheetFooter, Txt};
use super::notice;
use super::theme::*;
use crate::model::{self, index_of};
use crate::prefs::LastUsed;
use crate::server::CreateSession;
use crate::services::{run_shared, services};
use crate::ui::coexist::shared_ui;

/// Width of the option popups, a little under the 560 px sheet's field.
const POPUP_WIDTH: f32 = 440.0;

/// Agents offered when the server has not answered `GET /agents` yet.
const FALLBACK_AGENTS: [&str; 2] = ["claude-code", "codex"];

/// The local computer's id in the server's registry (`ember/server/src/computers`).
const LOCAL_COMPUTER: &str = "local";

#[derive(Debug, Clone, PartialEq)]
struct Choice {
    /// `None` = the default ("router chooses", "this server").
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

/// The request for the server and the combination to remember for the project (FR-L8).
pub fn build_request(
    project: &str,
    agent: &str,
    cwd: &str,
    model: &str,
    title: &str,
    account: Option<String>,
    computer: Option<String>,
) -> (CreateSession, LastUsed) {
    let model = (!model.is_empty()).then(|| model.to_string());
    let title = title.trim();
    let body = CreateSession {
        project: project.to_string(),
        agent: agent.to_string(),
        cwd: cwd.to_string(),
        model: model.clone(),
        title: (!title.is_empty()).then(|| title.to_string()),
        account: account.clone(),
    };
    let remember = LastUsed { agent: agent.to_string(), account, computer, model, cwd: cwd.to_string() };
    (body, remember)
}

fn muted() -> Option<Paint> {
    Some(Paint::Role(ColorRole::OnSurfaceVariant))
}

fn NewSessionForm(preset: &str) {
    let ui = shared_ui().clone();
    let key = preset.to_string();

    let projects: Vec<String> = {
        let _ = ui.live.launcher.get();
        services().client.read(|s| s.projects().into_iter().map(|p| p.name).collect())
    };

    // Initial selection: the preset project (or the last one used), and what was last used
    // with it (FR-L8).
    let initial = remember_with_key(key.clone(), || {
        let prefs = ui.prefs.get_untracked();
        let p = if preset.is_empty() {
            prefs
                .last_project
                .clone()
                .filter(|p| projects.contains(p))
                .or_else(|| projects.first().cloned())
                .unwrap_or_default()
        } else {
            preset.to_string()
        };
        let last = prefs.last_used.get(&p).cloned().unwrap_or_default();
        (p, last)
    });
    let project = remember_with_key(key.clone(), || mutable_state_of(initial.0.clone()));
    let new_project = remember_with_key(key.clone(), || mutable_state_of(String::new()));
    let agent = remember_with_key(key.clone(), || mutable_state_of(initial.1.agent.clone()));
    let account = remember_with_key(key.clone(), || mutable_state_of(initial.1.account.clone()));
    let computer = remember_with_key(key.clone(), || mutable_state_of(initial.1.computer.clone()));
    let cwd = remember_with_key(key.clone(), || mutable_state_of(String::new()));
    let model_text = remember_with_key(key.clone(), || mutable_state_of(String::new()));
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
    let effective_project = if creating_project { new_project.get().trim().to_string() } else { project_now };

    let agents: Vec<String> = {
        let detected = ui.live.agents.get();
        let installed: Vec<String> = detected.iter().filter(|a| a.installed).map(|a| a.kind.clone()).collect();
        if installed.is_empty() {
            FALLBACK_AGENTS.iter().map(|s| s.to_string()).collect()
        } else {
            installed
        }
    };
    let agent_now = agent.get();
    let agent_value = if agents.contains(&agent_now) {
        agent_now
    } else {
        agents.first().cloned().unwrap_or_default()
    };
    let agent_index = index_of(&agents, Some(&agent_value));

    let accounts: Vec<Choice> = {
        let list = ui.live.accounts.get();
        let mut out = vec![Choice { value: None, label: "자동 (서버 라우터가 고름)".into() }];
        out.extend(list.iter().filter(|a| a.agent == agent_value).map(|a| Choice {
            value: Some(a.id.clone()),
            label: format!(
                "{}{}{}",
                a.label,
                if a.is_default { " (기본)" } else { "" },
                if a.limited { " \u{00b7} 한도 도달" } else { "" }
            ),
        }));
        out
    };
    let account_now = account.get();
    let account_index = accounts.iter().position(|c| c.value == account_now).unwrap_or(0);

    let computers: Vec<Choice> = {
        let _ = ui.live.launcher.get();
        let mut out = vec![Choice { value: None, label: "메인 서버 (기본)".into() }];
        services().client.read(|s| {
            out.extend(s.computers().iter().filter(|c| c.id != LOCAL_COMPUTER).map(|c| Choice {
                value: Some(c.id.clone()),
                label: format!("{} ({})", c.name, model::reachability_label(c.reachability)),
            }))
        });
        out
    };
    let computer_now = computer.get();
    let computer_index = computers.iter().position(|c| c.value == computer_now).unwrap_or(0);

    // Remembered or derived defaults for the free-text fields.
    let remembered = ui.prefs.get().last_used.get(&effective_project).cloned();
    let default_cwd = remembered
        .as_ref()
        .map(|l| l.cwd.clone())
        .filter(|c| !c.is_empty())
        .or_else(|| services().client.read(|s| model::latest_cwd(s, &effective_project)))
        .unwrap_or_default();
    let default_model = remembered.as_ref().and_then(|l| l.model.clone()).unwrap_or_default();
    let cwd_text = cwd.get();
    let cwd_value = if cwd_text.trim().is_empty() { default_cwd.clone() } else { cwd_text.trim().to_string() };
    let model_typed = model_text.get();
    let model_value = if model_typed.trim().is_empty() { default_model.clone() } else { model_typed.trim().to_string() };
    let can_start = can_start(busy.get(), &effective_project, &cwd_value, &agent_value);

    let close = {
        let open = ui.new_session.clone();
        move || open.set(None)
    };

    let start = {
        let accounts = accounts.clone();
        let computers = computers.clone();
        let (ui, busy, title) = (ui.clone(), busy.clone(), title.clone());
        let (effective_project, agent_value, cwd_value) =
            (effective_project.clone(), agent_value.clone(), cwd_value.clone());
        move || {
            if !can_start {
                return;
            }
            busy.set(true);
            let account_id = accounts.get(account_index).and_then(|c| c.value.clone());
            let computer_id = computers.get(computer_index).and_then(|c| c.value.clone());
            let t = title.get_untracked();
            let (body, remember) = build_request(
                &effective_project,
                &agent_value,
                &cwd_value,
                &model_value,
                &t,
                account_id,
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
                            notice = Some(format!("메인 서버에서 시작했습니다. 컴퓨터를 바꾸지 못했습니다({e})"));
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
    let body_height = (compose_rust::runtime::current_window_size().height_dp - 32.0 - 60.0 - 61.0).clamp(240.0, 560.0);
    EmberSheet("새 세션", true, close.clone(), None, || {
        // A long form on a short window scrolls inside the dialog.
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().height(body_height)).content(|| {
            cu::Column().space_role(SpaceRole::Md).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                Field("프로젝트", || {
                    let (options, projects) = (project_options.clone(), projects.clone());
                    let (ui, project, agent, account, computer) =
                        (ui.clone(), project.clone(), agent.clone(), account.clone(), computer.clone());
                    Select(
                        &project_options[project_index.min(project_options.len() - 1)],
                        &project_options,
                        project_index,
                        POPUP_WIDTH,
                        move |i| {
                            let p = options.get(i).cloned().unwrap_or_default();
                            let p = if projects.contains(&p) { p } else { String::new() };
                            let remembered = ui.prefs.get_untracked().last_used.get(&p).cloned();
                            seed(&p, remembered, &project, &agent, &account, &computer);
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
                        placeholder: if default_cwd.is_empty() { "/path/to/project".to_string() } else { default_cwd.clone() },
                        on_value_change: Some(Box::new(move |t| v.set(t))),
                        ..Default::default()
                    });
                });
                Field("모델 (선택)", || {
                    let v = model_text.clone();
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: if default_model.is_empty() { "에이전트 기본값".to_string() } else { default_model.clone() },
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
                Field("에이전트", || {
                    let (agents_for, agent, account) = (agents.clone(), agent.clone(), account.clone());
                    Select(&agent_value, &agents, agent_index, POPUP_WIDTH, move |i| {
                        if let Some(a) = agents_for.get(i) {
                            agent.set(a.clone());
                            // Accounts belong to an agent.
                            account.set(None);
                        }
                    });
                });
                Field("계정", || {
                    let (choices, account) = (accounts.clone(), account.clone());
                    let labels: Vec<String> = accounts.iter().map(|c| c.label.clone()).collect();
                    Select(&labels[account_index.min(labels.len() - 1)], &labels, account_index, POPUP_WIDTH, move |i| {
                        account.set(choices.get(i).and_then(|c| c.value.clone()))
                    });
                });
                Field("컴퓨터", || {
                    let (choices, computer) = (computers.clone(), computer.clone());
                    let labels: Vec<String> = computers.iter().map(|c| c.label.clone()).collect();
                    Select(&labels[computer_index.min(labels.len() - 1)], &labels, computer_index, POPUP_WIDTH, move |i| {
                        computer.set(choices.get(i).and_then(|c| c.value.clone()))
                    });
                });
            });
        });
        SheetFooter(|| {
            if busy.get() {
                cu::IndeterminateProgressIndicator().circular(true).modifier(Modifier::new().size(18.0, 18.0));
            }
            AppButton("취소", None, ButtonKind::Default, true, true, false, close.clone());
            AppButton("만들기", None, ButtonKind::Primary, true, can_start, false, start);
        });
    });
}

/// Apply a project's remembered combination to the selection states.
fn seed(
    project_name: &str,
    remembered: Option<LastUsed>,
    project: &MutableState<String>,
    agent: &MutableState<String>,
    account: &MutableState<Option<String>>,
    computer: &MutableState<Option<String>>,
) {
    project.set(project_name.to_string());
    match remembered {
        Some(l) => {
            agent.set(l.agent);
            account.set(l.account);
            computer.set(l.computer);
        }
        None => {
            account.set(None);
            computer.set(None);
        }
    }
}

/// A labelled form row.
fn Field(label: &str, content: impl FnOnce()) {
    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
        Txt(label, TypeRole::Label, muted(), None, None, None, None);
        content();
    });
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
        let (body, remember) = build_request(
            "ember",
            "codex",
            "/work",
            "o3",
            "  Fix it  ",
            Some("acc1".into()),
            Some("laptop".into()),
        );
        assert_eq!(body.project, "ember");
        assert_eq!(body.title.as_deref(), Some("Fix it"));
        assert_eq!(body.model.as_deref(), Some("o3"));
        assert_eq!(body.account.as_deref(), Some("acc1"));
        assert_eq!(remember.computer.as_deref(), Some("laptop"));
        assert_eq!(remember.cwd, "/work");
        let (blank, _) = build_request("p", "a", "/w", "", "   ", None, None);
        assert_eq!((blank.model, blank.title), (None, None));
    }

    #[test]
    fn fr_l8_the_dialog_is_composed_only_while_it_is_open() {
        let _guard = crate::services::test_guard();
        let ui = shared_ui();
        let texts = |batch: &[Mutation<'_>]| -> Vec<String> {
            batch
                .iter()
                .filter_map(|m| match m {
                    Mutation::SetProp { value: PropertyValue::String(s), .. } => Some(s.to_string()),
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
        for want in ["새 세션", "프로젝트", "에이전트", "계정", "컴퓨터", "만들기"] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
        ui.close_new_session();
    }
}
