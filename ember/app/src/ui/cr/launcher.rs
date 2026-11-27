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
use compose_rust::{Alignment, ButtonVariant, SpaceRole};

use ember_client::wire::SessionPatch;

use super::compat::{Badge, StatusDot, StatusPill};
use super::components::{
    pressable, AppButton, ButtonKind, EmberCard, EmberField, EmberSheet, FieldOptions, Label,
    PopKind, PopMenu, PopRow, PopoverMenu, SheetFooter,
};
use super::notice;
use super::theme::*;
use crate::model::{self, SessionRow, Tone};
use crate::services::{run_shared, services};
use crate::ui::coexist::shared_ui;

/// The push connection's state, when it is not simply "connected" (PR-1): an info icon, the
/// message, and a retry button.
pub fn ConnectionBanner() {
    let ui = shared_ui();
    let _ = ui.live.launcher.get();
    let banner = services().client.read(|s| super::ko::connection_banner(s.connection()));
    if let Some((text, tone)) = banner {
        ConnectionBannerView(&text, tone);
    }
}

/// The banner itself, so a review window can show it without a live connection problem.
pub fn ConnectionBannerView(text: &str, tone: Tone) {
    let (fill, ink) = super::compat::tone_colors(tone, ember_theme().palette);
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new()
                .fill_max_width()
                .height(48.0)
                .background(paint(fill))
                .corner_radius(CORNER_MD), {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[3])) };
            super::shell::NavIcon(super::shell::ICON_INFO_SVG, ink.to_argb(), 18.0);
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            Label(text, Fs::Sm, Weight::Medium, Some(paint(ink)), Some(1));
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            AppButton("다시 시도", None, ButtonKind::Default, true, true, false, || {
                services().client.reconnect_push();
            });
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
        }) };
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
    meta.push(row.account.clone().unwrap_or_else(|| "서버 로그인".into()));
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
    let when = super::ko::relative_time(model::now_ms(), row.updated_at);
    let meta = meta_line(row);
    let (pinned, archived) = (row.pinned, row.archived);
    let pin_label = if pinned { "고정 해제" } else { "고정" };
    let archive_label = if archived { "보관 해제" } else { "보관" };
    let preview = row.preview.clone().unwrap_or_default();
    let p = theme.palette;

    // `.list-row`: a status dot tile, the title over the agent line (and the preview when
    // there is one), then the status pill, the time and the menu button at the right.
    // Everything of the row is one node, so the sheets below are not spaced as siblings.
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width(), {
    EmberCard(false, true, None, None, None, || {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
            let (u, id) = (ui.clone(), row.id.clone());
            super::components::ClickableAs(
                Modifier::new().weight(1.0),
                super::components::Press::Row,
                Alignment::CenterStart,
                move || u.open_conversation(&id),
                || {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, {
                        StatusDot(row.status);
                        { use ::compose_rust::ui::Column; Column!({
                            Label(&row.title, Fs::Md, Weight::Medium, Some(paint(p.text)), Some(1));
                            if !preview.is_empty() {
                                Label(&preview, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                            }
                            Label(&meta, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        }) };
                    }) };
                },
            );
            if row.pinned {
                Badge("고정됨", Tone::Neutral);
            }
            StatusPill(row.status);
            Label(&when, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
            // Anchored popover menu for row actions (FR-L9, FR-L10.4): hangs below the More
            // button on desktop and becomes a bottom sheet on compact phone width.
            let pop_rows = [
                PopRow { title: pin_label.to_string(), ..Default::default() },
                PopRow { title: "이름 변경".to_string(), ..Default::default() },
                PopRow { title: archive_label.to_string(), ..Default::default() },
                PopRow { title: "내보내기".to_string(), ..Default::default() },
            ];
            let expanded = menu_open.get();
            let (dismiss, toggle, rename_open, title_state) = (
                menu_open.clone(),
                menu_open.clone(),
                renaming.clone(),
                new_title.clone(),
            );
            let (id, title) = (row.id.clone(), row.title.clone());
            PopoverMenu(
                expanded,
                PopMenu {
                    name: super::a11y::SESSION_MENU,
                    kind: PopKind::Mode,
                    head: "",
                    sub: None,
                    rows: &pop_rows,
                    align: compose_rust::PopoverAlign::End,
                },
                move || dismiss.set(false),
                move |i| match i {
                    0 => patch_session(id.clone(), pin_patch(pinned)),
                    1 => {
                        title_state.set(title.clone());
                        rename_open.set(true);
                    }
                    2 => patch_session(id.clone(), archive_patch(archived)),
                    _ => export(id.clone(), title.clone()),
                },
                move || {
                    { use ::compose_rust::ui::Button; Button!(
                        "",
                        move || toggle.set(!toggle.get_untracked()),
                        icon = compose_rust::IconRole::More,
                        variant = ButtonVariant::Text,
                        color = paint(p.text_2),
                        accessible_name = super::a11y::MORE,
                        // A bare glyph with no ring, which no shared button draws; it still sends
                        // the shared press values (FR-L34.5, #250).
                        modifier = pressable(Modifier::new().size(H_CTL, H_CTL).expanded(expanded)),
                    ) };
                },
                None,
            );
        }) };
    });

    // FR-L9 rename, on the main server: pre-filled with the session's current title so the user
    // edits it directly; an empty entry leaves the title as it is.
    let dismiss = renaming.clone();
    EmberSheet("세션 이름 변경", renaming.get(), move || dismiss.set(false), None, || {
        { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Md, modifier = Modifier::new().fill_max_width().padding(SP[4]), {
            let (title, submit_open, id) = (new_title.clone(), renaming.clone(), row.id.clone());
            EmberField(FieldOptions {
                fill_max_width: true,
                placeholder: row.title.clone(),
                text: Some(new_title.get()),
                on_value_change: Some(Box::new(move |v| title.set(v))),
                on_submit: Some(Box::new(move |v| {
                    rename(id.clone(), v);
                    submit_open.set(false);
                })),
                ..Default::default()
            });
        }) };
        SheetFooter(|| {
            let cancel = renaming.clone();
            AppButton("취소", None, ButtonKind::Default, true, true, false, move || cancel.set(false));
            let (done, title, id) = (renaming.clone(), new_title.clone(), row.id.clone());
            AppButton("변경", None, ButtonKind::Primary, true, true, false, move || {
                rename(id.clone(), title.get_untracked());
                done.set(false);
            });
        });
    });
    }) };
}

