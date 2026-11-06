//! compose-rust version of the 버전 관리 (VCS) screen (FR-L19, INTENT D34, `docs/DESIGN.md` section 6.6).
//!
//! Pure decisions (sections, tabs, ahead/behind text, diff parsing, line/hunk/file staging,
//! file tree, commit validation, merge gate) are re-exported from `crate::ui::vcs`.
//!
//! The repository view loads the selected project's server or demo data asynchronously.
//! When `std::env::var("EMBER_CR_ROUTE") == Ok("vcs")`, sample data from mockup v8 is used
//! so review captures show the full layout.

#![allow(non_snake_case)]

use std::collections::BTreeSet;

use compose_rust::runtime::{current_window_size, key, launched_effect, mutable_state_of, remember, remember_with_key, MutableState};
use crate::ui::vcs::{banner_sub, banner_title, fetch_merge_state, banner_button_text, merge_tap, merge_tap_key, run_merge_abort, run_merge_commit, MergeAction, MergeLoad, MergeView, MERGE_ABORT_NOTE, MERGE_COMMIT_NOTE, choice_label, fetch_conflict, run_resolve, ConflictChoice, ConflictStep, cancel_preview, confirm, ResolveTap, ConflictFile, ConflictLoad, ConflictSegment, fetch_stashes, run_stash_action, run_stash_push, stash_action_notice, stash_from, StashAction, StashEntry, StashLoad, fetch_history, history_append, history_from, fetch_prs, prs_from, PrEntry, PrFilter, PrLoad, section_count_with, section_count_with_branches, fetch_branches, branches_from, split_branches, branch_bar_fractions, worktree_tail, branch_create_body, stash_now_gate, stash_now_error, BranchEntry, BranchLoad, HistoryEntry, HistoryLoad, HistoryRef, Load, fetch_snapshot, load_from, CommitRequest, plan_staging, run_commit, run_remote, git_call, valid_branch_name, failure_text, CONFLICT_PREVIEW_LINES, amend_warning, amend_button_label, amend_tap, AmendTap, armed_label_visible, delete_tap, stash_button_label, stash_button_text, stash_tap, stash_tap_key, DeleteTap, STASH_DIRTY_WARNING, STASH_POP_NOTE, section_count_with_team, team_rows, MemberCwd, TeamRow, fetch_pr_detail, pr_detail_from, run_pr_review, run_pr_merge, run_pr_create, review_request_gate, pr_create_gate, review_tap, ReviewTap, pr_action_key, review_button_label, review_draft_key, is_stale_head, clean_body, merge_button_state, merge_button_text, MERGE_METHOD_TEXT, PrDetailLoad, PrDetailView, CheckClass, ReviewerState};
use ember_client::api::{GitPrMergeMethod, GitPrReviewAction};
use serde_json::{json, Value};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{
    Alignment, ButtonVariant, Color, IconRole, Paint, ShapeRole, TextOverflow, TypeRole,
    WindowSizeClass,
};

pub use crate::ui::vcs::{
    ahead_behind_text, bar_fractions, build_tree, check_glyph, check_of, commit_enabled,
    commit_label, effective_tab, fetched_text, folder_check, merge_gate, pane_layout, pane_tabs,
    parse_hunk_header, parse_unified, pick_project, remote_action, repo_projects, route_section,
    section_count, section_count_with_stash, section_nav, soon_line, tab_after_select, upstream_text, Check,
    CiState, DiffLine, FileDiff, FileStaging, FileStatus, Hunk, LineKind, PaneLayout, PaneTab,
    RemoteAction, RepoSnapshot, ReviewState, Section, SectionNav, Staging, TreeRow, SOON,
};
use crate::ui::coexist::SharedUi;
use crate::ui::project::{display_name, project_boxes, ProjectBox};
use crate::ui::shell::NavMenu;
use crate::ui::Route;

use super::components::{
    AppButton, ButtonKind, EmberCard, EmberField, FieldOptions, InsetPanel, Label, MenuRow, OptionSheet,
    PickChip, Pill, PillTone, Txt,
};
use super::notice;
use super::shell::NavIcon;
use super::theme::*;

/// Open 버전 관리 for a project in compose-rust shell.
pub fn open_for_project_cr(ui: &SharedUi, project: &str) {
    ui.set_selected_project(Some(project.to_string()));
    ui.go(Route::Projects);
    ui.set_menu(NavMenu::VersionControl);
}

#[derive(Clone)]
struct Actions {
    project: String,
    reload: MutableState<u64>,
}

impl Actions {
    fn finish(&self, what: &'static str, task: impl std::future::Future<Output = Result<String, ember_client::api::ApiError>> + Send + 'static) {
        self.finish_with(what, task, || {});
    }

    fn finish_with(&self, what: &'static str, task: impl std::future::Future<Output = Result<String, ember_client::api::ApiError>> + Send + 'static, mut on_success: impl FnMut() + Send + 'static) {
        let reload = self.reload.clone();
        crate::services::run_shared(task, move |result| {
            notice::post(match result {
                Ok(text) => {
                    on_success();
                    reload.set(reload.get() + 1);
                    text
                }
                Err(e) => failure_text(what, &e),
            });
        });
    }

    fn remote(&self, action: RemoteAction) {
        let project = self.project.clone();
        self.finish("원격 작업을 ", async move { run_remote(&project, action).await });
    }

    fn branch(&self, name: String, create: bool, on_success: impl FnMut() + Send + 'static) {
        let project = self.project.clone();
        self.finish_with(if create { "브랜치 만들기를 " } else { "브랜치 전환을 " }, async move {
            let (op, body) = if create { ("branch", json!({"name": name})) } else { ("checkout", json!({"branch": name})) };
            git_call::<Value>(&project, op, body).await?;
            Ok(if create { "새 브랜치를 만들었습니다" } else { "브랜치를 전환했습니다" }.to_string())
        }, on_success);
    }

    /// Create `name` from `base` (the current branch when `None`) and switch to it: the node's
    /// `branch` op runs `git switch -c`.
    fn create_from(&self, name: String, base: Option<String>, on_success: impl FnMut() + Send + 'static) {
        let project = self.project.clone();
        self.finish_with("브랜치 만들기를 ", async move {
            git_call::<Value>(&project, "branch", branch_create_body(&name, base.as_deref())).await?;
            Ok("새 브랜치를 만들어 전환했습니다".to_string())
        }, on_success);
    }

    /// The 스태시 button of 변경 사항 (#485): stash the work tree now, with no message and the
    /// untracked files left alone. A clean tree says so and does not call the API.
    fn stash_now(&self, changes_len: usize) {
        if let Err(line) = stash_now_gate(changes_len) {
            notice::post(line.to_string());
            return;
        }
        let (project, reload) = (self.project.clone(), self.reload.clone());
        crate::services::run_shared(async move { run_stash_push(&project, "", false).await }, move |result| {
            match result {
                Ok(()) => {
                    notice::post("스태시를 만들었습니다".to_string());
                    reload.set(reload.get() + 1);
                }
                Err(line) => notice::post(stash_now_error(&line)),
            }
        });
    }
}

fn text_after_success(current: &str, submitted: &str) -> String {
    if current == submitted { String::new() } else { current.to_string() }
}

fn screen_subtitle(project: &ProjectBox, snap: Option<&RepoSnapshot>, sample: bool) -> String {
    if sample {
        if let Some(s) = snap {
            return format!("darkpyonix-ember · {} · 변경 {} · PR {}", s.branch, s.changes.len(), sample_pr_count());
        }
    }
    match snap {
        Some(s) => format!("{} · {} · {} · 변경 {}", project.name, s.branch, ahead_behind_text(s.ahead, s.behind), s.changes.len()),
        None => project.name.clone(),
    }
}

fn branch_input(name: &str) -> Option<String> {
    let name = name.trim();
    valid_branch_name(name).then(|| name.to_string())
}

fn commit_request(snap: &RepoSnapshot, staging: &Staging, summary: &str, description: &str, amend: bool) -> CommitRequest {
    CommitRequest { ops: plan_staging(snap, staging), summary: summary.to_string(), description: description.to_string(), amend }
}

// ---------------------------------------------------------------------------------------------
// Sample data for review captures (mockup v8)
// ---------------------------------------------------------------------------------------------

const SAMPLE_DIFF: &str = "\
diff --git a/ember/proxy/src/shim.rs b/ember/proxy/src/shim.rs
index 111..222 100644
--- a/ember/proxy/src/shim.rs
+++ b/ember/proxy/src/shim.rs
@@ -41,5 +41,8 @@ impl SessionShim {
     pub fn attach(&mut self, session: &Session) -> Result<()> {
         let id = session.id().clone();
-        self.router.insert(id, session.handle());
+        // Clone before the IDE router takes ownership (#128).
+        let handle = session.handle().clone();
+        self.router.insert(id.clone(), handle);
+        self.egress.watch(&id)?;
         Ok(())
     }
@@ -88,4 +92,7 @@ impl SessionShim {
     pub fn detach(&mut self, id: &SessionId) {
-        self.router.remove(id);
-        // TODO: stop egress
+        if let Some(h) = self.router.remove(id) {
+            h.close();
         }
         self.egress.stop(id);
         tracing::debug!(%id, \"shim detached\");
     }
diff --git a/ember/proxy/src/router.rs b/ember/proxy/src/router.rs
index 333..444 100644
--- a/ember/proxy/src/router.rs
+++ b/ember/proxy/src/router.rs
@@ -12,3 +12,4 @@ use crate::shim::SessionShim;
 pub struct IdeRouter {
-    routes: HashMap<SessionId, Handle>,
+    routes: HashMap<SessionId, Handle>,
+    attached: HashSet<SessionId>,
 }
diff --git a/ember/proxy/tests/shim_attach.rs b/ember/proxy/tests/shim_attach.rs
new file mode 100644
--- /dev/null
+++ b/ember/proxy/tests/shim_attach.rs
@@ -0,0 +1,9 @@
+use ember_proxy::shim::SessionShim;
+
+#[tokio::test]
+async fn attach_clones_handle_before_router() {
+    let mut shim = SessionShim::for_test();
+    let s = shim.spawn_session().await;
+    shim.attach(&s).unwrap();
+    assert!(s.handle().is_alive());
+}
diff --git a/docs/SPEC.md b/docs/SPEC.md
index 555..666 100644
--- a/docs/SPEC.md
+++ b/docs/SPEC.md
@@ -204,2 +204,3 @@ ## Proxy
 - PX-12 The proxy serves the VS Code Web workbench.
-- PX-13 Sessions attach through the shim.
+- PX-13 Sessions attach through the shim.
+- PX-14 Detach stops egress for that session.
diff --git a/ember/proxy/src/legacy_bridge.rs b/ember/proxy/src/legacy_bridge.rs
deleted file mode 100644
--- a/ember/proxy/src/legacy_bridge.rs
+++ /dev/null
@@ -1,4 +0,0 @@
-//! Bridge kept for the pre-shim proxy. Removed with PX-13.
-use crate::router::IdeRouter;
-
-pub fn bridge(r: &mut IdeRouter) { /* ... */ }
";

/// Sample snapshot from mockup v8 (project ember).
pub fn sample_snapshot() -> RepoSnapshot {
    RepoSnapshot {
        branch: "feat/session-shim".to_string(),
        upstream: Some("origin/feat/session-shim".to_string()),
        ahead: 2,
        behind: 1,
        fetched_age_secs: Some(180),
        branches: vec![
            "feat/session-shim".into(),
            "develop".into(),
            "main".into(),
            "feat/vendor-workbench-css".into(),
            "fix/session-clone-race".into(),
            "feat/egress-stop".into(),
            "feat/ose-windows".into(),
            "feat/channel-unread-push".into(),
            "feat/routine-scheduler".into(),
        ],
        changes: parse_unified(SAMPLE_DIFF),
        staged: Default::default(),
        conflicts: Vec::new(),
        notes: Vec::new(),
    }
}

/// Sample project box from mockup v8.
pub fn sample_project_box() -> ProjectBox {
    ProjectBox {
        id: "ember".into(),
        name: "Ember".into(),
        members: Vec::new(),
        running: 3,
        waiting: 1,
        computers: vec!["mac-mini".into()],
        top: None,
        repo: Some("github.com/DarkPyonix/darkpyonix-ember".into()),
        branch: Some("feat/session-shim".into()),
    }
}

/// Sample projects list from mockup v8.
pub fn sample_projects() -> Vec<ProjectBox> {
    vec![
        sample_project_box(),
        ProjectBox {
            id: "core".into(),
            name: "Core".into(),
            members: Vec::new(),
            running: 1,
            waiting: 0,
            computers: vec!["mac-mini".into()],
            top: None,
            repo: Some("github.com/DarkPyonix/darkpyonix-core".into()),
            branch: Some("feat/hub-api".into()),
        },
    ]
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

fn SmallNote(text: &str) {
    let p = ember_theme().palette;
    Label(text, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
}

// TODO(compose-rust#425, #429): an invisible `Button(label)` over a painted row, only to get focus,
// role and the accessible name. Remove when Button takes content or a semantics modifier exists.
/// An accessible invisible button overlay for a painted row or pill.
/// Gives assistive tech the item's label and provides keyboard focus and click handling.
fn FocusOverlay(label: &str, on_click: impl FnMut() + 'static) {
    cu::Button(label)
        .variant(ButtonVariant::Text)
        .color(Paint::Literal(Color::argb(0)))
        .modifier(Modifier::new().fill_max_size())
        .on_click(on_click);
}

/// Card shown when a section's server data is not yet available.
pub fn Unavailable(section: Section) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, Some(SP[3]), None, || {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                Label(section.label(), Fs::Lg, Weight::Semibold, None, None);
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                Pill(SOON, PillTone::Grey, true);
            });
            cu::Spacer().modifier(Modifier::new().height(SP[0]));
            // TODO(server, #168): this section needs the repository endpoints listed there.
            Label(&soon_line(section.label()), Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
        });
    });
}

const ICON_GIT_SVG: &str = include_str!("../../../assets/nav/git.svg");
const ICON_USERS_SVG: &str = include_str!("../../../assets/nav/users.svg");
const ICON_FOLDER_SVG: &str = include_str!("../../../assets/nav/folder.svg");
const ICON_CHEVRON_DOWN_SVG: &str = include_str!("../../../assets/nav/chevron-down.svg");
const ICON_CHEVRON_RIGHT_SVG: &str = include_str!("../../../assets/nav/chevron-right.svg");

const ICON_FILE_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8l-5-5Zm0 0v5h5"/></svg>"#;
const ICON_CLOCK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path stroke-linecap="round" stroke-linejoin="round" d="M12 6v6h4.5m4.5 0a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z"/></svg>"#;
const ICON_PR_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="6" r="2.25"/><circle cx="6" cy="18" r="2.25"/><circle cx="18" cy="18" r="2.25"/><path d="M6 8.25v7.5M18 15.75V9a3 3 0 0 0-3-3h-4.5M12.5 3.5 10 6l2.5 2.5"/></svg>"#;
const ICON_STASH_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3.75 4.5h16.5v3H3.75zM5.25 7.5v11.25c0 .41.34.75.75.75h12c.41 0 .75-.34.75-.75V7.5M9.75 11.25h4.5"/></svg>"#;
const ICON_WARN_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 9.5v4M12 16.75h.01M10.3 3.9 2.4 17.6a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z"/></svg>"#;
const ICON_FETCH_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4.5 12a7.5 7.5 0 0 1 13-5.1M19.5 12a7.5 7.5 0 0 1-13 5.1M17.5 3.5v3.4h-3.4M6.5 20.5v-3.4h3.4"/></svg>"#;
const ICON_ARROW_DOWN_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 5v14M18.5 12.5 12 19l-6.5-6.5"/></svg>"#;
const ICON_ARROW_UP_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5M5.5 11.5 12 5l6.5 6.5"/></svg>"#;

fn section_svg(sec: Section) -> &'static str {
    match sec {
        Section::Changes => ICON_FILE_SVG,
        Section::History => ICON_CLOCK_SVG,
        Section::Branches => ICON_GIT_SVG,
        Section::PullRequests => ICON_PR_SVG,
        Section::Stash => ICON_STASH_SVG,
        Section::Conflicts => ICON_WARN_SVG,
        Section::Team => ICON_USERS_SVG,
    }
}

/// Minimum hit and touch target of an icon-only control, in dp (FR-L19, #293).
pub const ICON_TOUCH_TARGET: f32 = 44.0;

/// The labels of the remote icon buttons and the stash button, in the order shown.
pub fn remote_button_labels() -> [&'static str; 4] {
    ["가져오기", "내려받기", "올리기", "스태시"]
}

/// A 32 px circle icon button with a 44 dp hit target, named for assistive tech via `FocusOverlay`.
fn CircleIconButton(
    icon: &'static str,
    label: &str,
    on_click: impl FnMut() + 'static,
) {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(Modifier::new().size(ICON_TOUCH_TARGET, ICON_TOUCH_TARGET))
        .alignment(Alignment::Center)
        .content(move || {
            cu::Box()
                .modifier(
                    Modifier::new()
                        .size(32.0, 32.0)
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .shape_role(ShapeRole::Full),
                )
                .alignment(Alignment::Center)
                .content(move || {
                    NavIcon(icon, p.text_2.to_argb(), 16.0);
                });
            FocusOverlay(label, on_click);
        });
}

/// Selector row for repository in the bar.
fn RepoSelector(name: &str, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(Modifier::new().fill_max_width().height(ICON_TOUCH_TARGET))
        .content(move || {
            cu::Row()
                .alignment(Alignment::CenterStart)
                .modifier(Modifier::new().fill_max_size())
                .content(|| {
                    NavIcon(ICON_FOLDER_SVG, p.text.to_argb(), 18.0);
                    cu::Spacer().modifier(Modifier::new().width(6.0));
                    Label(name, Fs::Md, Weight::Semibold, Some(paint(p.text)), Some(1));
                    cu::Spacer().modifier(Modifier::new().width(6.0));
                    NavIcon(ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 16.0);
                });
            FocusOverlay(name, on_click);
        });
}

/// Selector row for current branch in the bar.
fn BranchSelector(name: &str, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    cu::Box()
        .modifier(Modifier::new().fill_max_width().height(36.0))
        .content(move || {
            cu::Row()
                .alignment(Alignment::CenterStart)
                .modifier(Modifier::new().fill_max_size())
                .content(|| {
                    NavIcon(ICON_GIT_SVG, p.text.to_argb(), 18.0);
                    cu::Spacer().modifier(Modifier::new().width(6.0));
                    Label(name, Fs::Md, Weight::Semibold, Some(paint(p.text)), Some(1));
                    cu::Spacer().modifier(Modifier::new().width(6.0));
                    NavIcon(ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 16.0);
                });
            FocusOverlay(name, on_click);
        });
}

/// Main remote action button (Pull with down arrow and count pill, Push, Fetch).
fn RemoteMainButton(
    action: RemoteAction,
    enabled: bool,
    on_click: impl FnMut() + 'static,
) {
    // TODO(compose-rust#328): keep button labels visible when the available width is narrow.
    let p = ember_theme().palette;
    let (action_icon, action_text, action_count) = match action {
        RemoteAction::Pull(n) => (ICON_ARROW_DOWN_SVG, "Pull", Some(n)),
        RemoteAction::Push(n) => (ICON_ARROW_UP_SVG, "Push", Some(n)),
        RemoteAction::Fetch => (ICON_FETCH_SVG, "Fetch", None),
    };
    let btn_bg = if enabled { p.ink } else { p.surface_2 };
    let btn_fg = if enabled { p.ink_text } else { p.muted };
    let pill_bg = if enabled { p.ink_text } else { p.muted };
    let pill_fg = if enabled { p.ink } else { p.surface_2 };
    let label = action.label();

    cu::Box()
        .modifier(
            Modifier::new()
                .size(100.0, 32.0)
                .background(paint(btn_bg))
                .shape_role(ShapeRole::Full),
        )
        .alignment(Alignment::Center)
        .content(move || {
            cu::Row()
                .alignment(Alignment::Center)
                .spacing(6.0)
                .modifier(Modifier::new().fill_max_size())
                .content(|| {
                    cu::Spacer().modifier(Modifier::new().width(SP[0]));
                    NavIcon(action_icon, btn_fg.to_argb(), 16.0);
                    Label(action_text, Fs::Sm, Weight::Medium, Some(paint(btn_fg)), Some(1));
                    if let Some(n) = action_count {
                        cu::Box()
                            .modifier(
                                Modifier::new()
                                    .size(20.0, 20.0)
                                    .background(paint(pill_bg))
                                    .shape_role(ShapeRole::Full),
                            )
                            .alignment(Alignment::Center)
                            .content(|| {
                                Label(
                                    &n.to_string(),
                                    Fs::Xs,
                                    Weight::Semibold,
                                    Some(paint(pill_fg)),
                                    Some(1),
                                );
                            });
                    }
                    cu::Spacer().modifier(Modifier::new().width(SP[0]));
                });
            FocusOverlay(&label, on_click);
        });
}

/// Ahead/behind counters with arrows and relative fetch age.
fn RemoteSubNote(snap: Option<&RepoSnapshot>) {
    let p = ember_theme().palette;
    match snap {
        Some(s) => {
            cu::Row()
                .alignment(Alignment::CenterStart)
                .spacing(4.0)
                .content(|| {
                    let up_fg = if s.ahead > 0 { p.text } else { p.muted };
                    NavIcon(ICON_ARROW_UP_SVG, up_fg.to_argb(), 12.0);
                    Label(&s.ahead.to_string(), Fs::Xs, Weight::Regular, Some(paint(up_fg)), Some(1));
                    cu::Spacer().modifier(Modifier::new().width(4.0));
                    let down_fg = if s.behind > 0 { p.text } else { p.muted };
                    NavIcon(ICON_ARROW_DOWN_SVG, down_fg.to_argb(), 12.0);
                    Label(&s.behind.to_string(), Fs::Xs, Weight::Regular, Some(paint(down_fg)), Some(1));
                    cu::Spacer().modifier(Modifier::new().width(4.0));
                    Label(
                        &format!("\u{00b7} {}", fetched_text(s.fetched_age_secs)),
                        Fs::Xs,
                        Weight::Regular,
                        Some(paint(p.text_2)),
                        Some(1),
                    );
                });
        }
        None => {
            SmallNote(SOON);
        }
    }
}

