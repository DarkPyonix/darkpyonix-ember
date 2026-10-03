//! New session dialog (FR-L8): project, folder, agent (`GET /agents`), account
//! (`GET /accounts`), computer (`GET /computers`) and model, defaulting to the last-used
//! combination for the project.
//!
//! TODO(dioxus-compose): `TextField` is uncontrolled and has no initial value, so remembered
//! text (folder, model) is shown as the placeholder and an untouched field means "use it".

use dioxus_compose::prelude::*;

use ember_client::state::Input;

use crate::model::{self, index_of};
use crate::prefs::LastUsed;
use crate::server::CreateSession;
use crate::services::{run, services};
use crate::ui::use_ui;

/// Agents offered when the server has not answered `GET /agents` yet.
const FALLBACK_AGENTS: [&str; 2] = ["claude-code", "codex"];

/// The local computer's id in the server's registry (`crates/server/src/computers`).
const LOCAL_COMPUTER: &str = "local";

#[derive(Debug, Clone, PartialEq)]
struct Choice {
    /// `None` = the default ("router chooses", "this server").
    value: Option<String>,
    label: String,
}

#[component]
pub fn NewSessionDialog() -> Element {
    let ui = use_ui();
    // The form is mounted only while the dialog is open, so every opening starts from the
    // remembered combination with empty text fields.
    match (ui.new_session)() {
        Some(preset) => rsx! { NewSessionForm { key: "{preset}", preset } },
        None => rsx! {},
    }
}