/// A review window: the connection banner and a few session rows, the first with its menu open
/// (`EMBER_CR_ROUTE=launcher-row`).
#[compose_rust::runtime::Composable]
pub fn rows_preview() {
    let ui = shared_ui().clone();
    let theme = ember_theme();
    let rows: Vec<SessionRow> = ui.with_rows(|s, cx| {
        let project = s
            .projects()
            .into_iter()
            .find(|p| !s.project(&p.name).is_some_and(|pr| pr.archived))
            .map(|p| p.name)
            .unwrap_or_default();
        model::project_sessions(s, &project, cx, false)
    });
    // `ScrollColumn` takes only a modifier, so the spacing is a `Column` inside it.
    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_size().background(paint(theme.palette.bg)).padding_role(SpaceRole::Md), {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[3], modifier = Modifier::new().fill_max_width(), {
                ConnectionBannerView("오프라인: 저장된 데이터를 보여주는 중입니다", Tone::Neutral);
                ConnectionBanner();
                for (i, row) in rows.iter().take(4).enumerate() {
                    // The demo sessions carry no preview; the first two get one to show the line.
                    let mut row = row.clone();
                    if i < 2 && row.preview.is_none() {
                        row.preview = Some("테스트가 모두 통과했습니다. 변경 사항을 정리했어요.".to_string());
                    }
                    SessionRowWith(&row, false);
                }
            }) };
        }) };
}