// EmberCard's inner row does not expand its content to the card width.
fn VcsCard(content: impl FnOnce()) {
    let p = ember_theme().palette;
    cu::Box().modifier(Modifier::new().fill_max_width().background(paint(p.surface))
        .border(1.0, paint(p.line)).corner_radius(CORNER_LG)).content(|| {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(content);
    });
}

/// The bar of three: repository, current branch, remote origin in ONE split card.
pub fn RepoBar(
    projects: &[(String, String)],
    project: &ProjectBox,
    snap: Option<&RepoSnapshot>,
    compact: bool,
    mut on_pick: impl FnMut(String) + Clone + 'static,
) {
    RepoBarWithActions(projects, project, snap, compact, on_pick, None);
}

fn RepoBarWithActions(
    projects: &[(String, String)], project: &ProjectBox, snap: Option<&RepoSnapshot>, compact: bool,
    mut on_pick: impl FnMut(String) + Clone + 'static, actions: Option<Actions>,
) {
    let project_menu = remember(|| mutable_state_of(false));
    let branch_menu = remember(|| mutable_state_of(false));
    let new_branch = remember(|| mutable_state_of(String::new()));
    let branch_generation = remember(|| mutable_state_of(0_u64));

    let has = snap.is_some();
    let (ahead, behind) = snap.map_or((0, 0), |s| (s.ahead, s.behind));
    let action = remote_action(ahead, behind);
    let branch_text = match (snap, &project.branch) {
        (Some(s), _) => s.branch.clone(),
        (None, Some(b)) => b.clone(),
        _ => "브랜치".to_string(),
    };
    let upstream = snap
        .map(|s| upstream_text(s.upstream.as_deref()))
        .unwrap_or_else(|| SOON.to_string());
    let branches = snap.map(|s| s.branches.clone()).unwrap_or_default();
    let current_branch = snap.map(|s| s.branch.clone());

    let proj_id = project.id.clone();
    let proj_name = display_name(&project.id);
    let repo_line = crate::ui::vcs::repo_card_line(project, snap);
    let project_opts: Vec<String> = projects.iter().map(|(_, name)| name.clone()).collect();
    let chosen_proj = projects.iter().position(|(id, _)| id == &proj_id);

    let branch_opts: Vec<String> = if branches.is_empty() {
        vec![soon_line("브랜치 목록")]
    } else {
        branches.clone()
    };
    let chosen_br = current_branch.as_ref().and_then(|cb| branches.iter().position(|b| b == cb));

    let p_dismiss = project_menu.clone();
    let p_projects = projects.to_vec();
    let mut on_pick_proj = on_pick.clone();
    let b_dismiss = branch_menu.clone();

    cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
        let (project_menu_c, branch_menu_c) = (project_menu.clone(), branch_menu.clone());
        let p = ember_theme().palette;
        let remote_actions = actions.clone();
        VcsCard(move || {
            if compact {
                cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
                    let p_open = project_menu_c.clone();
                    let p_name = proj_name.clone();
                    let r_line = repo_line.clone();
                    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[2])).spacing(SP[1]).content(move || {
                        SmallNote("저장소");
                        RepoSelector(&p_name, move || p_open.set(true));
                        SmallNote(&r_line);
                    });

                    cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));

                    let b_open = branch_menu_c.clone();
                    let b_text = branch_text.clone();
                    let u_text = upstream.clone();
                    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[2])).spacing(SP[1]).content(move || {
                        SmallNote("현재 브랜치");
                        BranchSelector(&b_text, move || b_open.set(true));
                        SmallNote(&u_text);
                    });

                    cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));

                    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[2])).spacing(SP[1]).content(move || {
                        SmallNote("원격 origin");
                        cu::Row().modifier(Modifier::new().fill_max_width()).alignment(Alignment::CenterStart).spacing(10.0).content(move || {
                            RemoteControls(action, has, remote_actions.clone());
                        });
                        RemoteSubNote(snap);
                    });
                });
            } else {
                // No fixed height (#407): the row sizes to its tallest column and `equal_height` stretches the
                // others, so the URL line and the ahead/behind line are never cut.
                cu::Row().modifier(Modifier::new().fill_max_width()).alignment(Alignment::TopStart).equal_height(true).content(move || {
                    let p_open = project_menu_c.clone();
                    let p_name = proj_name.clone();
                    let r_line = repo_line.clone();
                    cu::Column().modifier(Modifier::new().weight(1.1).fill_max_height().padding(SP[3])).spacing(SP[1]).content(move || {
                        SmallNote("저장소");
                        RepoSelector(&p_name, move || p_open.set(true));
                        SmallNote(&r_line);
                    });

                    cu::Box().modifier(Modifier::new().width(1.0).fill_max_height().background(paint(p.line)));

                    let b_open = branch_menu_c.clone();
                    let b_text = branch_text.clone();
                    let u_text = upstream.clone();
                    cu::Column().modifier(Modifier::new().weight(1.1).fill_max_height().padding(SP[3])).spacing(SP[1]).content(move || {
                        SmallNote("현재 브랜치");
                        BranchSelector(&b_text, move || b_open.set(true));
                        SmallNote(&u_text);
                    });

                    cu::Box().modifier(Modifier::new().width(1.0).fill_max_height().background(paint(p.line)));

                    cu::Column().modifier(Modifier::new().weight(1.0).fill_max_height().padding(SP[3])).spacing(SP[1]).content(move || {
                        SmallNote("원격 origin");
                        cu::Row().modifier(Modifier::new().fill_max_width()).alignment(Alignment::CenterStart).spacing(10.0).content(move || {
                            RemoteControls(action, has, remote_actions.clone());
                        });
                        RemoteSubNote(snap);
                    });
                });
            }
        });

        // OptionSheets for project and branch dialogs
        // TODO(compose-rust#383): anchored popup placed below or beside anchor
        OptionSheet(
            project_menu.get(),
            &project_opts,
            chosen_proj,
            240.0,
            move || p_dismiss.set(false),
            move |idx| {
                if let Some((id, _)) = p_projects.get(idx) {
                    on_pick_proj(id.clone());
                }
            },
        );

        cu::Dialog(branch_menu.get())
            .modifier(Modifier::new().width(248.0).background(paint(p.surface))
                .border(1.0, paint(p.line)).corner_radius(CORNER_MD).padding(SP[0]))
            .on_dismiss(move || b_dismiss.set(false))
            .content(|| {
                cu::Column().modifier(Modifier::new().fill_max_width()).spacing(SP[1]).content(|| {
                    for (idx, name) in branch_opts.iter().enumerate() {
                        let dismiss = branch_menu.clone();
                        let select_actions = actions.clone();
                        let selected_name = branches.get(idx).cloned();
                        MenuRow(name, 240.0, chosen_br == Some(idx), move || {
                            dismiss.set(false);
                            if let (Some(actions), Some(name)) = (select_actions.as_ref(), selected_name.as_ref()) {
                                actions.branch(name.clone(), false, || {});
                            }
                        });
                    }
                    let branch_value = new_branch.clone();
                    key(branch_generation.get(), || {
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            text: Some(new_branch.get()),
                            placeholder: "새 브랜치 이름".to_string(),
                            on_value_change: Some(Box::new(move |v| branch_value.set(v))),
                            ..Default::default()
                        });
                    });
                    let create_actions = actions.clone();
                    let create_value = new_branch.clone();
                    let generation = branch_generation.clone();
                    AppButton("새 브랜치", None, ButtonKind::Primary, false, has && branch_input(&new_branch.get()).is_some(), true, move || {
                        if let (Some(actions), Some(name)) = (create_actions.as_ref(), branch_input(&create_value.get())) {
                            let submitted = create_value.get();
                            let value = create_value.clone();
                            let generation = generation.clone();
                            actions.branch(name, true, move || {
                                value.set(text_after_success(&value.get(), &submitted));
                                generation.set(generation.get() + 1);
                            });
                        }
                    });
                });
            });
    });
}

fn RemoteControls(action: RemoteAction, enabled: bool, actions: Option<Actions>) {
    let main = actions.clone();
    RemoteMainButton(action, enabled, move || { if let Some(a) = &main { a.remote(action); } });
    cu::Row().spacing(4.0).content(move || {
        let labels = remote_button_labels();
        for (icon, label, action) in [(ICON_FETCH_SVG, labels[0], RemoteAction::Fetch), (ICON_ARROW_DOWN_SVG, labels[1], RemoteAction::Pull(0)), (ICON_ARROW_UP_SVG, labels[2], RemoteAction::Push(0))] {
            let a = actions.clone();
            CircleIconButton(icon, label, move || { if let Some(a) = &a { a.remote(action); } });
        }
    });
}

/// Section list on the left (wide windows) with counts.
pub fn SectionList(
    active: Section,
    snap: Option<&RepoSnapshot>,
    mut on_pick: impl FnMut(Section) + Clone + 'static,
) {
    SectionListWithSample(active, snap, false, None, None, None, None, on_pick);
}

fn SectionListWithSample(
    active: Section, snap: Option<&RepoSnapshot>, is_sample: bool, stash_count: Option<usize>, pr_count: Option<usize>, branch_count: Option<usize>, team_count: Option<usize>,
    mut on_pick: impl FnMut(Section) + Clone + 'static,
) {
    let p = ember_theme().palette;
    cu::Column().modifier(Modifier::new().width(200.0)).spacing(2.0).content(|| {
        for sec in Section::ALL {
            let is_active = sec == active;
            let bg = if is_active { paint(p.surface) } else { Paint::Literal(Color::argb(0)) };
            let fg = if is_active { p.text } else { p.muted };
            let count = section_count_with_team(sec, snap, stash_count, pr_count, branch_count, team_count).or_else(|| {
                if is_sample {
                    match sec {
                        Section::PullRequests => Some(sample_pr_count()),
                        Section::Stash => Some(2),
                        Section::Conflicts => Some(2),
                        Section::Team => Some(sample_team_len()),
                        _ => None,
                    }
                } else {
                    None
                }
            });
            let mut pick = on_pick.clone();

            let mut m = Modifier::new()
                .fill_max_width()
                .height(40.0)
                .background(bg)
                .corner_radius(CORNER_SM);
            if is_active {
                m = with_shadows(m.border(1.0, paint(p.line)), ember_theme().shadow_sm());
            }

            cu::Box()
                .modifier(m)
                .content(move || {
                    cu::Row()
                        .alignment(Alignment::CenterStart)
                        .modifier(Modifier::new().fill_max_size())
                        .content(move || {
                            cu::Spacer().modifier(Modifier::new().width(SP[2]));
                            NavIcon(section_svg(sec), fg.to_argb(), 18.0);
                            cu::Spacer().modifier(Modifier::new().width(SP[1]));
                            Label(sec.label(), Fs::Sm, Weight::Medium, Some(paint(fg)), Some(1));
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            if let Some(n) = count {
                                if sec == Section::Conflicts && n > 0 {
                                    cu::Box()
                                        .modifier(
                                            Modifier::new()
                                                .height(18.0)
                                                .background(paint(p.orange.strong))
                                                .shape_role(ShapeRole::Full)
                                                .padding(2.0),
                                        )
                                        .alignment(Alignment::Center)
                                        .content(|| {
                                            Label(
                                                &format!(" {n} "),
                                                Fs::Xs,
                                                Weight::Semibold,
                                                Some(Paint::Literal(Color::argb(0xffff_ffff))),
                                                Some(1),
                                            );
                                        });
                                } else {
                                    Label(
                                        &n.to_string(),
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(if is_active { p.text } else { p.muted })),
                                        Some(1),
                                    );
                                }
                            }
                            cu::Spacer().modifier(Modifier::new().width(SP[2]));
                        });
                    FocusOverlay(sec.label(), move || pick(sec));
                });
        }
    });
}

/// Section pills above the panes (medium and compact windows).
pub fn SectionPills(
    active: Section,
    mut on_pick: impl FnMut(Section) + Clone + 'static,
) {
    cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
        cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
            for sec in Section::ALL {
                let mut pick = on_pick.clone();
                PickChip(sec.label(), None, sec == active, false, move || pick(sec));
            }
        });
    });
}

/// Tabs within a section on compact windows.
pub fn PaneTabs(
    section: Section,
    active: Option<PaneTab>,
    mut on_pick: impl FnMut(PaneTab) + Clone + 'static,
) {
    cu::Row().modifier(Modifier::new().fill_max_width()).spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
        for t in pane_tabs(section).iter().copied() {
            let mut pick = on_pick.clone();
            PickChip(t.label(), None, Some(t) == active, false, move || pick(t));
        }
    });
}

