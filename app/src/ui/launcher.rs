//! The main screen (FR-L1–FR-L4): project cards with status badges, the selected project's
//! sessions, and the computers at the bottom with their assignment to the selected project.
//!
//! Pin, archive and rename (FR-L9) and computer assignment (FR-L4) are main-server calls; the
//! answer updates this client at once and the server's push updates every other client.
//!
//! Everything here renders from the client's state, which starts from the on-disk cache, so
//! the screen is complete before the first network answer (FR-L1) and updates in place as
//! pushes arrive (FR-L2).

use dioxus_compose::prelude::*;

use ember_client::state::Reachability;
use ember_client::wire::SessionPatch;

use crate::model::{self, ProjectCard, SessionRow, Tone};
use crate::services::{run, services};
use crate::ui::compat::{Badge, Banner, Dot, SplitPane, StatusIndicator};
use crate::ui::use_ui;

#[component]
pub fn Launcher() -> Element {
    let ui = use_ui();
    let window = use_window_size();

    let cards: Vec<ProjectCard> = {
        let _ = *ui.live.launcher.read();
        services().client.read(model::project_cards)
    };
    // The selected project, falling back to the most recently active one.
    let selected = ui
        .selected_project
        .read()
        .clone()
        .filter(|p| cards.iter().any(|c| &c.name == p))
        .or_else(|| cards.first().map(|c| c.name.clone()));

    let projects = rsx! { ProjectGrid { cards: cards.clone(), selected: selected.clone() } };
    let sessions = rsx! { SessionList { project: selected.clone() } };

    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            padding_role: SpaceRole::Md,
            space_role: SpaceRole::Md,
            ConnectionBanner {}
            if cards.is_empty() {
                EmptyState {}
            } else if window.is_expanded() {
                dioxus_compose::Box {
                    fill_max_width: true,
                    weight: 1.0,
                    SplitPane { start: projects, end: sessions, start_weight: 0.42 }
                }
            } else {
                Column {
                    fill_max_width: true,
                    weight: 0.45,
                    {projects}
                }
                Divider {}
                Column {
                    fill_max_width: true,
                    weight: 0.55,
                    {sessions}
                }
            }
            Divider {}
            ComputerList { project: selected.clone() }
        }
    }
}

/// What the main screen says before there is anything to list.
#[component]
fn EmptyState() -> Element {
    let ui = use_ui();
    rsx! {
        Column {
            fill_max_width: true,
            weight: 1.0,
            alignment: Alignment::Center,
            arrangement: Arrangement::Center,
            space_role: SpaceRole::Md,
            Text { text: "No conversations yet", type_role: TypeRole::Headline }
            Text {
                text: "Start one in a project folder on the main server.",
                type_role: TypeRole::Body,
                color: Paint::Role(ColorRole::OnSurfaceVariant),
            }
            Button {
                text: "New session",
                icon: IconRole::Compose,
                variant: ButtonVariant::Filled,
                on_click: move |_| ui.open_new_session(None),
            }
        }
    }
}

/// The push connection's state, when it is not simply "connected" (PR-1).
#[component]
pub fn ConnectionBanner() -> Element {
    let ui = use_ui();
    let _ = *ui.live.launcher.read();
    let banner = services().client.read(|s| model::connection_banner(s.connection()));
    match banner {
        Some((text, tone)) => rsx! { Banner { text, tone } },
        None => rsx! {},
    }
}

/// Project cards (FR-L1) with status badges (FR-L2).
#[component]
fn ProjectGrid(cards: Vec<ProjectCard>, selected: Option<String>) -> Element {
    let count = cards.len();
    let keys: Vec<String> = cards.iter().map(|c| c.name.clone()).collect();
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            space_role: SpaceRole::Sm,
            Text { text: "Projects", type_role: TypeRole::Subtitle }
            LazyGrid {
                fill_max_width: true,
                weight: 1.0,
                min_column_width: 200.0,
                item_count: count,
                key_of: move |i: usize| keys[i].clone(),
                item: move |i: usize| {
                    let card = cards[i].clone();
                    let is_selected = selected.as_deref() == Some(card.name.as_str());
                    rsx! { ProjectCardView { card, selected: is_selected } }
                },
            }
        }
    }
}