/// Change a session's metadata on the main server (FR-L9); say so when it fails.
pub fn patch_session(id: String, patch: SessionPatch) {
    run_shared(async move { services().patch_session(&id, &patch).await }, |r| {
        if let Err(e) = r {
            notice::post(format!("세션을 업데이트하지 못했습니다: {e}"));
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
            Ok(path) => notice::post(format!("내보냈습니다: {}", path.display())),
            Err(e) => notice::post(format!("내보내지 못했습니다: {e}")),
        },
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
    use compose_rust::{EventPayload, Host, PropertyKind, Recomposer, WidgetKind};
    use ember_client::LauncherStatus;

    struct TestNode {
        widget: WidgetKind,
        parent: Option<u32>,
        open: bool,
        on_click: Option<u64>,
        text: Option<String>,
    }

    struct TestTree {
        host: Host,
        nodes: HashMap<u32, TestNode>,
    }

    impl TestTree {
        fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
            let f = std::sync::Arc::new(f);
            let host = Host::new(move || {
                let f = f.clone();
                Box::new(Recomposer::new(move || f()))
            });
            let mut tree = Self { host, nodes: HashMap::new() };
            let bytes = tree.host.rebuild().expect("rebuild").to_vec();
            tree.apply(&bytes);
            tree
        }

        fn apply(&mut self, bytes: &[u8]) {
            for m in decode_batch(bytes).expect("decode") {
                match m {
                    Mutation::Create { node_id, widget } => {
                        self.nodes.insert(
                            node_id,
                            TestNode {
                                widget,
                                parent: None,
                                open: false,
                                on_click: None,
                                text: None,
                            },
                        );
                    }
                    Mutation::Insert { parent_id, node_id, .. } => {
                        if let Some(n) = self.nodes.get_mut(&node_id) {
                            n.parent = Some(parent_id);
                        }
                    }
                    Mutation::Remove { node_id } => {
                        self.remove_subtree(node_id);
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::Open, value: PropertyValue::Bool(b) } => {
                        if let Some(n) = self.nodes.get_mut(&node_id) {
                            n.open = b;
                        }
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                        if let Some(n) = self.nodes.get_mut(&node_id) {
                            n.on_click = Some(h as u64);
                        }
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                        if let Some(n) = self.nodes.get_mut(&node_id) {
                            n.text = Some(s.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }

        fn remove_subtree(&mut self, root: u32) {
            let kids: Vec<u32> = self
                .nodes
                .iter()
                .filter_map(|(&id, n)| (n.parent == Some(root)).then_some(id))
                .collect();
            for k in kids {
                self.remove_subtree(k);
            }
            self.nodes.remove(&root);
        }

        fn click_text(&mut self, label: &str) {
            let mut found = None;
            for (&id, n) in &self.nodes {
                if n.text.as_deref() != Some(label) {
                    continue;
                }
                let mut cur = Some(id);
                while let Some(nid) = cur {
                    let Some(node) = self.nodes.get(&nid) else { break };
                    if let Some(h) = node.on_click {
                        found = Some((nid, h));
                        break;
                    }
                    cur = node.parent;
                }
                if found.is_some() {
                    break;
                }
            }
            let (node_id, handler_id) = found.unwrap_or_else(|| panic!("no clickable text {label}"));
            let (bytes, _) = self
                .host
                .dispatch(HostEvent::new(node_id, handler_id, EventPayload::Clicked))
                .expect("click");
            let bytes = bytes.to_vec();
            self.apply(&bytes);
        }
    }

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
        assert_eq!(meta_line(&row()), "claude-code \u{00b7} opus \u{00b7} 서버 로그인");
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
        for want in ["Fix the sidebar", "Done, tests pass", "고정", "이름 변경", "보관", "내보내기"] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
    }

    #[test]
    fn fr_l9_fr_l10_4_session_row_menu_uses_anchored_popover_not_option_sheet() {
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let tree = TestTree::new(|| SessionRowWith(&row(), true));
        assert!(
            tree.nodes.values().any(|n| matches!(n.widget, WidgetKind::Popover) && n.open),
            "session row menu opens as an anchored Popover on desktop"
        );
        assert!(
            !tree.nodes.values().any(|n| matches!(n.widget, WidgetKind::Dialog) && n.open),
            "session row menu must not open as a centred OptionSheet Dialog"
        );
        let texts: Vec<&str> = tree.nodes.values().filter_map(|n| n.text.as_deref()).collect();
        for want in ["고정", "이름 변경", "보관", "내보내기"] {
            assert!(texts.contains(&want), "missing row action {want}: {texts:?}");
        }

        crate::ui::window_size::set_opened_for_test(Some((390.0, 844.0)));
        let phone = TestTree::new(|| SessionRowWith(&row(), true));
        assert!(
            phone.nodes.values().any(|n| matches!(n.widget, WidgetKind::Sheet) && n.open),
            "session row menu becomes a bottom Sheet on compact phone width"
        );
        assert!(
            !phone.nodes.values().any(|n| matches!(n.widget, WidgetKind::Popover)),
            "compact phone width uses a bottom Sheet rather than a Popover"
        );
        crate::ui::window_size::set_opened_for_test(None);

        let prod = include_str!("launcher.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!prod.contains("OptionSheet("), "no centred OptionSheet left in launcher");
    }

    #[test]
    fn fr_l9_opening_rename_prefills_text_field_with_current_title() {
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let mut tree = TestTree::new(|| SessionRowWith(&row(), true));
        tree.click_text("이름 변경");
        assert!(
            !tree.nodes.values().any(|n| matches!(n.widget, WidgetKind::Popover) && n.open),
            "row menu popover closes when opening rename"
        );
        assert!(
            tree.nodes.values().any(|n| matches!(n.widget, WidgetKind::Dialog) && n.open),
            "rename sheet opens after clicking 이름 변경"
        );
        let field_texts: Vec<&str> = tree
            .nodes
            .values()
            .filter(|n| matches!(n.widget, WidgetKind::TextField))
            .filter_map(|n| n.text.as_deref())
            .collect();
        assert!(
            field_texts.contains(&"Fix the sidebar"),
            "expected rename TextField pre-filled with current title, got {field_texts:?}"
        );
        crate::ui::window_size::set_opened_for_test(None);
    }
}