/// One file diff with a check per changed line and per hunk.
pub fn DiffView(file: &FileDiff, staging: &MutableState<Staging>) {
    let p = ember_theme().palette;
    let (on, total) = staging
        .get()
        .file(&file.path)
        .map_or((0, file.added() + file.removed()), |s| s.counts(file));

    let (tone_fill, tone_text) = match file.status {
        FileStatus::Modified => (p.orange.soft, p.orange.strong),
        FileStatus::Added => (p.green.soft, p.green.strong),
        FileStatus::Deleted => (p.red.soft, p.red.strong),
        FileStatus::Renamed => (p.blue.soft, p.blue.strong),
    };

    cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
        // File header row (52 px)
        cu::Row().modifier(Modifier::new().fill_max_width().height(52.0).padding(SP[2])).alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
            cu::Box().modifier(Modifier::new().size(20.0, 20.0).background(paint(tone_fill)).corner_radius(CORNER_XS)).alignment(Alignment::Center).content(|| {
                Label(file.status.letter(), Fs::Xs2, Weight::Semibold, Some(paint(tone_text)), Some(1));
            });
            cu::Spacer().modifier(Modifier::new().width(SP[0]));
            Txt(&file.path, TypeRole::Mono, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            Label(&format!("+{}", file.added()), Fs::Xs, Weight::Medium, Some(paint(p.green.strong)), Some(1));
            Label(&format!("-{}", file.removed()), Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(1));
            Pill(&format!("{on}/{total}줄 스테이징"), PillTone::Grey, true);
        });

        cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));

        if file.binary {
            cu::Box().modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
                Label("이진 파일은 차이를 보여 주지 않습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            });
        }

        for (hi, h) in file.hunks.iter().enumerate() {
            // Keep the action below the header so long hunk labels cannot overlap it.
            cu::Column().modifier(Modifier::new().fill_max_width().background(paint(p.surface_2)).padding(SP[2])).spacing(SP[1]).content(|| {
                Label(&h.header, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                let st_hunk = staging.clone();
                let f_hunk = file.clone();
                cu::Box()
                    .modifier(
                        Modifier::new()
                            .width(110.0)
                            .height(32.0)
                            .border(2.0, paint(p.ink))
                            .background(paint(p.surface))
                            .shape_role(ShapeRole::Full),
                    )
                    .alignment(Alignment::Center)
                    .content(move || {
                        Label("이 덩어리 전체", Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                        FocusOverlay("이 덩어리 전체", move || {
                            let mut s = st_hunk.get();
                            s.file_mut(&f_hunk.path).toggle_hunk(&f_hunk, hi);
                            st_hunk.set(s);
                        });
                    });
            });

            cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));

            // Hunk lines (each exactly 21.0 px high, spacing 0.0)
            cu::Column().modifier(Modifier::new().fill_max_width()).spacing(0.0).content(|| {
                for (li, l) in h.lines.iter().enumerate() {
                    let bg = match l.kind {
                        LineKind::Add => paint(p.green.soft),
                        LineKind::Del => paint(p.red.soft),
                        LineKind::Context => Paint::Literal(Color::argb(0)),
                    };
                    cu::Row().modifier(Modifier::new().fill_max_width().height(21.0).background(bg)).alignment(Alignment::CenterStart).content(|| {
                        if l.is_change() {
                            let is_line_staged = staging.get().file(&file.path).is_some_and(|s| s.is_staged(hi, li));
                            let st_line = staging.clone();
                            let f_line = file.clone();
                            let cb_alpha = if is_line_staged { 1.0 } else { 0.45 };
                            cu::Box().modifier(Modifier::new().width(24.0).height(21.0).alpha(cb_alpha)).alignment(Alignment::Center).content(|| {
                                cu::Checkbox(is_line_staged)
                                    .modifier(Modifier::new().size(14.0, 14.0))
                                    .on_change(move |_| {
                                        let mut s = st_line.get();
                                        s.file_mut(&f_line.path).toggle_line(&f_line, hi, li);
                                        st_line.set(s);
                                    });
                            });
                        } else {
                            cu::Spacer().modifier(Modifier::new().width(24.0));
                        }

                        let old_str = l.old_no.map(|n| format!("{:>4}", n)).unwrap_or_else(|| "    ".to_string());
                        let new_str = l.new_no.map(|n| format!("{:>4}", n)).unwrap_or_else(|| "    ".to_string());
                        let sign_str = match l.kind {
                            LineKind::Add => "+",
                            LineKind::Del => "-",
                            LineKind::Context => " ",
                        };

                        cu::Box().modifier(Modifier::new().width(32.0)).alignment(Alignment::CenterEnd).content(|| {
                            Label(&old_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                        cu::Box().modifier(Modifier::new().width(32.0)).alignment(Alignment::CenterEnd).content(|| {
                            Label(&new_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                        cu::Box().modifier(Modifier::new().width(14.0)).alignment(Alignment::Center).content(|| {
                            Label(sign_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                        cu::Text(&l.text)
                            .type_role(TypeRole::Mono)
                            .font_size(Fs::Xs2.size())
                            .line_height(Fs::Xs2.line_height())
                            .color(paint(p.text))
                            .max_lines(1)
                            .word_break(compose_rust::WordBreak::KeepAll)
                            .overflow(TextOverflow::Ellipsis);
                    });
                }
            });
        }
    });
}

fn FilesHeader(
    stash: Option<Actions>,
    diffs_len: usize,
    all_checks: &[Check],
    staging: &MutableState<Staging>,
    diffs: &[FileDiff],
) {
    let p = ember_theme().palette;
    let st_toggle = staging.clone();
    let d_toggle = diffs.to_vec();
    let all_on = folder_check(all_checks) == Check::On;
    cu::Row().modifier(Modifier::new().fill_max_width().height(48.0).padding(SP[2])).alignment(Alignment::CenterStart).spacing(SP[1]).content(move || {
        cu::Checkbox(all_on)
            .modifier(Modifier::new().size(18.0, 18.0))
            .on_change(move |_| {
                let mut s = st_toggle.get();
                s.toggle_all(&d_toggle);
                st_toggle.set(s);
            });
        cu::Spacer().modifier(Modifier::new().width(SP[0]));
        Label(&format!("변경 파일 {}", diffs_len), Fs::Sm, Weight::Semibold, Some(paint(p.text)), Some(1));
        cu::Spacer().modifier(Modifier::new().weight(1.0));
        CircleIconButton(ICON_STASH_SVG, remote_button_labels()[3], move || {
            match &stash {
                Some(a) => a.stash_now(diffs_len),
                None => notice::post(soon_line("스태시")),
            }
        });
    });
}

fn FilesTree(
    diffs: &[FileDiff],
    staging: &MutableState<Staging>,
    folded: &MutableState<BTreeSet<String>>,
    selected: &MutableState<usize>,
    on_tab: impl FnMut(PaneTab) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let paths: Vec<String> = diffs.iter().map(|d| d.path.clone()).collect();
    let rows = build_tree(&paths, &folded.get());
    let sel = selected.get().min(diffs.len().saturating_sub(1));
    let st = staging.get();

    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[1])).spacing(0.0).content(move || {
        if diffs.is_empty() {
            cu::Box().modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
                Label("변경 사항이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            });
        } else {
            for row in rows.iter().cloned() {
                match row {
                    TreeRow::Dir { label, path, depth, open } => {
                        let f_state = folded.clone();
                        let pth = path.clone();
                        cu::Box()
                            .modifier(
                                Modifier::new()
                                    .fill_max_width()
                                    .height(34.0)
                                    .corner_radius(CORNER_XS),
                            )
                            .content(move || {
                                cu::Row()
                                    .alignment(Alignment::CenterStart)
                                    .modifier(Modifier::new().fill_max_size())
                                    .content(|| {
                                        if depth > 0 {
                                            cu::Spacer().modifier(Modifier::new().width(depth as f32 * 14.0));
                                        }
                                        cu::Spacer().modifier(Modifier::new().width(4.0));
                                        let chev_svg = if open { ICON_CHEVRON_DOWN_SVG } else { ICON_CHEVRON_RIGHT_SVG };
                                        NavIcon(chev_svg, p.muted.to_argb(), 14.0);
                                        cu::Spacer().modifier(Modifier::new().width(4.0));
                                        NavIcon(ICON_FOLDER_SVG, p.muted.to_argb(), 15.0);
                                        cu::Spacer().modifier(Modifier::new().width(6.0));
                                        Label(
                                            &label,
                                            Fs::Xs,
                                            Weight::Regular,
                                            Some(paint(p.text_2)),
                                            Some(1),
                                        );
                                    });
                                FocusOverlay(&label, move || {
                                    let mut f = f_state.get();
                                    if !f.remove(&pth) {
                                        f.insert(pth.clone());
                                    }
                                    f_state.set(f);
                                });
                            });
                    }
                    TreeRow::File { index, name, depth } => {
                        let is_selected = index == sel;
                        let diff_file = diffs[index].clone();
                        let is_staged = st.check(&diff_file);
                        let letter = diff_file.status.letter();
                        let (tone_fill, tone_text) = match diff_file.status {
                            FileStatus::Modified => (p.orange.soft, p.orange.strong),
                            FileStatus::Added => (p.green.soft, p.green.strong),
                            FileStatus::Deleted => (p.red.soft, p.red.strong),
                            FileStatus::Renamed => (p.blue.soft, p.blue.strong),
                        };
                        let bg = if is_selected { paint(p.sel) } else { Paint::Literal(Color::argb(0)) };
                        let st_file = staging.clone();
                        let sel_state = selected.clone();
                        let mut on_tab_file = on_tab.clone();
                        let d_file = diff_file.clone();

                        cu::Row()
                            .modifier(
                                Modifier::new()
                                    .fill_max_width()
                                    .height(34.0)
                                    .background(bg)
                                    .corner_radius(CORNER_XS),
                            )
                            .alignment(Alignment::CenterStart)
                            .content(move || {
                                if depth > 0 {
                                    cu::Spacer().modifier(Modifier::new().width(depth as f32 * 14.0));
                                }
                                cu::Spacer().modifier(Modifier::new().width(4.0));
                                cu::Checkbox(is_staged == Check::On)
                                    .modifier(Modifier::new().size(18.0, 18.0))
                                    .on_change(move |_| {
                                        let mut s = st_file.get();
                                        s.toggle_file(&d_file);
                                        st_file.set(s);
                                    });
                                cu::Spacer().modifier(Modifier::new().width(4.0));
                                let file_name = name.clone();
                                cu::Box()
                                    .modifier(Modifier::new().weight(1.0).height(34.0))
                                    .content(move || {
                                        cu::Row()
                                            .alignment(Alignment::CenterStart)
                                            .modifier(Modifier::new().fill_max_size())
                                            .content(|| {
                                                Label(
                                                    &file_name,
                                                    Fs::Sm,
                                                    Weight::Regular,
                                                    Some(paint(p.text)),
                                                    Some(1),
                                                );
                                                cu::Spacer().modifier(Modifier::new().weight(1.0));
                                                cu::Box()
                                                    .modifier(
                                                        Modifier::new()
                                                            .size(18.0, 18.0)
                                                            .background(paint(tone_fill))
                                                            .corner_radius(CORNER_XS),
                                                    )
                                                    .alignment(Alignment::Center)
                                                    .content(|| {
                                                        Label(
                                                            letter,
                                                            Fs::Xs2,
                                                            Weight::Semibold,
                                                            Some(paint(tone_text)),
                                                            Some(1),
                                                        );
                                                    });
                                                cu::Spacer().modifier(Modifier::new().width(SP[1]));
                                            });
                                        FocusOverlay(&file_name, move || {
                                            sel_state.set(index);
                                            on_tab_file(tab_after_select(PaneTab::Files));
                                        });
                                    });
                            });
                    }
                }
            }
        }
    });
}

fn FilesPane(
    diffs: &[FileDiff],
    staging: &MutableState<Staging>,
    folded: &MutableState<BTreeSet<String>>,
    selected: &MutableState<usize>,
    on_tab: impl FnMut(PaneTab) + Clone + 'static,
) {
    let st = staging.get();
    let all_checks: Vec<Check> = diffs.iter().map(|d| st.check(d)).collect();
    let diffs_len = diffs.len();
    let d_clone = diffs.to_vec();
    let st_clone = staging.clone();
    let fold_clone = folded.clone();
    let sel_clone = selected.clone();
    let p = ember_theme().palette;

    VcsCard(move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
            FilesHeader(None, diffs_len, &all_checks, &st_clone, &d_clone);
            cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
            FilesTree(&d_clone, &st_clone, &fold_clone, &sel_clone, on_tab);
        });
    });
}

fn DiffPane(diffs: &[FileDiff], selected: usize, staging: &MutableState<Staging>) {
    let p = ember_theme().palette;
    let selected_file = diffs.get(selected).cloned();
    let st_diff = staging.clone();
    VcsCard(move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
            if let Some(file) = selected_file {
                DiffView(&file, &st_diff);
            } else {
                cu::Box().modifier(Modifier::new().fill_max_width().padding(SP[3])).content(|| {
                    Label("변경 사항이 없습니다. 작업 트리가 깨끗합니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                });
            }
        });
    });
}

fn CommitBox(
    branch: &str,
    staged_files: usize,
    summary: &MutableState<String>,
    body: &MutableState<String>,
    amend: &MutableState<bool>,
    commit: Option<(Actions, RepoSnapshot, MutableState<Staging>)>,
) {
    let p = ember_theme().palette;
    let sum_state = summary.clone();
    let body_state = body.clone();
    let amend_state = amend.clone();
    let field_generation = remember(|| mutable_state_of(0_u64));
    let is_amend = amend.get();
    let branch_name = branch.to_string();
    let can_commit = commit_enabled(&summary.get(), staged_files, is_amend);
    // Amending a commit already on the upstream asks twice (#269): the armed flag is the first tap.
    let armed = remember(|| mutable_state_of(0_i64));
    let warning = if is_amend { commit.as_ref().and_then(|(_, snap, _)| amend_warning(snap)) } else { None };
    let is_armed = warning.is_some() && armed.get() != 0;

    cu::Column()
        .modifier(
            Modifier::new()
                .fill_max_width()
                .background(paint(p.surface_2))
                .padding(SP[2]),
        )
        .spacing(SP[2])
        .content(move || {
            let s_state = sum_state.clone();
            let s_armed = armed.clone();
            key((field_generation.get(), 0_u8), || {
                EmberField(FieldOptions {
                    fill_max_width: true,
                    text: Some(sum_state.get()),
                    placeholder: "요약 (필수) 예: Feat: Wire shim".to_string(),
                    on_value_change: Some(Box::new(move |v| {
                        s_armed.set(0);
                        s_state.set(v);
                    })),
                    ..Default::default()
                });
            });

            let b_state = body_state.clone();
            let b_armed = armed.clone();
            key((field_generation.get(), 1_u8), || {
                EmberField(FieldOptions {
                    fill_max_width: true,
                    height: Some(64.0),
                    text: Some(body_state.get()),
                    placeholder: "설명 (선택)".to_string(),
                    multiline: true,
                    on_value_change: Some(Box::new(move |v| {
                        b_armed.set(0);
                        b_state.set(v);
                    })),
                    ..Default::default()
                });
            });

            // Amend checkbox
            cu::Row().modifier(Modifier::new().fill_max_width().height(32.0)).alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                let am = amend_state.clone();
                let am_armed = armed.clone();
                // TODO(compose-rust#428): Checkbox has no ink fill or tick style, so staging looks hollow.
                cu::Checkbox(is_amend)
                    .modifier(Modifier::new().size(18.0, 18.0))
                    .on_change(move |v| {
                        am_armed.set(0);
                        am.set(v);
                    });
                Label("마지막 커밋 수정 (amend)", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
            });
            if let Some(text) = warning {
                Label(text, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
            }

            let click_armed = armed.clone();
            AppButton(
                &amend_button_label(&commit_label(&branch_name, staged_files), warning, is_armed),
                Some(IconRole::Check),
                ButtonKind::Primary,
                false,
                can_commit,
                true,
                move || {
                    if let Some((actions, snap, staging)) = &commit {
                        let needs_confirm = amend_state.get() && amend_warning(snap).is_some();
                        match amend_tap(needs_confirm, click_armed.get(), crate::demo::now_ms()) {
                            AmendTap::Arm => {
                                click_armed.set(crate::demo::now_ms().max(1));
                                return;
                            }
                            AmendTap::Wait => return,
                            AmendTap::Commit => {}
                        }
                        click_armed.set(0);
                        let req = commit_request(snap, &staging.get(), &sum_state.get(), &body_state.get(), amend_state.get());
                        let project = actions.project.clone();
                        let submitted_summary = req.summary.clone();
                        let submitted_body = req.description.clone();
                        let summary = sum_state.clone();
                        let body = body_state.clone();
                        let generation = field_generation.clone();
                        actions.finish_with("커밋을 ", async move { run_commit(&project, req).await }, move || {
                            summary.set(text_after_success(&summary.get(), &submitted_summary));
                            body.set(text_after_success(&body.get(), &submitted_body));
                            generation.set(generation.get() + 1);
                        });
                    }
                },
            );

            let note_text = if can_commit {
                "선택한 변경 사항을 커밋합니다".to_string()
            } else {
                "요약과 스테이징한 파일이 필요합니다".to_string()
            };
            Label(&note_text, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
        });
}

fn CommitPane(
    branch: &str,
    staged_files: usize,
    summary: &MutableState<String>,
    body: &MutableState<String>,
    amend: &MutableState<bool>,
    commit: Option<(Actions, RepoSnapshot, MutableState<Staging>)>,
) {
    VcsCard(move || {
        CommitBox(branch, staged_files, summary, body, amend, commit);
    });
}

/// Changes pane: tree, diff and commit box.
pub fn ChangesPane(
    snap: &RepoSnapshot,
    layout: PaneLayout,
    tab: Option<PaneTab>,
    on_tab: impl FnMut(PaneTab) + Clone + 'static,
) {
    ChangesPaneWithActions(snap, layout, tab, on_tab, None);
}

fn ChangesPaneWithActions(snap: &RepoSnapshot, layout: PaneLayout, tab: Option<PaneTab>, on_tab: impl FnMut(PaneTab) + Clone + 'static, actions: Option<Actions>) {
    let p = ember_theme().palette;
    let diffs = snap.changes.clone();
    let staging_key = (actions.as_ref().map(|a| (a.project.clone(), a.reload.get())), snap.branch.clone(), snap.staged.clone(), diffs.clone());
    let staging = remember_with_key(staging_key, || {
        let mut state = Staging::new(&diffs, false);
        for diff in &diffs { if snap.staged.contains(&diff.path) { state.toggle_file(diff); } }
        mutable_state_of(state)
    });
    let stash_actions = actions.clone();
    let commit = actions.map(|a| (a, snap.clone(), staging.clone()));
    let folded = remember(|| mutable_state_of(BTreeSet::<String>::new()));
    let selected = remember(|| mutable_state_of(0usize));
    let summary = remember(|| mutable_state_of(String::new()));
    let body = remember(|| mutable_state_of(String::new()));
    let amend = remember(|| mutable_state_of(false));

    let sel = selected.get().min(diffs.len().saturating_sub(1));
    let staged_files = staging.get().staged_files(&diffs);
    let st = staging.get();
    let all_checks: Vec<Check> = diffs.iter().map(|d| st.check(d)).collect();
    let diffs_len = diffs.len();

    match layout {
        PaneLayout::Two => {
            // The panes scroll on their own inside a bounded row. A row in an unbounded
            // scroll centres its shorter children vertically (TODO(compose-rust#389)).
            let d_files = diffs.clone();
            let st_files = staging.clone();
            let fold_files = folded.clone();
            let sel_files = selected.clone();
            let on_tab_files = on_tab.clone();
            let branch = snap.branch.clone();
            let sum_files = summary.clone();
            let body_files = body.clone();
            let am_files = amend.clone();

            cu::Row()
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .alignment(Alignment::TopStart)
                .spacing(GAP_CARD)
                .content(move || {
                    // Left column: the tree scrolls, the commit box stays pinned below it (#290)
                    cu::Column().spacing(GAP_CARD).modifier(Modifier::new().width(320.0).fill_max_height()).content(move || {
                        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().weight(1.0)).content(move || {
                            VcsCard(move || {
                                cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
                                    FilesHeader(stash_actions.clone(), diffs_len, &all_checks, &st_files, &d_files);
                                    cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                                    FilesTree(&d_files, &st_files, &fold_files, &sel_files, on_tab_files);
                                });
                            });
                        });
                        VcsCard(move || {
                            CommitBox(&branch, staged_files, &sum_files, &body_files, &am_files, commit.clone());
                        });
                    });

                    // Right card: diff
                    cu::ScrollColumn().modifier(Modifier::new().weight(1.0).fill_max_height()).content(move || {
                        DiffPane(&diffs, sel, &staging);
                    });
                });
        }
        PaneLayout::Stacked => {
            let d_files = diffs.clone();
            let st_files = staging.clone();
            let fold_files = folded.clone();
            let sel_files = selected.clone();
            let on_tab_files = on_tab.clone();
            let branch = snap.branch.clone();
            let sum_files = summary.clone();
            let body_files = body.clone();
            let am_files = amend.clone();

            cu::Column().modifier(Modifier::new().fill_max_width()).spacing(GAP_CARD).content(move || {
                VcsCard(move || {
                    cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
                        FilesHeader(stash_actions.clone(), diffs_len, &all_checks, &st_files, &d_files);
                        cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                        FilesTree(&d_files, &st_files, &fold_files, &sel_files, on_tab_files);
                        cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                        CommitBox(&branch, staged_files, &sum_files, &body_files, &am_files, commit.clone());
                    });
                });
                DiffPane(&diffs, sel, &staging);
            });
        }
        PaneLayout::Tabs => {
            let mut on_tab_pick = on_tab.clone();
            let d_files = diffs.clone();
            let st_files = staging.clone();
            let fold_files = folded.clone();
            let sel_files = selected.clone();
            let on_tab_files = on_tab.clone();

            cu::Column().modifier(Modifier::new().fill_max_width()).spacing(GAP_CARD).content(move || {
                PaneTabs(Section::Changes, tab, move |t| on_tab_pick(t));
                match tab.unwrap_or(PaneTab::Files) {
                    PaneTab::Diff => DiffPane(&diffs, sel, &staging),
                    PaneTab::Commit => {
                        CommitPane(&snap.branch, staged_files, &summary, &body, &amend, commit.clone());
                        cu::Spacer().modifier(Modifier::new().height(super::routines::compact_bottom_clearance()));
                    }
                    _ => {
                        VcsCard(move || {
                            cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
                                FilesHeader(stash_actions.clone(), diffs_len, &all_checks, &st_files, &d_files);
                                cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                                FilesTree(&d_files, &st_files, &fold_files, &sel_files, on_tab_files);
                            });
                        });
                    }
                }
            });
        }
    }
}

fn SectionContent(
    snap: Option<&RepoSnapshot>,
    sec: Section,
    layout: PaneLayout,
    active_tab: Option<PaneTab>,
    tab: &MutableState<PaneTab>,
    actions: Option<Actions>,
    stash_count: MutableState<Option<usize>>,
    pr_count: MutableState<Option<usize>>,
    branch_count: MutableState<Option<usize>>,
    team_count: MutableState<Option<usize>>,
    pr_list: MutableState<Vec<PrEntry>>,
) {
    match (snap, sec) {
        (Some(s), Section::Changes) => {
            let tab_state = tab.clone();
            ChangesPaneWithActions(
                s,
                layout,
                active_tab,
                move |t| tab_state.set(t),
                actions,
            );
        }
        (Some(s), Section::Conflicts) => ConflictsPane(&s.conflicts, actions, layout),
        (Some(_), Section::History) => HistoryPane(actions.as_ref().map(|a| (a.project.clone(), a.reload.get())), layout),
        (Some(s), Section::Stash) => StashPane(actions, stash_count, layout, !s.changes.is_empty()),
        (Some(s), Section::PullRequests) => PullRequestsPane(actions, pr_count, pr_list, layout, Some(s.branch.clone())),
        (Some(_), Section::Team) => TeamPane(actions, team_count, pr_list, layout),
        (Some(_), Section::Branches) => BranchesPane(actions, branch_count, layout),
        _ => Unavailable(sec),
    }
}

/// A conflict file for the review captures and a screen without a server.
fn sample_conflict() -> ConflictFile {
    use ember_client::api::{GitConflict, GitConflictBlock};
    let block = |ours: &str, theirs: &str| GitConflictBlock::Conflict {
        ours: ours.into(),
        theirs: theirs.into(),
        ours_label: "HEAD (feat/git-api)".into(),
        theirs_label: "origin/main".into(),
    };
    let c = GitConflict {
        blocks: vec![
            GitConflictBlock::Common { text: "use compose_rust::ui as cu;\n".into() },
            block("use crate::ui::vcs::Section;\n", "use crate::ui::vcs::{Section, SectionNav};\n"),
            GitConflictBlock::Common { text: "\npub fn shell() {\n".into() },
            block("    let pad = 16.0;\n", "    let pad = 24.0;\n"),
            GitConflictBlock::Common { text: "}\n".into() },
        ],
    };
    ConflictFile::from_node("ember/app/src/ui/cr/shell.rs", &c)
}

/// The merge banner state for the sample screen (no services).
fn sample_merge() -> MergeLoad {
    MergeLoad::Ready(Some(MergeView { from: "origin/main".into(), into: "feat/session-shim".into(), remaining: 2, can_commit: false }))
}

/// 충돌 with the merge banner (FR-G16, FR-G17, #382): the banner card above the list or editor
/// while a merge is in progress, with 병합 취소 and 병합 커밋 (each asks twice). A conflict list
/// without a merge (a stash pop, a cherry-pick) shows no banner. `actions` is `None` for the
/// sample screen. A finished abort or commit remounts the body so an open editor closes.
fn ConflictsPane(paths: &[String], actions: Option<Actions>, layout: PaneLayout) {
    let merge = remember(|| mutable_state_of(MergeLoad::Idle));
    let mlocal = remember(|| mutable_state_of(0_u64));
    let mbusy = remember(|| mutable_state_of(false));
    let armed = remember(|| mutable_state_of(None::<(usize, i64)>));
    let generation = remember(|| mutable_state_of(0_u64));

    if let Some(a) = actions.clone() {
        let m = merge.clone();
        launched_effect((a.project.clone(), a.reload.get(), mlocal.get()), move || async move {
            if crate::services::try_services().is_some() {
                if m.get() == MergeLoad::Idle {
                    m.set(MergeLoad::Loading);
                }
                let name = a.project.clone();
                crate::services::run_shared(async move { fetch_merge_state(&name).await }, move |result| m.set(result));
            } else {
                m.set(MergeLoad::Ready(None));
            }
        });
    }
    let current = if actions.is_some() { merge.get() } else { sample_merge() };
    let view = match current {
        MergeLoad::Ready(Some(v)) => Some(v),
        _ => None,
    };

    let tap = {
        let (actions, armed, mbusy, mlocal, generation) = (actions.clone(), armed.clone(), mbusy.clone(), mlocal.clone(), generation.clone());
        move |action: MergeAction| {
            if mbusy.get() {
                return;
            }
            let now = crate::demo::now_ms();
            match merge_tap(action, armed.get(), now) {
                DeleteTap::Arm | DeleteTap::Expired => armed.set(Some((merge_tap_key(action), now))),
                DeleteTap::Wait => {}
                DeleteTap::Confirm => {
                    armed.set(None);
                    let Some(a) = actions.clone() else { return };
                    // Busy is set before the request is spawned, so a second quick tap is ignored.
                    mbusy.set(true);
                    let (name, mbusy, mlocal, generation, reload) = (a.project.clone(), mbusy.clone(), mlocal.clone(), generation.clone(), a.reload.clone());
                    crate::services::run_shared(
                        async move {
                            match action {
                                MergeAction::Abort => run_merge_abort(&name).await,
                                MergeAction::Commit => run_merge_commit(&name).await,
                            }
                        },
                        move |result| {
                            match result {
                                Ok(text) => notice::post(text),
                                Err(line) => notice::post(line),
                            }
                            // Read the state, list and snapshot again after either answer: a 409
                            // means the state on screen was stale.
                            mlocal.set(mlocal.get() + 1);
                            generation.set(generation.get() + 1);
                            reload.set(reload.get() + 1);
                            mbusy.set(false);
                        },
                    );
                }
            }
        }
    };

    let two = layout == PaneLayout::Two;
    let body_paths = paths.to_vec();
    let body = {
        let (actions, generation) = (actions.clone(), generation.clone());
        move || {
            key(generation.get(), || ConflictsBody(&body_paths, actions.clone(), layout));
        }
    };
    let banner = {
        let (armed, mbusy, tap) = (armed.clone(), mbusy.clone(), tap.clone());
        move || {
            if let Some(v) = &view {
                MergeBanner(v, &armed, mbusy.get(), tap.clone());
            }
        }
    };
    if two {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().fill_max_height()).content(move || {
            banner();
            cu::Box().modifier(Modifier::new().fill_max_width().weight(1.0)).content(body.clone());
        });
    } else {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
            banner();
            body();
        });
    }
}

/// The banner card: "<from> -> <into> 병합 중", the remaining files, 병합 취소 and 병합 커밋.
/// 병합 커밋 is enabled only when no conflict file remains; both ask twice (the armed label shows
/// while `armed_label_visible` holds at composition).
fn MergeBanner(v: &MergeView, armed: &MutableState<Option<(usize, i64)>>, busy: bool, tap: impl Fn(MergeAction) + Clone + 'static) {
    let p = ember_theme().palette;
    let now = crate::demo::now_ms();
    let shows = |action: MergeAction| armed_label_visible(armed.get(), merge_tap_key(action), now);
    let (abort_armed, commit_armed) = (shows(MergeAction::Abort), shows(MergeAction::Commit));
    let (title, sub, can_commit) = (banner_title(v), banner_sub(v.remaining), v.can_commit);
    VcsCard(move || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(move || {
            Txt(&title, TypeRole::BodyStrong, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            Label(&sub, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(move || {
                let on_abort = tap.clone();
                let kind = if abort_armed { ButtonKind::Destructive } else { ButtonKind::Default };
                AppButton(banner_button_text(MergeAction::Abort, abort_armed), None, kind, true, !busy, false, move || on_abort(MergeAction::Abort));
                let on_commit = tap.clone();
                AppButton(banner_button_text(MergeAction::Commit, commit_armed), None, ButtonKind::Primary, true, can_commit && !busy, false, move || on_commit(MergeAction::Commit));
            });
            Label(MERGE_ABORT_NOTE, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            if commit_armed {
                Label(MERGE_COMMIT_NOTE, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
        });
    });
}

/// 충돌: the unmerged files, and (after a tap) the editor of one file. `paths` is the list from
/// the repository snapshot; resolving a file bumps the screen's reload counter so the list and
/// 변경 사항 read again. `actions` is `None` for the sample screen, which edits a sample file.
fn ConflictsBody(paths: &[String], actions: Option<Actions>, layout: PaneLayout) {
    let p = ember_theme().palette;
    // `EMBER_CR_VCS_CONFLICT=<path>` opens that file's editor directly, for captures without clicks.
    let open = remember(|| mutable_state_of(std::env::var("EMBER_CR_VCS_CONFLICT").ok().filter(|p| !p.is_empty())));
    let state = remember(|| mutable_state_of(ConflictLoad::Loading));
    let local = remember(|| mutable_state_of(0_u64));
    let busy = remember(|| mutable_state_of(false));

    if let (Some(a), Some(path)) = (actions.clone(), open.get()) {
        let st = state.clone();
        launched_effect((a.project.clone(), path.clone(), local.get()), move || async move {
            if crate::services::try_services().is_some() {
                st.set(ConflictLoad::Loading);
                let name = a.project.clone();
                crate::services::run_shared(async move { fetch_conflict(&name, &path).await }, move |result| st.set(result));
            } else {
                st.set(ConflictLoad::Failed("서버에 연결되어 있지 않아 충돌 파일을 불러올 수 없습니다.".to_string()));
            }
        });
    }

    let Some(path) = open.get() else {
        // The file list: a tap opens the editor.
        if paths.is_empty() {
            EmberCard(false, true, None, Some(SP[3]), None, || {
                Label("충돌", Fs::Lg, Weight::Semibold, None, None);
                Label("충돌이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            });
            return;
        }
        let paths = paths.to_vec();
        EmberCard(false, true, None, Some(SP[3]), None, move || {
            Label("충돌", Fs::Lg, Weight::Semibold, None, None);
            Label("파일을 눌러 충돌을 해결합니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            for path in paths {
                let (open, state, has_server) = (open.clone(), state.clone(), actions.is_some());
                let target = path.clone();
                AppButton(&path, None, ButtonKind::Default, true, true, true, move || {
                    state.set(if has_server { ConflictLoad::Loading } else { ConflictLoad::Ready(sample_conflict()) });
                    open.set(Some(target.clone()));
                });
            }
        });
        return;
    };

    let step = remember(|| mutable_state_of(ConflictStep::Editing));
    let back = {
        let (open, busy, step) = (open.clone(), busy.clone(), step.clone());
        move || {
            open.set(None);
            busy.set(false);
            step.set(ConflictStep::Editing);
        }
    };
    let two = layout == PaneLayout::Two;
    // The scrolling part: the path, then the blocks, or the preview of the merged text.
    let content = {
        let (state, local, busy, step, path) = (state.clone(), local.clone(), busy.clone(), step.clone(), path.clone());
        move || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                Txt(&path, TypeRole::Mono, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
                match state.get() {
                    ConflictLoad::Loading => {
                        Label("충돌 내용을 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    ConflictLoad::Failed(text) => {
                        Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        let retry = local.clone();
                        AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
                    }
                    ConflictLoad::Ready(file) => {
                        if step.get().is_preview() {
                            ConflictPreview(&file, &step);
                        } else {
                            // 모두 현재 것 / 모두 들어오는 것: settle every block of this file in one tap.
                            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                                for (text, pick) in [("모두 현재 것", ConflictChoice::Ours), ("모두 들어오는 것", ConflictChoice::Theirs)] {
                                    let (file, state) = (file.clone(), state.clone());
                                    AppButton(text, None, ButtonKind::Default, true, !busy.get(), false, move || {
                                        state.set(ConflictLoad::Ready(file.pick_all(pick)));
                                    });
                                }
                            });
                            ConflictBlocks(&file, layout, busy.get(), &state);
                        }
                    }
                }
            });
        }
    };
    // The action bar: always visible below the scroll area at the wide layout, after the blocks
    // (with room for the floating tab bar) at the compact one.
    let bar = {
        let (state, busy, open, actions, path, step) = (state.clone(), busy.clone(), open.clone(), actions.clone(), path.clone(), step.clone());
        move || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                if let ConflictLoad::Ready(file) = state.get() {
                    ConflictActions(&file, busy.get(), &step, &busy, &open, &actions, &path);
                }
                let back = back.clone();
                AppButton("목록으로", None, ButtonKind::Default, true, !busy.get(), false, back);
                if !two {
                    cu::Spacer().modifier(Modifier::new().height(super::routines::compact_bottom_clearance()));
                }
            });
        }
    };
    if two {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().fill_max_height()).content(move || {
            cu::ScrollColumn().modifier(Modifier::new().fill_max_width().weight(1.0)).content(content.clone());
            bar();
        });
    } else {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
            content();
            bar();
        });
    }
}