#[component]
fn ProjectCardView(card: ProjectCard, selected: bool) -> Element {
    let ui = use_ui();
    let name = card.name.clone();
    let name_for_new = card.name.clone();
    let badges = card.summary.badges();
    let count = card.summary.total;
    let when = model::relative_time(model::now_ms(), card.updated_at);
    rsx! {
        Card {
            fill_max_width: true,
            padding_role: SpaceRole::Sm,
            border_width: if selected { Some(2.0) } else { None },
            border_color: if selected { Some(Paint::Role(ColorRole::Primary)) } else { None },
            Column {
                fill_max_width: true,
                space_role: SpaceRole::Xs,
                // TODO(dioxus-compose): a clickable Card; the title button is the hit target.
                Button {
                    text: card.name.clone(),
                    variant: ButtonVariant::Text,
                    fill_max_width: true,
                    on_click: move |_| {
                        let mut sel = ui.selected_project;
                        sel.set(Some(name.clone()));
                        let n = name.clone();
                        ui.update_prefs(move |p| p.last_project = Some(n));
                    },
                }
                if !badges.is_empty() {
                    Row {
                        space_role: SpaceRole::Xs,
                        for (text, tone) in badges {
                            Badge { key: "{text}", text: text.clone(), tone }
                        }
                    }
                }
                Row {
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    Text {
                        text: format!("{count} session{} \u{00b7} {when}", if count == 1 { "" } else { "s" }),
                        type_role: TypeRole::Caption,
                        color: Paint::Role(ColorRole::OnSurfaceVariant),
                        weight: 1.0,
                    }
                    Tooltip {
                        text: "New session in this project",
                        Button {
                            text: "",
                            icon: IconRole::Add,
                            variant: ButtonVariant::Text,
                            on_click: move |_| ui.open_new_session(Some(name_for_new.clone())),
                        }
                    }
                }
            }
        }
    }
}

/// The selected project's sessions (FR-L2).
#[component]
fn SessionList(project: Option<String>) -> Element {
    let ui = use_ui();
    let show_archived = (ui.show_archived)();
    let rows: Vec<SessionRow> = match &project {
        Some(p) => ui.with_rows(|s, cx| model::project_sessions(s, p, cx, show_archived)),
        None => Vec::new(),
    };
    let count = rows.len();
    let keys: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let title = project.clone().unwrap_or_else(|| "Sessions".into());
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            space_role: SpaceRole::Sm,
            Row {
                fill_max_width: true,
                alignment: Alignment::CenterStart,
                space_role: SpaceRole::Sm,
                Text { text: title, type_role: TypeRole::Subtitle, weight: 1.0, max_lines: 1, overflow: TextOverflow::Ellipsis }
                Text { text: "Archived", type_role: TypeRole::Caption, color: Paint::Role(ColorRole::OnSurfaceVariant) }
                Switch {
                    checked: show_archived,
                    on_change: move |v: bool| {
                        let mut s = ui.show_archived;
                        s.set(v);
                    },
                }
                Button {
                    text: "New",
                    icon: IconRole::Add,
                    variant: ButtonVariant::Tonal,
                    on_click: move |_| ui.open_new_session(project.clone()),
                }
            }
            if count == 0 {
                Text {
                    text: "No sessions in this project.",
                    type_role: TypeRole::Body,
                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                }
            }
            LazyColumn {
                fill_max_width: true,
                weight: 1.0,
                item_count: count,
                key_of: move |i: usize| keys[i].clone(),
                item: move |i: usize| {
                    let row = rows[i].clone();
                    rsx! { SessionRowView { row } }
                },
            }
        }
    }
}