#[component]
fn NewSessionForm(preset: String) -> Element {
    let ui = use_ui();

    let projects: Vec<String> = {
        let _ = *ui.live.launcher.read();
        services().client.read(|s| s.projects().into_iter().map(|p| p.name).collect())
    };

    // Initial selection: the preset project (or the last one used), and what was last used
    // with it (FR-L8).
    let initial = use_hook(|| {
        let prefs = ui.prefs.peek();
        let p = if preset.is_empty() {
            prefs.last_project.clone().filter(|p| projects.contains(p)).or_else(|| projects.first().cloned()).unwrap_or_default()
        } else {
            preset.clone()
        };
        let last = prefs.last_used.get(&p).cloned().unwrap_or_default();
        (p, last)
    });
    let mut project = use_signal(|| initial.0.clone());
    let mut new_project = use_signal(String::new);
    let mut agent = use_signal(|| initial.1.agent.clone());
    let mut account = use_signal(|| initial.1.account.clone());
    let mut computer = use_signal(|| initial.1.computer.clone());
    let mut cwd = use_signal(String::new);
    let mut model_text = use_signal(String::new);
    let mut title = use_signal(String::new);
    let busy = use_signal(|| false);

    // ---- options -------------------------------------------------------------------------------
    const NEW_PROJECT: &str = "New project\u{2026}";
    let mut project_options = projects.clone();
    project_options.push(NEW_PROJECT.to_string());
    let creating_project = project().is_empty() || !projects.contains(&project());
    let project_index = if creating_project { project_options.len() - 1 } else { index_of(&project_options, Some(&project())) };
    let effective_project = if creating_project { new_project().trim().to_string() } else { project() };

    let agents: Vec<String> = {
        let detected = ui.live.agents.read();
        let installed: Vec<String> = detected.iter().filter(|a| a.installed).map(|a| a.kind.clone()).collect();
        if installed.is_empty() { FALLBACK_AGENTS.iter().map(|s| s.to_string()).collect() } else { installed }
    };
    let agent_value = if agents.contains(&agent()) { agent() } else { agents.first().cloned().unwrap_or_default() };
    let agent_index = index_of(&agents, Some(&agent_value));

    let accounts: Vec<Choice> = {
        let list = ui.live.accounts.read();
        let mut out = vec![Choice { value: None, label: "Automatic (the server's router chooses)".into() }];
        out.extend(list.iter().filter(|a| a.agent == agent_value).map(|a| Choice {
            value: Some(a.id.clone()),
            label: format!(
                "{}{}{}",
                a.label,
                if a.is_default { " (default)" } else { "" },
                if a.limited { " \u{00b7} limited" } else { "" }
            ),
        }));
        out
    };
    let account_index = accounts.iter().position(|c| c.value == account()).unwrap_or(0);

    let computers: Vec<Choice> = {
        let _ = *ui.live.launcher.read();
        let mut out = vec![Choice { value: None, label: "The main server (default)".into() }];
        services().client.read(|s| {
            out.extend(s.computers().iter().filter(|c| c.id != LOCAL_COMPUTER).map(|c| Choice {
                value: Some(c.id.clone()),
                label: format!("{} ({})", c.name, model::reachability_label(c.reachability)),
            }))
        });
        out
    };
    let computer_index = computers.iter().position(|c| c.value == computer()).unwrap_or(0);

    // Remembered or derived defaults for the free-text fields.
    let remembered = ui.prefs.read().last_used.get(&effective_project).cloned();
    let default_cwd = remembered
        .as_ref()
        .map(|l| l.cwd.clone())
        .filter(|c| !c.is_empty())
        .or_else(|| services().client.read(|s| model::latest_cwd(s, &effective_project)))
        .unwrap_or_default();
    let default_model = remembered.as_ref().and_then(|l| l.model.clone()).unwrap_or_default();
    let cwd_value = if cwd().trim().is_empty() { default_cwd.clone() } else { cwd().trim().to_string() };
    let model_value = if model_text().trim().is_empty() { default_model.clone() } else { model_text().trim().to_string() };
    let can_start = !busy() && !effective_project.is_empty() && !cwd_value.is_empty() && !agent_value.is_empty();

    let close = move || {
        let mut d = ui.new_session;
        d.set(None);
    };

    let start = {
        let accounts = accounts.clone();
        let computers = computers.clone();
        move |_: ()| {
            if !can_start {
                return;
            }
            let mut b = busy;
            b.set(true);
            let account_id = accounts.get(account_index).and_then(|c| c.value.clone());
            let computer_id = computers.get(computer_index).and_then(|c| c.value.clone());
            let model = (!model_value.is_empty()).then(|| model_value.clone());
            let t = title().trim().to_string();
            let body = CreateSession {
                project: effective_project.clone(),
                agent: agent_value.clone(),
                cwd: cwd_value.clone(),
                model: model.clone(),
                title: (!t.is_empty()).then_some(t),
                account: account_id.clone(),
            };
            let remember = LastUsed {
                agent: agent_value.clone(),
                account: account_id,
                computer: computer_id.clone(),
                model,
                cwd: cwd_value.clone(),
            };
            let project_name = effective_project.clone();
            run(
                async move {
                    let s = services();
                    let rec = s.server.create_session(&body).await.map_err(|e| e.to_string())?;
                    s.client.apply(Input::SessionLoaded(rec.clone()));
                    let mut notice = None;
                    if let Some(cid) = computer_id {
                        // A session starts on the main server; move it before the first message.
                        if let Err(e) = s.server.switch_computer(&rec.id, &cid).await {
                            notice = Some(format!("Started on the main server: could not switch computer ({e})"));
                        }
                    }
                    Ok::<_, String>((rec, notice))
                },
                move |r| {
                    let mut busy = busy;
                    busy.set(false);
                    match r {
                        Ok((rec, notice)) => {
                            ui.update_prefs(|p| {
                                p.last_used.insert(project_name.clone(), remember);
                                p.last_project = Some(project_name);
                            });
                            let mut d = ui.new_session;
                            d.set(None);
                            ui.open_conversation(&rec.id);
                            if let Some(n) = notice {
                                Message::new(n).with_duration(MessageDuration::Long).show();
                            }
                        }
                        Err(e) => Message::new(format!("Could not start the session: {e}"))
                            .with_duration(MessageDuration::Long)
                            .show(),
                    }
                },
            );
        }
    };

    rsx! {
        Dialog {
            open: true,
            on_dismiss: move |_| close(),
            // A long form on a short window scrolls inside the dialog.
            ScrollColumn {
                fill_max_width: true,
                Column {
                    space_role: SpaceRole::Md,
                    padding_role: SpaceRole::Md,
                    Text { text: "New session", type_role: TypeRole::Title }

                    Field { label: "Project",
                        Dropdown {
                            fill_max_width: true,
                            selected_index: project_index,
                            on_change: {
                                let options = project_options.clone();
                                let projects = projects.clone();
                                move |i: usize| {
                                    let p = options.get(i).cloned().unwrap_or_default();
                                    let p = if projects.contains(&p) { p } else { String::new() };
                                    let remembered = ui.prefs.peek().last_used.get(&p).cloned();
                                    seed(&p, remembered, &mut project, &mut agent, &mut account, &mut computer);
                                }
                            },
                            for o in project_options.iter() {
                                Text { key: "{o}", text: o.clone() }
                            }
                        }
                    }
                    Column {
                        fill_max_width: true,
                        space_role: SpaceRole::Md,
                        if creating_project {
                            Field { label: "Project name",
                                TextField {
                                    fill_max_width: true,
                                    placeholder: "my-project",
                                    on_value_change: move |v: String| new_project.set(v),
                                }
                            }
                        }
                        Field { label: "Folder on the computer",
                            TextField {
                                fill_max_width: true,
                                placeholder: if default_cwd.is_empty() { "/path/to/project".to_string() } else { default_cwd.clone() },
                                on_value_change: move |v: String| cwd.set(v),
                            }
                        }
                        Field { label: "Model (optional)",
                            TextField {
                                fill_max_width: true,
                                placeholder: if default_model.is_empty() { "Agent default".to_string() } else { default_model.clone() },
                                on_value_change: move |v: String| model_text.set(v),
                            }
                        }
                        Field { label: "Title (optional)",
                            TextField {
                                fill_max_width: true,
                                placeholder: "New conversation",
                                on_value_change: move |v: String| title.set(v),
                            }
                        }
                }
                    Field { label: "Agent",
                        Dropdown {
                            fill_max_width: true,
                            selected_index: agent_index,
                            on_change: {
                                let agents = agents.clone();
                                move |i: usize| {
                                    if let Some(a) = agents.get(i) {
                                        agent.set(a.clone());
                                        // Accounts belong to an agent.
                                        account.set(None);
                                    }
                                }
                            },
                            for a in agents.iter() {
                                Text { key: "{a}", text: a.clone() }
                            }
                        }
                    }
                    Field { label: "Account",
                        Dropdown {
                            fill_max_width: true,
                            selected_index: account_index,
                            on_change: {
                                let accounts = accounts.clone();
                                move |i: usize| account.set(accounts.get(i).and_then(|c| c.value.clone()))
                            },
                            for c in accounts.iter() {
                                Text { key: "{c.label}", text: c.label.clone() }
                            }
                        }
                    }
                    Field { label: "Computer",
                        Dropdown {
                            fill_max_width: true,
                            selected_index: computer_index,
                            on_change: {
                                let computers = computers.clone();
                                move |i: usize| computer.set(computers.get(i).and_then(|c| c.value.clone()))
                            },
                            for c in computers.iter() {
                                Text { key: "{c.label}", text: c.label.clone() }
                            }
                        }
                    }
                    Row {
                        fill_max_width: true,
                        arrangement: Arrangement::End,
                        space_role: SpaceRole::Sm,
                        alignment: Alignment::CenterStart,
                        if busy() {
                            ProgressIndicator { determinate: false, circular: true, width: 18.0, height: 18.0 }
                        }
                        Button { text: "Cancel", variant: ButtonVariant::Text, on_click: move |_| close() }
                        Button {
                            text: "Start",
                            variant: ButtonVariant::Filled,
                            enabled: can_start,
                            on_click: start,
                        }
                    }
                }
            }
        }
    }
}

/// Apply a project's remembered combination to the selection signals.
fn seed(
    project_name: &str,
    remembered: Option<LastUsed>,
    project: &mut Signal<String>,
    agent: &mut Signal<String>,
    account: &mut Signal<Option<String>>,
    computer: &mut Signal<Option<String>>,
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
#[component]
fn Field(#[props(into)] label: String, children: Element) -> Element {
    rsx! {
        Column {
            fill_max_width: true,
            space_role: SpaceRole::Xs,
            Text { text: label, type_role: TypeRole::Label, color: Paint::Role(ColorRole::OnSurfaceVariant) }
            {children}
        }
    }
}
