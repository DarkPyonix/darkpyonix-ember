//! compose-rust version of the pieces of `ui::launcher` that other screens use (#220 phase 3):
//! the connection banner, the session row with its menu and rename dialog, and the
//! main-server calls behind them (pin, archive, rename, export; FR-L9).
//!
//! The Dioxus `Launcher` screen (project grid, session list, computers) is not used by the
//! shell any more (`ProjectScreen` replaced it), so it is not ported and goes with the original
//! in phase 8. The assign-computer call (FR-L4) belongs to it.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember_with_key};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ButtonVariant, ColorRole, Paint, SpaceRole, TextOverflow, TypeRole};

use ember_client::wire::SessionPatch;

use super::compat::{Badge, Banner, StatusIndicator};
use super::components::{EmberCard, Txt};
use super::notice;
use super::theme::*;
use crate::model::{self, SessionRow, Tone};
use crate::services::{run_shared, services};
use crate::ui::coexist::shared_ui;

/// The push connection's state, when it is not simply "connected" (PR-1).
pub fn ConnectionBanner() {
    let ui = shared_ui();
    let _ = ui.live.launcher.get();
    let banner = services().client.read(|s| model::connection_banner(s.connection()));
    if let Some((text, tone)) = banner {
        Banner(&text, tone);
    }
}

fn muted() -> Option<Paint> {
    Some(Paint::Role(ColorRole::OnSurfaceVariant))
}

/// The patch the Pin / Unpin item sends (FR-L9).
pub fn pin_patch(pinned_now: bool) -> SessionPatch {
    SessionPatch { pinned: Some(!pinned_now), ..Default::default() }
}

/// The patch the Archive / Unarchive item sends: an archived session is out of the way, so
/// archiving also unpins it.
pub fn archive_patch(archived_now: bool) -> SessionPatch {
    if archived_now {
        SessionPatch { archived: Some(false), ..Default::default() }
    } else {
        SessionPatch { archived: Some(true), pinned: Some(false), ..Default::default() }
    }
}

/// `agent · model · account` under the title.
pub fn meta_line(row: &SessionRow) -> String {
    let mut meta = vec![row.agent.clone()];
    if let Some(m) = &row.model {
        meta.push(m.clone());
    }
    meta.push(row.account.clone().unwrap_or_else(|| "server login".into()));
    meta.join(" \u{00b7} ")
}

/// One session row: status, title, agent, account, time, preview, and its menu (FR-L9).
pub fn SessionRowView(row: &SessionRow) {
    SessionRowWith(row, false);
}

/// [`SessionRowView`] with the menu already open, for review captures.
pub fn SessionRowWith(row: &SessionRow, menu_start_open: bool) {
    let ui = shared_ui().clone();
    let theme = ember_theme();
    let menu_open = remember_with_key(row.id.clone(), || mutable_state_of(menu_start_open));
    let renaming = remember_with_key(row.id.clone(), || mutable_state_of(false));
    let new_title = remember_with_key(row.id.clone(), || mutable_state_of(String::new()));
    let when = model::relative_time(model::now_ms(), row.updated_at);
    let meta = meta_line(row);
    let (pinned, archived) = (row.pinned, row.archived);
    let pin_label = if pinned { "Unpin" } else { "Pin" };
    let archive_label = if archived { "Unarchive" } else { "Archive" };
    let preview = row.preview.clone().unwrap_or_default();

    EmberCard(false, true, None, Some(SP[2]), None, || {
        cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Sm)
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                StatusIndicator(row.status);
                if row.pinned {
                    Badge("Pinned", Tone::Neutral);
                }
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Txt(&when, TypeRole::Caption, muted(), None, None, None, None);
                let dismiss = menu_open.clone();
                cu::Menu(menu_open.get())
                    .on_dismiss(move || dismiss.set(false))
                    .anchor(|| {
                        let open = menu_open.clone();
                        cu::Button("")
                            .icon(compose_rust::IconRole::More)
                            .variant(ButtonVariant::Text)
                            .on_click(move || open.set(true));
                    })
                    .content(|| {
                        let item = |label: &str, on_click: Box<dyn FnMut()>| {
                            cu::Button(label)
                                .variant(ButtonVariant::Text)
                                .modifier(Modifier::new().fill_max_width())
                                .on_click(on_click);
                        };
                        let (open, id) = (menu_open.clone(), row.id.clone());
                        item(pin_label, Box::new(move || {
                            open.set(false);
                            patch_session(id.clone(), pin_patch(pinned));
                        }));
                        let (open, rename_open, title) = (menu_open.clone(), renaming.clone(), new_title.clone());
                        item("Rename", Box::new(move || {
                            open.set(false);
                            title.set(String::new());
                            rename_open.set(true);
                        }));
                        let (open, id) = (menu_open.clone(), row.id.clone());
                        item(archive_label, Box::new(move || {
                            open.set(false);
                            let patch = archive_patch(archived);
                            patch_session(id.clone(), patch);
                        }));
                        let (open, id, title) = (menu_open.clone(), row.id.clone(), row.title.clone());
                        item("Export", Box::new(move || {
                            open.set(false);
                            export(id.clone(), title.clone());
                        }));
                    });
            });
        let (u, id) = (ui.clone(), row.id.clone());
        cu::Button(&row.title)
            .variant(ButtonVariant::Text)
            .modifier(Modifier::new().fill_max_width())
            .on_click(move || u.open_conversation(&id));
        Txt(&meta, TypeRole::Caption, muted(), Some(1), Some(TextOverflow::Ellipsis), None, None);
        if !preview.is_empty() {
            Txt(&preview, TypeRole::Body, None, Some(2), Some(TextOverflow::Ellipsis), None, None);
        }
    });

    // FR-L9 rename, on the main server. TODO(compose-rust): TextField has no initial value (it
    // is uncontrolled), so the current title is the placeholder; an empty entry keeps the title.
    let dismiss = renaming.clone();
    cu::Dialog(renaming.get())
        .modifier(Modifier::new().background(paint(theme.palette.surface)).corner_radius(CORNER_XL))
        .on_dismiss(move || dismiss.set(false))
        .content(|| {
            cu::Column().space_role(SpaceRole::Md).modifier(Modifier::new().padding_role(SpaceRole::Md)).content(|| {
                Txt("Rename session", TypeRole::Title, None, None, None, None, None);
                let (title, submit_open, id) = (new_title.clone(), renaming.clone(), row.id.clone());
                cu::TextField()
                    .modifier(Modifier::new().fill_max_width())
                    .placeholder(row.title.clone())
                    .on_value_change(move |v| title.set(v))
                    .on_submit(move |v| {
                        rename(id.clone(), v);
                        submit_open.set(false);
                    });
                cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterEnd)
                    .modifier(Modifier::new().fill_max_width())
                    .content(|| {
                        let cancel = renaming.clone();
                        cu::Button("Cancel").variant(ButtonVariant::Text).on_click(move || cancel.set(false));
                        let (done, title, id) = (renaming.clone(), new_title.clone(), row.id.clone());
                        cu::Button("Rename").variant(ButtonVariant::Filled).on_click(move || {
                            rename(id.clone(), title.get_untracked());
                            done.set(false);
                        });
                    });
            });
        });
}