/// One session row: status, title, agent, account, time, preview, and its menu (FR-L9).
#[component]
pub fn SessionRowView(row: SessionRow) -> Element {
    let ui = use_ui();
    let mut menu_open = use_signal(|| false);
    let mut renaming = use_signal(|| false);
    let mut new_title = use_signal(String::new);
    let id = row.id.clone();
    let when = model::relative_time(model::now_ms(), row.updated_at);
    let mut meta = vec![row.agent.clone()];
    if let Some(m) = &row.model {
        meta.push(m.clone());
    }
    meta.push(row.account.clone().unwrap_or_else(|| "server login".into()));
    let meta = meta.join(" \u{00b7} ");
    let (pinned, archived) = (row.pinned, row.archived);
    let pin_label = if pinned { "Unpin" } else { "Pin" };
    let archive_label = if archived { "Unarchive" } else { "Archive" };
    let (id_open, id_pin, id_arch, id_export, id_rename) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
    let title_for_export = row.title.clone();
    let current_title = row.title.clone();
    let preview = row.preview.clone().unwrap_or_default();

    rsx! {
        Card {
            fill_max_width: true,
            padding_role: SpaceRole::Sm,
            Column {
                fill_max_width: true,
                space_role: SpaceRole::Xs,
                Row {
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    space_role: SpaceRole::Sm,
                    StatusIndicator { status: row.status }
                    if row.pinned {
                        Badge { text: "Pinned", tone: Tone::Neutral }
                    }
                    Spacer { weight: 1.0 }
                    Text { text: when, type_role: TypeRole::Caption, color: Paint::Role(ColorRole::OnSurfaceVariant) }
                    Menu {
                        expanded: menu_open(),
                        on_dismiss: move |_| menu_open.set(false),
                        anchor: rsx! {
                            Button {
                                text: "",
                                icon: IconRole::More,
                                variant: ButtonVariant::Text,
                                on_click: move |_| menu_open.set(true),
                            }
                        },
                        Button {
                            text: pin_label,
                            variant: ButtonVariant::Text,
                            fill_max_width: true,
                            on_click: move |_| {
                                menu_open.set(false);
                                patch_session(id_pin.clone(), SessionPatch { pinned: Some(!pinned), ..Default::default() });
                            },
                        }
                        Button {
                            text: "Rename",
                            variant: ButtonVariant::Text,
                            fill_max_width: true,
                            on_click: move |_| {
                                menu_open.set(false);
                                new_title.set(String::new());
                                renaming.set(true);
                            },
                        }
                        Button {
                            text: archive_label,
                            variant: ButtonVariant::Text,
                            fill_max_width: true,
                            on_click: move |_| {
                                menu_open.set(false);
                                // An archived session is out of the way; it is not also pinned.
                                let patch = if archived {
                                    SessionPatch { archived: Some(false), ..Default::default() }
                                } else {
                                    SessionPatch { archived: Some(true), pinned: Some(false), ..Default::default() }
                                };
                                patch_session(id_arch.clone(), patch);
                            },
                        }
                        Button {
                            text: "Export",
                            variant: ButtonVariant::Text,
                            fill_max_width: true,
                            on_click: move |_| {
                                menu_open.set(false);
                                export(id_export.clone(), title_for_export.clone());
                            },
                        }
                    }
                }
                Button {
                    text: row.title.clone(),
                    variant: ButtonVariant::Text,
                    fill_max_width: true,
                    on_click: move |_| ui.open_conversation(&id_open),
                }
                Text {
                    text: meta,
                    type_role: TypeRole::Caption,
                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                    max_lines: 1,
                    overflow: TextOverflow::Ellipsis,
                }
                if !preview.is_empty() {
                    Text {
                        text: preview,
                        type_role: TypeRole::Body,
                        max_lines: 2,
                        overflow: TextOverflow::Ellipsis,
                    }
                }
            }
        }
        // FR-L9 rename, on the main server. TODO(dioxus-compose): TextField has no initial
        // value (it is uncontrolled), so the current title is the placeholder; an empty entry
        // keeps the title.
        Dialog {
            open: renaming(),
            on_dismiss: move |_| renaming.set(false),
            Column {
                space_role: SpaceRole::Md,
                padding_role: SpaceRole::Md,
                Text { text: "Rename session", type_role: TypeRole::Title }
                TextField {
                    fill_max_width: true,
                    placeholder: current_title.clone(),
                    on_value_change: move |v: String| new_title.set(v),
                    on_submit: move |v: String| {
                        rename(id_rename.clone(), v);
                        renaming.set(false);
                    },
                }
                Row {
                    fill_max_width: true,
                    arrangement: Arrangement::End,
                    space_role: SpaceRole::Sm,
                    Button { text: "Cancel", variant: ButtonVariant::Text, on_click: move |_| renaming.set(false) }
                    Button {
                        text: "Rename",
                        variant: ButtonVariant::Filled,
                        on_click: move |_| {
                            rename(id.clone(), new_title());
                            renaming.set(false);
                        },
                    }
                }
            }
        }
    }
}