/// The blocks of one file as cards, each with its three choices.
fn ConflictBlocks(file: &ConflictFile, layout: PaneLayout, is_busy: bool, state: &MutableState<ConflictLoad>) {
    let p = ember_theme().palette;
    let mut index = 0;
    for seg in &file.segments {
        match seg {
            ConflictSegment::Common(text) => {
                if !text.trim().is_empty() {
                    VcsCard(|| {
                        cu::Box().modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
                            Txt(text, TypeRole::Mono, Some(paint(p.text_2)), None, None, None, None);
                        });
                    });
                }
            }
            ConflictSegment::Block { ours_label, theirs_label, ours, theirs, choice } => {
                let n = index;
                index += 1;
                let (choice, file, state) = (*choice, file.clone(), state.clone());
                VcsCard(|| {
                    cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
                        Label(&format!("충돌 {}", n + 1), Fs::Sm, Weight::Semibold, None, Some(1));
                        let two = layout == PaneLayout::Two;
                        let side = |label: &str, text: &str, ours_side: bool| {
                            let (bg, fg) = if ours_side { (p.green.soft, p.green.strong) } else { (p.blue.soft, p.blue.strong) };
                            let size = if two { Modifier::new().weight(1.0) } else { Modifier::new().fill_max_width() };
                            cu::Column().spacing(SP[1]).modifier(size.background(paint(bg)).corner_radius(CORNER_LG).padding(SP[2])).content(|| {
                                Txt(&chip_text(label), TypeRole::Caption, Some(paint(fg)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                                Txt(text, TypeRole::Mono, Some(paint(p.text)), None, None, None, None);
                            });
                        };
                        if two {
                            cu::Row().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                                side(ours_label, ours, true);
                                side(theirs_label, theirs, false);
                            });
                        } else {
                            side(ours_label, ours, true);
                            side(theirs_label, theirs, false);
                        }
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                            for (text, pick) in [("내 변경", ConflictChoice::Ours), ("상대 변경", ConflictChoice::Theirs), ("둘 다", ConflictChoice::Both)] {
                                let (file, state) = (file.clone(), state.clone());
                                let selected = choice == pick;
                                let kind = if selected { ButtonKind::Primary } else { ButtonKind::Default };
                                AppButton(&choice_label(text, selected), None, kind, true, !is_busy, false, move || {
                                    state.set(ConflictLoad::Ready(file.with_choice(n, pick)));
                                });
                            }
                        });
                    });
                });
            }
        }
    }
}

/// The merged text as it will be written, cut to `CONFLICT_PREVIEW_LINES` lines.
fn ConflictPreview(file: &ConflictFile, step: &MutableState<ConflictStep>) {
    let p = ember_theme().palette;
    let Some((text, more)) = file.preview(step.get().preview_max()) else {
        Label("아직 해결하지 않은 충돌이 있습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
        return;
    };
    VcsCard(|| {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
            Label("해결 결과 미리 보기", Fs::Sm, Weight::Semibold, None, Some(1));
            Txt(&text, TypeRole::Mono, Some(paint(p.text)), None, None, None, None);
            if more > 0 {
                Label(&format!("... {more}줄 더 있습니다"), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
            if more > 0 || step.get() == ConflictStep::PreviewFull {
                let toggle = step.clone();
                let text = if step.get() == ConflictStep::PreviewFull { "접기" } else { "전체 보기" };
                AppButton(text, None, ButtonKind::Default, true, true, false, move || toggle.set(toggle.get().toggle_full()));
            }
        });
    });
}

/// The progress text and the buttons of the current step: 해결 완료 while editing, 취소 and
/// 해결하고 스테이징 while previewing. Only the second writes the file.
fn ConflictActions(
    file: &ConflictFile,
    is_busy: bool,
    step: &MutableState<ConflictStep>,
    busy: &MutableState<bool>,
    open: &MutableState<Option<String>>,
    actions: &Option<Actions>,
    path: &str,
) {
    let p = ember_theme().palette;
    let content = file.resolved_content();
    if step.get() == ConflictStep::Editing {
        Label(&file.progress_text(), Fs::Sm, Weight::Medium, Some(paint(p.text_2)), Some(1));
        let (step, file) = (step.clone(), file.clone());
        AppButton("해결 완료", None, ButtonKind::Primary, false, content.is_some() && !is_busy, true, move || {
            step.set(ConflictStep::Editing.enter_preview(&file));
        });
        return;
    }
    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
        let cancel = step.clone();
        AppButton("취소", None, ButtonKind::Default, true, !is_busy, false, move || cancel.set(cancel_preview(cancel.get())));
        let (step, busy, open, actions, path, file) = (step.clone(), busy.clone(), open.clone(), actions.clone(), path.to_string(), file.clone());
        AppButton("해결하고 스테이징", None, ButtonKind::Primary, true, content.is_some() && !is_busy, false, move || {
            // Busy is set before the request is spawned, so a second quick tap is ignored.
            let ResolveTap::Send(text) = confirm(step.get(), busy.get(), &file) else { return };
            let Some(a) = actions.clone() else {
                open.set(None);
                step.set(ConflictStep::Editing);
                return;
            };
            busy.set(true);
            let (name, target, busy, open, step, reload) = (a.project.clone(), path.clone(), busy.clone(), open.clone(), step.clone(), a.reload.clone());
            crate::services::run_shared(async move { run_resolve(&name, &target, &text).await }, move |result| {
                match &result {
                    Ok(()) => {
                        notice::post("충돌을 해결했습니다".to_string());
                        open.set(None);
                        step.set(ConflictStep::Editing);
                        busy.set(false);
                        reload.set(reload.get() + 1);
                    }
                    Err(line) => {
                        notice::post(line.clone());
                        busy.set(false);
                    }
                }
            });
        });
    });
}

/// How many ref chips a history row shows before "+N".
fn chip_limit(layout: PaneLayout) -> usize {
    if layout == PaneLayout::Tabs { 2 } else { 4 }
}

/// The chip tone of a ref: HEAD and branches blue, remotes grey, tags orange.
fn ref_tone(r: &HistoryRef) -> PillTone {
    use ember_client::api::GitLogRefKind::*;
    match r.kind {
        Head => PillTone::Green,
        Branch => PillTone::Blue,
        Remote => PillTone::Grey,
        Tag => PillTone::Orange,
    }
}

/// A chip text cut to 24 characters so a long branch name cannot push the row past the screen.
fn chip_text(label: &str) -> String {
    if label.chars().count() <= 24 {
        label.to_string()
    } else {
        format!("{}\u{2026}", label.chars().take(23).collect::<String>())
    }
}

/// The meta line of a history row: short sha, author and time.
fn history_meta(e: &HistoryEntry) -> String {
    format!("{} \u{00b7} {} \u{00b7} {}", e.short, e.author, e.time_text)
}

/// One commit of the history list. The row has no click yet (the 상세 pane comes with the lane
/// graph).
fn HistoryRow(e: &HistoryEntry, layout: PaneLayout) {
    let p = ember_theme().palette;
    let limit = chip_limit(layout);
    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
        // TODO(compose-rust#308): lane graph (a `Canvas` column of dots and edges) goes left of the text.
        Txt(&e.subject, TypeRole::BodyStrong, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            Txt(&history_meta(e), TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), Some(1.0), None);
        });
        if e.is_merge || !e.refs.is_empty() {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                if e.is_merge {
                    Pill("병합", PillTone::Sky, true);
                }
                for r in e.refs.iter().take(limit) {
                    Pill(&chip_text(&r.label), ref_tone(r), true);
                }
                if e.refs.len() > limit {
                    Pill(&format!("+{}", e.refs.len() - limit), PillTone::Grey, true);
                }
            });
        }
    });
}

/// Commits for the review captures (`EMBER_CR_ROUTE=vcs`): the only path that reaches `HistoryPane`
/// without actions, because a screen without services has no snapshot and shows 준비 중입니다.
fn sample_history() -> HistoryLoad {
    let e = |short: &str, subject: &str, author: &str, time: &str, merge: bool, refs: &[(&str, ember_client::api::GitLogRefKind)]| HistoryEntry {
        sha: format!("{short}0000000000000000000000000000000000"),
        short: short.to_string(),
        subject: subject.to_string(),
        author: author.to_string(),
        time_text: time.to_string(),
        refs: refs.iter().map(|(l, k)| HistoryRef { label: l.to_string(), kind: k.clone() }).collect(),
        is_merge: merge,
    };
    use ember_client::api::GitLogRefKind::*;
    HistoryLoad::Ready {
        entries: vec![
            e("a1b2c3d", "Feat: attach the session shim before routing", "BREW", "12분 전", false, &[("HEAD", Head), ("feat/session-shim", Branch)]),
            e("2b3c4d5", "Merge branch 'feat/egress-stop' into develop", "BREW", "3시간 전", true, &[("develop", Branch), ("origin/develop", Remote)]),
            e("8f90123", "Fix: stop egress when a session detaches", "Minji Kim", "1일 전", false, &[("v0.3.0", Tag)]),
        ],
        more: false,
        loading_more: false,
    }
}

/// 히스토리: the commits of the current branch as a list, newest first. `project` is the project
/// and the screen's reload counter (a commit or a branch change reads the list again); `None`
/// shows sample commits.
fn HistoryPane(project: Option<(String, u64)>, layout: PaneLayout) {
    let p = ember_theme().palette;
    let key_project = project.as_ref().map(|(name, _)| name.clone());
    let state = remember_with_key(key_project, || mutable_state_of(HistoryLoad::Loading));
    let retry = remember(|| mutable_state_of(0_u64));
    if let Some((name, reload)) = project.clone() {
        let (st, again) = (state.clone(), retry.get());
        launched_effect((name.clone(), reload, again), move || async move {
            if crate::services::try_services().is_some() {
                st.set(HistoryLoad::Loading);
                let now = crate::demo::now_ms() / 1000;
                crate::services::run_shared(async move { history_from(fetch_history(&name, 0).await, now) }, move |result| st.set(result));
            } else {
                st.set(HistoryLoad::Failed("서버에 연결되어 있지 않아 히스토리를 불러올 수 없습니다.".to_string()));
            }
        });
    }
    let current = if project.is_some() { state.get() } else { sample_history() };

    let more_state = state.clone();
    let more_project = project.as_ref().map(|(name, _)| name.clone());
    let load_more = move || {
        let (Some(name), HistoryLoad::Ready { entries, more: true, loading_more: false }) = (more_project.clone(), more_state.get()) else {
            return;
        };
        let skip = entries.len() as u64;
        more_state.set(HistoryLoad::Ready { entries, more: true, loading_more: true });
        let st = more_state.clone();
        let now = crate::demo::now_ms() / 1000;
        crate::services::run_shared(async move { fetch_history(&name, skip).await }, move |result| {
            let (next, error) = history_append(st.get(), result, now);
            st.set(next);
            if let Some(line) = error {
                notice::post(line);
            }
        });
    };

    let list = move || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || match current {
            HistoryLoad::Loading => {
                Label("히스토리를 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
            HistoryLoad::Failed(text) => {
                Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
            }
            HistoryLoad::Ready { entries, more, loading_more } => {
                if entries.is_empty() {
                    EmberCard(false, true, None, Some(SP[3]), None, || {
                        Label("커밋이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    });
                    return;
                }
                VcsCard(|| {
                    for (i, e) in entries.iter().enumerate() {
                        if i > 0 {
                            cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                        }
                        HistoryRow(e, layout);
                    }
                });
                if more {
                    let label = if loading_more { "불러오는 중입니다" } else { "더 보기" };
                    AppButton(label, None, ButtonKind::Default, false, !loading_more, true, load_more);
                }
            }
        });
    };
    if layout == PaneLayout::Two {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(list);
    } else {
        list();
    }
}

/// Pull requests for the review captures (`EMBER_CR_ROUTE=vcs`) and for a screen without a server.
/// How many pull requests the sample list holds (the header and the tab count follow it).
fn sample_pr_count() -> usize {
    match sample_prs() {
        PrLoad::Ready(list) => list.iter().filter(|e| e.state == ember_client::api::GitPrState::Open).count(),
        _ => 0,
    }
}

fn sample_prs() -> PrLoad {
    use ember_client::api::{GitPrReview::*, GitPrState::*};
    let e = |number: u64, title: &str, author: &str, head: &str, time: &str, state, draft: bool, review, checks: (u32, u32, u32), gate: (bool, &str)| PrEntry {
        number,
        title: title.to_string(),
        author: author.to_string(),
        head: head.to_string(),
        base: "develop".to_string(),
        time_text: time.to_string(),
        state,
        state_text: crate::ui::vcs::pr_state_text(state),
        draft,
        review,
        review_text: crate::ui::vcs::pr_review_text(review),
        checks_text: crate::ui::vcs::pr_checks_text(checks.0, checks.1, checks.2),
        checks_failed: checks.1,
        checks_pending: checks.2,
        can_merge: gate.0,
        gate_text: gate.1.to_string(),
        url: format!("https://github.com/DarkPyonix/darkpyonix-ember/pull/{number}"),
    };
    PrLoad::Ready(vec![
        e(412, "Feat: attach the session shim before routing", "BREW", "feat/session-shim", "12분 전", Open, false, Approved, (4, 0, 0), (true, "병합할 수 있습니다")),
        e(409, "Fix: stop egress when a session detaches", "Minji Kim", "fix/egress-stop", "5시간 전", Open, false, ReviewRequired, (3, 1, 0), (false, "CI 체크가 실패했습니다 (실패 1개). 통과해야 병합할 수 있습니다.")),
        e(405, "Feat: channel feed with mentions", "Jisoo Park", "feat/channel-feed", "1일 전", Open, true, None, (1, 0, 2), (false, "초안 상태라 병합할 수 없습니다. 검토 준비 후 초안을 해제하세요.")),
        e(398, "Docs: describe the commit message rules", "Minji Kim", "docs/commit-rules", "3일 전", Merged, false, Approved, (4, 0, 0), (false, "이미 병합되었습니다.")),
    ])
}

/// The meta line of a pull request row: author, head branch to base branch, and time.
fn pr_meta(e: &PrEntry) -> String {
    format!("{} \u{00b7} {} \u{2192} {} \u{00b7} {}", e.author, e.head, e.base, e.time_text)
}

fn pr_state_tone(e: &PrEntry) -> PillTone {
    use ember_client::api::GitPrState::*;
    match e.state {
        Open => PillTone::Green,
        Merged => PillTone::Blue,
        Closed => PillTone::Grey,
    }
}

fn pr_review_tone(e: &PrEntry) -> PillTone {
    use ember_client::api::GitPrReview::*;
    match e.review {
        Approved => PillTone::Green,
        ChangesRequested => PillTone::Red,
        ReviewRequired => PillTone::Orange,
        None => PillTone::Grey,
    }
}

/// Red when a check failed, orange while some run, green when all passed, grey with none.
fn pr_checks_tone(e: &PrEntry) -> PillTone {
    if e.checks_failed > 0 {
        PillTone::Red
    } else if e.checks_pending > 0 {
        PillTone::Orange
    } else if e.checks_text == "체크 없음" {
        PillTone::Grey
    } else {
        PillTone::Green
    }
}

/// One pull request: number and title, meta line, the pills and the merge gate line. A tap on
/// the row selects it for the detail; the selected row is highlighted.
fn PullRequestRow(e: &PrEntry, selected: bool, on_select: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let fill = if selected { p.surface_2 } else { p.surface };
    let mut frame = Modifier::new().fill_max_width().background(paint(fill));
    if selected {
        frame = frame.border(2.0, paint(p.ink));
    }
    cu::Box().modifier(frame).content(move || {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
            Txt(&format!("#{} {}", e.number, e.title), TypeRole::BodyStrong, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            Txt(&pr_meta(e), TypeRole::Caption, Some(paint(p.text_2)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                Pill(e.state_text, pr_state_tone(e), true);
                if e.draft {
                    Pill("초안", PillTone::Grey, true);
                }
                Pill(e.review_text, pr_review_tone(e), true);
                Pill(&e.checks_text, pr_checks_tone(e), true);
            });
            let gate = if e.can_merge { paint(p.green.strong) } else { paint(p.text_2) };
            Label(&e.gate_text, Fs::Sm, Weight::Regular, Some(gate), None);
        });
        FocusOverlay(&format!("풀 리퀘스트 #{} {}", e.number, e.title), on_select);
    });
}

/// The sample detail of a sample pull request (captures and a screen without a server).
fn sample_pr_detail(number: u64) -> PrDetailLoad {
    let PrLoad::Ready(list) = sample_prs() else { return PrDetailLoad::Idle };
    let Some(e) = list.iter().find(|e| e.number == number) else {
        return PrDetailLoad::Failed("풀 리퀘스트를 찾을 수 없습니다".to_string());
    };
    use CheckClass::*;
    use ReviewerState as R;
    let (files, checks, reviewers, comments): (&str, Vec<(&str, CheckClass)>, Vec<(&str, R)>, Vec<(&str, &str)>) = match number {
        412 => (
            "파일 3 +40 -2",
            vec![("build", Passed), ("test", Passed), ("lint", Passed), ("clippy", Passed)],
            vec![("Minji Kim", R::Approved)],
            vec![("Minji Kim", "세션 shim 위치가 좋습니다. 병합해도 되겠습니다.")],
        ),
        409 => (
            "파일 2 +18 -6",
            vec![("build", Passed), ("test", Failed), ("lint", Passed), ("clippy", Passed)],
            vec![("BREW", R::Waiting)],
            vec![("Jisoo Park", "detach가 끝나기 전에 egress를 멈추면 어떻게 되나요?")],
        ),
        405 => (
            "파일 7 +210 -14",
            vec![("build", Passed), ("test", Pending), ("lint", Pending)],
            Vec::new(),
            Vec::new(),
        ),
        _ => (
            "파일 1 +22 -0",
            vec![("build", Passed), ("test", Passed), ("lint", Passed), ("clippy", Passed)],
            vec![("BREW", R::Approved)],
            Vec::new(),
        ),
    };
    PrDetailLoad::Ready(PrDetailView {
        number,
        title: e.title.clone(),
        state_text: e.state_text,
        body_lines: vec!["요약과 변경 이유는 설명에 적습니다.".to_string()],
        author: e.author.clone(),
        head: e.head.clone(),
        base: e.base.clone(),
        files_text: files.to_string(),
        checks: checks.into_iter().map(|(n, c)| (n.to_string(), c, c.label().to_string())).collect(),
        ci_pill: e.checks_text.clone(),
        checks_failed: e.checks_failed,
        checks_pending: e.checks_pending,
        review_pill: e.review_text,
        review: e.review,
        reviewers: reviewers.into_iter().map(|(n, s)| (n.to_string(), s)).collect(),
        comments: comments.into_iter().map(|(a, t)| (a.to_string(), t.to_string())).collect(),
        can_merge: e.can_merge,
        merge_reason: e.gate_text.clone(),
        open: e.state == ember_client::api::GitPrState::Open,
        draft: e.draft,
        head_sha: format!("{number:08x}").repeat(5),
    })
}

fn reviewer_tone(s: ReviewerState) -> PillTone {
    match s {
        ReviewerState::Approved => PillTone::Green,
        ReviewerState::ChangesRequested => PillTone::Red,
        ReviewerState::Waiting => PillTone::Orange,
        ReviewerState::Commented => PillTone::Grey,
    }
}

fn detail_review_tone(r: ember_client::api::GitPrReview) -> PillTone {
    use ember_client::api::GitPrReview::*;
    match r {
        Approved => PillTone::Green,
        ChangesRequested => PillTone::Red,
        ReviewRequired => PillTone::Orange,
        None => PillTone::Grey,
    }
}

fn detail_ci_tone(v: &PrDetailView) -> PillTone {
    if v.checks_failed > 0 {
        PillTone::Red
    } else if v.checks_pending > 0 {
        PillTone::Orange
    } else if v.ci_pill == "체크 없음" {
        PillTone::Grey
    } else {
        PillTone::Green
    }
}

/// The meta line of the detail: author, head to base, and the file counts.
fn pr_detail_meta(v: &PrDetailView) -> String {
    format!("{} \u{00b7} {} \u{2192} {} \u{00b7} {}", v.author, v.head, v.base, v.files_text)
}

/// A filled 8 dp dot in the check's tone.
fn StatusDot(class: CheckClass) {
    let p = ember_theme().palette;
    let color = match class {
        CheckClass::Passed => p.green.strong,
        CheckClass::Failed => p.red.strong,
        CheckClass::Pending => p.orange.strong,
    };
    cu::Box().modifier(Modifier::new().size(8.0, 8.0).background(paint(color)).shape_role(ShapeRole::Full));
}

/// What the detail card asks of the pane: the review box text, the busy flag, whether the merge
/// button is armed, and the two actions.
struct DetailControls<R: Fn(GitPrReviewAction) + Clone + 'static, M: Fn() + Clone + 'static> {
    review_text: MutableState<String>,
    review_gen: MutableState<u64>,
    /// The review action that is armed and asks again, if any.
    armed_review: Option<GitPrReviewAction>,
    busy: bool,
    merge_armed: bool,
    on_review: R,
    on_merge: M,
}

/// The selected pull request, the part that scrolls: title, meta line, description, 검사, 리뷰
/// and 리뷰 코멘트. The review box and 병합 are `PrDetailActions`, pinned below the scroll area.
fn PrDetailBody(v: &PrDetailView) {
    let p = ember_theme().palette;
    VcsCard(|| {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
            Txt(&format!("#{} {}", v.number, v.title), TypeRole::BodyStrong, Some(paint(p.text)), Some(3), Some(TextOverflow::Ellipsis), None, None);
            Txt(&pr_detail_meta(v), TypeRole::Caption, Some(paint(p.text_2)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            for line in &v.body_lines {
                Label(line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }

            InsetPanel(true, || {
                cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                        Label("검사", Fs::Md, Weight::Semibold, None, Some(1));
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        Pill(&v.ci_pill, detail_ci_tone(v), true);
                    });
                    if v.checks.is_empty() {
                        Label("검사가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    for (name, class, label) in &v.checks {
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                            StatusDot(*class);
                            Label(name, Fs::Sm, Weight::Regular, None, Some(1));
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            Label(label, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                        });
                    }
                });
            });

            InsetPanel(true, || {
                cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                        Label("리뷰", Fs::Md, Weight::Semibold, None, Some(1));
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        Pill(v.review_pill, detail_review_tone(v.review), true);
                    });
                    if v.reviewers.is_empty() {
                        Label("리뷰어 없음", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    for (name, state) in &v.reviewers {
                        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                            Label(name, Fs::Sm, Weight::Regular, None, Some(1));
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            Pill(state.label(), reviewer_tone(*state), true);
                        });
                    }
                });
            });

            Label("리뷰 코멘트", Fs::Md, Weight::Semibold, None, Some(1));
            if v.comments.is_empty() {
                Label("코멘트가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
            for (author, text) in &v.comments {
                cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label(author, Fs::Sm, Weight::Semibold, None, Some(1));
                    Label(text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                });
            }

        });
    });
}