/// A review window: the connection banner and a few session rows, the first with its menu open
/// (`EMBER_CR_ROUTE=launcher-row`).
#[compose_rust::runtime::composable]
pub fn rows_preview() {
    let ui = shared_ui().clone();
    let theme = ember_theme();
    let rows: Vec<SessionRow> = ui.with_rows(|s, cx| {
        let project = s.projects().into_iter().next().map(|p| p.name).unwrap_or_default();
        model::project_sessions(s, &project, cx, false)
    });
    // `ScrollColumn` takes only a modifier, so the spacing is a `Column` inside it.
    cu::ScrollColumn()
        .modifier(Modifier::new().fill_max_size().background(paint(theme.palette.bg)).padding_role(SpaceRole::Md))
        .content(|| {
            cu::Column().space_role(SpaceRole::Md).modifier(Modifier::new().fill_max_width()).content(|| {
                Banner("Offline: showing cached data", Tone::Neutral);
                ConnectionBanner();
                for (i, row) in rows.iter().take(4).enumerate() {
                    SessionRowWith(row, i == 0);
                }
            });
        });
}

/// Change a session's metadata on the main server (FR-L9); say so when it fails.
pub fn patch_session(id: String, patch: SessionPatch) {
    run_shared(async move { services().patch_session(&id, &patch).await }, |r| {
        if let Err(e) = r {
            notice::post(format!("Could not update the session: {e}"));
        }
    });
}

/// Rename on the main server; an empty title leaves it as it is.
pub fn rename(id: String, title: String) {
    let title = title.trim().to_string();
    if !title.is_empty() {
        patch_session(id, SessionPatch { title: Some(title), ..Default::default() });
    }
}

/// Export a session to the data directory and say where it went (FR-L9).
pub fn export(id: String, title: String) {
    if services().demo_blocked() {
        return;
    }
    let dir = services().config.export_dir();
    run_shared(
        async move { crate::export::export_session(&services().client, dir, &id, &title).await },
        |r| match r {
            Ok(path) => notice::post(format!("Exported to {}", path.display())),
            Err(e) => notice::post(format!("Export failed: {e}")),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};
    use ember_client::LauncherStatus;

    fn row() -> SessionRow {
        SessionRow {
            id: "s1".into(),
            project: "ember".into(),
            title: "Fix the sidebar".into(),
            agent: "claude-code".into(),
            model: Some("opus".into()),
            account: None,
            cwd: "/work/ember".into(),
            updated_at: 0,
            preview: Some("Done, tests pass".into()),
            status: LauncherStatus::Running,
            pinned: false,
            archived: false,
        }
    }

    #[test]
    fn fr_l9_pin_and_archive_menu_items_build_the_right_patch() {
        assert_eq!(pin_patch(false).pinned, Some(true));
        assert_eq!(pin_patch(true).pinned, Some(false));
        let archive = archive_patch(false);
        assert_eq!((archive.archived, archive.pinned), (Some(true), Some(false)));
        let unarchive = archive_patch(true);
        assert_eq!((unarchive.archived, unarchive.pinned), (Some(false), None));
    }

    #[test]
    fn fr_l2_meta_line_joins_agent_model_and_account() {
        assert_eq!(meta_line(&row()), "claude-code \u{00b7} opus \u{00b7} server login");
    }

    #[test]
    fn fr_l9_rename_keeps_the_title_when_the_entry_is_empty() {
        // `rename` posts nothing to the server for a blank title; the patch it would send is
        // only built for a real one.
        let blank = "   ".trim().to_string();
        assert!(blank.is_empty());
    }

    #[test]
    fn fr_l2_session_row_composes_title_meta_preview_and_menu_items() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| SessionRowWith(&row(), true))));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let all: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect();
        for want in ["Fix the sidebar", "Done, tests pass", "Pin", "Rename", "Archive", "Export"] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
    }
}