/// Change a session's metadata on the main server (FR-L9); say so when it fails.
pub fn patch_session(id: String, patch: SessionPatch) {
    run(async move { services().client.patch_session(&id, &patch).await }, |r| {
        if let Err(e) = r {
            Message::new(format!("Could not update the session: {e}")).with_duration(MessageDuration::Long).show();
        }
    });
}

/// Rename on the main server; an empty title leaves it as it is.
fn rename(id: String, title: String) {
    let title = title.trim().to_string();
    if !title.is_empty() {
        patch_session(id, SessionPatch { title: Some(title), ..Default::default() });
    }
}

/// Assign or unassign a computer to a project on the main server (FR-L4).
fn set_assignment(project: String, computer_id: String, assign: bool) {
    run(
        async move {
            let c = &services().client;
            if assign {
                c.assign_computer(&project, &computer_id).await
            } else {
                c.unassign_computer(&project, &computer_id).await
            }
        },
        |r| {
            if let Err(e) = r {
                Message::new(format!("Could not change the assignment: {e}")).with_duration(MessageDuration::Long).show();
            }
        },
    );
}

/// Export a session to the data directory and say where it went (FR-L9).
pub fn export(id: String, title: String) {
    let dir = services().config.export_dir();
    run(
        async move { crate::export::export_session(&services().client, dir, &id, &title).await },
        |r| match r {
            Ok(path) => Message::new(format!("Exported to {}", path.display()))
                .with_duration(MessageDuration::Long)
                .show(),
            Err(e) => Message::new(format!("Export failed: {e}")).with_duration(MessageDuration::Long).show(),
        },
    );
}

/// The computers at the bottom of the main screen (FR-L3): reachability, assigned projects,
/// and a switch assigning each to the selected project (FR-L4).
#[component]
fn ComputerList(project: Option<String>) -> Element {
    let ui = use_ui();
    let _ = *ui.live.launcher.read();
    let (computers, assigned): (Vec<_>, Vec<bool>) = services().client.read(|s| {
        let list = s.computers().to_vec();
        let assigned = list
            .iter()
            .map(|c| project.as_deref().is_some_and(|p| model::is_assigned(s, p, &c.id)))
            .collect();
        (list, assigned)
    });
    let has_project = project.is_some();
    let selected = project.clone().unwrap_or_default();
    rsx! {
        Column {
            fill_max_width: true,
            space_role: SpaceRole::Xs,
            Text { text: "Computers", type_role: TypeRole::Subtitle }
            if computers.is_empty() {
                Text {
                    text: "No computers yet.",
                    type_role: TypeRole::Caption,
                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                }
            }
            // TODO(dioxus-compose M9, ~10-08): a horizontally scrolling strip of cards.
            for (c, is_assigned) in computers.into_iter().zip(assigned) {
                Row {
                    key: "{c.id}",
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    space_role: SpaceRole::Sm,
                    Dot {
                        tone: match c.reachability {
                            Reachability::Online => Tone::Success,
                            Reachability::Offline => Tone::Error,
                            Reachability::Unknown => Tone::Neutral,
                        },
                    }
                    Text { text: c.name.clone(), type_role: TypeRole::BodyStrong }
                    Text {
                        text: model::reachability_label(c.reachability),
                        type_role: TypeRole::Caption,
                        color: Paint::Role(ColorRole::OnSurfaceVariant),
                    }
                    Spacer { weight: 1.0 }
                    Text {
                        text: if c.projects.is_empty() { "No projects assigned".to_string() } else { model::join_projects(&c.projects) },
                        type_role: TypeRole::Caption,
                        color: Paint::Role(ColorRole::OnSurfaceVariant),
                        max_lines: 1,
                        overflow: TextOverflow::Ellipsis,
                    }
                    if has_project {
                        Tooltip {
                            text: if is_assigned { format!("Unassign from {selected}") } else { format!("Assign to {selected}") },
                            Switch {
                                checked: is_assigned,
                                on_change: {
                                    let (p, id) = (selected.clone(), c.id.clone());
                                    move |v: bool| set_assignment(p.clone(), id.clone(), v)
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}