/// The review box (the text, 코멘트, 변경 요청, 승인) and 병합 with its hint, for an open pull
/// request. At the wide layout it sits below the scroll area, so it is always reachable.
fn PrDetailActions<R: Fn(GitPrReviewAction) + Clone + 'static, M: Fn() + Clone + 'static>(v: &PrDetailView, ctl: DetailControls<R, M>) {
    let p = ember_theme().palette;
    let (merge_enabled, hint) = merge_button_state(v);
    VcsCard(|| {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
            {
                Label("리뷰 남기기", Fs::Md, Weight::Semibold, None, Some(1));
                let text = ctl.review_text.clone();
                key(ctl.review_gen.get(), || {
                    cu::TextField().modifier(Modifier::new().fill_max_width().height(64.0))
                        .text(text.get()).placeholder("코멘트나 변경 요청의 내용").multiline(true)
                        .on_value_change(move |value| text.set(value));
                });
                cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    for (action, kind) in [
                        (GitPrReviewAction::Comment, ButtonKind::Default),
                        (GitPrReviewAction::RequestChanges, ButtonKind::Destructive),
                        (GitPrReviewAction::Approve, ButtonKind::Default),
                    ] {
                        let on_review = ctl.on_review.clone();
                        let label = review_button_label(action, ctl.armed_review == Some(action));
                        AppButton(label, None, kind, true, !ctl.busy, false, move || on_review(action));
                    }
                });
                let on_merge = ctl.on_merge.clone();
                AppButton(merge_button_text(ctl.merge_armed), None, ButtonKind::Primary, false, merge_enabled && !ctl.busy, true, move || on_merge());
                match hint {
                    Some(reason) => Label(&reason, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None),
                    None => Label(MERGE_METHOD_TEXT, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None),
                }
            }
        });
    });
}

/// 풀 리퀘스트: the repository's pull requests with checks, review and the merge gate, and the
/// selected one in full (checks, reviewers, comments, review box, merge; FR-G10 to FR-G12,
/// #483). `actions` is the project and the screen's reload counter; `None` shows sample pull
/// requests. `count` is written for the section list (the open ones). `branch` is the current
/// branch, the head of a new pull request. The wide layout shows the list and the detail side by
/// side (the first one selected until a tap); the compact one shows one at a time, with 목록으로.
fn PullRequestsPane(actions: Option<Actions>, count: MutableState<Option<usize>>, shared: MutableState<Vec<PrEntry>>, layout: PaneLayout, branch: Option<String>) {
    let p = ember_theme().palette;
    let key_project = actions.as_ref().map(|a| a.project.clone());
    let state = remember_with_key(key_project.clone(), || mutable_state_of(PrLoad::Loading));
    let filter = remember(|| mutable_state_of(PrFilter::Open));
    let retry = remember(|| mutable_state_of(0_u64));
    // `EMBER_CR_VCS_PR=<n>` opens that pull request directly, for captures without clicks.
    let selected = remember_with_key(key_project.clone(), || mutable_state_of(std::env::var("EMBER_CR_VCS_PR").ok().and_then(|v| v.parse::<u64>().ok())));
    let detail = remember_with_key(key_project, || mutable_state_of(PrDetailLoad::Idle));
    let wanted = remember(|| mutable_state_of(None::<u64>));
    let busy = remember(|| mutable_state_of(false));
    // The merge button as (pull request number, arm time in ms); cleared by a reload or a timeout.
    let armed = remember(|| mutable_state_of(None::<(usize, i64)>));
    let form_open = remember(|| mutable_state_of(false));
    let f_title = remember(|| mutable_state_of(String::new()));
    let f_body = remember(|| mutable_state_of(String::new()));
    let f_draft = remember(|| mutable_state_of(false));
    let f_gen = remember(|| mutable_state_of(0_u64));
    if let Some(a) = actions.clone() {
        let (st, cnt, shown, shared) = (state.clone(), count.clone(), filter.get(), shared.clone());
        launched_effect((a.project.clone(), a.reload.get(), retry.get(), shown.arg()), move || async move {
            if crate::services::try_services().is_some() {
                st.set(PrLoad::Loading);
                let (name, now) = (a.project.clone(), crate::demo::now_ms() / 1000);
                crate::services::run_shared(async move { prs_from(fetch_prs(&name, shown).await, now) }, move |result| {
                    if shown == PrFilter::Open {
                        cnt.set(if let PrLoad::Ready(list) = &result { Some(list.len()) } else { None });
                        // 팀 작업 shows the open pull requests of the branches (no extra read).
                        shared.set(if let PrLoad::Ready(list) = &result { list.clone() } else { Vec::new() });
                    }
                    st.set(result);
                });
            } else {
                st.set(PrLoad::Failed("서버에 연결되어 있지 않아 풀 리퀘스트를 불러올 수 없습니다.".to_string()));
            }
        });
    }
    let shown = filter.get();
    let current = if actions.is_some() { state.get() } else { sample_prs() };
    let two = layout == PaneLayout::Two;
    let first = match &current {
        PrLoad::Ready(list) if two => list.first().map(|e| e.number),
        _ => None,
    };
    let effective = selected.get().or(first);
    // The draft of the review box belongs to one pull request: another one starts empty.
    let draft_key = review_draft_key(actions.as_ref().map(|a| a.project.as_str()).unwrap_or(""), effective);
    let review_text = remember_with_key(draft_key.clone(), || mutable_state_of(String::new()));
    let review_gen = remember_with_key(draft_key, || mutable_state_of(0_u64));
    // 승인 and 변경 요청 as (key of the action, arm time in ms); only one is armed, and 병합 clears it.
    let review_armed = remember(|| mutable_state_of(None::<(usize, i64)>));

    if let Some(a) = actions.clone() {
        let (det, disarm, disarm_review, wanted) = (detail.clone(), armed.clone(), review_armed.clone(), wanted.clone());
        launched_effect((a.project.clone(), effective, a.reload.get(), retry.get()), move || async move {
            disarm.set(None);
            disarm_review.set(None);
            wanted.set(effective);
            let Some(number) = effective else {
                det.set(PrDetailLoad::Idle);
                return;
            };
            if crate::services::try_services().is_some() {
                if !matches!(det.get(), PrDetailLoad::Ready(ref v) if v.number == number) {
                    det.set(PrDetailLoad::Loading);
                }
                let name = a.project.clone();
                crate::services::run_shared(async move { pr_detail_from(fetch_pr_detail(&name, number).await) }, move |result| {
                    if wanted.get() == Some(number) {
                        det.set(result);
                    }
                });
            } else {
                det.set(PrDetailLoad::Failed("서버에 연결되어 있지 않아 풀 리퀘스트를 불러올 수 없습니다.".to_string()));
            }
        });
    }
    let detail_now = if actions.is_some() {
        detail.get()
    } else {
        effective.map(sample_pr_detail).unwrap_or(PrDetailLoad::Idle)
    };
    let is_busy = busy.get();
    // The head the user sees; the merge carries it, and a view without one cannot merge.
    let head_sha = match &detail_now {
        PrDetailLoad::Ready(v) => v.head_sha.clone(),
        _ => String::new(),
    };
    // The review box and 병합 are pinned below the scroll area, for an open pull request only.
    let actions_view = match &detail_now {
        PrDetailLoad::Ready(v) if v.open => Some(v.clone()),
        _ => None,
    };

    // A review: busy before the request, then the notice, and the list and the detail again.
    let on_review = {
        let (actions, busy, retry, review_text, review_gen, armed, review_armed) =
            (actions.clone(), busy.clone(), retry.clone(), review_text.clone(), review_gen.clone(), armed.clone(), review_armed.clone());
        move |action: GitPrReviewAction| {
            let Some(a) = actions.clone() else { return };
            let Some(number) = effective else { return };
            let body = clean_body(&review_text.get());
            if let Err(line) = review_request_gate(action, &body) {
                notice::post(line.to_string());
                return;
            }
            // 승인 and 변경 요청 ask twice; any other tap disarms 병합 and the other review button.
            let now = crate::demo::now_ms();
            armed.set(None);
            match review_tap(action, review_armed.get(), number, now) {
                ReviewTap::Arm | ReviewTap::Expired => {
                    review_armed.set(Some((pr_action_key(number, action), now)));
                    return;
                }
                ReviewTap::Wait => return,
                ReviewTap::Send => review_armed.set(None),
            }
            busy.set(true);
            let (busy, retry, review_text, review_gen) = (busy.clone(), retry.clone(), review_text.clone(), review_gen.clone());
            crate::services::run_shared(async move { run_pr_review(&a.project, number, action, &body).await }, move |result| {
                match result {
                    Ok(line) => {
                        notice::post(line);
                        review_text.set(String::new());
                        review_gen.set(review_gen.get() + 1);
                        retry.set(retry.get() + 1);
                    }
                    Err(line) => notice::post(line),
                }
                busy.set(false);
            });
        }
    };
    // 병합 asks twice: the first tap arms it, the second counts after 600 ms and before 4 s.
    let on_merge = {
        let (actions, busy, retry, armed, head_sha, review_armed) = (actions.clone(), busy.clone(), retry.clone(), armed.clone(), head_sha.clone(), review_armed.clone());
        move || {
            let Some(a) = actions.clone() else { return };
            let Some(number) = effective else { return };
            let now = crate::demo::now_ms();
            review_armed.set(None);
            match delete_tap(armed.get(), number as usize, now) {
                DeleteTap::Arm | DeleteTap::Expired => armed.set(Some((number as usize, now))),
                DeleteTap::Wait => {}
                DeleteTap::Confirm => {
                    armed.set(None);
                    busy.set(true);
                    let (busy, retry, head) = (busy.clone(), retry.clone(), head_sha.clone());
                    crate::services::run_shared(async move { run_pr_merge(&a.project, number, GitPrMergeMethod::Squash, &head).await }, move |result| {
                        match result {
                            Ok(line) => {
                                notice::post(line);
                                retry.set(retry.get() + 1);
                            }
                            Err(line) => {
                                // The head moved since the detail was read: show the Korean line and read it again.
                                if is_stale_head(&line) {
                                    retry.set(retry.get() + 1);
                                }
                                notice::post(line);
                            }
                        }
                        busy.set(false);
                    });
                }
            }
        }
    };
    let armed_review = effective.and_then(|n| {
        [GitPrReviewAction::Approve, GitPrReviewAction::RequestChanges]
            .into_iter()
            .find(|&act| armed_label_visible(review_armed.get(), pr_action_key(n, act), crate::demo::now_ms()))
    });
    let merge_armed = effective.is_some_and(|n| armed_label_visible(armed.get(), n as usize, crate::demo::now_ms()));

    let create = {
        let (actions, busy, retry, form_open, f_title, f_body, f_draft, f_gen, branch) =
            (actions.clone(), busy.clone(), retry.clone(), form_open.clone(), f_title.clone(), f_body.clone(), f_draft.clone(), f_gen.clone(), branch.clone());
        move || {
            let Some(a) = actions.clone() else { return };
            let (title, body, draft) = (f_title.get(), clean_body(&f_body.get()), f_draft.get());
            if let Err(line) = pr_create_gate(&title) {
                notice::post(line.to_string());
                return;
            }
            busy.set(true);
            let head = branch.clone().unwrap_or_default();
            let (busy, retry, form_open, f_title, f_body, f_gen) = (busy.clone(), retry.clone(), form_open.clone(), f_title.clone(), f_body.clone(), f_gen.clone());
            crate::services::run_shared(async move { run_pr_create(&a.project, &title, &body, "", &head, draft).await }, move |result| {
                match result {
                    Ok(line) => {
                        notice::post(line);
                        f_title.set(String::new());
                        f_body.set(String::new());
                        f_gen.set(f_gen.get() + 1);
                        form_open.set(false);
                        retry.set(retry.get() + 1);
                    }
                    Err(line) => notice::post(line),
                }
                busy.set(false);
            });
        }
    };

    let chips = {
        let filter = filter.clone();
        move || {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(move || {
                for f in PrFilter::ALL {
                    let filter = filter.clone();
                    PickChip(f.label(), None, f == shown, false, move || filter.set(f));
                }
            });
        }
    };
    let open_count = if actions.is_some() { count.get() } else { Some(sample_pr_count()) };
    let new_form = {
        let (form_open, f_title, f_body, f_draft, f_gen, branch) = (form_open.clone(), f_title.clone(), f_body.clone(), f_draft.clone(), f_gen.clone(), branch.clone());
        move || {
            VcsCard(move || {
                cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(move || {
                    Label("새 풀 리퀘스트", Fs::Md, Weight::Semibold, None, Some(1));
                    let t = f_title.clone();
                    key((f_gen.get(), 0_u8), || {
                        cu::TextField().modifier(Modifier::new().fill_max_width().height(H_BTN))
                            .text(f_title.get()).placeholder("제목 (필수)")
                            .on_value_change(move |v| t.set(v));
                    });
                    let b = f_body.clone();
                    key((f_gen.get(), 1_u8), || {
                        cu::TextField().modifier(Modifier::new().fill_max_width().height(64.0))
                            .text(f_body.get()).placeholder("설명 (선택)").multiline(true)
                            .on_value_change(move |v| b.set(v));
                    });
                    let head_text = branch.clone().unwrap_or_else(|| "현재 브랜치".to_string());
                    Label(&format!("{head_text} \u{2192} 기본 브랜치"), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(2));
                    Label("원격에 올라가 있는 브랜치여야 합니다", Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                    cu::Row().modifier(Modifier::new().fill_max_width().height(32.0)).alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        let flag = f_draft.clone();
                        // TODO(compose-rust#428): Checkbox has no ink fill or tick style, so it looks hollow.
                        cu::Checkbox(f_draft.get())
                            .modifier(Modifier::new().size(18.0, 18.0))
                            .on_change(move |v| flag.set(v));
                        Label("초안", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                    });
                    cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                        AppButton("만들기", None, ButtonKind::Primary, true, !is_busy, false, create.clone());
                        let close = form_open.clone();
                        AppButton("취소", None, ButtonKind::Default, true, true, false, move || close.set(false));
                    });
                });
            });
        }
    };

    let list = {
        let (selected, retry, form_open) = (selected.clone(), retry.clone(), form_open.clone());
        move || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("풀 리퀘스트", Fs::Lg, Weight::Semibold, None, Some(1));
                    if let Some(n) = open_count {
                        Pill(&format!("열림 {n}"), PillTone::Green, true);
                    }
                    cu::Spacer().modifier(Modifier::new().weight(1.0));
                    let open = form_open.clone();
                    AppButton("새 PR", None, ButtonKind::Primary, true, true, false, move || open.set(!open.get()));
                });
                if form_open.get() {
                    new_form();
                }
                chips();
                match current {
                    PrLoad::Loading => {
                        Label("풀 리퀘스트를 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    PrLoad::Failed(text) => {
                        Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        let retry = retry.clone();
                        AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
                    }
                    PrLoad::Ready(entries) => {
                        if entries.is_empty() {
                            EmberCard(false, true, None, Some(SP[3]), None, || {
                                Label(shown.empty_text(), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                            });
                            return;
                        }
                        VcsCard(|| {
                            for (i, e) in entries.iter().enumerate() {
                                if i > 0 {
                                    cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                                }
                                let pick = selected.clone();
                                let number = e.number;
                                PullRequestRow(e, effective == Some(number), move || pick.set(Some(number)));
                            }
                        });
                    }
                }
            });
        }
    };

    let detail_view = {
        let (selected, retry) = (selected.clone(), retry.clone());
        move || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
                if !two {
                    let back = selected.clone();
                    AppButton("목록으로", None, ButtonKind::Default, true, true, false, move || back.set(None));
                }
                match detail_now {
                    PrDetailLoad::Idle => {
                        EmberCard(false, true, None, Some(SP[3]), None, || {
                            Label("풀 리퀘스트를 선택해 주세요", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        });
                    }
                    PrDetailLoad::Loading => {
                        Label("풀 리퀘스트를 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    PrDetailLoad::Failed(text) => {
                        Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
                    }
                    PrDetailLoad::Ready(v) => {
                        PrDetailBody(&v);
                    }
                }
            });
        }
    };

    let ctl = DetailControls { review_text, review_gen, armed_review, busy: is_busy, merge_armed, on_review, on_merge };
    if two {
        cu::Row()
            .modifier(Modifier::new().fill_max_width().fill_max_height())
            .alignment(Alignment::TopStart)
            .spacing(GAP_CARD)
            .content(move || {
                cu::ScrollColumn().modifier(Modifier::new().weight(1.0).fill_max_height()).content(list);
                // Bounded column: the detail scrolls, the review box and 병합 stay below it.
                cu::Column().spacing(SP[2]).modifier(Modifier::new().weight(1.4).fill_max_height()).content(move || {
                    cu::ScrollColumn().modifier(Modifier::new().fill_max_width().weight(1.0)).content(detail_view);
                    if let Some(v) = actions_view {
                        PrDetailActions(&v, ctl);
                    }
                });
            });
    } else if selected.get().is_some() {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
            detail_view();
            if let Some(v) = actions_view {
                PrDetailActions(&v, ctl);
            }
            cu::Spacer().modifier(Modifier::new().height(super::routines::compact_bottom_clearance()));
        });
    } else {
        list();
    }
}

/// Branches for the review captures (`EMBER_CR_ROUTE=vcs`) and for a screen without a server.
fn sample_branches() -> BranchLoad {
    let e = |name: &str, current: bool, default: bool, upstream: Option<&str>, ahead: u32, behind: u32, last: &str, worktree: Option<&str>| BranchEntry {
        name: name.to_string(),
        current,
        default,
        upstream_text: upstream.unwrap_or("업스트림 없음").to_string(),
        has_upstream: upstream.is_some(),
        ahead,
        behind,
        last_line: last.to_string(),
        worktree: worktree.map(String::from),
    };
    BranchLoad::Ready(vec![
        e("feat/git-api", true, false, Some("origin/feat/git-api"), 2, 1, "a1b2c3d \u{00b7} Feat: Git log API and history list \u{00b7} 12분 전", Some("/Users/demo/darkpyonix-ember")),
        e("main", false, true, Some("origin/main"), 0, 0, "7f6e5d4 \u{00b7} Merge pull request #398 \u{00b7} 1일 전", None),
        e("develop", false, false, Some("origin/develop"), 0, 3, "3c2b1a0 \u{00b7} Docs: describe the commit message rules \u{00b7} 5시간 전", None),
        e("feat/ui-phase-6", false, false, None, 0, 0, "9e8d7c6 \u{00b7} Feat: screen 6 layout \u{00b7} 1일 전", Some("/Users/demo/worktrees/ui-phase-6")),
    ])
}

/// Width of a full ahead or behind bar in dp.
const BRANCH_BAR_W: f32 = 56.0;

/// One thin bar: `fraction` of `BRANCH_BAR_W` painted over a track. A zero fraction paints only the track.
fn BranchBar(fraction: f32, fill: Paint) {
    let p = ember_theme().palette;
    cu::Box().modifier(Modifier::new().width(BRANCH_BAR_W).height(4.0).background(paint(p.line)).corner_radius(2.0)).content(move || {
        if fraction > 0.0 {
            cu::Box().modifier(Modifier::new().width(BRANCH_BAR_W * fraction).height(4.0).background(fill).corner_radius(2.0));
        }
    });
}

/// One branch: name with the 현재 pill, ahead/behind text and bars, the last commit line, the
/// worktree pill and the 전환 button (off for the current branch).
fn BranchRow(e: &BranchEntry, busy: bool, on_switch: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (a, b) = branch_bar_fractions(e.ahead, e.behind);
    let show_bars = e.has_upstream && (e.ahead > 0 || e.behind > 0);
    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(move || {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().weight(1.0)).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                Txt(&e.name, TypeRole::BodyStrong, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                if e.current {
                    Pill("현재", PillTone::Green, true);
                }
                if let Some(w) = &e.worktree {
                    Pill(&format!("워크트리 {}", worktree_tail(w)), PillTone::Sky, true);
                }
            });
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                Txt(&e.sync_text(), TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                if show_bars {
                    BranchBar(a, paint(p.green.strong));
                    BranchBar(b, paint(p.orange.strong));
                }
            });
            if e.has_upstream {
                Txt(&e.upstream_text, TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
            }
            if !e.last_line.is_empty() {
                Txt(&e.last_line, TypeRole::Caption, Some(paint(p.text_2)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            }
        });
        AppButton("전환", None, ButtonKind::Default, true, !e.current && !busy, false, on_switch);
    });
}

/// 브랜치: create a branch from a base and switch to it, then the 기본 브랜치 row and the 작업
/// 브랜치 rows (the node's order: current, default, newest). `actions` is the project and the
/// screen's reload counter; `None` shows sample branches. `count` is written for the section list.
fn BranchesPane(actions: Option<Actions>, count: MutableState<Option<usize>>, layout: PaneLayout) {
    let p = ember_theme().palette;
    let key_project = actions.as_ref().map(|a| a.project.clone());
    let state = remember_with_key(key_project, || mutable_state_of(BranchLoad::Loading));
    let retry = remember(|| mutable_state_of(0_u64));
    let name = remember(|| mutable_state_of(String::new()));
    let generation = remember(|| mutable_state_of(0_u64));
    let base = remember(|| mutable_state_of(None::<String>));
    let base_menu = remember(|| mutable_state_of(false));
    if let Some(a) = actions.clone() {
        let (st, cnt) = (state.clone(), count.clone());
        launched_effect((a.project.clone(), a.reload.get(), retry.get()), move || async move {
            if crate::services::try_services().is_some() {
                if !matches!(st.get(), BranchLoad::Ready(_)) {
                    st.set(BranchLoad::Loading);
                }
                let (project, now) = (a.project.clone(), crate::demo::now_ms() / 1000);
                crate::services::run_shared(async move { branches_from(fetch_branches(&project).await, now) }, move |result| {
                    cnt.set(if let BranchLoad::Ready(list) = &result { Some(list.len()) } else { None });
                    st.set(result);
                });
            } else {
                st.set(BranchLoad::Failed("서버에 연결되어 있지 않아 브랜치를 불러올 수 없습니다.".to_string()));
            }
        });
    }
    let current = if actions.is_some() { state.get() } else { sample_branches() };
    let names: Vec<String> = if let BranchLoad::Ready(list) = &current { list.iter().map(|b| b.name.clone()).collect() } else { Vec::new() };
    let base_text = match base.get() {
        Some(b) => format!("기준: {b}"),
        None => "기준: 현재 브랜치".to_string(),
    };
    let chosen_base = base.get().and_then(|b| names.iter().position(|n| *n == b));

    let creator = {
        let (actions, name, generation, base, base_menu) = (actions.clone(), name.clone(), generation.clone(), base.clone(), base_menu.clone());
        let (names, base_text) = (names.clone(), base_text.clone());
        move || {
            VcsCard(|| {
                cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
                    Label("새 브랜치", Fs::Md, Weight::Semibold, None, Some(1));
                    let value = name.clone();
                    key(generation.get(), || {
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            text: Some(name.get()),
                            placeholder: "새 브랜치 이름".to_string(),
                            on_value_change: Some(Box::new(move |v| value.set(v))),
                            ..Default::default()
                        });
                    });
                    let open = base_menu.clone();
                    AppButton(&base_text, None, ButtonKind::Default, false, !names.is_empty(), true, move || open.set(true));
                    SmallNote(if base.get().is_some() { "고른 브랜치에서 만듭니다" } else { "현재 브랜치에서 만듭니다" });
                    let (act, nm, gn, bs) = (actions.clone(), name.clone(), generation.clone(), base.clone());
                    AppButton("만들고 전환", None, ButtonKind::Primary, false, actions.is_some() && valid_branch_name(nm.get().trim()), true, move || {
                        if let (Some(a), Some(new_name)) = (act.as_ref(), branch_input(&nm.get())) {
                            let submitted = nm.get();
                            let (value, gn, bs) = (nm.clone(), gn.clone(), bs.clone());
                            a.create_from(new_name, bs.get(), move || {
                                value.set(text_after_success(&value.get(), &submitted));
                                gn.set(gn.get() + 1);
                                bs.set(None);
                            });
                        }
                    });
                });
            });
        }
    };

    let switch_actions = actions.clone();
    let retry_state = retry.clone();
    let status = current.clone();
    let list = move || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
            creator();
            match status {
                BranchLoad::Loading => {
                    Label("브랜치를 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                }
                BranchLoad::Failed(text) => {
                    Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry_state.set(retry_state.get() + 1));
                }
                BranchLoad::Ready(entries) => {
                    if entries.is_empty() {
                        EmberCard(false, true, None, Some(SP[3]), None, || {
                            Label("브랜치가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        });
                        return;
                    }
                    let (default, rest) = split_branches(&entries);
                    let row = |e: &BranchEntry| {
                        let (a, target) = (switch_actions.clone(), e.name.clone());
                        BranchRow(e, false, move || {
                            if let Some(a) = &a {
                                a.branch(target.clone(), false, || {});
                            }
                        });
                    };
                    if let Some(d) = default {
                        Label("기본 브랜치", Fs::Sm, Weight::Semibold, Some(paint(p.text_2)), Some(1));
                        VcsCard(|| row(d));
                    }
                    if !rest.is_empty() {
                        Label("작업 브랜치", Fs::Sm, Weight::Semibold, Some(paint(p.text_2)), Some(1));
                        VcsCard(|| {
                            for (i, e) in rest.iter().enumerate() {
                                if i > 0 {
                                    cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                                }
                                row(e);
                            }
                        });
                    }
                }
            }
        });
    };
    let (dismiss, pick_base) = (base_menu.clone(), base.clone());
    let sheet_menu = base_menu.clone();
    let content = move || {
        list();
        let (d, b, m) = (dismiss.clone(), pick_base.clone(), sheet_menu.clone());
        let picked = names.clone();
        OptionSheet(sheet_menu.get(), &names, chosen_base, 240.0, move || d.set(false), move |idx| {
            b.set(picked.get(idx).cloned());
            m.set(false);
        });
    };
    if layout == PaneLayout::Two {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(content);
    } else {
        content();
    }
}

/// The project's sessions with their folders, for matching to worktrees. The client state holds
/// each session's `cwd`; the computer a session runs on is not in the record, so it is not shown.
fn member_cwds(state: &ember_client::State, project: &str) -> Vec<MemberCwd> {
    project_boxes(state)
        .into_iter()
        .find(|b| b.id == project)
        .map(|b| {
            b.members
                .iter()
                .filter_map(|m| {
                    let cwd = state.session(&m.session_id)?.record.cwd.clone();
                    Some(MemberCwd { name: m.name.clone(), harness: m.agent.clone(), leader: m.leader, cwd, state_text: crate::ui::project::status_pill(m.status).0.to_string(), status: m.status })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Sessions for the review captures: one in the main checkout, one in the ui worktree.
fn sample_members() -> Vec<MemberCwd> {
    use ember_client::LauncherStatus as S;
    let m = |name: &str, harness: &str, leader: bool, cwd: &str, status: S| MemberCwd {
        name: name.to_string(),
        harness: harness.to_string(),
        leader,
        cwd: cwd.to_string(),
        state_text: crate::ui::project::status_pill(status).0.to_string(),
        status,
    };
    vec![
        m("ember-리더", "claude-code", true, "/Users/demo/darkpyonix-ember/ember/app", S::Running),
        m("ember-codex", "codex", false, "/Users/demo/worktrees/ui-phase-6", S::WaitingForApproval),
    ]
}

/// The sample rows of 팀 작업 (the sample branches, members and pull requests).
fn sample_team_rows() -> Vec<TeamRow> {
    let (BranchLoad::Ready(branches), PrLoad::Ready(prs)) = (sample_branches(), sample_prs()) else { return Vec::new() };
    team_rows(&branches, &sample_members(), &prs)
}

fn sample_team_len() -> usize {
    sample_team_rows().len()
}

/// One worktree: the member's character, name and state, the branch, the worktree folder, the
/// ahead/behind text and the branch's pull request.
fn TeamRowView(r: &TeamRow, project: &str) {
    let p = ember_theme().palette;
    cu::Row().alignment(Alignment::CenterStart).spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
        match &r.member {
            Some(m) => super::characters::Character(&m.harness, m.leader, 36.0, &m.name, project, super::characters::CharState::from_status(m.status), None),
            None => super::channels::Avatar("-", "", 36.0),
        }
        cu::Column().spacing(SP[1]).modifier(Modifier::new().weight(1.0)).content(|| {
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                match &r.member {
                    Some(m) => {
                        Txt(&m.name, TypeRole::BodyStrong, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                        if m.leader {
                            Pill("리더", PillTone::Orange, true);
                        }
                        Pill(&m.state_text, crate::ui::project::status_pill(m.status).1.into(), true);
                    }
                    None => Txt("담당 없음", TypeRole::BodyStrong, Some(paint(p.text_2)), Some(1), None, None, None),
                }
            });
            cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                Txt(&r.branch, TypeRole::Caption, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                if r.current {
                    Pill("현재", PillTone::Green, true);
                }
                if let Some(pr) = &r.pr_text {
                    Pill(pr, PillTone::Blue, true);
                }
            });
            Txt(&r.worktree, TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
            Txt(&r.sync_text, TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
        });
    });
}

/// 팀 작업: one row per worktree with its member, branch, ahead/behind and pull request. The
/// branches come from the branches API; the members from the project's sessions (a session belongs
/// to the worktree that holds its folder). `prs` are the pull requests the 풀 리퀘스트 section has
/// read (none until it has been opened). `None` actions show sample rows. `count` is written for
/// the section list.
fn TeamPane(actions: Option<Actions>, count: MutableState<Option<usize>>, prs: MutableState<Vec<PrEntry>>, layout: PaneLayout) {
    let p = ember_theme().palette;
    let key_project = actions.as_ref().map(|a| a.project.clone());
    let state = remember_with_key(key_project, || mutable_state_of(BranchLoad::Loading));
    let retry = remember(|| mutable_state_of(0_u64));
    if let Some(a) = actions.clone() {
        let (st, cnt) = (state.clone(), count.clone());
        launched_effect((a.project.clone(), a.reload.get(), retry.get()), move || async move {
            if crate::services::try_services().is_some() {
                if !matches!(st.get(), BranchLoad::Ready(_)) {
                    st.set(BranchLoad::Loading);
                }
                let (project, now) = (a.project.clone(), crate::demo::now_ms() / 1000);
                crate::services::run_shared(async move { branches_from(fetch_branches(&project).await, now) }, move |result| {
                    cnt.set(if let BranchLoad::Ready(list) = &result { Some(list.iter().filter(|b| b.worktree.is_some()).count()) } else { None });
                    st.set(result);
                });
            } else {
                st.set(BranchLoad::Failed("서버에 연결되어 있지 않아 팀 작업을 불러올 수 없습니다.".to_string()));
            }
        });
    }
    let project = actions.as_ref().map(|a| a.project.clone()).unwrap_or_default();
    let status = if actions.is_some() { state.get() } else { sample_branches() };
    let members = match &actions {
        // Demo sessions run in temporary folders that no demo worktree path can match, so demo
        // mode uses the sample members (the same two as the sample route).
        Some(_) if crate::services::try_services().is_some_and(|s| s.demo.is_some()) => sample_members(),
        Some(a) => crate::services::try_services().map(|s| s.client.read(|st| member_cwds(st, &a.project))).unwrap_or_default(),
        None => sample_members(),
    };
    let prs = if actions.is_some() { prs.get() } else if let PrLoad::Ready(l) = sample_prs() { l } else { Vec::new() };
    let retry_state = retry.clone();
    let list = move || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || match status {
            BranchLoad::Loading => {
                Label("팀 작업을 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
            BranchLoad::Failed(text) => {
                Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry_state.set(retry_state.get() + 1));
            }
            BranchLoad::Ready(entries) => {
                let rows = team_rows(&entries, &members, &prs);
                if rows.is_empty() {
                    EmberCard(false, true, None, Some(SP[3]), None, || {
                        Label("작업 중인 워크트리가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    });
                    return;
                }
                VcsCard(|| {
                    for (i, r) in rows.iter().enumerate() {
                        if i > 0 {
                            cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                        }
                        TeamRowView(r, &project);
                    }
                });
            }
        });
    };
    if layout == PaneLayout::Two {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(list);
    } else {
        list();
    }
}

/// Stashes for the review captures (`EMBER_CR_ROUTE=vcs`) and for a screen without a server.
fn sample_stashes() -> StashLoad {
    let e = |index: u32, message: &str, branch: &str, time: &str| StashEntry {
        index,
        label: format!("stash@{{{index}}}"),
        message: message.to_string(),
        branch: branch.to_string(),
        time_text: time.to_string(),
    };
    StashLoad::Ready(vec![
        e(0, "세션 shim 작업 중", "feat/session-shim", "40분 전"),
        e(1, "릴리스 문서 초안", "main", "2일 전"),
    ])
}

/// The meta line of a stash row: label, branch and time.
fn stash_meta(e: &StashEntry) -> String {
    format!("{} \u{00b7} {} \u{00b7} {}", e.label, e.branch, e.time_text)
}

/// The label of a stash's delete button: the first tap asks, the second deletes.
fn drop_label(confirming: bool) -> &'static str {
    if confirming { "정말 삭제합니다" } else { "삭제" }
}

/// 스태시: make a stash, and apply, pop or drop the saved ones. `actions` is the project and the
/// screen's reload counter (an action bumps it so 변경 사항 reads the work tree again); `None`
/// shows sample stashes. `count` is written for the section list.
///
/// `dirty` is whether the work tree has changes (the snapshot's `changes` is not empty; no snapshot
/// counts as clean): 적용 and 꺼내기 then ask once before acting (#350).
fn StashPane(actions: Option<Actions>, count: MutableState<Option<usize>>, layout: PaneLayout, dirty: bool) {
    let p = ember_theme().palette;
    let key_project = actions.as_ref().map(|a| a.project.clone());
    let state = remember_with_key(key_project, || mutable_state_of(StashLoad::Loading));
    let local = remember(|| mutable_state_of(0_u64));
    let busy = remember(|| mutable_state_of(false));
    // The armed button as (key from `stash_tap_key`, arm time in ms); cleared by a timeout, another tap or a reload.
    let armed = remember(|| mutable_state_of(None::<(usize, i64)>));
    let message = remember(|| mutable_state_of(String::new()));
    let untracked = remember(|| mutable_state_of(false));
    let generation = remember(|| mutable_state_of(0_u64));

    if let Some(a) = actions.clone() {
        let (st, cnt, disarm) = (state.clone(), count.clone(), armed.clone());
        launched_effect((a.project.clone(), a.reload.get(), local.get()), move || async move {
            disarm.set(None);
            if crate::services::try_services().is_some() {
                if !matches!(st.get(), StashLoad::Ready(_)) {
                    st.set(StashLoad::Loading);
                }
                let (name, now) = (a.project.clone(), crate::demo::now_ms() / 1000);
                crate::services::run_shared(async move { stash_from(fetch_stashes(&name).await, now) }, move |result| {
                    cnt.set(if let StashLoad::Ready(list) = &result { Some(list.len()) } else { None });
                    st.set(result);
                });
            } else {
                st.set(StashLoad::Failed("서버에 연결되어 있지 않아 스태시를 불러올 수 없습니다.".to_string()));
            }
        });
    }
    let current = if actions.is_some() { state.get() } else { sample_stashes() };
    let is_busy = busy.get();

    // One finished action: say what happened, read the list again after a success.
    let run = {
        let (actions, busy, armed, local) = (actions.clone(), busy.clone(), armed.clone(), local.clone());
        move |action: StashAction, index: u32| {
            let Some(a) = actions.clone() else { return };
            busy.set(true);
            let (name, busy, armed, local, reload) = (a.project.clone(), busy.clone(), armed.clone(), local.clone(), a.reload.clone());
            crate::services::run_shared(async move { run_stash_action(&name, action, index).await }, move |result| {
                notice::post(stash_action_notice(action, &result));
                if result.is_ok() {
                    local.set(local.get() + 1);
                    reload.set(reload.get() + 1);
                }
                armed.set(None);
                busy.set(false);
            });
        }
    };
    let make = {
        let (actions, busy, local, message, untracked, generation) =
            (actions.clone(), busy.clone(), local.clone(), message.clone(), untracked.clone(), generation.clone());
        move || {
            let Some(a) = actions.clone() else { return };
            busy.set(true);
            let (name, text, with_untracked) = (a.project.clone(), message.get(), untracked.get());
            let (busy, local, message, generation, reload) = (busy.clone(), local.clone(), message.clone(), generation.clone(), a.reload.clone());
            crate::services::run_shared(async move { run_stash_push(&name, &text, with_untracked).await }, move |result| {
                match &result {
                    Ok(()) => {
                        notice::post("스태시를 만들었습니다".to_string());
                        message.set(String::new());
                        generation.set(generation.get() + 1);
                        local.set(local.get() + 1);
                        reload.set(reload.get() + 1);
                    }
                    Err(line) => notice::post(line.clone()),
                }
                busy.set(false);
            });
        }
    };

    let creator = {
        let (message, untracked, generation) = (message.clone(), untracked.clone(), generation.clone());
        move || {
            VcsCard(|| {
                cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
                    Label("스태시 만들기", Fs::Md, Weight::Semibold, None, Some(1));
                    let value = message.clone();
                    key(generation.get(), || {
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            text: Some(message.get()),
                            placeholder: "메시지 (선택)".to_string(),
                            on_value_change: Some(Box::new(move |v| value.set(v))),
                            ..Default::default()
                        });
                    });
                    cu::Row().modifier(Modifier::new().fill_max_width().height(32.0)).alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
                        let flag = untracked.clone();
                        // TODO(compose-rust#428): Checkbox has no ink fill or tick style, so it looks hollow.
                        cu::Checkbox(untracked.get())
                            .modifier(Modifier::new().size(18.0, 18.0))
                            .on_change(move |v| flag.set(v));
                        Label("추적하지 않는 파일 포함", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                    });
                    AppButton("스태시 만들기", None, ButtonKind::Primary, false, !is_busy, true, make);
                });
            });
        }
    };

    let (retry, status) = (local.clone(), current.clone());
    let list = move || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(move || {
            creator();
            match status {
                StashLoad::Loading => {
                    Label("스태시를 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                }
                StashLoad::Failed(text) => {
                    Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
                }
                StashLoad::Ready(entries) => {
                    if entries.is_empty() {
                        EmberCard(false, true, None, Some(SP[3]), None, || {
                            Label("스태시가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        });
                        return;
                    }
                    VcsCard(|| {
                        for (i, e) in entries.iter().enumerate() {
                            if i > 0 {
                                cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                            }
                            let (run, armed) = (run.clone(), armed.clone());
                            StashRow(e, layout, is_busy, dirty, armed, run);
                        }
                    });
                }
            }
        });
    };
    if layout == PaneLayout::Two {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(list);
    } else {
        list();
    }
}

/// One stash: message, meta line and the 적용, 꺼내기, 삭제 buttons. 삭제 needs a second tap on
/// the same button, which then reads "정말 삭제합니다"; 적용 and 꺼내기 ask the same way when
/// `dirty`, under a line that says why. The armed label shows only while `armed_label_visible`
/// holds at composition: nothing re-renders on a timer, so it may stay on screen until the next
/// recomposition after 4 seconds, and `stash_tap` decides what a tap does. Each button has an
/// accessible name with the index and message (visible text stays 적용, 꺼내기, 삭제).
fn StashRow(e: &StashEntry, _layout: PaneLayout, busy: bool, dirty: bool, armed: MutableState<Option<(usize, i64)>>, run: impl Fn(StashAction, u32) + Clone + 'static) {
    let p = ember_theme().palette;
    let index = e.index;
    let now = crate::demo::now_ms();
    let shows = |action: StashAction| armed_label_visible(armed.get(), stash_tap_key(action, index), now);
    let asking = dirty && (shows(StashAction::Apply) || shows(StashAction::Pop));
    let tap = {
        let (armed, run) = (armed.clone(), run.clone());
        move |action: StashAction| {
            let now = crate::demo::now_ms();
            match stash_tap(action, dirty, armed.get(), index, now) {
                DeleteTap::Arm | DeleteTap::Expired => armed.set(Some((stash_tap_key(action, index), now))),
                DeleteTap::Wait => {}
                DeleteTap::Confirm => {
                    armed.set(None);
                    run(action, index);
                }
            }
        }
    };
    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width().padding(SP[2])).content(|| {
        Txt(&e.message, TypeRole::BodyStrong, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
        Txt(&stash_meta(e), TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
        if asking {
            Label(STASH_DIRTY_WARNING, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            if shows(StashAction::Pop) {
                Label(STASH_POP_NOTE, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
        }
        cu::Row().alignment(Alignment::CenterStart).spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            for (action, verb, kind, ask) in [
                (StashAction::Apply, "적용", ButtonKind::Default, dirty),
                (StashAction::Pop, "꺼내기", ButtonKind::Default, dirty),
                (StashAction::Drop, "삭제", ButtonKind::Destructive, true),
            ] {
                let armed_now = ask && shows(action);
                let on_tap = tap.clone();
                StashPill(stash_button_text(action, armed_now), &stash_button_label(index, &e.message, verb), kind, !busy, move || on_tap(action));
            }
        });
    });
}

// TODO(compose-rust#425, #429): the pill is painted and an invisible `Button` over it carries the
// accessible name, because `Button` takes only its visible text as the name.
// TODO(compose-rust#427): a Box in a Row takes the remaining width, so the width is estimated and fixed.
/// A small pill whose visible text is `text` and whose accessible name is `access`.
fn StashPill(text: &str, access: &str, kind: ButtonKind, enabled: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let s = super::components::button_style_for(kind, true, enabled, p);
    let width = (super::home::text_px(text, s.fs.size()) + 28.0).max(64.0);
    cu::Box()
        .modifier(
            Modifier::new()
                .width(width)
                .height(s.height)
                .background(paint(s.fill))
                .border(1.0, paint(s.border))
                .shape_role(ShapeRole::Full),
        )
        .alignment(Alignment::Center)
        .content(move || {
            Label(text, s.fs, Weight::Medium, Some(paint(s.text)), Some(1));
            cu::Button(access)
                .enabled(enabled)
                .variant(ButtonVariant::Text)
                .color(Paint::Literal(Color::argb(0)))
                .modifier(Modifier::new().fill_max_size())
                .on_click(on_click);
        });
}

/// The screen wired as 버전 관리.
pub fn VcsScreen() {
    let is_sample_route = std::env::var("EMBER_CR_ROUTE").as_deref() == Ok("vcs");
    let boxes = if is_sample_route {
        sample_projects()
    } else if let Some(s) = crate::services::try_services() {
        let _ = s.ui.live.launcher.get();
        s.client.read(project_boxes)
    } else {
        Vec::new()
    };
    let selected = if is_sample_route {
        Some("ember".to_string())
    } else if let Some(s) = crate::services::try_services() {
        s.ui.selected_project.get()
    } else {
        None
    };
    let current = pick_project(selected.as_deref(), &boxes);
    if is_sample_route {
        let reload = remember(|| mutable_state_of(0_u64));
        VcsScreenWithLoad(Load::Ready(sample_snapshot()), boxes, current, reload, true);
        return;
    }
    let load = remember_with_key(current.clone(), || mutable_state_of(Load::Loading));
    let reload = remember_with_key(current.clone(), || mutable_state_of(0_u64));
    let key = (current.clone(), reload.get());
    let load_effect = load.clone();
    let project_effect = current.clone();
    launched_effect(key, move || async move {
        if let (Some(project), Some(_)) = (project_effect, crate::services::try_services()) {
            load_effect.set(Load::Loading);
            crate::services::run_shared(async move { load_from(fetch_snapshot(&project).await) }, move |result| load_effect.set(result));
        }
    });
    let state = load.get();
    VcsScreenWithLoad(state, boxes, current, reload, false);
}

/// VCS screen with explicit snapshot and projects.
pub fn VcsScreenWith(
    snap: Option<RepoSnapshot>,
    boxes: Vec<ProjectBox>,
    current_id: Option<String>,
) {
    let state = snap.map(Load::Ready).unwrap_or_else(|| Load::Unavailable(String::new()));
    let reload = remember(|| mutable_state_of(0_u64));
    VcsScreenWithLoad(state, boxes, current_id, reload, false);
}

fn VcsScreenWithLoad(
    load: Load,
    boxes: Vec<ProjectBox>,
    current_id: Option<String>,
    reload: MutableState<u64>,
    sample: bool,
) {
    let snap = load.snapshot().cloned();
    let p = ember_theme().palette;
    let window = current_window_size();
    let class = window.class;
    let layout = pane_layout(class);
    let wide = section_nav(class) == SectionNav::List;
    let compact = class == WindowSizeClass::Compact;
    let pad = if compact { SP[2] } else { 32.0 };

    // `EMBER_CR_VCS_SECTION=<key>` opens a section directly, for captures without clicks.
    let section = remember(|| mutable_state_of(std::env::var("EMBER_CR_VCS_SECTION").map(|k| route_section(&k)).unwrap_or(Section::Changes)));
    let tab = remember(|| mutable_state_of(PaneTab::Files));
    let selected_proj = remember(|| mutable_state_of(current_id));
    let stash_count = remember(|| mutable_state_of(None::<usize>));
    let pr_count = remember(|| mutable_state_of(None::<usize>));
    let branch_count = remember(|| mutable_state_of(None::<usize>));
    let team_count = remember(|| mutable_state_of(None::<usize>));
    // Keyed by the project: the PR pills of another project's branches must not carry over.
    let pr_list = remember_with_key(selected_proj.get(), || mutable_state_of(Vec::<PrEntry>::new()));

    let current = selected_proj.get();
    let project = current.as_ref().and_then(|id| boxes.iter().find(|b| &b.id == id).cloned());
    let projects: Vec<(String, String)> = repo_projects(&boxes)
        .into_iter()
        .map(|b| (b.id.clone(), b.name.clone()))
        .collect();

    let sec = section.get();
    let active_tab = effective_tab(sec, tab.get());

    let body = || {
        cu::Column().modifier(Modifier::new().fill_max_width()).spacing(4.0).content(|| {
            Label("버전 관리", Fs::Xl3, Weight::Semibold, None, None);
            if let Some(ref proj) = project {
                let subtitle = screen_subtitle(proj, snap.as_ref(), sample);
                Label(&subtitle, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
        });

        if let Some(project) = project {
                    let sel_proj = selected_proj.clone();
                    let actions = crate::services::try_services().map(|_| Actions { project: project.id.clone(), reload: reload.clone() });
                    RepoBarWithActions(
                        &projects,
                        &project,
                        snap.as_ref(),
                        compact,
                        move |id: String| {
                            sel_proj.set(Some(id.clone()));
                            if let Some(s) = crate::services::try_services() {
                                s.ui.set_selected_project(Some(id));
                            }
                        },
                        actions.clone(),
                    );

                    if let Some(snapshot) = load.snapshot() {
                        for n in &snapshot.notes {
                            Label(n, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        }
                    }
                    if let Some(note) = load.note() {
                        Label(&note, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        if matches!(load, Load::Failed(_)) {
                            let retry = reload.clone();
                            AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
                        }
                    }

                    if wide {
                        cu::Row().modifier(Modifier::new().fill_max_width().weight(1.0)).spacing(SP[3]).content(|| {
                            let sec_state = section.clone();
                            cu::ScrollColumn().modifier(Modifier::new().width(200.0).fill_max_height()).content(|| {
                                SectionListWithSample(sec, snap.as_ref(), sample, stash_count.get(), pr_count.get(), branch_count.get(), team_count.get(), move |s| sec_state.set(s));
                            });
                            cu::Column().modifier(Modifier::new().weight(1.0).fill_max_height()).content(|| {
                                SectionContent(snap.as_ref(), sec, layout, active_tab, &tab, actions.clone(), stash_count.clone(), pr_count.clone(), branch_count.clone(), team_count.clone(), pr_list.clone());
                            });
                        });
                    } else {
                        cu::Column().modifier(Modifier::new().fill_max_width()).spacing(SP[2]).content(|| {
                            let sec_state = section.clone();
                            SectionPills(sec, move |s| sec_state.set(s));
                            if layout == PaneLayout::Tabs && snap.is_none() && !pane_tabs(sec).is_empty() {
                                let tab_state = tab.clone();
                                PaneTabs(sec, active_tab, move |t| tab_state.set(t));
                            }
                            SectionContent(snap.as_ref(), sec, layout, active_tab, &tab, actions.clone(), stash_count.clone(), pr_count.clone(), branch_count.clone(), team_count.clone(), pr_list.clone());
                        });
                    }
                } else {
                    Label("저장소를 둘 수 있는 프로젝트가 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                    Label(SOON, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                }
    };
    if wide {
        // Wide: a bounded page, so the section list and the panes can fill its height.
        cu::Column().modifier(Modifier::new().fill_max_width().fill_max_height().padding(pad)).spacing(SP[3]).content(body);
    } else {
        cu::Box().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
            cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
                cu::Column().modifier(Modifier::new().fill_max_width().padding(pad)).spacing(SP[3]).content(body);
            });
        });
    }
}

/// A review window (`EMBER_CR_ROUTE=vcs`): sample repository view in compose-rust.
#[compose_rust::runtime::composable]
pub fn preview() {
    let p = ember_theme().palette;
    cu::Box().modifier(Modifier::new().fill_max_size().background(paint(p.bg))).content(|| {
        let reload = remember(|| mutable_state_of(0_u64));
        VcsScreenWithLoad(Load::Ready(sample_snapshot()), sample_projects(), Some("ember".into()), reload, true);
    });
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fr_l19_success_clears_only_submitted_text() {
        assert_eq!(text_after_success("요약", "요약"), "");
        assert_eq!(text_after_success("새 요약", "요약"), "새 요약");
    }

    #[test]
    fn fr_l19_subtitle_uses_snapshot_and_project() {
        let project = ProjectBox { name: "프로젝트".into(), repo: None, ..sample_project_box() };
        let snap = RepoSnapshot { branch: "main".into(), ahead: 3, behind: 2, ..Default::default() };
        assert_eq!(screen_subtitle(&project, Some(&snap), false), "프로젝트 · main · 앞섬 3 · 뒤처짐 2 · 변경 0");
        assert_eq!(screen_subtitle(&project, None, false), "프로젝트");
        assert!(screen_subtitle(&sample_project_box(), Some(&sample_snapshot()), true).contains(&format!("PR {}", sample_pr_count())));
        assert!(!screen_subtitle(&project, Some(&snap), false).contains("PR "));
    }

    #[test]
    fn fr_l19_repo_card_uses_loaded_snapshot() {
        let project = ProjectBox { repo: None, ..sample_project_box() };
        let snap = RepoSnapshot { branch: "main".into(), ..Default::default() };
        assert_eq!(crate::ui::vcs::repo_card_line(&project, Some(&snap)), "git 저장소 · main");
        assert_eq!(crate::ui::vcs::repo_card_line(&project, None), project.repo_line());
    }

    #[test]
    fn fr_l19_commit_request_preserves_description_and_staging() {
        let snap = sample_snapshot();
        let mut staging = Staging::new(&snap.changes, false);
        staging.toggle_file(&snap.changes[0]);
        let req = commit_request(&snap, &staging, "요약", "설명", false);
        assert_eq!(req.summary, "요약");
        assert_eq!(req.description, "설명");
        assert!(!req.ops.is_empty());
    }

    #[test]
    fn fr_l19_branch_input_requires_valid_name() {
        assert_eq!(branch_input("  feat/new  "), Some("feat/new".to_string()));
        assert_eq!(branch_input("bad name"), None);
    }

    #[test]
    fn fr_l19_load_notes_distinguish_failure_and_missing_repository() {
        assert!(crate::ui::vcs::Load::NotRepo.note().unwrap().contains("저장소가 아닙니다"));
        assert_eq!(crate::ui::vcs::Load::Failed("오류입니다".into()).note().as_deref(), Some("오류입니다"));
    }

    use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
    use compose_rust::{
        EventPayload, Host, PropertyKind, Recomposer, WidgetKind, WindowHeightClass,
        WindowSizeClass,
    };

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn a_diff() -> FileDiff {
        parse_unified(SAMPLE_DIFF).remove(0)
    }

    fn boxed(id: &str) -> ProjectBox {
        ProjectBox {
            id: id.into(),
            name: id.into(),
            members: Vec::new(),
            running: 0,
            waiting: 0,
            computers: Vec::new(),
            top: None,
            repo: None,
            branch: None,
        }
    }

    #[test]
    fn fr_l19_sections_are_in_the_design_order_with_stable_keys() {
        let labels: Vec<_> = Section::ALL.iter().map(|x| x.label()).collect();
        assert_eq!(
            labels,
            [
                "변경 사항",
                "히스토리",
                "브랜치",
                "풀 리퀘스트",
                "스태시",
                "충돌",
                "팀 작업"
            ]
        );
        for sec in Section::ALL {
            assert_eq!(Section::from_key(sec.key()), Some(sec));
        }
    }

    #[test]
    fn fr_l19_routing_an_unknown_key_opens_changes() {
        assert_eq!(route_section("prs"), Section::PullRequests);
        assert_eq!(route_section("conflict"), Section::Conflicts);
        assert_eq!(route_section("nope"), Section::Changes);
        assert_eq!(route_section(""), Section::Changes);
    }

    #[test]
    fn fr_l19_layout_follows_the_size_class() {
        assert_eq!(section_nav(WindowSizeClass::Expanded), SectionNav::List);
        assert_eq!(section_nav(WindowSizeClass::Medium), SectionNav::Pills);
        assert_eq!(section_nav(WindowSizeClass::Compact), SectionNav::Pills);
        assert_eq!(pane_layout(WindowSizeClass::Expanded), PaneLayout::Two);
        assert_eq!(pane_layout(WindowSizeClass::Medium), PaneLayout::Stacked);
        assert_eq!(pane_layout(WindowSizeClass::Compact), PaneLayout::Tabs);
    }

    #[test]
    fn fr_l19_compact_tabs_per_section() {
        assert_eq!(
            pane_tabs(Section::Changes),
            [PaneTab::Files, PaneTab::Diff, PaneTab::Commit]
        );
        assert_eq!(
            pane_tabs(Section::History),
            [PaneTab::Graph, PaneTab::Detail]
        );
        assert_eq!(
            pane_tabs(Section::PullRequests),
            [PaneTab::List, PaneTab::Detail]
        );
        assert!(pane_tabs(Section::Stash).is_empty());
        assert!(pane_tabs(Section::Team).is_empty());
    }

    #[test]
    fn fr_l19_picking_an_item_moves_to_the_next_tab() {
        assert_eq!(tab_after_select(PaneTab::Files), PaneTab::Diff);
        assert_eq!(tab_after_select(PaneTab::Graph), PaneTab::Detail);
        assert_eq!(tab_after_select(PaneTab::List), PaneTab::Detail);
        assert_eq!(tab_after_select(PaneTab::Commit), PaneTab::Commit);
    }

    #[test]
    fn fr_l19_a_stale_tab_falls_back_to_the_first() {
        assert_eq!(
            effective_tab(Section::Changes, PaneTab::Diff),
            Some(PaneTab::Diff)
        );
        assert_eq!(
            effective_tab(Section::History, PaneTab::Diff),
            Some(PaneTab::Graph)
        );
        assert_eq!(effective_tab(Section::Stash, PaneTab::Diff), None);
    }

    #[test]
    fn fr_l19_the_remote_button_follows_the_state() {
        assert_eq!(remote_action(0, 0), RemoteAction::Fetch);
        assert_eq!(remote_action(2, 0), RemoteAction::Push(2));
        assert_eq!(remote_action(0, 3), RemoteAction::Pull(3));
        assert_eq!(remote_action(2, 3), RemoteAction::Pull(3));
        assert_eq!(RemoteAction::Pull(3).label(), "Pull 3");
        assert_eq!(RemoteAction::Push(2).label(), "Push 2");
        assert_eq!(RemoteAction::Fetch.label(), "Fetch");
    }

    #[test]
    fn fr_l19_ahead_behind_is_formatted_in_korean() {
        assert_eq!(ahead_behind_text(0, 0), "원격과 같습니다");
        assert_eq!(ahead_behind_text(2, 0), "앞섬 2");
        assert_eq!(ahead_behind_text(0, 1), "뒤처짐 1");
        assert_eq!(ahead_behind_text(2, 1), "앞섬 2 \u{00b7} 뒤처짐 1");
    }

    #[test]
    fn fr_l19_the_last_fetch_is_relative() {
        assert_eq!(fetched_text(None), "아직 fetch하지 않았습니다");
        assert_eq!(fetched_text(Some(5)), "방금 fetch했습니다");
        assert_eq!(fetched_text(Some(60)), "1분 전에 fetch했습니다");
        assert_eq!(fetched_text(Some(7_300)), "2시간 전에 fetch했습니다");
        assert_eq!(fetched_text(Some(200_000)), "2일 전에 fetch했습니다");
    }

    #[test]
    fn fr_l19_the_branch_bar_splits_behind_and_ahead() {
        assert_eq!(bar_fractions(0, 0), (0.0, 0.0));
        assert_eq!(bar_fractions(1, 3), (0.75, 0.25));
        assert_eq!(bar_fractions(4, 0), (0.0, 1.0));
        assert_eq!(upstream_text(Some("origin/develop")), "origin/develop 추적");
        assert_eq!(upstream_text(None), "추적하는 원격 브랜치가 없습니다");
    }

    #[test]
    fn fr_l19_merge_needs_passing_ci_and_an_approval() {
        assert_eq!(merge_gate(CiState::Passed, ReviewState::Approved), Ok(()));
        assert_eq!(
            merge_gate(CiState::Failed, ReviewState::Approved),
            Err("CI가 실패했습니다")
        );
        assert_eq!(
            merge_gate(CiState::Running, ReviewState::Approved),
            Err("CI가 실행 중입니다")
        );
        assert_eq!(
            merge_gate(CiState::Waiting, ReviewState::Approved),
            Err("CI를 기다리는 중입니다")
        );
        assert_eq!(
            merge_gate(CiState::Passed, ReviewState::ChangesRequested),
            Err("변경 요청이 있습니다")
        );
        assert_eq!(
            merge_gate(CiState::Passed, ReviewState::Pending),
            Err("승인이 필요합니다")
        );
        assert_eq!(
            merge_gate(CiState::Passed, ReviewState::NoReviewer),
            Err("승인이 필요합니다")
        );
    }

    #[test]
    fn fr_l19_hunk_headers_parse_with_and_without_lengths() {
        assert_eq!(
            parse_hunk_header("@@ -1,4 +1,5 @@ fn main"),
            Some((1, 4, 1, 5))
        );
        assert_eq!(parse_hunk_header("@@ -20 +21,2 @@"), Some((20, 1, 21, 2)));
        assert_eq!(parse_hunk_header("@@ -0,0 +1,2 @@"), Some((0, 0, 1, 2)));
        assert_eq!(parse_hunk_header("not a header"), None);
        assert_eq!(parse_hunk_header("@@ -x +1 @@"), None);
    }

    #[test]
    fn fr_l19_sample_diff_becomes_files_hunks_and_lines() {
        let files = parse_unified(SAMPLE_DIFF);
        assert_eq!(files.len(), 5);

        let a = &files[0];
        assert_eq!(a.path, "ember/proxy/src/shim.rs");
        assert_eq!(a.status, FileStatus::Modified);
        assert_eq!(a.hunks.len(), 2);
        assert_eq!((a.added(), a.removed()), (6, 3));

        let attach = &files[2];
        assert_eq!(attach.path, "ember/proxy/tests/shim_attach.rs");
        assert_eq!(attach.status, FileStatus::Added);
        assert_eq!((attach.added(), attach.removed()), (9, 0));

        let bridge = &files[4];
        assert_eq!(bridge.path, "ember/proxy/src/legacy_bridge.rs");
        assert_eq!(bridge.status, FileStatus::Deleted);
        assert_eq!((bridge.added(), bridge.removed()), (0, 4));
    }

    #[test]
    fn fr_l19_staging_toggles_lines_hunks_and_files() {
        let d = a_diff();
        let mut st = FileStaging::none();
        assert_eq!(st.counts(&d), (0, 9));
        st.toggle_line(&d, 0, 2); // line 2 is del
        assert!(st.is_staged(0, 2));
        assert_eq!(st.counts(&d), (1, 9));
        assert_eq!(st.check(&d), Check::Partial);

        st.toggle_hunk(&d, 0);
        assert_eq!(st.hunk_check(&d, 0), Check::On);

        let mut all = Staging::new(std::slice::from_ref(&d), false);
        assert_eq!(all.check(&d), Check::Off);
        all.toggle_file(&d);
        assert_eq!(all.check(&d), Check::On);
        all.toggle_file(&d);
        assert_eq!(all.check(&d), Check::Off);
    }

    #[test]
    fn fr_l19_tri_state_checks() {
        assert_eq!(check_of(0, 0), Check::Off);
        assert_eq!(check_of(0, 3), Check::Off);
        assert_eq!(check_of(2, 3), Check::Partial);
        assert_eq!(check_of(3, 3), Check::On);
        assert_eq!(folder_check(&[]), Check::Off);
        assert_eq!(folder_check(&[Check::On, Check::On]), Check::On);
        assert_eq!(folder_check(&[Check::Off, Check::Off]), Check::Off);
        assert_eq!(folder_check(&[Check::On, Check::Off]), Check::Partial);
    }

    #[test]
    fn fr_l19_the_commit_button_needs_a_summary_and_a_staged_file() {
        assert!(!commit_enabled("", 2, false));
        assert!(!commit_enabled("  ", 2, false));
        assert!(!commit_enabled("Feat: x", 0, false));
        assert!(commit_enabled("Feat: x", 1, false));
        assert!(commit_enabled("", 0, true));
        assert_eq!(commit_label("main", 0), "main에 커밋");
        assert_eq!(commit_label("main", 2), "main에 커밋 (2개 파일)");
    }

    #[test]
    fn fr_l19_file_tree_merges_single_child_folders() {
        let paths = s(&[
            "ember/proxy/src/shim.rs",
            "ember/proxy/src/router.rs",
            "docs/SPEC.md",
        ]);
        let rows = build_tree(&paths, &BTreeSet::new());
        assert_eq!(
            rows,
            vec![
                TreeRow::Dir {
                    label: "docs".into(),
                    path: "docs/".into(),
                    depth: 0,
                    open: true
                },
                TreeRow::File {
                    index: 2,
                    name: "SPEC.md".into(),
                    depth: 1
                },
                TreeRow::Dir {
                    label: "ember/proxy/src".into(),
                    path: "ember/proxy/src/".into(),
                    depth: 0,
                    open: true
                },
                TreeRow::File {
                    index: 0,
                    name: "shim.rs".into(),
                    depth: 1
                },
                TreeRow::File {
                    index: 1,
                    name: "router.rs".into(),
                    depth: 1
                },
            ]
        );
    }

    #[test]
    fn fr_l19_the_default_project_has_no_repository() {
        let boxes = vec![boxed("unsorted"), boxed("ember"), boxed("core")];
        assert_eq!(repo_projects(&boxes).len(), 2);
        assert_eq!(pick_project(Some("core"), &boxes).as_deref(), Some("core"));
        assert_eq!(
            pick_project(Some("unsorted"), &boxes).as_deref(),
            Some("ember")
        );
        assert_eq!(pick_project(None, &boxes).as_deref(), Some("ember"));
        assert_eq!(pick_project(None, &[boxed("unsorted")]), None);
    }

    #[test]
    fn fr_l19_no_repository_data_is_invented() {
        for sec in Section::ALL {
            assert_eq!(section_count(sec, None), None);
        }
        let snap = RepoSnapshot {
            changes: vec![a_diff()],
            branches: s(&["main", "develop"]),
            ..Default::default()
        };
        assert_eq!(section_count(Section::Changes, Some(&snap)), Some(1));
        assert_eq!(section_count(Section::Branches, Some(&snap)), Some(2));
        assert_eq!(section_count(Section::Stash, Some(&snap)), None, "no stash read yet");
        assert_eq!(section_count_with_stash(Section::Stash, Some(&snap), Some(3)), Some(3));
        assert_eq!(section_count_with_stash(Section::Stash, None, Some(0)), Some(0));
        assert_eq!(section_count_with_stash(Section::Changes, Some(&snap), Some(3)), Some(1));
        assert_eq!(SOON, "준비 중입니다");
    }

    #[test]
    fn nfr_l4_texts_end_in_hamnida_form() {
        let mut texts = vec![
            ahead_behind_text(0, 0),
            fetched_text(None),
            fetched_text(Some(1)),
            fetched_text(Some(90)),
            upstream_text(None),
            commit_label("main", 1),
        ];
        for (ci, rv) in [
            (CiState::Failed, ReviewState::Approved),
            (CiState::Running, ReviewState::Approved),
            (CiState::Waiting, ReviewState::Approved),
            (CiState::Passed, ReviewState::ChangesRequested),
            (CiState::Passed, ReviewState::Pending),
        ] {
            texts.push(merge_gate(ci, rv).unwrap_err().to_string());
        }
        for t in &texts {
            assert!(
                !t.ends_with('다') || t.ends_with("니다"),
                "plain ending: {t}"
            );
        }
    }

    // ---- Host rendering tests ----

    fn text_of(content: fn()) -> String {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    #[test]
    fn fr_l19_without_git_data_the_vcs_screen_says_it_is_not_ready() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(VcsScreen);
        assert!(text.contains("버전 관리"));
        assert!(text.contains(SOON));
    }

    #[compose_rust::runtime::composable]
    fn history_sample_wide() {
        HistoryPane(None, PaneLayout::Two);
    }

    #[compose_rust::runtime::composable]
    fn history_sample_compact() {
        HistoryPane(None, PaneLayout::Tabs);
    }

    #[test]
    fn fr_g5_history_list_shows_subject_sha_author_time_chips_and_merge_marker() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for content in [history_sample_wide as fn(), history_sample_compact as fn()] {
            let text = text_of(content);
            assert!(text.contains("Feat: attach the session shim before routing"));
            assert!(text.contains("a1b2c3d"));
            assert!(text.contains("BREW"));
            assert!(text.contains("12분 전"));
            assert!(text.contains("feat/session-shim"));
            assert!(text.contains("병합"));
            assert!(!text.contains(SOON));
        }
    }

    #[test]
    fn fr_g5_chips_are_cut_and_the_meta_line_joins_sha_author_time() {
        assert_eq!(chip_text("main"), "main");
        let long = "feat/".to_string() + &"x".repeat(40);
        assert_eq!(chip_text(&long).chars().count(), 24);
        assert!(chip_text(&long).ends_with('\u{2026}'));
        assert_eq!(chip_limit(PaneLayout::Tabs), 2);
        assert_eq!(chip_limit(PaneLayout::Two), 4);
        let HistoryLoad::Ready { entries, .. } = sample_history() else { panic!() };
        assert_eq!(history_meta(&entries[0]), "a1b2c3d \u{00b7} BREW \u{00b7} 12분 전");
    }

    #[compose_rust::runtime::composable]
    fn prs_sample_wide() {
        PullRequestsPane(None, mutable_state_of(None), mutable_state_of(Vec::new()), PaneLayout::Two, Some("feat/x".to_string()));
    }

    #[compose_rust::runtime::composable]
    fn prs_sample_compact() {
        PullRequestsPane(None, mutable_state_of(None), mutable_state_of(Vec::new()), PaneLayout::Tabs, None);
    }

    #[test]
    fn fr_g8_pull_request_list_shows_title_meta_pills_and_the_gate_at_both_widths() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for content in [prs_sample_wide as fn(), prs_sample_compact as fn()] {
            let text = text_of(content);
            assert!(text.contains("#412 Feat: attach the session shim before routing"));
            assert!(text.contains("BREW"));
            assert!(text.contains("feat/session-shim"));
            assert!(text.contains("12분 전"));
            assert!(text.contains("열림"));
            assert!(text.contains("병합됨"));
            assert!(text.contains("초안"));
            assert!(text.contains("승인됨"));
            assert!(text.contains("체크 4/4 통과"));
            assert!(text.contains("체크 3/4 통과"));
            assert!(text.contains("병합할 수 있습니다"));
            assert!(text.contains("CI 체크가 실패했습니다"));
            assert!(!text.contains(SOON));
            assert!(!text.contains("병합 조건: CI 통과와 승인이 필요합니다."), "the placeholder is gone");
        }
    }

    #[test]
    fn fr_l19_pr_wide_layout_shows_the_first_pull_request_in_full() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(prs_sample_wide);
        for want in ["풀 리퀘스트", "열림 3", "새 PR", "검사", "build", "통과", "리뷰", "Minji Kim", "승인", "리뷰 코멘트", "리뷰 남기기", "코멘트", "변경 요청", "병합", "스쿼시로 병합합니다", "파일 3 +40 -2"] {
            assert!(text.contains(want), "{want}");
        }
        assert!(!text.contains("목록으로"), "the wide layout has no back button");
    }

    #[test]
    fn fr_l19_pr_wide_layout_pins_the_review_box_and_merge_below_the_scroll() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(prs_sample_wide);
        assert!(text.contains("병합"));
        assert!(text.contains("리뷰 남기기"));
        let (body, pinned) = (text.find("리뷰 코멘트").expect("comments"), text.find("리뷰 남기기").expect("review box"));
        assert!(body < pinned, "the review box follows the scrolling detail");
    }

    #[test]
    fn fr_l19_pr_compact_layout_starts_on_the_list() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(prs_sample_compact);
        assert!(text.contains("#409 Fix: stop egress when a session detaches"));
        assert!(!text.contains("리뷰 남기기"));
    }

    #[test]
    fn fr_l19_pr_sample_detail_covers_failed_pending_and_no_reviewers() {
        let PrDetailLoad::Ready(v) = sample_pr_detail(409) else { panic!() };
        assert!(v.checks.iter().any(|c| c.1 == CheckClass::Failed));
        assert_eq!(merge_button_state(&v).0, false);
        let PrDetailLoad::Ready(v) = sample_pr_detail(405) else { panic!() };
        assert!(v.reviewers.is_empty());
        assert!(matches!(sample_pr_detail(1), PrDetailLoad::Failed(_)));
    }

    #[test]
    fn fr_g8_the_meta_line_and_the_tones_follow_the_row() {
        let PrLoad::Ready(list) = sample_prs() else { panic!() };
        assert_eq!(pr_meta(&list[0]), "BREW \u{00b7} feat/session-shim \u{2192} develop \u{00b7} 12분 전");
        assert_eq!(pr_state_tone(&list[0]), PillTone::Green);
        assert_eq!(pr_state_tone(&list[3]), PillTone::Blue);
        assert_eq!(pr_review_tone(&list[0]), PillTone::Green);
        assert_eq!(pr_review_tone(&list[1]), PillTone::Orange);
        assert_eq!(pr_review_tone(&list[2]), PillTone::Grey);
        assert_eq!(pr_checks_tone(&list[0]), PillTone::Green);
        assert_eq!(pr_checks_tone(&list[1]), PillTone::Red);
        assert_eq!(pr_checks_tone(&list[2]), PillTone::Orange);
    }

    #[test]
    fn fr_g8_the_filter_chips_and_the_loaded_count_replace_the_placeholder() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(prs_sample_wide);
        for label in ["열림", "닫힘", "병합됨", "전체"] {
            assert!(text.contains(label), "{label}");
        }
        // Read by the section (not the sample), the count is the open ones; unread, there is none.
        assert_eq!(section_count_with(Section::PullRequests, None, None, None), None);
        assert_eq!(section_count_with(Section::PullRequests, None, None, Some(3)), Some(3));
    }

    #[compose_rust::runtime::composable]
    fn stash_sample_wide() {
        StashPane(None, mutable_state_of(None), PaneLayout::Two, false);
    }

    #[compose_rust::runtime::composable]
    fn stash_sample_compact() {
        StashPane(None, mutable_state_of(None), PaneLayout::Tabs, false);
    }

    #[compose_rust::runtime::composable]
    fn conflicts_sample_wide() {
        ConflictsPane(&["a.rs".to_string()], None, PaneLayout::Two);
    }

    #[compose_rust::runtime::composable]
    fn conflicts_sample_compact() {
        ConflictsPane(&["a.rs".to_string()], None, PaneLayout::Tabs);
    }

    #[test]
    fn fr_g7_conflicts_sample_file_has_two_blocks_to_settle() {
        let f = sample_conflict();
        assert_eq!(f.block_count(), 2);
        assert!(f.resolved_content().is_none());
    }

    #[test]
    fn fr_g7_conflicts_editor_wide_and_compact_show_blocks_and_choices() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for content in [conflicts_sample_wide as fn(), conflicts_sample_compact as fn()] {
            let text = text_of(content);
            assert!(text.contains("a.rs"));
        }
    }

    #[test]
    fn fr_g7_conflicts_preview_shows_the_merged_text_and_both_buttons() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(conflicts_preview_composed);
        assert!(text.contains("해결 결과 미리 보기"));
        assert!(text.contains("취소"));
        assert!(text.contains("해결하고 스테이징"));
        assert!(!text.contains("해결 완료"));
    }

    #[compose_rust::runtime::composable]
    fn conflicts_preview_composed() {
        let f = sample_conflict().with_choice(0, ConflictChoice::Ours).with_choice(1, ConflictChoice::Theirs);
        let step = mutable_state_of(ConflictStep::Preview);
        ConflictPreview(&f, &step);
        ConflictActions(&f, false, &step, &mutable_state_of(false), &mutable_state_of(None), &None, "a.rs");
    }

    #[test]
    fn fr_g7_conflicts_pane_lists_the_files_at_both_widths() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for content in [conflicts_sample_wide as fn(), conflicts_sample_compact as fn()] {
            let text = text_of(content);
            assert!(text.contains("a.rs"));
            assert!(!text.contains("충돌이 없습니다"));
            assert!(!text.contains(SOON));
        }
    }

    #[compose_rust::runtime::composable]
    fn branches_sample_wide() {
        BranchesPane(None, mutable_state_of(None), PaneLayout::Two);
    }

    #[compose_rust::runtime::composable]
    fn branches_sample_compact() {
        BranchesPane(None, mutable_state_of(None), PaneLayout::Tabs);
    }

    #[test]
    fn fr_g9_branches_pane_shows_the_creator_the_default_and_the_work_branches_at_both_widths() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for content in [branches_sample_wide as fn(), branches_sample_compact as fn()] {
            let text = text_of(content);
            assert!(text.contains("새 브랜치"));
            assert!(text.contains("만들고 전환"));
            assert!(text.contains("현재 브랜치에서 만듭니다"));
            assert!(text.contains("기본 브랜치"));
            assert!(text.contains("작업 브랜치"));
            assert!(text.contains("feat/git-api"));
            assert!(text.contains("현재"));
            assert!(text.contains("앞섬 2 \u{00b7} 뒤처짐 1"));
            assert!(text.contains("원격과 같음"));
            assert!(text.contains("업스트림 없음"));
            assert!(text.contains("워크트리 ui-phase-6"));
            assert!(text.contains("전환"));
            assert!(!text.contains(SOON));
        }
    }

    #[test]
    fn fr_g6_stash_section_shows_the_creator_and_the_buttons_of_each_entry() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for content in [stash_sample_wide as fn(), stash_sample_compact as fn()] {
            let text = text_of(content);
            assert!(text.contains("스태시 만들기"));
            assert!(text.contains("추적하지 않는 파일 포함"));
            assert!(text.contains("세션 shim 작업 중"));
            assert!(text.contains("stash@{0}"));
            assert!(text.contains("적용"));
            assert!(text.contains("꺼내기"));
            assert!(text.contains("삭제"));
            assert!(!text.contains("정말 삭제합니다"), "no delete is asked before the first tap");
            assert!(!text.contains(SOON));
        }
    }

    #[test]
    fn fr_g6_the_delete_button_asks_once_and_the_meta_line_joins_label_branch_time() {
        assert_eq!(drop_label(false), "삭제");
        assert_eq!(drop_label(true), "정말 삭제합니다");
        let StashLoad::Ready(list) = sample_stashes() else { panic!() };
        assert_eq!(stash_meta(&list[0]), "stash@{0} \u{00b7} feat/session-shim \u{00b7} 40분 전");
    }

    #[compose_rust::runtime::composable]
    fn sample_screen() {
        VcsScreenWith(Some(sample_snapshot()), sample_projects(), Some("ember".to_string()));
    }

    #[test]
    fn fr_l19_sample_route_renders_full_mockup_layout_in_compose_rust() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(sample_screen)));
        let mut text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        assert!(text.contains("feat/session-shim"));
        assert!(text.contains("Pull 1"));
        assert!(text.contains("변경 파일 5"));
        assert!(text.contains("shim.rs"));
    }

    #[test]
    fn fr_l19_commit_box_is_pinned_below_the_tree_at_the_wide_layout() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(sample_screen)));
        let mut text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        assert!(text.contains("커밋"), "the commit button is in the wide layout");
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l19_icon_buttons_have_a_44_touch_target_and_a_label() {
        assert_eq!(ICON_TOUCH_TARGET, 44.0);
        let labels = remote_button_labels();
        assert_eq!(labels, ["가져오기", "내려받기", "올리기", "스태시"]);
        assert!(labels.iter().all(|l| !l.is_empty()));
    }

    #[test]
    fn fr_l19_sample_route_compact_layout_emits_tabs() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(sample_screen)));
        let mut text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 390.0,
                height_dp: 844.0,
                class: WindowSizeClass::Compact,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        assert!(text.contains("파일"));
        assert!(text.contains("차이"));
        assert!(text.contains("커밋"));
    }

    #[test]
    fn focus_overlay_emits_button_with_text_and_fill_max_size() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                cu::Box().content(|| {
                    FocusOverlay("테스트 행", || {});
                });
            }))
        });
        let mutations = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let has_button = mutations.iter().any(|m| match m {
            Mutation::Create {
                widget: WidgetKind::Button,
                ..
            } => true,
            _ => false,
        });
        assert!(has_button, "expected Button widget");

        let has_text = mutations.iter().any(|m| match m {
            Mutation::SetProp {
                property,
                value: PropertyValue::String(s),
                ..
            } => *property == PropertyKind::Text && *s == "테스트 행",
            _ => false,
        });
        assert!(has_text, "expected text prop matching label");

        let has_fill_width = mutations.iter().any(|m| match m {
            Mutation::SetModifier {
                modifier: compose_rust::Modifier::FillMaxWidth,
                ..
            } => true,
            _ => false,
        });
        let has_fill_height = mutations.iter().any(|m| match m {
            Mutation::SetModifier {
                modifier: compose_rust::Modifier::FillMaxHeight,
                ..
            } => true,
            _ => false,
        });
        assert!(
            has_fill_width && has_fill_height,
            "expected fill_max_size modifier"
        );
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l19_runtime_buttons_have_non_empty_text_and_rows_expose_labels() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(sample_screen)));
        let mut button_nodes = std::collections::HashSet::new();
        let mut button_texts: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut scan = |batch: &[Mutation<'_>]| {
            for m in batch {
                match m {
                    Mutation::Create { node_id, widget: WidgetKind::Button } => {
                        button_nodes.insert(*node_id);
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                        button_texts.insert(*node_id, s.to_string());
                    }
                    _ => {}
                }
            }
        };
        scan(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        scan(&decode_batch(bytes).expect("decode"));


        assert!(
            !button_nodes.is_empty(),
            "expected at least one Button node in rendered VCS screen"
        );

        for id in &button_nodes {
            let text = button_texts.get(id).map(|s| s.as_str()).unwrap_or("");
            assert!(
                !text.trim().is_empty(),
                "Button node {id} has empty Text prop (nameless button found)"
            );
        }

        // Section row exposes its own label
        let has_section_label = button_texts.values().any(|t| t == "변경 사항");
        assert!(
            has_section_label,
            "expected a Button node with section row label '변경 사항'"
        );

        // Tree row exposes its own label
        let has_tree_file_label = button_texts.values().any(|t| t == "shim.rs");
        assert!(
            has_tree_file_label,
            "expected a Button node with tree row label 'shim.rs'"
        );

        // Remote action circle buttons at 1440
        assert!(
            button_texts.values().any(|t| t == "가져오기"),
            "expected a Button node with label '가져오기' at 1440"
        );
        assert!(
            button_texts.values().any(|t| t == "내려받기"),
            "expected a Button node with label '내려받기' at 1440"
        );
        assert!(
            button_texts.values().any(|t| t == "올리기"),
            "expected a Button node with label '올리기' at 1440"
        );
    }

    #[test]
    fn fr_l19_runtime_buttons_have_non_empty_text_at_compact_390_for_all_tabs() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(sample_screen)));
        let mut button_nodes = std::collections::HashSet::new();
        let mut button_texts: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut scan = |batch: &[Mutation<'_>]| {
            for m in batch {
                match m {
                    Mutation::Create { node_id, widget: WidgetKind::Button } => {
                        button_nodes.insert(*node_id);
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                        button_texts.insert(*node_id, s.to_string());
                    }
                    _ => {}
                }
            }
        };
        scan(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));

        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 390.0,
                height_dp: 844.0,
                class: WindowSizeClass::Compact,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        scan(&decode_batch(bytes).expect("decode"));

        assert!(
            !button_nodes.is_empty(),
            "expected at least one Button node in rendered VCS screen at 390 dp"
        );

        for id in &button_nodes {
            let text = button_texts.get(id).map(|s| s.as_str()).unwrap_or("");
            assert!(
                !text.trim().is_empty(),
                "Button node {id} has empty Text prop at 390 dp"
            );
        }

        // Remote action circle buttons at 390
        assert!(
            button_texts.values().any(|t| t == "가져오기"),
            "expected a Button node with label '가져오기' at 390"
        );
        assert!(
            button_texts.values().any(|t| t == "내려받기"),
            "expected a Button node with label '내려받기' at 390"
        );
        assert!(
            button_texts.values().any(|t| t == "올리기"),
            "expected a Button node with label '올리기' at 390"
        );

        // Pane tab buttons must be present at 390
        assert!(
            button_texts.values().any(|t| t == "파일"),
            "expected a Button node with tab label '파일' at 390"
        );
        assert!(
            button_texts.values().any(|t| t == "차이"),
            "expected a Button node with tab label '차이' at 390"
        );
        assert!(
            button_texts.values().any(|t| t == "커밋"),
            "expected a Button node with tab label '커밋' at 390"
        );

        // Exercise each PaneTab (파일, 차이, 커밋) in PaneLayout::Tabs
        let snap = sample_snapshot();
        for tab in [PaneTab::Files, PaneTab::Diff, PaneTab::Commit] {
            compose_rust::window::reset_window_size();
            let snap_tab = snap.clone();
            let mut tab_host = Host::new(move || {
                let s = snap_tab.clone();
                Box::new(Recomposer::new(move || {
                    cu::Column().content(|| {
                        ChangesPane(&s, PaneLayout::Tabs, Some(tab), |_| {});
                    });
                }))
            });
            let mut tab_nodes = std::collections::HashSet::new();
            let mut tab_texts: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
            let mut tab_scan = |batch: &[Mutation<'_>]| {
                for m in batch {
                    match m {
                        Mutation::Create { node_id, widget: WidgetKind::Button } => {
                            tab_nodes.insert(*node_id);
                        }
                        Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                            tab_texts.insert(*node_id, s.to_string());
                        }
                        _ => {}
                    }
                }
            };
            tab_scan(&decode_batch(tab_host.rebuild().expect("rebuild")).expect("decode"));

            assert!(
                !tab_nodes.is_empty(),
                "expected Button nodes in ChangesPane for tab {tab:?}"
            );

            for id in &tab_nodes {
                let text = tab_texts.get(id).map(|s| s.as_str()).unwrap_or("");
                assert!(
                    !text.trim().is_empty(),
                    "Button node {id} has empty Text prop in ChangesPane tab {tab:?}"
                );
            }

            assert!(
                tab_texts.values().any(|t| t == "파일"),
                "expected '파일' button in ChangesPane for tab {tab:?}"
            );
            assert!(
                tab_texts.values().any(|t| t == "차이"),
                "expected '차이' button in ChangesPane for tab {tab:?}"
            );
            assert!(
                tab_texts.values().any(|t| t == "커밋"),
                "expected '커밋' button in ChangesPane for tab {tab:?}"
            );
        }
    }

    #[test]
    fn section_row_and_tree_row_expose_their_own_labels() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                cu::Column().content(|| {
                    SectionList(Section::Changes, None, |_| {});
                    let diffs = sample_snapshot().changes;
                    let st = mutable_state_of(Staging::new(&diffs, false));
                    let folded = mutable_state_of(BTreeSet::new());
                    let sel = mutable_state_of(0usize);
                    FilesTree(&diffs, &st, &folded, &sel, |_| {});
                });
            }))
        });
        let mutations = decode_batch(host.rebuild().expect("rebuild")).expect("decode");

        let mut button_texts: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        for m in &mutations {
            if let Mutation::SetProp {
                node_id,
                property: PropertyKind::Text,
                value: PropertyValue::String(s),
            } = m
            {
                button_texts.insert(*node_id, s.to_string());
            }
        }

        assert!(
            button_texts.values().any(|t| t == "변경 사항"),
            "section row '변경 사항' must expose its own label on Button"
        );
        assert!(
            button_texts.values().any(|t| t == "히스토리"),
            "section row '히스토리' must expose its own label on Button"
        );
        assert!(
            button_texts.values().any(|t| t == "shim.rs"),
            "tree row 'shim.rs' must expose its own label on Button"
        );
        assert!(
            button_texts.values().any(|t| t == "docs"),
            "tree dir row 'docs' must expose its own label on Button"
        );
    }
}
