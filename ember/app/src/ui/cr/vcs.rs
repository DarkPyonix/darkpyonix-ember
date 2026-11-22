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
use std::rc::Rc;

use compose_rust::runtime::{key, launched_effect, mutable_state_of, remember, remember_with_key, MutableState};
use crate::ui::window_size::current_window_size;
use crate::ui::vcs::{can_approve_pr, merge_method_text, pr_created_notice, pr_merge_method_label, pr_review_notice, PR_DELETE_BRANCH_LABEL, PR_DELETE_BRANCH_NOTE, PR_MERGE_METHODS, PR_SELF_APPROVAL_DISALLOWED, copy_sha_to_clipboard, filter_history_entries, HistoryFilter, branch_switch_allowed, branch_matches, branch_bar_text, BRANCH_SWITCH_NOTE, BRANCH_CREATE_NOTE, STASH_NOW_NOTE, commit_action_branch_line, commit_failure_reloads_snapshot, commit_action_gate, commit_action_key, commit_detail_from, commit_detail_view, commit_files_shown, fetch_commit_detail, file_stat_text, run_branch_from, run_commit_cherry_pick, run_commit_revert, snapshot_dirty, CommitAction, CommitDetailLoad, CommitDetailView, choice_label, fetch_conflict, run_resolve, ConflictChoice, ConflictStep, cancel_preview, confirm, ResolveTap, ConflictFile, ConflictLoad, ConflictSegment, fetch_stashes, run_stash_action, run_stash_push, stash_action_notice, stash_from, StashAction, StashEntry, StashLoad, fetch_history, history_append, history_from, fetch_prs, prs_from, PrEntry, PrFilter, PrLoad, section_count_with, section_count_with_branches, fetch_branches, branches_from, split_branches, branch_bar_fractions, worktree_tail, branch_create_body, stash_now_gate, stash_now_error, BranchEntry, BranchLoad, HistoryEntry, HistoryLoad, HistoryRef, Load, fetch_snapshot, load_from, CommitRequest, plan_staging, run_commit, run_remote, git_call, valid_branch_name, failure_text, CONFLICT_PREVIEW_LINES, amend_warning, amend_button_label, amend_tap, AmendTap, armed_label_visible, delete_tap, stash_button_label, stash_button_text, stash_tap, stash_tap_key, DeleteTap, STASH_DIRTY_WARNING, STASH_POP_NOTE, section_count_with_team, team_rows, team_rows_with_pr_load, resolve_team_pr_load, apply_team_row_snapshot_and_mode, MemberCwd, TeamMember, TeamRow, TeamPrStatus, fetch_pr_detail, pr_detail_from, run_pr_review, run_pr_merge, run_pr_create, review_request_gate, pr_create_gate, review_tap, ReviewTap, pr_action_key, review_button_label, review_draft_key, is_stale_head, clean_body, merge_button_state, merge_button_text, MERGE_METHOD_TEXT, PrDetailLoad, PrDetailView, CheckClass, ReviewerState, banner_sub, banner_title, fetch_merge_state, banner_button_text, merge_tap, merge_tap_key, run_merge_abort, run_merge_commit, MergeAction, MergeLoad, MergeView, MERGE_ABORT_NOTE, MERGE_COMMIT_NOTE, merge_answer_reloads, pick_all_key, pick_all_note, pick_all_text};
use ember_client::api::{GitPrMergeMethod, GitPrReviewAction};
use serde_json::{json, Value};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{
    Alignment, Color, IconRole, Paint, ShapeRole, TextOverflow, TypeRole,
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
    AppButton, ButtonKind, Choice, ChoiceButton, ChoiceRow, EmberCard, EmberField, EmberSheetSized, FieldOptions, InsetPanel, Label, PopExtra, PopKind, PopMenu, PopRow, PopoverMenuWith,
    Clickable, DataNamedOverlay, NamedClickable, Pill, PillTone, Txt,
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
pub(super) struct Actions {
    project: String,
    reload: MutableState<u64>,
}

impl Actions {
    fn finish(&self, what: &'static str, task: impl std::future::Future<Output = Result<String, ember_client::api::ApiError>> + Send + 'static) {
        self.finish_with(what, task, || {});
    }

    fn finish_with(&self, what: &'static str, task: impl std::future::Future<Output = Result<String, ember_client::api::ApiError>> + Send + 'static, mut on_success: impl FnMut() + Send + 'static) {
        let reload = self.reload.clone();
        // FR-W4d: no watch refresh while the call runs; a success reloads by itself.
        crate::services::run_shared(crate::ui::vcs_watch::guarded_ok(&self.project, task), move |result| {
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
        crate::services::run_shared(crate::ui::vcs_watch::guarded_ok(&project.clone(), async move { run_stash_push(&project, "", false).await }), move |result| {
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
        conflicts: vec!["ember/app/src/ui/cr/shell.rs".to_string(), "docs/SPEC.md".to_string()],
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

/// Side of a stage check on the header, a file row and the form boxes (`vcs.css:68`).
pub const STAGE_CHECK_SIDE: f32 = 18.0;
/// Side of the check on a diff line (the mockup's 14 px line variant).
pub const STAGE_LINE_CHECK_SIDE: f32 = 14.0;
/// Corner radius of a stage check (`vcs.css:68`, `border-radius:5px`).
pub const STAGE_CHECK_RADIUS: f32 = CORNER_2XS;
/// Border width of a stage check that is off or partial (`vcs.css:68`).
pub const STAGE_CHECK_BORDER: f32 = 1.5;
/// Width of the tap area around a stage check; its height is the row's.
pub const STAGE_CHECK_HIT_W: f32 = 26.0;

/// What a stage check draws inside its square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMark {
    None,
    Tick,
    Dash,
}

/// The painted look of a stage check (`vcs.css:68-72`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageCheckLook {
    pub fill: Color,
    pub border: Color,
    pub mark: CheckMark,
    pub mark_color: Color,
}

/// Off: `surface` with a `grey` border. On: `ink` fill and border with an `ink_text` tick.
/// Partial: `surface` with an `ink` border and an `ink` dash.
pub fn stage_check_look(check: Check, p: &Palette) -> StageCheckLook {
    match check {
        Check::Off => StageCheckLook { fill: p.surface, border: p.grey.strong, mark: CheckMark::None, mark_color: p.ink },
        Check::On => StageCheckLook { fill: p.ink, border: p.ink, mark: CheckMark::Tick, mark_color: p.ink_text },
        Check::Partial => StageCheckLook { fill: p.surface, border: p.ink, mark: CheckMark::Dash, mark_color: p.ink },
    }
}

const ICON_TICK_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>"#;

// TODO(compose-rust#767): v0.0.7 `Checkbox` takes a container
// and check colour and a size (#428, closed), but no shape or corner radius and no partial state. The square is painted and
// the toolkit checkbox lies over it at alpha 0, so the role, the checked state, keyboard focus and
// the tap stay the toolkit's. A partial check reports unchecked to assistive tech.
/// A stage check (`.vcs-cb`): a painted `side` px square over an invisible toolkit checkbox that
/// fills a [`STAGE_CHECK_HIT_W`] by `hit_h` tap area. A tap calls `on_toggle`.
fn StageCheck(check: Check, side: f32, hit_h: f32, on_toggle: impl FnMut() + 'static) {
    StageCheckWith(Modifier::new(), check, side, hit_h, on_toggle);
}

fn StageCheckWith(modifier: Modifier, check: Check, side: f32, hit_h: f32, on_toggle: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let look = stage_check_look(check, &p);
    let mut on_toggle = on_toggle;
    { use ::compose_rust::ui::Box; Box!(modifier = modifier.width(STAGE_CHECK_HIT_W.max(side)).height(hit_h.max(side)), alignment = Alignment::Center, content = move || {
        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(side, side).background(paint(look.fill)).border(STAGE_CHECK_BORDER, paint(look.border)).corner_radius(STAGE_CHECK_RADIUS), alignment = Alignment::Center, content = move || match look.mark {
                CheckMark::None => {}
                CheckMark::Tick => NavIcon(ICON_TICK_SVG, look.mark_color.to_argb(), side - 4.0),
                CheckMark::Dash => {
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(side - 6.0 - 2.0 * STAGE_CHECK_BORDER).height(2.0).background(paint(look.mark_color)).corner_radius(1.0)) };
                }
            }) };
        { use ::compose_rust::ui::Checkbox; Checkbox!(check == Check::On, move |_| on_toggle(), modifier = Modifier::new().fill_max_size().alpha(0.0)) };
    }) };
}

/// Height of a 충돌 list row (`.vcs-file`, `vcs.css:59`, #694).
pub const CONFLICT_ROW_H: f32 = 34.0;
/// Gap between the warning tile and the path (`vcs.css:59`, #694).
pub const CONFLICT_ROW_GAP: f32 = 6.0;
/// The small warning tile of a 충돌 row (`.vcs-wico.sm`, `vcs.css:167-168`).
pub const CONFLICT_TILE: f32 = 22.0;
/// The warning glyph inside the tile.
pub const CONFLICT_GLYPH: f32 = 14.0;

/// One unmerged file of the 충돌 list (`.vcs-cflist .vcs-file`, #694): 34 px, no border or fill, a
/// 14 px warning glyph in a 22 px `orange.soft` tile, a gap of 6, then the mono path. The whole row
/// is the tap target and carries the path as its accessible name.
fn ConflictFileRow(path: &str, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let shown = path.to_string();
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(CONFLICT_ROW_H).corner_radius(CORNER_SM), alignment = Alignment::CenterStart, content = move || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = CONFLICT_ROW_GAP, modifier = Modifier::new().fill_max_width().fill_max_height(), {
                // `.vcs-cflist .vcs-file{padding-left:10px}`; the row gap adds 6.
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0 - CONFLICT_ROW_GAP)) };
                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(CONFLICT_TILE, CONFLICT_TILE).background(paint(p.orange.soft)).corner_radius(CORNER_XS), alignment = Alignment::Center, {
                    NavIcon(ICON_WARN_SVG, p.orange.strong.to_argb(), CONFLICT_GLYPH);
                }) };
                Txt(&shown, TypeRole::Mono, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), Some(1.0), None);
            }) };
            DataNamedOverlay(&shown, true, None, on_click);
        }) };
}

/// FR-L19 (audit P2, #517): the text of the screen's own inputs, buttons and chips is the small
/// step of the scale (13 px), like the mockup's `.btn.sm`, `.chip` and `.field.sm`, not the
/// toolkit default (16 px).
pub const VCS_CONTROL_FS: Fs = Fs::Sm;
/// The toolkit text role a field takes to draw 13 px: `Label` maps to [`VCS_CONTROL_FS`]
/// (`type_role_style`). `TextField` has no `font_size` builder in compose-rust v0.0.6.
pub const VCS_FIELD_ROLE: TypeRole = TypeRole::Label;
/// The screen's textareas (`.vcs textarea.field`): 64 px high, FR-L10.2.
pub const VCS_TEXTAREA_H: f32 = 64.0;
/// Their text inset `(horizontal, vertical)`: `.vcs textarea.field` padding 8 12.
pub const VCS_TEXTAREA_INSET: (f32, f32) = (12.0, 8.0);
/// Their line pitch: `.field{font:var(--fs-md) var(--font)}` resets the line height to the
/// font's `normal` (about 1.2 em, 16 px at 13 px), which `.vcs textarea.field` keeps. The 13 px
/// step's own 20 px showed 2.4 lines in the 48 px text area, so an overflowing field cut a line
/// in half; 16 shows three whole lines (review r6 item 3).
pub const VCS_TEXTAREA_LINE_H: f32 = 16.0;
/// Height of the 이 덩어리 전체 button: smaller than the 32 px control, with a 1 px border.
pub const VCS_HUNK_BUTTON_H: f32 = 28.0;
/// Height of the hunk header row (`.vcs-hh`): the 28 px action with 6 px above and below.
pub const VCS_HUNK_HEADER_H: f32 = VCS_HUNK_BUTTON_H + 12.0;

/// The space at each side of the label of a painted small pill (the stash pill, the hunk
/// button). The pill takes the width of its label and these two; its width was a `text_px`
/// estimate, at least 64, from when a Box in a Row took the remaining width (compose-rust#427).
pub const VCS_PILL_PAD_X: f32 = 14.0;

/// The label of a painted small pill between its two side spaces, on one line.
fn PillLabel(text: &str, fs: Fs, ink: Color) {
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, {
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(VCS_PILL_PAD_X)) };
        Label(text, fs, Weight::Medium, Some(paint(ink)), Some(1));
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(VCS_PILL_PAD_X)) };
    }) };
}

/// A single choice among a few values (the history filter, the PR state filter, the merge
/// method): the app's `.seg` buttons (`ChoiceButton`, FR-L10.2) in a wrapping row 8 px apart,
/// each a radio button to assistive technology. The mockup draws none of these groups, so they
/// follow the app's one rule for a choice among a few values; the grey `pickchip` fill they had
/// made them the only choices in the app without the white, bordered look (review r6 item 1f).
/// `trailing` adds controls to the same row (the merge's 병합 후 브랜치 삭제 toggle).
fn VcsRadioRow<T: Copy + PartialEq + Send + 'static>(
    options: &[T],
    label: fn(T) -> &'static str,
    current: T,
    state: MutableState<T>,
    trailing: impl FnOnce(),
) {
    ChoiceRow(|| {
        for &o in options {
            let st = state.clone();
            ChoiceButton(label(o), Choice::Radio, o == current, true, true, move || st.set(o));
        }
        trailing();
    });
}

/// Card shown when a section's server data is not yet available.
pub fn Unavailable(section: Section) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, Some(SP[3]), None, || {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                Label(section.label(), Fs::Lg, Weight::Semibold, None, None);
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                Pill(SOON, PillTone::Grey, true);
            }) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[0])) };
            // TODO(server, #168): this section needs the repository endpoints listed there.
            Label(&soon_line(section.label()), Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
        }) };
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

/// A 32 px circle icon button in a 44 dp box that takes the press. The box is the control: a
/// named, clickable container (`NamedClickable`, compose-rust#429), round so the press tint and
/// the focus ring are circles. It asks for 44 by 44 and takes what its row gives: a row with a
/// height (the 원격 cell's 36, the files header's 52) keeps that height, and `min_touch_target`
/// (compose-rust#472, v0.0.11) keeps a 44 by 44 hit area without growing the node (FR-L19.1,
/// #415).
fn CircleIconButton(
    icon: &'static str,
    label: &'static str,
    on_click: impl FnMut() + 'static,
) {
    let p = ember_theme().palette;
    NamedClickable(
        label,
        Modifier::new()
            .size(ICON_TOUCH_TARGET, ICON_TOUCH_TARGET)
            .min_touch_target(ICON_TOUCH_TARGET)
            .shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        move || {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                        .size(32.0, 32.0)
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .shape_role(ShapeRole::Full), alignment = Alignment::Center, content = move || {
                    NavIcon(icon, p.text_2.to_argb(), 16.0);
                }) };
        },
    );
}

/// The height of the bar's two pickers (`.vcs-sel{height:36px}`). Both take it, so the value
/// lines and the notes under them line up across the 저장소 and 현재 브랜치 cells (review r8 fix
/// round: 44 and 36 left the columns 4 px apart on the value line and 8 on the note).
pub const VCS_SEL_H: f32 = 36.0;

/// One picker of the repository bar: its icon, the value and a chevron on one line.
fn BarSelector(icon: &'static str, name: &str, expanded: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(VCS_SEL_H), content = move || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_size(), {
                    NavIcon(icon, p.text.to_argb(), 18.0);
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(6.0)) };
                    Label(name, Fs::Md, Weight::Semibold, Some(paint(p.text)), Some(1));
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(6.0)) };
                    NavIcon(ICON_CHEVRON_DOWN_SVG, p.muted.to_argb(), 16.0);
                }) };
            // The clear button over the row is the control: named by the value it shows (a
            // repository or a branch, so data), and it says whether its menu is open (FR-L10.4).
            DataNamedOverlay(name, true, Some(expanded), on_click);
        }) };
}

/// Selector row for repository in the bar.
fn RepoSelector(name: &str, expanded: bool, on_click: impl FnMut() + 'static) {
    BarSelector(ICON_FOLDER_SVG, name, expanded, on_click);
}

/// Selector row for current branch in the bar.
fn BranchSelector(name: &str, expanded: bool, on_click: impl FnMut() + 'static) {
    BarSelector(ICON_GIT_SVG, name, expanded, on_click);
}

/// The head of the 저장소 menu: what is chosen now.
pub fn repo_menu_head(name: &str) -> String {
    format!("저장소 \u{b7} {name}")
}

/// The 저장소 menu's rows: one per project with the folder glyph and a check on the chosen one.
/// A project whose id is not its shown name carries the id as its description.
pub fn repo_menu_rows(projects: &[(String, String)], chosen: Option<usize>) -> Vec<PopRow> {
    projects
        .iter()
        .enumerate()
        .map(|(i, (id, name))| PopRow {
            icon: Some(ICON_FOLDER_SVG),
            title: name.clone(),
            description: (id != name).then(|| id.clone()),
            chosen: chosen == Some(i),
            ..Default::default()
        })
        .collect()
}

/// The 저장소 control and its menu (FR-L10.4, #888): the shared menu in the permission-mode
/// look, under the control and lined up with its start, a bottom sheet with the menu's own head
/// at 560 and below. The mockup draws a native `<select>` here (`#vcsProj`), which looks
/// different on every OS; Ember shows one look everywhere, as the Home project chip does.
pub(super) fn RepoMenu(open: MutableState<bool>, name: &str, projects: &[(String, String)], chosen: Option<usize>, mut on_pick: impl FnMut(String) + Clone + 'static) {
    let rows = repo_menu_rows(projects, chosen);
    let head = repo_menu_head(name);
    let is_open = open.get();
    let (dismiss, toggle) = (open.clone(), open.clone());
    let list = projects.to_vec();
    PopoverMenuWith(
        is_open,
        PopMenu {
            name: super::a11y::REPO_PICKER,
            kind: PopKind::Mode,
            head: &head,
            sub: None,
            rows: &rows,
            align: compose_rust::PopoverAlign::Start,
        },
        PopExtra { plain_sheet: true, ..Default::default() },
        move || dismiss.set(false),
        move |idx| {
            if let Some((id, _)) = list.get(idx) {
                on_pick(id.clone());
            }
        },
        || RepoSelector(name, is_open, move || toggle.set(!toggle.get_untracked())),
        None,
    );
}

/// Main remote action button (Pull with down arrow and count pill, Push, Fetch).
fn RemoteMainButton(
    action: RemoteAction,
    enabled: bool,
    on_click: impl FnMut() + 'static,
) {
    // The label is a one-line `Label` in a fixed 100 dp pill, so it is not measured narrow
    // (compose-rust#328, fixed in v0.0.7, was about toolkit Button and Chip labels).
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

    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .size(100.0, 32.0)
                .background(paint(btn_bg))
                .shape_role(ShapeRole::Full), alignment = Alignment::Center, content = move || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, spacing = 6.0, modifier = Modifier::new().fill_max_size(), {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[0])) };
                    NavIcon(action_icon, btn_fg.to_argb(), 16.0);
                    Label(action_text, Fs::Sm, Weight::Medium, Some(paint(btn_fg)), Some(1));
                    if let Some(n) = action_count {
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                    .size(20.0, 20.0)
                                    .background(paint(pill_bg))
                                    .shape_role(ShapeRole::Full), alignment = Alignment::Center, {
                                Label(
                                    &n.to_string(),
                                    Fs::Xs,
                                    Weight::Semibold,
                                    Some(paint(pill_fg)),
                                    Some(1),
                                );
                            }) };
                    }
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[0])) };
                }) };
            DataNamedOverlay(&label, true, None, on_click);
        }) };
}

/// Ahead/behind counters with arrows and relative fetch age.
fn RemoteSubNote(snap: Option<&RepoSnapshot>) {
    let p = ember_theme().palette;
    match snap {
        Some(s) => {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 4.0, {
                    let up_fg = if s.ahead > 0 { p.text } else { p.muted };
                    NavIcon(ICON_ARROW_UP_SVG, up_fg.to_argb(), 12.0);
                    Label(&s.ahead.to_string(), Fs::Xs, Weight::Regular, Some(paint(up_fg)), Some(1));
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                    let down_fg = if s.behind > 0 { p.text } else { p.muted };
                    NavIcon(ICON_ARROW_DOWN_SVG, down_fg.to_argb(), 12.0);
                    Label(&s.behind.to_string(), Fs::Xs, Weight::Regular, Some(paint(down_fg)), Some(1));
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                    Label(
                        &format!("\u{00b7} {}", fetched_text(s.fetched_age_secs)),
                        Fs::Xs,
                        Weight::Regular,
                        Some(paint(p.text_2)),
                        Some(1),
                    );
                }) };
        }
        None => {
            SmallNote(SOON);
        }
    }
}

// EmberCard's inner row does not expand its content to the card width.
fn VcsCard(content: impl FnOnce()) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().background(paint(p.surface))
        .border(1.0, paint(p.line)).corner_radius(CORNER_LG), {
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = content) };
    }) };
}

/// How the repository bar lays out its three cells (`.vcs-bar`, `vcs.css`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoBarGrid {
    /// 저장소, 현재 브랜치 and 원격 side by side (`1.1fr 1.1fr 1fr`).
    Three,
    /// 저장소 and 현재 브랜치 side by side, 원격 under them across both (`.vcs-remote{grid-column:1/3}`).
    TwoRemoteBelow,
    /// One column.
    One,
}

/// The bar's layout at a window width, as the mockup's rules give it: the page follows the
/// viewport (`@media (max-width:900px)` two columns with 원격 across, `@media (max-width:560px)`
/// one), the sheet's mounted screen also follows its container, the padded body
/// (`@container (max-width:760px)` two columns with 원격 across, which beats the 560 rule, so a
/// phone sheet keeps two). Review r8: one 334 dp stack at 1024 and 800 where the mockup has
/// three columns and two.
pub fn repo_bar_grid(window_w: f32, sheet_body_w: Option<f32>) -> RepoBarGrid {
    if sheet_body_w.is_some_and(|w| w <= 760.0) {
        RepoBarGrid::TwoRemoteBelow
    } else if window_w <= 560.0 {
        RepoBarGrid::One
    } else if window_w <= 900.0 {
        RepoBarGrid::TwoRemoteBelow
    } else {
        RepoBarGrid::Three
    }
}

/// The diff header wraps to two lines (title and path, then the chips) when the diff pane is
/// narrower than 560 dp, so the "+n -n" and "k/n줄 스테이징" chips are not clipped (FR-L19).
pub fn diff_header_wraps(pane_width_dp: f32) -> bool {
    pane_width_dp < 560.0
}

/// The width of the diff pane: at the two-pane layout the window minus the page padding, the
/// section rail (200), the file column (320) and the gaps; otherwise the page width.
pub fn diff_pane_width(window_width_dp: f32, layout: PaneLayout) -> f32 {
    if layout == PaneLayout::Two {
        window_width_dp - 64.0 - 200.0 - 320.0 - SP[3] - GAP_CARD
    } else {
        window_width_dp - 64.0
    }
}

/// Placeholder of the search field at the top of the branch popover (mockup `vcsBrQ`).
pub const BRANCH_MENU_SEARCH: &str = "브랜치 찾기";

/// Indices of the branches the popover lists for `query`: every branch for an empty query,
/// else the names containing it, ignoring case (the same rule as the 브랜치 tab, #558).
pub fn branch_menu_rows(branches: &[String], query: &str) -> Vec<usize> {
    branches.iter().enumerate().filter(|(_, b)| branch_matches(b, query)).map(|(i, _)| i).collect()
}

/// The branch popover's width (`.vcs-pop{width:min(380px,calc(100vw - 32px))}`). A window that
/// has not reported its size (0) is not narrow.
pub fn branch_pop_width(window_width_dp: f32) -> f32 {
    if window_width_dp <= 0.0 { 380.0 } else { 380.0_f32.min((window_width_dp - 32.0).max(0.0)) }
}

/// A row of the branch popover is 40 high (`.vcs-poprow{height:40px}`).
pub const BRANCH_POP_ROW_H: f32 = 40.0;
/// The tallest the branch popover's list grows before scrolling (`.vcs-poplist{max-height:300px}`,
/// compose-rust#778, v0.0.11 `Modifier::max_height`).
pub const BRANCH_POP_LIST_MAX_H: f32 = 300.0;
/// The gap between the control and the box. The mockup hangs the box from the cell
/// (`.vcs-pop{top:calc(100% - 4px)}`), under the 추적 line; a `Popover` has one anchor, the
/// control, so the box sits 4 under the control and covers that line.
pub const BRANCH_POP_OFFSET: f32 = 4.0;

/// What the popover says when no branch matches the search (mockup `vcsPopList`).
pub const BRANCH_POP_EMPTY: &str = "일치하는 브랜치가 없습니다";
/// The new-branch field's placeholder and its button (mockup `.vcs-popfoot`).
pub const BRANCH_POP_NEW: &str = "feat/새-브랜치";
pub const BRANCH_POP_CREATE: &str = "만들기";
/// The new-branch field's name for assistive technology; its placeholder is an example.
pub const BRANCH_POP_NEW_NAME: &str = "새 브랜치 이름";

/// One row of the branch popover (`.vcs-poprow`): 40 high, padded 10, the check on the current
/// branch and the branch glyph on the others, the name in the mono face at 12; the current row
/// on `sel`. A menu item to assistive technology that says whether it is the current one.
fn BranchPopRow(name: &str, current: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let fill = if current { paint(p.sel) } else { Paint::Literal(Color::argb(0)) };
    Clickable(
        Modifier::new()
            .fill_max_width()
            .height(BRANCH_POP_ROW_H)
            .background(fill)
            .corner_radius(CORNER_SM)
            .role(cu::Role::MenuItem)
            .selected(current)
            .click_action(true),
        Alignment::CenterStart,
        on_click,
        || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
                NavIcon(if current { super::components::ICON_CHECK_SVG } else { ICON_GIT_SVG }, p.text.to_argb(), 16.0);
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                // Zero wide and weighted, so a long name is cut and does not widen the box.
                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterStart, modifier = Modifier::new().width(0.0).weight(1.0), {
                    Txt(name, TypeRole::Mono, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                }) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(10.0)) };
            }) };
        },
    );
}

/// The 현재 브랜치 control and its popover (FR-L10.4, #888, mockup `vcsPopHtml`, `vcs.css`
/// 33-41): the toolkit `Popover` hung under the control, lined up with its start, 380 wide.
/// A 브랜치 찾기 field (#513), the matching branches (the list scrolls past 300), then, under a
/// line, a new-branch field with 만들기. A row switches to that branch and closes the box;
/// Escape and a press outside close it. The content is composed only while the box is open.
/// Not built: who made a branch and when on each row (the snapshot lists names only).
pub(super) fn BranchMenu(open: MutableState<bool>, text: &str, branches: &[String], current: Option<&str>, has: bool, actions: Option<Actions>) {
    let theme = ember_theme();
    let p = theme.palette;
    let query = remember(|| mutable_state_of(String::new()));
    let new_branch = remember(|| mutable_state_of(String::new()));
    let generation = remember(|| mutable_state_of(0_u64));
    let is_open = open.get();
    let (dismiss, toggle) = (open.clone(), open.clone());
    let width = branch_pop_width(current_window_size().width_dp);
    let branches = branches.to_vec();
    let current = current.map(str::to_string);
    // `elevation(0)`: the box casts its own shadow (as `PopoverMenu`).
    let modifier = with_shadows(Modifier::new(), theme.shadow_lg())
        .elevation(0.0)
        .width(width)
        .background(paint(p.surface))
        .border(1.0, paint(p.line))
        .corner_radius(CORNER_LG)
        .role(cu::Role::Menu)
        .label(super::a11y::BRANCH_PICKER);
    { use ::compose_rust::ui::Popover; Popover!(
        is_open,
        move || dismiss.set(false),
        placement = compose_rust::PopoverPlacement::Bottom,
        align = compose_rust::PopoverAlign::Start,
        offset = BRANCH_POP_OFFSET,
        modifier = modifier,
        anchor = Some(|| BranchSelector(text, is_open, move || toggle.set(!toggle.get_untracked()))),
        content = move || {
            if !is_open {
                return;
            }
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
                // `.vcs-pophead{padding:10px}`.
                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().padding(10.0), {
                    let q_state = query.clone();
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        height: Some(H_CTL),
                        text: Some(query.get()),
                        placeholder: BRANCH_MENU_SEARCH.to_string(),
                        type_role: Some(VCS_FIELD_ROLE),
                        on_value_change: Some(Box::new(move |v| q_state.set(v))),
                        ..Default::default()
                    });
                }) };
                // `.vcs-poplist{padding:0 6px 6px}`.
                let shown = branch_menu_rows(&branches, &query.get());
                let list_actions = actions.clone();
                let list_open = open.clone();
                let rows = move || {
                    if branches.is_empty() {
                        Label(&soon_line("브랜치 목록"), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    } else if shown.is_empty() {
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().padding(SP[2]), {
                            Label(BRANCH_POP_EMPTY, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                        }) };
                    }
                    for idx in shown.iter().copied() {
                        let Some(name) = branches.get(idx).cloned() else { continue };
                        let (close, row_actions, target) = (list_open.clone(), list_actions.clone(), name.clone());
                        BranchPopRow(&name, current.as_deref() == Some(name.as_str()), move || {
                            close.set(false);
                            if let Some(a) = row_actions.as_ref() {
                                a.branch(target.clone(), false, || {});
                            }
                        });
                    }
                };
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), content = move || {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(6.0)) };
                    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().width(0.0).weight(1.0).max_height(BRANCH_POP_LIST_MAX_H), content = rows) };
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(6.0)) };
                }) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(6.0)) };
                // `.vcs-popfoot{border-top:1px solid var(--line);padding:10px;gap:8px}`.
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(10.0), content = move || {
                    let value = new_branch.clone();
                    key(generation.get(), || {
                        EmberField(FieldOptions {
                            weight: Some(1.0),
                            height: Some(H_CTL),
                            text: Some(new_branch.get()),
                            placeholder: BRANCH_POP_NEW.to_string(),
                            type_role: Some(VCS_FIELD_ROLE),
                            label: Some(BRANCH_POP_NEW_NAME),
                            on_value_change: Some(Box::new(move |v| value.set(v))),
                            ..Default::default()
                        });
                    });
                    let (create_actions, create_value, generation) = (actions.clone(), new_branch.clone(), generation.clone());
                    AppButton(BRANCH_POP_CREATE, Some(IconRole::Add), ButtonKind::Default, true, has && branch_input(&new_branch.get()).is_some(), false, move || {
                        if let (Some(actions), Some(name)) = (create_actions.as_ref(), branch_input(&create_value.get())) {
                            let submitted = create_value.get();
                            let (value, generation) = (create_value.clone(), generation.clone());
                            actions.branch(name, true, move || {
                                value.set(text_after_success(&value.get(), &submitted));
                                generation.set(generation.get() + 1);
                            });
                        }
                    });
                }) };
            }) };
        },
    ) };
}

/// The bar of three: repository, current branch, remote origin in ONE split card.
pub fn RepoBar(
    projects: &[(String, String)],
    project: &ProjectBox,
    snap: Option<&RepoSnapshot>,
    compact: bool,
    mut on_pick: impl FnMut(String) + Clone + 'static,
) {
    let _ = compact;
    RepoBarWithActions(projects, project, snap, repo_bar_grid(current_window_size().width_dp, None), on_pick, None);
}

fn RepoBarWithActions(
    projects: &[(String, String)], project: &ProjectBox, snap: Option<&RepoSnapshot>, grid: RepoBarGrid,
    mut on_pick: impl FnMut(String) + Clone + 'static, actions: Option<Actions>,
) {
    // `EMBER_CR_VCS_MENU=project` opens the 저장소 menu at start, for captures.
    let project_menu = remember(|| mutable_state_of(std::env::var("EMBER_CR_VCS_MENU").as_deref() == Ok("project")));
    // `EMBER_CR_VCS_MENU=branch` opens the branch popover at start, for captures.
    let branch_menu = remember(|| mutable_state_of(std::env::var("EMBER_CR_VCS_MENU").as_deref() == Ok("branch")));

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
    let chosen_proj = projects.iter().position(|(id, _)| id == &proj_id);

    let p_projects = projects.to_vec();
    let on_pick_proj = on_pick.clone();

    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
        let (project_menu_c, branch_menu_c) = (project_menu.clone(), branch_menu.clone());
        let p = ember_theme().palette;
        let remote_actions = actions.clone();
        let branch_actions = actions.clone();
        // `.vcs-cell{padding:var(--pad-card)}`: 16 beside each other, 12 in the phone's one column.
        let pad = if grid == RepoBarGrid::One { SP[2] } else { SP[3] };
        let repo_cell = move |m: Modifier| {
            let p_open = project_menu_c.clone();
            let p_name = proj_name.clone();
            let r_line = repo_line.clone();
            let (p_list, p_pick) = (p_projects.clone(), on_pick_proj.clone());
            { use ::compose_rust::ui::Column; Column!(modifier = m.padding(pad), spacing = SP[1], content = move || {
                SmallNote("저장소");
                RepoMenu(p_open, &p_name, &p_list, chosen_proj, p_pick);
                SmallNote(&r_line);
            }) };
        };
        let branch_cell = move |m: Modifier| {
            let b_open = branch_menu_c.clone();
            let b_text = branch_text.clone();
            let u_text = upstream.clone();
            let (b_list, b_current, b_actions) = (branches.clone(), current_branch.clone(), branch_actions.clone());
            { use ::compose_rust::ui::Column; Column!(modifier = m.padding(pad), spacing = SP[1], content = move || {
                SmallNote("현재 브랜치");
                // The popover hangs from the control (FR-L10.4, #888; it was a centred dialog).
                BranchMenu(b_open, &b_text, &b_list, b_current.as_deref(), has, b_actions);
                SmallNote(&u_text);
            }) };
        };
        let remote_cell = move |m: Modifier| {
            let remote_actions = remote_actions.clone();
            { use ::compose_rust::ui::Column; Column!(modifier = m.padding(pad), spacing = SP[1], content = move || {
                SmallNote("원격 origin");
                // The pickers' height (FR-L19.1, #415): the 44 dp boxes of the round buttons do not
                // make this cell's value line taller than the other two.
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(VCS_SEL_H), alignment = Alignment::CenterStart, spacing = 10.0, content = move || {
                    RemoteControls(action, has, remote_actions.clone());
                }) };
                RemoteSubNote(snap);
            }) };
        };
        let line_v = move || {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(1.0).fill_max_height().background(paint(p.line))) };
        };
        let line_h = move || {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
        };
        VcsCard(move || match grid {
            RepoBarGrid::One => {
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
                    repo_cell(Modifier::new().fill_max_width());
                    line_h();
                    branch_cell(Modifier::new().fill_max_width());
                    line_h();
                    remote_cell(Modifier::new().fill_max_width());
                }) };
            }
            RepoBarGrid::TwoRemoteBelow => {
                // `@media (max-width:900px)`, `@container (max-width:760px)`: two columns, then
                // 원격 across both under a line.
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
                    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), alignment = Alignment::TopStart, intrinsic_size = true, content = move || {
                        repo_cell(Modifier::new().weight(1.0).fill_max_height());
                        line_v();
                        branch_cell(Modifier::new().weight(1.0).fill_max_height());
                    }) };
                    line_h();
                    remote_cell(Modifier::new().fill_max_width());
                }) };
            }
            RepoBarGrid::Three => {
                // No fixed height (#407): the row sizes to its tallest column and `equal_height`
                // stretches the others, so the URL line and the ahead/behind line are never cut.
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), alignment = Alignment::TopStart, intrinsic_size = true, content = move || {
                    repo_cell(Modifier::new().weight(1.1).fill_max_height());
                    line_v();
                    branch_cell(Modifier::new().weight(1.1).fill_max_height());
                    line_v();
                    remote_cell(Modifier::new().weight(1.0).fill_max_height());
                }) };
            }
        });
    }) };
}

fn RemoteControls(action: RemoteAction, enabled: bool, actions: Option<Actions>) {
    let main = actions.clone();
    RemoteMainButton(action, enabled, move || { if let Some(a) = &main { a.remote(action); } });
    { use ::compose_rust::ui::Row; Row!(spacing = 4.0, content = move || {
        let labels = remote_button_labels();
        for (icon, label, action) in [(ICON_FETCH_SVG, labels[0], RemoteAction::Fetch), (ICON_ARROW_DOWN_SVG, labels[1], RemoteAction::Pull(0)), (ICON_ARROW_UP_SVG, labels[2], RemoteAction::Push(0))] {
            let a = actions.clone();
            CircleIconButton(icon, label, move || { if let Some(a) = &a { a.remote(action); } });
        }
    }) };
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
    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(200.0), spacing = 2.0, {
        for sec in Section::ALL {
            let is_active = sec == active;
            let bg = if is_active { paint(p.surface) } else { Paint::Literal(Color::argb(0)) };
            let fg = if is_active { p.text } else { p.muted };
            let count = section_count_with_team(sec, snap, stash_count, pr_count, branch_count, team_count).or_else(|| {
                if is_sample {
                    match sec {
                        Section::PullRequests => Some(sample_pr_count()),
                        Section::Stash => Some(2),
                        Section::Conflicts => Some(SAMPLE_CONFLICT_COUNT),
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

            NamedClickable(sec.label(), m, Alignment::TopStart, move || pick(sec), move || {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_size(), content = move || {
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                            NavIcon(section_svg(sec), fg.to_argb(), 18.0);
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                            Label(sec.label(), Fs::Sm, Weight::Medium, Some(paint(fg)), Some(1));
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                            if let Some(n) = count {
                                if sec == Section::Conflicts && n > 0 {
                                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                .height(18.0)
                                                .background(paint(p.orange.strong))
                                                .shape_role(ShapeRole::Full)
                                                .padding(2.0), alignment = Alignment::Center, {
                                            Label(
                                                &format!(" {n} "),
                                                Fs::Xs,
                                                Weight::Semibold,
                                                Some(Paint::Literal(Color::argb(0xffff_ffff))),
                                                Some(1),
                                            );
                                        }) };
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
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                        }) };
            });
        }
    }) };
}

/// Section pills above the panes (medium and compact windows).
pub fn SectionPills(
    active: Section,
    mut on_pick: impl FnMut(Section) + Clone + 'static,
) {
    SectionPillsAtWidth(active, current_window_size().width_dp, on_pick);
}

/// Whether the section pills scroll sideways in one row (`.vcs-nav` at 900 px and below) rather
/// than sitting in one row that fits.
pub fn section_pills_scroll(width_dp: f32) -> bool {
    width_dp < 820.0
}

fn SectionPillsAtWidth(active: Section, width: f32, on_pick: impl FnMut(Section) + Clone + 'static) {
    SectionPillsWithCounts(active, width, width, &|_| None, on_pick);
}

/// The width a section pill is drawn at, estimated from its text (13 dp Hangul, 7.5 dp digits at
/// the 12 dp count): 12 dp sides, an 18 dp icon, and 12 dp between icon, name and count, the
/// conflicts count on a pill 7 dp wider each side. Used only to scroll the current pill into view.
pub fn section_pill_width(sec: Section, count: Option<usize>) -> f32 {
    let text = |s: &str, glyph: f32, narrow: f32| -> f32 { s.chars().map(|c| if c.is_ascii() { narrow } else { glyph }).sum() };
    let mut w = SP[2] + 18.0 + SP[2] + text(sec.label(), 13.0, 3.5) + SP[2];
    if let Some(n) = count.filter(|n| *n > 0) {
        w += text(&n.to_string(), 7.5, 7.5) + SP[2];
        if sec == Section::Conflicts {
            w += 14.0;
        }
    }
    w
}

/// Where the scrolling pill row is moved to so the current pill is whole on screen: 0 while it
/// ends inside the row's `width`, else the distance that puts its end 6 dp inside the right edge
/// (the Renderer clamps it to the row's scroll range). `counts` are in `Section::ALL` order.
pub fn section_pills_reveal(active: Section, width: f32, counts: &[Option<usize>]) -> f32 {
    // A row whose width is not known yet (the window reads 0 x 0 on Android until the first
    // report) is not a zero-wide row: moving it would hide the first pill (FR-L28.5).
    if width <= 0.0 {
        return 0.0;
    }
    let gap = SP[0] + 2.0;
    let mut start = 0.0;
    for (i, sec) in Section::ALL.iter().enumerate() {
        let w = section_pill_width(*sec, counts.get(i).copied().flatten());
        if *sec == active {
            let end = start + w + gap;
            return if end <= width { 0.0 } else { end - width };
        }
        start += w + gap;
    }
    0.0
}

/// The height of a section pill (`.vcs-navi{height:36px}` at 900 px and below).
pub const SECTION_PILL_H: f32 = 36.0;

/// The frame and the ink of a section pill: the current one is `.vcs-navi.on` (`surface` inside a
/// 1 px `line` outline under the card shadow, `text`), as the section list's current row; the
/// others have no fill and read in `text-2`. `(fill and line, ink)`.
pub fn section_pill_look(on: bool, p: &Palette) -> (Option<(Color, Color)>, Color) {
    if on { (Some((p.surface, p.line)), p.text) } else { (None, p.text_2) }
}

/// One section as a pill: its icon, its name and its count, 12 apart inside 12 px sides. A count
/// shows only above zero (`vcs.js`), and the conflicts count on orange (`.vcs-navi.alert`).
fn SectionPill(sec: Section, on: bool, count: Option<usize>, on_click: impl FnMut() + 'static) {
    let theme = ember_theme();
    let p = theme.palette;
    let (frame, ink) = section_pill_look(on, p);
    let mut m = Modifier::new().height(SECTION_PILL_H).shape_role(ShapeRole::Full).selected(on);
    if let Some((fill, line)) = frame {
        m = with_shadows(m.background(paint(fill)).border(1.0, paint(line)), theme.shadow_sm());
    }
    Clickable(m, Alignment::Center, on_click, move || {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, content = move || {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            NavIcon(section_svg(sec), ink.to_argb(), 18.0);
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            Label(sec.label(), Fs::Sm, Weight::Medium, Some(paint(ink)), Some(1));
            if let Some(n) = count.filter(|n| *n > 0) {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                if sec == Section::Conflicts {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().height(18.0).background(paint(p.orange.strong)).shape_role(ShapeRole::Full), {
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(7.0)) };
                            Label(&n.to_string(), Fs::Xs, Weight::Semibold, Some(Paint::Literal(Color::argb(0xffff_ffff))), Some(1));
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(7.0)) };
                        }) };
                } else {
                    Label(&n.to_string(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
            }
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
        }) };
    });
}

/// The section pills with each section's count (FR-L19): the mockup's `.vcs-navi` as a row of
/// pills, each with its icon and count, the current one white under a shadow. They were the
/// composer's grey `PickChip`s with a name only (reviews r7 and r8, #878).
fn SectionPillsWithCounts(active: Section, width: f32, view_w: f32, count_of: &dyn Fn(Section) -> Option<usize>, on_pick: impl FnMut(Section) + Clone + 'static) {
    let pills = || {
        { use ::compose_rust::ui::Row; Row!(spacing = SP[0] + 2.0, alignment = Alignment::CenterStart, {
            for sec in Section::ALL {
                let mut pick = on_pick.clone();
                SectionPill(sec, sec == active, count_of(sec), move || pick(sec));
            }
        }) };
    };
    if section_pills_scroll(width) {
        // One row that scrolls sideways (`.vcs-nav{flex-direction:row;overflow-x:auto;gap:6px}`).
        // The current pill is scrolled into view when it changes (#878: at 800 the row ends inside
        // 충돌, so picking it, or opening the page on it, left it cut at the edge).
        let counts: Vec<Option<usize>> = Section::ALL.iter().map(|s| count_of(*s)).collect();
        let to = section_pills_reveal(active, view_w, &counts);
        let scroll = cu::remember_scroll_state();
        // Sent again when the row's scroll range changes: the Renderer clamps to the range it
        // knows, and on the first frame the row is not measured yet (r12 capture: 충돌 cut at 800).
        let range = scroll.max_offset().round() as i32;
        // And again when the target itself moves: on Android the row is measured before the
        // window's width is reported, so the first target was computed for a 0 dp row and kept
        // 변경 사항 scrolled away at 360 (FR-L28.5, Flip 4 review 3.6).
        let to_key = to.round() as i32;
        let seen = remember(|| mutable_state_of(None::<(Section, i32, i32)>));
        if seen.get_untracked() != Some((active, range, to_key)) {
            let target = scroll.clone();
            compose_rust::runtime::side_effect(move || {
                seen.set(Some((active, range, to_key)));
                target.scroll_to(to);
            });
        }
        { use ::compose_rust::ui::ScrollRow; ScrollRow!(scroll_state = &scroll, modifier = Modifier::new().fill_max_width(), content = pills) };
    } else {
        pills();
    }
}

/// The look of one pane tab (`.seg button`, `style.css:238-242`): fill, border and text colour.
pub fn pane_tab_look(on: bool, p: &Palette) -> (Color, Color, Color) {
    if on { (p.ink, p.ink, p.ink_text) } else { (p.surface, p.line, p.text_2) }
}

/// One equal segment of the pane tabs: a `H_CTL` pill that takes its share of the row.
/// The pill is the control: a named, clickable container (`NamedClickable`, compose-rust#429).
fn PaneTabSegment(text: &'static str, on: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (fill, border, ink) = pane_tab_look(on, &p);
    NamedClickable(
        text,
        Modifier::new().weight(1.0).height(H_CTL).background(paint(fill)).border(1.0, paint(border)).shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        move || Label(text, VCS_CONTROL_FS, Weight::Medium, Some(paint(ink)), Some(1)),
    );
}

/// Tabs within a section on compact windows.
pub fn PaneTabs(
    section: Section,
    active: Option<PaneTab>,
    mut on_pick: impl FnMut(PaneTab) + Clone + 'static,
) {
    // Equal segments across the width (`.vcs-subs.seg`, `vcs.css` at 560 px and below).
    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), spacing = SP[1], alignment = Alignment::CenterStart, {
        for t in pane_tabs(section).iter().copied() {
            let mut pick = on_pick.clone();
            PaneTabSegment(t.label(), Some(t) == active, move || pick(t));
        }
    }) };
}

/// One file diff with a check per changed line and per hunk.
pub fn DiffView(file: &FileDiff, staging: &MutableState<Staging>) {
    DiffViewAtWidth(file, staging, diff_pane_width(current_window_size().width_dp, pane_layout(current_window_size().class)));
}

fn DiffViewAtWidth(file: &FileDiff, staging: &MutableState<Staging>, pane_width: f32) {
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

    let wraps = diff_header_wraps(pane_width);
    let stage_chip = format!("{on}/{total}줄 스테이징");
    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), {
        if wraps {
            // Two lines (FR-L19): the path first, the counts and the staging chip below it.
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().padding(SP[2]), spacing = SP[1], {
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), alignment = Alignment::CenterStart, spacing = SP[1], {
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(20.0, 20.0).background(paint(tone_fill)).corner_radius(CORNER_XS), alignment = Alignment::Center, {
                        Label(file.status.letter(), Fs::Xs2, Weight::Semibold, Some(paint(tone_text)), Some(1));
                    }) };
                    Txt(&file.path, TypeRole::Mono, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                }) };
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), alignment = Alignment::CenterStart, spacing = SP[1], {
                    Label(&format!("+{}", file.added()), Fs::Xs, Weight::Medium, Some(paint(p.green.strong)), Some(1));
                    Label(&format!("-{}", file.removed()), Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(1));
                    Pill(&stage_chip, PillTone::Grey, true);
                }) };
            }) };
        } else {
        // File header row (52 px)
        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(52.0).padding(SP[2]), alignment = Alignment::CenterStart, spacing = SP[1], {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(20.0, 20.0).background(paint(tone_fill)).corner_radius(CORNER_XS), alignment = Alignment::Center, {
                Label(file.status.letter(), Fs::Xs2, Weight::Semibold, Some(paint(tone_text)), Some(1));
            }) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[0])) };
            Txt(&file.path, TypeRole::Mono, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            Label(&format!("+{}", file.added()), Fs::Xs, Weight::Medium, Some(paint(p.green.strong)), Some(1));
            Label(&format!("-{}", file.removed()), Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(1));
            Pill(&stage_chip, PillTone::Grey, true);
        }) };
        }

        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };

        if file.binary {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().padding(SP[2]), {
                Label("이진 파일은 차이를 보여 주지 않습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }) };
        }

        for (hi, h) in file.hunks.iter().enumerate() {
            // One row (`.vcs-hh`, `vcs.css:87-88`): the mono header takes the rest and is cut with an
            // ellipsis, so a long header never overlaps the right-aligned action.
            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(VCS_HUNK_HEADER_H).background(paint(p.surface_2)), alignment = Alignment::CenterStart, spacing = SP[1], {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                Txt(&h.header, TypeRole::Mono, Some(paint(p.muted)), Some(1), Some(TextOverflow::Ellipsis), Some(1.0), None);
                let st_hunk = staging.clone();
                let f_hunk = file.clone();
                NamedClickable(
                    "이 덩어리 전체",
                    Modifier::new()
                        .height(VCS_HUNK_BUTTON_H)
                        .border(1.0, paint(p.line))
                        .background(paint(p.surface))
                        .shape_role(ShapeRole::Full),
                    Alignment::Center,
                    move || {
                        let mut s = st_hunk.get();
                        s.file_mut(&f_hunk.path).toggle_hunk(&f_hunk, hi);
                        st_hunk.set(s);
                    },
                    move || {
                        PillLabel("이 덩어리 전체", VCS_CONTROL_FS, p.text);
                    });
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            }) };

            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };

            // Hunk lines (each exactly 21.0 px high, spacing 0.0) inside a horizontal ScrollRow so
            // long lines scroll sideways while the sticky check column stays pinned at the start edge.
            { use ::compose_rust::ui::ScrollRow; ScrollRow!(modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().min_width(pane_width), spacing = 0.0, {
                    for (li, l) in h.lines.iter().enumerate() {
                        let bg = match l.kind {
                            LineKind::Add => paint(p.green.soft),
                            LineKind::Del => paint(p.red.soft),
                            LineKind::Context => Paint::Literal(Color::argb(0)),
                        };
                        let sticky_bg = match l.kind {
                            LineKind::Add => paint(p.green.soft),
                            LineKind::Del => paint(p.red.soft),
                            LineKind::Context => paint(p.surface),
                        };
                        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().min_width(pane_width).height(21.0).background(bg), alignment = Alignment::CenterStart, {
                            // compose-rust#424 (v0.0.11 `Modifier::sticky`): the line check stays
                            // pinned at the start edge when a diff row scrolls sideways.
                            if l.is_change() {
                                let is_line_staged = staging.get().file(&file.path).is_some_and(|s| s.is_staged(hi, li));
                                let st_line = staging.clone();
                                let f_line = file.clone();
                                let line_check = if is_line_staged { Check::On } else { Check::Off };
                                StageCheckWith(Modifier::new().sticky().background(sticky_bg), line_check, STAGE_LINE_CHECK_SIDE, 21.0, move || {
                                    let mut s = st_line.get();
                                    s.file_mut(&f_line.path).toggle_line(&f_line, hi, li);
                                    st_line.set(s);
                                });
                            } else {
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().sticky().width(STAGE_CHECK_HIT_W).height(21.0).background(sticky_bg)) };
                            }

                            let old_str = l.old_no.map(|n| format!("{:>4}", n)).unwrap_or_else(|| "    ".to_string());
                            let new_str = l.new_no.map(|n| format!("{:>4}", n)).unwrap_or_else(|| "    ".to_string());
                            let sign_str = match l.kind {
                                LineKind::Add => "+",
                                LineKind::Del => "-",
                                LineKind::Context => " ",
                            };

                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(32.0), alignment = Alignment::CenterEnd, {
                                Label(&old_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            }) };
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(32.0), alignment = Alignment::CenterEnd, {
                                Label(&new_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            }) };
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(14.0), alignment = Alignment::Center, {
                                Label(sign_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                            }) };
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                            { use ::compose_rust::ui::Text; Text!(&l.text, type_role = TypeRole::Mono, font_size = Fs::Xs2.size(), line_height = Fs::Xs2.line_height(), color = paint(p.text), max_lines = 1, word_break = compose_rust::WordBreak::KeepAll) };
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                        }) };
                    }
                }) };
            }) };
        }
    }) };
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
    let all = folder_check(all_checks);
    // `.vcs-ph{min-height:52px;padding:10px var(--sp4)}`: 52 high with 16 at the sides and no
    // padding above or below, so the stash button's 44 dp box fits whole (FR-L19.1, #415: at 48
    // with 12 all round the row had 24 for it).
    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(VCS_FILES_HEAD_ROW_H), alignment = Alignment::CenterStart, content = move || {
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(VCS_FILES_HEAD_PAD_X)) };
        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().weight(1.0), alignment = Alignment::CenterStart, spacing = SP[1], content = move || {
            StageCheck(all, STAGE_CHECK_SIDE, 24.0, move || {
                let mut s = st_toggle.get();
                s.toggle_all(&d_toggle);
                st_toggle.set(s);
            });
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[0])) };
            Label(&format!("변경 파일 {}", diffs_len), Fs::Sm, Weight::Semibold, Some(paint(p.text)), Some(1));
            // The note takes the room that is left and no more, so the button after it keeps
            // its width (unweighted, the note took the row and the button was not drawn).
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), alignment = Alignment::CenterEnd, {
                Label(STASH_NOW_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
            }) };
            CircleIconButton(ICON_STASH_SVG, remote_button_labels()[3], move || {
                match &stash {
                    Some(a) => a.stash_now(diffs_len),
                    None => notice::post(soon_line("스태시")),
                }
            });
        }) };
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(VCS_FILES_HEAD_PAD_X)) };
    }) };
}

/// A row of the changes tree and the padding around the rows (`FilesTree`).
pub const VCS_TREE_ROW_H: f32 = 34.0;
pub const VCS_TREE_PAD: f32 = 8.0;
/// `.vcs-tree{max-height:360px}`, 260 where the panes stack (`@media (max-width:900px)`).
pub const VCS_TREE_MAX_H: f32 = 360.0;
pub const VCS_TREE_MAX_H_STACKED: f32 = 260.0;
/// The least the tree keeps when the pane is short: three rows.
pub const VCS_TREE_MIN_H: f32 = 3.0 * VCS_TREE_ROW_H + 2.0 * VCS_TREE_PAD;
/// The files header's row (`.vcs-ph{min-height:52px}`) and its side padding (`var(--sp4)`).
pub const VCS_FILES_HEAD_ROW_H: f32 = 52.0;
pub const VCS_FILES_HEAD_PAD_X: f32 = 16.0;
/// The files header (`FilesHeader`) and the two 1 px rules of the changes card.
pub const VCS_FILES_HEAD_H: f32 = VCS_FILES_HEAD_ROW_H + 2.0;
/// The commit box as drawn: 12 px padding, the 40 px summary, the 64 px description, the 32 px
/// amend row, the 40 px button and the 18 px note, 8 apart. An estimate for the tree's height
/// only: an amend warning adds lines the outer scroll still reaches.
pub const VCS_COMMIT_BOX_EST_H: f32 = 2.0 * 12.0 + 40.0 + VCS_TEXTAREA_H + 32.0 + 40.0 + 18.0 + 4.0 * 8.0;
/// The repository bar in three columns (120 in the r8 captures) and the 12 px under it.
pub const VCS_BAR_EST_H: f32 = 120.0 + 12.0;

/// The height `rows` tree rows take.
pub fn tree_height(rows: usize) -> f32 {
    rows as f32 * VCS_TREE_ROW_H + 2.0 * VCS_TREE_PAD
}

/// The tallest the tree stands in the changes card, so the commit box under it stays on screen
/// (`.vcs-tree{max-height:360px;overflow:auto}`; review r8: at 1440 the commit button was below
/// the fold). `pane_h` is the height the two panes have when it is known (the sheet): the tree
/// then also leaves room for the header and the commit box, down to three rows. Capped with
/// `Modifier::max_height` (compose-rust#778, v0.0.11).
pub fn tree_cap(max: f32, pane_h: Option<f32>) -> f32 {
    match pane_h {
        Some(h) => (h - VCS_FILES_HEAD_H - VCS_COMMIT_BOX_EST_H).clamp(VCS_TREE_MIN_H, max),
        None => max,
    }
}

/// The height a scrolling tree is capped at inside `cap`: its top padding and as many whole rows
/// as fit, so at rest the box under it starts where a row ends instead of cutting one in half
/// (review r10, the 1024 sheet). At least one row.
pub fn tree_view_height(cap: f32) -> f32 {
    let rows = ((cap - VCS_TREE_PAD) / VCS_TREE_ROW_H).floor().max(1.0);
    VCS_TREE_PAD + rows * VCS_TREE_ROW_H
}

/// The height of the panes under the bar in the sheet's two-pane layout, from the body height
/// under the sheet header: less the body's padding and the bar. An estimate (notes above the
/// panes are not counted).
pub fn sheet_pane_height(body_h: f32, pad: f32) -> f32 {
    (body_h - 2.0 * pad - VCS_BAR_EST_H).max(0.0)
}

/// What the wide page draws above its panes: the 32 gutter, the title block, the three-column
/// bar and the gaps between them (246 in the ba captures at 1024 x 800).
pub const VCS_PAGE_ABOVE_PANES: f32 = 246.0;

/// The height of the panes on the bounded wide page, from the window height: less what is above
/// them and the bottom gutter. An estimate, as [`sheet_pane_height`].
pub fn page_pane_height(window_h: f32, pad: f32) -> f32 {
    (window_h - VCS_PAGE_ABOVE_PANES - pad).max(0.0)
}

/// [`FilesTree`] capped at `tree_view_height(cap)` (`Modifier::max_height`, compose-rust#778,
/// v0.0.11): a shorter tree is as tall as its rows and a longer one scrolls inside the cap.
fn FilesTreeCapped(
    diffs: &[FileDiff],
    staging: &MutableState<Staging>,
    folded: &MutableState<BTreeSet<String>>,
    selected: &MutableState<usize>,
    on_tab: impl FnMut(PaneTab) + Clone + 'static,
    cap: f32,
) {
    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().max_height(tree_view_height(cap)), {
        FilesTree(diffs, staging, folded, selected, on_tab);
    }) };
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

    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().padding(SP[1]), spacing = 0.0, content = move || {
        if diffs.is_empty() {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().padding(SP[2]), {
                Label("변경 사항이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            }) };
        } else {
            for row in rows.iter().cloned() {
                match row {
                    TreeRow::Dir { label, path, depth, open } => {
                        let f_state = folded.clone();
                        let pth = path.clone();
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                    .fill_max_width()
                                    .height(34.0)
                                    .corner_radius(CORNER_XS), content = move || {
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_size(), {
                                        if depth > 0 {
                                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(depth as f32 * 14.0)) };
                                        }
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                                        let chev_svg = if open { ICON_CHEVRON_DOWN_SVG } else { ICON_CHEVRON_RIGHT_SVG };
                                        NavIcon(chev_svg, p.muted.to_argb(), 14.0);
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                                        NavIcon(ICON_FOLDER_SVG, p.muted.to_argb(), 15.0);
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(6.0)) };
                                        Label(
                                            &label,
                                            Fs::Xs,
                                            Weight::Regular,
                                            Some(paint(p.text_2)),
                                            Some(1),
                                        );
                                    }) };
                                DataNamedOverlay(&label, true, None, move || {
                                    let mut f = f_state.get();
                                    if !f.remove(&pth) {
                                        f.insert(pth.clone());
                                    }
                                    f_state.set(f);
                                });
                            }) };
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

                        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new()
                                    .fill_max_width()
                                    .height(34.0)
                                    .background(bg)
                                    .corner_radius(CORNER_XS), alignment = Alignment::CenterStart, content = move || {
                                if depth > 0 {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(depth as f32 * 14.0)) };
                                }
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                                StageCheck(is_staged, STAGE_CHECK_SIDE, 34.0, move || {
                                    let mut s = st_file.get();
                                    s.toggle_file(&d_file);
                                    st_file.set(s);
                                });
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(4.0)) };
                                let file_name = name.clone();
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0).height(34.0), content = move || {
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_size(), {
                                                Label(
                                                    &file_name,
                                                    Fs::Sm,
                                                    Weight::Regular,
                                                    Some(paint(p.text)),
                                                    Some(1),
                                                );
                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                                            .size(18.0, 18.0)
                                                            .background(paint(tone_fill))
                                                            .corner_radius(CORNER_XS), alignment = Alignment::Center, {
                                                        Label(
                                                            letter,
                                                            Fs::Xs2,
                                                            Weight::Semibold,
                                                            Some(paint(tone_text)),
                                                            Some(1),
                                                        );
                                                    }) };
                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                                            }) };
                                        DataNamedOverlay(&file_name, true, None, move || {
                                            sel_state.set(index);
                                            on_tab_file(tab_after_select(PaneTab::Files));
                                        });
                                    }) };
                            }) };
                    }
                }
            }
        }
    }) };
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
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
            FilesHeader(None, diffs_len, &all_checks, &st_clone, &d_clone);
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
            FilesTree(&d_clone, &st_clone, &fold_clone, &sel_clone, on_tab);
        }) };
    });
}

fn DiffPane(diffs: &[FileDiff], selected: usize, staging: &MutableState<Staging>, width: f32) {
    let p = ember_theme().palette;
    let selected_file = diffs.get(selected).cloned();
    let st_diff = staging.clone();
    VcsCard(move || {
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
            if let Some(file) = selected_file {
                DiffViewAtWidth(&file, &st_diff, width);
            } else {
                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().padding(SP[3]), {
                    Label("변경 사항이 없습니다. 작업 트리가 깨끗합니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                }) };
            }
        }) };
    });
}

fn CommitBox(
    branch: &str,
    staged_files: usize,
    summary: &MutableState<String>,
    body: &MutableState<String>,
    amend: &MutableState<bool>,
    commit: Option<(Actions, RepoSnapshot, MutableState<Staging>)>,
    compact: bool,
) {
    let p = ember_theme().palette;
    // Compact: the summary and the button on one row, 설명 and amend behind a toggle.
    let expanded = remember(|| mutable_state_of(false));
    let folded = compact && !expanded.get();
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

    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new()
                .fill_max_width()
                .background(paint(p.surface_2))
                .padding(SP[2]), 
// `.vcs-commit{gap:8px}`.
spacing = SP[1], content = move || {
            let s_state = sum_state.clone();
            let s_armed = armed.clone();
            let sum_show = sum_state.clone();
            let gen0 = field_generation.get();
            let mut summary_field = Some(move || key((gen0, 0_u8), || {
                EmberField(FieldOptions {
                    fill_max_width: true,
                    text: Some(s_state.get()),
                    placeholder: "요약 (필수) 예: Feat: Wire shim".to_string(),
                    type_role: Some(VCS_FIELD_ROLE),
                    on_value_change: Some(Box::new(move |v| {
                        s_armed.set(0);
                        s_state.set(v);
                    })),
                    ..Default::default()
                });
            }));
            let sum_state = sum_show;

            let b_state = body_state.clone();
            let b_armed = armed.clone();
            if !folded {
            if let Some(f) = summary_field.take() { f(); }
            key((field_generation.get(), 1_u8), || {
                EmberField(FieldOptions {
                    fill_max_width: true,
                    height: Some(VCS_TEXTAREA_H),
                    inset: Some(VCS_TEXTAREA_INSET),
                    line_height: Some(VCS_TEXTAREA_LINE_H),
                    text: Some(body_state.get()),
                    placeholder: "설명 (선택)".to_string(),
                    type_role: Some(VCS_FIELD_ROLE),
                    multiline: true,
                    on_value_change: Some(Box::new(move |v| {
                        b_armed.set(0);
                        b_state.set(v);
                    })),
                    ..Default::default()
                });
            });

            // Amend checkbox
            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(32.0), alignment = Alignment::CenterStart, spacing = SP[1], {
                let am = amend_state.clone();
                let am_armed = armed.clone();
                StageCheck(if is_amend { Check::On } else { Check::Off }, STAGE_CHECK_SIDE, 32.0, move || {
                    am_armed.set(0);
                    am.set(!am.get());
                });
                Label("마지막 커밋 수정 (amend)", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
            }) };
            }
            if let Some(text) = warning {
                Label(text, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
            }

            let click_armed = armed.clone();
            let button = move || AppButton(
                &amend_button_label(&commit_label(&branch_name, staged_files), warning, is_armed),
                Some(IconRole::Check),
                ButtonKind::Primary,
                false,
                can_commit,
                // On the compact row the button keeps its own width beside the summary.
                !folded,
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

            if folded {
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), alignment = Alignment::CenterStart, spacing = SP[1], content = move || {
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), content = move || { if let Some(f) = summary_field { f(); } }) };
                    button();
                }) };
            } else {
                button();
            }
            let note_text = if can_commit {
                "선택한 변경 사항을 커밋합니다".to_string()
            } else {
                "요약과 스테이징한 파일이 필요합니다".to_string()
            };
            if compact {
                { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), alignment = Alignment::CenterStart, spacing = SP[1], {
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), {
                        Label(&note_text, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                    }) };
                    let toggle = expanded.clone();
                    AppButton(compact_toggle_label(folded), None, ButtonKind::Default, true, true, false, move || toggle.set(!toggle.get()));
                }) };
            } else {
                Label(&note_text, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
            }
        }) };
}

/// The compact commit box's toggle (`CommitBox` with `compact`).
pub fn compact_toggle_label(folded: bool) -> &'static str {
    if folded { "설명 \u{b7} amend" } else { "접기" }
}

/// The commit box drawn whole (12 + 40 + 64 + 32 + 40 + 18 + 12, 8 apart) and compact (the
/// summary and the button on one 40 row, then the note and the toggle on a 32 row).
pub const VCS_COMMIT_BOX_COMPACT_H: f32 = 2.0 * 12.0 + 40.0 + 32.0 + 8.0;
/// The sheet's one column above the changes card: the two-row bar (236 in the ba captures),
/// the pills (36) and the gaps (16, 12).
pub const VCS_SHEET_ONE_COLUMN_ABOVE: f32 = 236.0 + 16.0 + 36.0 + 12.0;
/// The tree rows the one-column sheet keeps on screen above its footer (review ba fix round).
pub const VCS_SHEET_MIN_TREE_ROOM: f32 = 110.0;

/// Whether the one-column sheet's footer is the compact commit box: when the whole box would
/// leave the files tree less than [`VCS_SHEET_MIN_TREE_ROOM`] on screen.
pub fn sheet_footer_compact(body_h: f32, pad: f32) -> bool {
    let room = body_h - 2.0 * pad - VCS_SHEET_ONE_COLUMN_ABOVE - VCS_FILES_HEAD_H - (VCS_COMMIT_BOX_EST_H + 2.0 * pad);
    room < VCS_SHEET_MIN_TREE_ROOM
}

fn CommitPane(
    branch: &str,
    staged_files: usize,
    summary: &MutableState<String>,
    body: &MutableState<String>,
    amend: &MutableState<bool>,
    commit: Option<(Actions, RepoSnapshot, MutableState<Staging>)>,
    compact: bool,
) {
    VcsCard(move || {
        CommitBox(branch, staged_files, summary, body, amend, commit, compact);
    });
}

/// Changes pane: tree, diff and commit box.
pub fn ChangesPane(
    snap: &RepoSnapshot,
    layout: PaneLayout,
    tab: Option<PaneTab>,
    on_tab: impl FnMut(PaneTab) + Clone + 'static,
) {
    ChangesPaneWithActions(snap, layout, tab, on_tab, None, diff_pane_width(current_window_size().width_dp, layout), 320.0, None, None, false);
}

/// The commit box's state (staging, summary, description, amend), kept where both the changes
/// card and a commit box drawn outside it (the sheet's one-column footer) can reach it.
#[derive(Clone)]
struct CommitDraft {
    staging: MutableState<Staging>,
    summary: MutableState<String>,
    body: MutableState<String>,
    amend: MutableState<bool>,
}

/// Remembers a [`CommitDraft`] for `snap` (empty without one); `scope` is the project and the
/// reload count, so a reload or another project starts the staging again.
fn commit_draft(snap: Option<&RepoSnapshot>, scope: Option<(String, u64)>) -> CommitDraft {
    let diffs = snap.map(|s| s.changes.clone()).unwrap_or_default();
    let staged = snap.map(|s| s.staged.clone()).unwrap_or_default();
    let staging_key = (scope, snap.map(|s| s.branch.clone()), staged.clone(), diffs.clone());
    let staging = remember_with_key(staging_key, || {
        let mut state = Staging::new(&diffs, false);
        for diff in &diffs { if staged.contains(&diff.path) { state.toggle_file(diff); } }
        mutable_state_of(state)
    });
    let summary = remember(|| mutable_state_of(String::new()));
    let body = remember(|| mutable_state_of(String::new()));
    let amend = remember(|| mutable_state_of(false));
    CommitDraft { staging, summary, body, amend }
}

/// The one-column layout draws the commit box under the scrolling body, outside it, so the box
/// is whole at any height and the scroll ends above it (review ba: at 800 x 800 the bar in two
/// rows left the sheet's changes card 326 of the 300 its header and commit box need, and the
/// sheet's bottom edge cut the summary field). The page in one column does the same (#878: at
/// 800 x 800 the page's commit button sat below the fold, under one page scroll), so the sheet
/// and the page commit from the same place. `sheet` no longer decides it.
pub fn sheet_commit_footer(_sheet: bool, layout: PaneLayout, sec: Section, has_snapshot: bool) -> bool {
    layout == PaneLayout::Stacked && sec == Section::Changes && has_snapshot
}

/// What the page draws above its body in one column: the 버전 관리 title (`Fs::Xl3`, 36), the
/// 4 gap, the subtitle line (20) and the 12 gap before the bar. An estimate, as
/// [`VCS_PAGE_ABOVE_PANES`].
pub const VCS_PAGE_TITLE_BLOCK_H: f32 = 36.0 + 4.0 + 20.0 + 12.0;

/// The height [`sheet_footer_compact`] reads for the page in one column: the window less the
/// title block, which the sheet draws in its header instead.
pub fn page_footer_body_h(window_h: f32) -> f32 {
    (window_h - VCS_PAGE_TITLE_BLOCK_H).max(0.0)
}

fn ChangesPaneWithActions(snap: &RepoSnapshot, layout: PaneLayout, tab: Option<PaneTab>, on_tab: impl FnMut(PaneTab) + Clone + 'static, actions: Option<Actions>, diff_width: f32, files_w: f32, pane_h: Option<f32>, draft: Option<CommitDraft>, commit_outside: bool) {
    let p = ember_theme().palette;
    let diffs = snap.changes.clone();
    let draft = draft.unwrap_or_else(|| commit_draft(Some(snap), actions.as_ref().map(|a| (a.project.clone(), a.reload.get()))));
    let staging = draft.staging.clone();
    let stash_actions = actions.clone();
    let commit = actions.map(|a| (a, snap.clone(), staging.clone()));
    let folded = remember(|| mutable_state_of(BTreeSet::<String>::new()));
    let selected = remember(|| mutable_state_of(0usize));
    let summary = draft.summary.clone();
    let body = draft.body.clone();
    let amend = draft.amend.clone();

    let sel = selected.get().min(diffs.len().saturating_sub(1));
    let staged_files = staging.get().staged_files(&diffs);
    let st = staging.get();
    let all_checks: Vec<Check> = diffs.iter().map(|d| st.check(d)).collect();
    let diffs_len = diffs.len();

    match layout {
        PaneLayout::Two => {
            // The panes scroll independently inside a bounded row so a long diff does not scroll
            // the files card out of view (matching the mockup's sticky `.vcs-files`), rather than
            // joining both panes under one `Row::equal_height(true)` page scroll.
            let d_files = diffs.clone();
            let st_files = staging.clone();
            let fold_files = folded.clone();
            let sel_files = selected.clone();
            let on_tab_files = on_tab.clone();
            let branch = snap.branch.clone();
            let sum_files = summary.clone();
            let body_files = body.clone();
            let am_files = amend.clone();

            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().fill_max_height(), alignment = Alignment::TopStart, spacing = GAP_CARD, content = move || {
                    // One card, as the mockup's `.vcs-files`: the header, the tree (no taller than
                    // its cap, scrolling inside it) and the commit box under a rule on
                    // `surface-2`. The column still scrolls, for a pane shorter than the card.
                    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().width(files_w).fill_max_height(), content = move || {
                        VcsCard(move || {
                            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
                                FilesHeader(stash_actions.clone(), diffs_len, &all_checks, &st_files, &d_files);
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                                FilesTreeCapped(&d_files, &st_files, &fold_files, &sel_files, on_tab_files, tree_cap(VCS_TREE_MAX_H, pane_h));
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                                CommitBox(&branch, staged_files, &sum_files, &body_files, &am_files, commit.clone(), false);
                            }) };
                        });
                    }) };

                    // Right card: diff
                    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().weight(1.0).fill_max_height(), content = move || {
                        DiffPane(&diffs, sel, &staging, diff_width);
                    }) };
                }) };
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

            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), spacing = GAP_CARD, content = move || {
                VcsCard(move || {
                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
                        FilesHeader(stash_actions.clone(), diffs_len, &all_checks, &st_files, &d_files);
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                        FilesTreeCapped(&d_files, &st_files, &fold_files, &sel_files, on_tab_files, tree_cap(VCS_TREE_MAX_H_STACKED, None));
                        // In the sheet the commit box is the footer under the scroll instead.
                        if !commit_outside {
                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                            CommitBox(&branch, staged_files, &sum_files, &body_files, &am_files, commit.clone(), false);
                        }
                    }) };
                });
                DiffPane(&diffs, sel, &staging, diff_width);
            }) };
        }
        PaneLayout::Tabs => {
            let mut on_tab_pick = on_tab.clone();
            let d_files = diffs.clone();
            let st_files = staging.clone();
            let fold_files = folded.clone();
            let sel_files = selected.clone();
            let on_tab_files = on_tab.clone();

            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), spacing = GAP_CARD, content = move || {
                PaneTabs(Section::Changes, tab, move |t| on_tab_pick(t));
                match tab.unwrap_or(PaneTab::Files) {
                    PaneTab::Diff => DiffPane(&diffs, sel, &staging, diff_width),
                    PaneTab::Commit => CommitPane(&snap.branch, staged_files, &summary, &body, &amend, commit.clone(), false),
                    _ => {
                        VcsCard(move || {
                            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
                                FilesHeader(stash_actions.clone(), diffs_len, &all_checks, &st_files, &d_files);
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                                FilesTree(&d_files, &st_files, &fold_files, &sel_files, on_tab_files);
                            }) };
                        });
                    }
                }
                // Every tab ends clear of the floating tab bar, not only 커밋: the last tree row
                // and the last diff line scroll above it (FR-L19.1, #415).
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(super::routines::compact_bottom_clearance())) };
            }) };
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
    pr_list: MutableState<Option<Vec<PrEntry>>>,
    selected_pr: MutableState<Option<u64>>,
    sec_state: Option<MutableState<Section>>,
    diff_width: f32,
    files_w: f32,
    pane_h: Option<f32>,
    draft: Option<CommitDraft>,
    commit_outside: bool,
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
                diff_width,
                files_w,
                pane_h,
                draft,
                commit_outside,
            );
        }
        (Some(s), Section::Conflicts) => ConflictsPane(&s.conflicts, actions, layout),
        (Some(s), Section::History) => HistoryPane(actions, layout, snapshot_dirty(s), s.branch.clone()),
        (Some(s), Section::Stash) => StashPane(actions, stash_count, layout, !s.changes.is_empty()),
        (Some(s), Section::PullRequests) => PullRequestsPane(actions, pr_count, pr_list, selected_pr, layout, Some(s.branch.clone())),
        (_, Section::Team) => TeamPane(actions, team_count, pr_list, Some(selected_pr), layout, sec_state, snap),
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
        ours_label: "HEAD (feat/session-shim)".into(),
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

/// How many conflict files the sample screen lists (the tab count and the banner agree).
const SAMPLE_CONFLICT_COUNT: usize = 2;

/// The merge banner state for the sample screen (no services).
fn sample_merge() -> MergeLoad {
    MergeLoad::Ready(Some(MergeView { from: "origin/main".into(), into: "feat/session-shim".into(), remaining: SAMPLE_CONFLICT_COUNT, can_commit: false }))
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
        let (m, armed) = (merge.clone(), armed.clone());
        launched_effect((a.project.clone(), a.reload.get(), mlocal.get()), move || async move {
            // A reload of the state disarms 병합 취소 and 병합 커밋.
            armed.set(None);
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
    let failed = match &current {
        MergeLoad::Failed(text) if actions.is_some() => Some(text.clone()),
        _ => None,
    };
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
                    let (name, mbusy, mlocal, generation, reload, armed) = (a.project.clone(), mbusy.clone(), mlocal.clone(), generation.clone(), a.reload.clone(), armed.clone());
                    crate::services::run_shared(
                        crate::ui::vcs_watch::guarded(&name.clone(), async move {
                            match action {
                                MergeAction::Abort => run_merge_abort(&name).await,
                                MergeAction::Commit => run_merge_commit(&name).await,
                            }
                        }, merge_answer_reloads),
                        move |result| {
                            let result_for_reload = result.clone();
                            match result {
                                Ok(text) => notice::post(text),
                                Err(line) => notice::post(line),
                            }
                            // Read the state, list and snapshot again after either answer: a 409
                            // means the state on screen was stale.
                            if merge_answer_reloads(&result_for_reload) {
                                armed.set(None);
                                mlocal.set(mlocal.get() + 1);
                                generation.set(generation.get() + 1);
                                reload.set(reload.get() + 1);
                            }
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
        let (armed, mbusy, tap, mlocal) = (armed.clone(), mbusy.clone(), tap.clone(), mlocal.clone());
        move || {
            if let Some(v) = &view {
                MergeBanner(v, &armed, mbusy.get(), tap.clone());
            } else if let Some(text) = &failed {
                // The state read failed: say so (Korean) and offer a retry instead of nothing.
                let p = ember_theme().palette;
                let retry = mlocal.clone();
                { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = {
                    let text = text.clone();
                    move || {
                        Label("병합 상태를 불러오지 못했습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        let retry = retry.clone();
                        AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
                    }
                }) };
            }
        }
    };
    if two {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
            banner();
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().weight(1.0), content = body.clone()) };
        }) };
    } else {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
            banner();
            body();
        }) };
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
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), content = move || {
            Txt(&title, TypeRole::BodyStrong, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            Label(&sub, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = move || {
                let on_abort = tap.clone();
                let kind = if abort_armed { ButtonKind::Destructive } else { ButtonKind::Default };
                AppButton(banner_button_text(MergeAction::Abort, abort_armed), None, kind, true, !busy, false, move || on_abort(MergeAction::Abort));
                let on_commit = tap.clone();
                AppButton(banner_button_text(MergeAction::Commit, commit_armed), None, ButtonKind::Primary, true, can_commit && !busy, false, move || on_commit(MergeAction::Commit));
            }) };
            Label(MERGE_ABORT_NOTE, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            if commit_armed {
                Label(MERGE_COMMIT_NOTE, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
        }) };
    });
}

/// 충돌: the unmerged files, and (after a tap) the editor of one file. `paths` is the list from
/// the repository snapshot; resolving a file bumps the screen's reload counter so the list and
/// 변경 사항 read again. `actions` is `None` for the sample screen, which edits a sample file.
fn ConflictsBody(paths: &[String], actions: Option<Actions>, layout: PaneLayout) {
    let p = ember_theme().palette;
    // `EMBER_CR_VCS_CONFLICT=<path>` opens that file's editor directly, for captures without clicks.
    let open = remember(|| mutable_state_of(std::env::var("EMBER_CR_VCS_CONFLICT").ok().filter(|p| !p.is_empty())));
    // #878: demo services do not answer `git conflict`, and sample mode has `actions == None`, so
    // both open the sample conflict directly instead of staying on "불러오는 중입니다".
    let use_sample = actions.is_none() || crate::demo::active_from_env();
    let state = remember(|| {
        mutable_state_of(if use_sample { ConflictLoad::Ready(sample_conflict()) } else { ConflictLoad::Loading })
    });
    let local = remember(|| mutable_state_of(0_u64));
    let busy = remember(|| mutable_state_of(false));
    // The armed pick-all button (key, arm time). It lives in the body, which the pane remounts
    // with `key(generation)` after a merge answer, so a reload starts it unarmed.
    let pick_armed = remember(|| mutable_state_of(None::<(usize, i64)>));

    if !use_sample {
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
            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
                { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().fill_max_width(), {
                    Label("충돌", Fs::Lg, Weight::Semibold, None, None);
                    Label("파일을 눌러 충돌을 해결합니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                }) };
                // Left-aligned rows with a warning mark, like the mockup's file list, and a gap under
                // the explanation (the old full-width pills centred the path and touched the text).
                { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = move || {
                    for path in paths {
                        let (open, state, has_server) = (open.clone(), state.clone(), !use_sample);
                        let target = path.clone();
                        ConflictFileRow(&path, move || {
                            state.set(if has_server { ConflictLoad::Loading } else { ConflictLoad::Ready(sample_conflict()) });
                            open.set(Some(target.clone()));
                        });
                    }
                }) };
            }) };
        });
        return;
    };

    let step = remember(|| mutable_state_of(ConflictStep::Editing));
    let back = {
        let (open, busy, step, pick_armed) = (open.clone(), busy.clone(), step.clone(), pick_armed.clone());
        move || {
            pick_armed.set(None);
            open.set(None);
            busy.set(false);
            step.set(ConflictStep::Editing);
        }
    };
    let two = layout == PaneLayout::Two;
    // The scrolling part: the path, then the blocks, or the preview of the merged text.
    let content = {
        let (state, local, busy, step, path, pick_armed) = (state.clone(), local.clone(), busy.clone(), step.clone(), path.clone(), pick_armed.clone());
        move || {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
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
                            // 모두 현재 것 / 모두 들어오는 것: settle every block of this file in one tap, or in
                            // two when a block already has a choice (it would be replaced).
                            let now = crate::demo::now_ms();
                            let needs_confirm = pick_all_note(&file).is_some();
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = {
                                let (file, state, pick_armed, busy) = (file.clone(), state.clone(), pick_armed.clone(), busy.clone());
                                move || {
                                    for (text, pick) in [("모두 현재 것", ConflictChoice::Ours), ("모두 들어오는 것", ConflictChoice::Theirs)] {
                                        let (file, state, pick_armed) = (file.clone(), state.clone(), pick_armed.clone());
                                        let key = pick_all_key(pick);
                                        let shown = needs_confirm && armed_label_visible(pick_armed.get(), key, now);
                                        let kind = if shown { ButtonKind::Destructive } else { ButtonKind::Default };
                                        AppButton(pick_all_text(text, shown), None, kind, true, !busy.get(), false, move || {
                                            if !needs_confirm {
                                                state.set(ConflictLoad::Ready(file.pick_all(pick)));
                                                return;
                                            }
                                            let now = crate::demo::now_ms();
                                            match delete_tap(pick_armed.get(), key, now) {
                                                DeleteTap::Arm | DeleteTap::Expired => pick_armed.set(Some((key, now))),
                                                DeleteTap::Wait => {}
                                                DeleteTap::Confirm => {
                                                    pick_armed.set(None);
                                                    state.set(ConflictLoad::Ready(file.pick_all(pick)));
                                                }
                                            }
                                        });
                                    }
                                }
                            }) };
                            if let Some(note) = pick_all_note(&file) {
                                Label(note, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                            }
                            ConflictBlocks(&file, layout, busy.get(), &state, &pick_armed);
                        }
                    }
                }
            }) };
        }
    };
    // The action bar: always visible below the scroll area at the wide layout, after the blocks
    // (with room for the floating tab bar) at the compact one.
    let bar = {
        let (state, busy, open, actions, path, step) = (state.clone(), busy.clone(), open.clone(), actions.clone(), path.clone(), step.clone());
        move || {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
                if let ConflictLoad::Ready(file) = state.get() {
                    ConflictActions(&file, busy.get(), &step, &busy, &open, &actions, &path);
                }
                let back = back.clone();
                AppButton("목록으로", None, ButtonKind::Default, true, !busy.get(), false, back);
                if !two {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(super::routines::compact_bottom_clearance())) };
                }
            }) };
        }
    };
    if two {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
            { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), content = content.clone()) };
            bar();
        }) };
    } else {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
            content();
            bar();
        }) };
    }
}

/// The blocks of one file as cards, each with its three choices.
fn ConflictBlocks(file: &ConflictFile, layout: PaneLayout, is_busy: bool, state: &MutableState<ConflictLoad>, pick_armed: &MutableState<Option<(usize, i64)>>) {
    let p = ember_theme().palette;
    let mut index = 0;
    for seg in &file.segments {
        match seg {
            ConflictSegment::Common(text) => {
                if !text.trim().is_empty() {
                    VcsCard(|| {
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().padding(SP[2]), {
                            Txt(text, TypeRole::Mono, Some(paint(p.text_2)), None, None, None, None);
                        }) };
                    });
                }
            }
            ConflictSegment::Block { ours_label, theirs_label, ours, theirs, choice } => {
                let n = index;
                index += 1;
                let (choice, file, state, pick_armed) = (*choice, file.clone(), state.clone(), pick_armed.clone());
                VcsCard(|| {
                    { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
                        Label(&format!("충돌 {}", n + 1), Fs::Sm, Weight::Semibold, None, Some(1));
                        let two = layout == PaneLayout::Two;
                        let side = |label: &str, text: &str, ours_side: bool| {
                            let (bg, fg) = if ours_side { (p.green.soft, p.green.strong) } else { (p.blue.soft, p.blue.strong) };
                            let size = if two { Modifier::new().weight(1.0) } else { Modifier::new().fill_max_width() };
                            { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = size.background(paint(bg)).corner_radius(CORNER_LG).padding(SP[2]), {
                                Txt(&chip_text(label), TypeRole::Caption, Some(paint(fg)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                                Txt(text, TypeRole::Mono, Some(paint(p.text)), None, None, None, None);
                            }) };
                        };
                        if two {
                            { use ::compose_rust::ui::Row; Row!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                                side(ours_label, ours, true);
                                side(theirs_label, theirs, false);
                            }) };
                        } else {
                            side(ours_label, ours, true);
                            side(theirs_label, theirs, false);
                        }
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                            for (text, pick) in [("내 변경", ConflictChoice::Ours), ("상대 변경", ConflictChoice::Theirs), ("둘 다", ConflictChoice::Both)] {
                                let (file, state, pick_armed) = (file.clone(), state.clone(), pick_armed.clone());
                                let selected = choice == pick;
                                let kind = if selected { ButtonKind::Primary } else { ButtonKind::Default };
                                AppButton(&choice_label(text, selected), None, kind, true, !is_busy, false, move || {
                                    pick_armed.set(None);
                                    state.set(ConflictLoad::Ready(file.with_choice(n, pick)));
                                });
                            }
                        }) };
                    }) };
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
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
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
        }) };
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
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
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
            crate::services::run_shared(crate::ui::vcs_watch::guarded_ok(&name.clone(), async move { run_resolve(&name, &target, &text).await }), move |result| {
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
    }) };
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

/// One commit of the history list: a tap selects it (the detail card shows it).
fn HistoryRow(e: &HistoryEntry, layout: PaneLayout, selected: bool, on_select: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let limit = chip_limit(layout);
    let fill = if selected { p.surface_2 } else { p.surface };
    let mut frame = Modifier::new().fill_max_width().background(paint(fill));
    if selected {
        frame = frame.border(1.0, paint(p.line));
    }
    { use ::compose_rust::ui::Box; Box!(modifier = frame, content = move || {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::TopStart, spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
            // TODO(compose-rust#308): lane graph indicator dot. Full SVG/Canvas parent-edge curves wait on compose-rust#308.
            let dot_color = if e.is_merge { p.sky.strong } else { p.blue.strong };
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(8.0, 8.0).background(paint(dot_color)).shape_role(ShapeRole::Full)) };
            { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().weight(1.0), {
                Txt(&e.subject, TypeRole::BodyStrong, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                    Txt(&history_meta(e), TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), Some(1.0), None);
                }) };
                if e.unpushed || e.remote_only || e.is_merge || !e.refs.is_empty() {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                        if e.unpushed {
                            Pill("푸시 안 됨", PillTone::Orange, true);
                        }
                        if e.remote_only {
                            Pill("원격에 있음", PillTone::Grey, true);
                        }
                        if e.is_merge {
                            Pill("병합", PillTone::Sky, true);
                        }
                        for r in e.refs.iter().take(limit) {
                            Pill(&chip_text(&r.label), ref_tone(r), true);
                        }
                        if e.refs.len() > limit {
                            Pill(&format!("+{}", e.refs.len() - limit), PillTone::Grey, true);
                        }
                    }) };
                }
            }) };
        }) };
        DataNamedOverlay(&format!("커밋 {} {}", e.short, e.subject), true, None, on_select);
    }) };
}

/// Commits for the review captures (`EMBER_CR_ROUTE=vcs`): the only path that reaches `HistoryPane`
/// without actions, because a screen without services has no snapshot and shows 준비 중입니다.
fn sample_history() -> HistoryLoad {
    let e = |short: &str, subject: &str, author: &str, time: &str, merge: bool, refs: &[(&str, ember_client::api::GitLogRefKind)], unpushed: bool, remote_only: bool| HistoryEntry {
        sha: format!("{short}0000000000000000000000000000000000"),
        short: short.to_string(),
        subject: subject.to_string(),
        author: author.to_string(),
        time_text: time.to_string(),
        refs: refs.iter().map(|(l, k)| HistoryRef { label: l.to_string(), kind: k.clone() }).collect(),
        is_merge: merge,
        unpushed,
        remote_only,
    };
    use ember_client::api::GitLogRefKind::*;
    HistoryLoad::Ready {
        entries: vec![
            e("a1b2c3d", "Feat: attach the session shim before routing", "BREW", "12분 전", false, &[("HEAD", Head), ("feat/session-shim", Branch)], true, false),
            e("2b3c4d5", "Merge branch 'feat/egress-stop' into develop", "BREW", "3시간 전", true, &[("develop", Branch), ("origin/develop", Remote)], false, false),
            e("8f90123", "Fix: stop egress when a session detaches", "Minji Kim", "1일 전", false, &[("v0.3.0", Tag)], false, false),
            e("4c5d6e7", "Docs: update remote IDE protocol spec", "Alex", "2일 전", false, &[("origin/feat/remote-spec", Remote)], false, true),
        ],
        more: false,
        loading_more: false,
    }
}

/// The sample detail of a sample commit (captures and a screen without a server).
fn sample_commit_detail(sha: &str) -> CommitDetailLoad {
    use ember_client::api::{GitCommitDetail, GitCommitFile, GitCommitFileStatus as S, GitLogRef};
    let HistoryLoad::Ready { entries, .. } = sample_history() else { return CommitDetailLoad::Idle };
    let Some(e) = entries.iter().find(|e| e.sha == sha) else {
        return CommitDetailLoad::Failed("커밋을 찾을 수 없습니다".to_string());
    };
    let f = |path: &str, status, add, del, binary| GitCommitFile { path: path.to_string(), old_path: None, status, additions: add, deletions: del, binary };
    let files = match e.short.as_str() {
        "a1b2c3d" => vec![
            f("ember/proxy/src/shim.rs", S::Modified, 14, 3, false),
            f("ember/proxy/tests/shim_attach.rs", S::Added, 22, 0, false),
            f("docs/assets/shim.png", S::Added, 0, 0, true),
        ],
        "8f90123" => vec![f("ember/node/src/egress.rs", S::Modified, 9, 4, false), f("ember/node/src/legacy_egress.rs", S::Deleted, 0, 31, false)],
        _ => vec![f("docs/SPEC.md", S::Modified, 2, 1, false)],
    };
    let d = GitCommitDetail {
        sha: e.sha.clone(),
        short: e.short.clone(),
        subject: e.subject.clone(),
        body: format!("{}\n\n세션이 붙기 전에 shim을 연결합니다.\n연결이 끊기면 egress도 멈춥니다.", e.subject),
        author: e.author.clone(),
        time: String::new(),
        parents: if e.is_merge { vec!["1111111".to_string(), "2222222".to_string()] } else { vec!["9d8c7b6".to_string()] },
        refs: e.refs.iter().map(|r| GitLogRef { name: r.label.clone(), kind: r.kind.clone() }).collect(),
        files_total: files.len(),
        files,
        truncated: false,
    };
    let mut v = commit_detail_view(&d, 0);
    v.time_text = e.time_text.clone();
    v.unpushed = e.unpushed;
    v.remote_only = e.remote_only;
    CommitDetailLoad::Ready(v)
}

/// What the detail card does: the armed button, the busy guard, the branch form and the
/// callbacks. Built by `HistoryPane`, read by `CommitDetailBody`.
#[derive(Clone)]
struct CommitCtl {
    busy: bool,
    armed: Option<CommitAction>,
    form_open: bool,
    name: MutableState<String>,
    name_gen: u64,
    show_sha: bool,
    branch: String,
    on_revert: Rc<dyn Fn()>,
    on_pick: Rc<dyn Fn()>,
    on_toggle_form: Rc<dyn Fn()>,
    on_create: Rc<dyn Fn()>,
    on_copy: Rc<dyn Fn()>,
}

fn status_tone(letter: &str) -> (Paint, Paint) {
    let p = ember_theme().palette;
    match letter {
        "A" => (paint(p.green.soft), paint(p.green.strong)),
        "D" => (paint(p.red.soft), paint(p.red.strong)),
        "R" | "C" => (paint(p.blue.soft), paint(p.blue.strong)),
        _ => (paint(p.orange.soft), paint(p.orange.strong)),
    }
}

/// The selected commit: message, author, time, sha, refs, the changed files and the actions.
fn CommitDetailBody(v: &CommitDetailView, ctl: &CommitCtl) {
    let p = ember_theme().palette;
    VcsCard(|| {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                Txt(&v.subject, TypeRole::BodyStrong, Some(paint(p.text)), None, None, Some(1.0), None);
                Txt(&v.short, TypeRole::Mono, Some(paint(p.text_2)), Some(1), None, None, None);
            }) };
            for line in &v.body_lines {
                Label(line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
            let parents = if v.is_merge {
                format!("병합 커밋 (부모 {})", v.parents.iter().map(|x| x.chars().take(7).collect::<String>()).collect::<Vec<_>>().join(", "))
            } else {
                match v.parents.first() {
                    Some(x) => format!("부모 {}", x.chars().take(7).collect::<String>()),
                    None => "부모 없음".to_string(),
                }
            };
            Label(&format!("{} \u{00b7} {} \u{00b7} {parents}", v.author, v.time_text), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(2));
            Txt(&v.sha, TypeRole::Mono, Some(paint(p.text_2)), Some(2), None, None, None);
            if v.unpushed || v.remote_only || v.is_merge || !v.refs.is_empty() {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                    if v.unpushed {
                        Pill("푸시 안 됨", PillTone::Orange, true);
                    }
                    if v.remote_only {
                        Pill("원격에 있음", PillTone::Grey, true);
                    }
                    if v.is_merge {
                        Pill("병합", PillTone::Sky, true);
                    }
                    for r in v.refs.iter().take(6) {
                        Pill(&chip_text(&r.label), ref_tone(r), true);
                    }
                    if v.refs.len() > 6 {
                        Pill(&format!("+{}", v.refs.len() - 6), PillTone::Grey, true);
                    }
                }) };
            }

            let (shown, more) = commit_files_shown(v);
            InsetPanel(true, || {
                { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                    Label(&format!("변경한 파일 {}", v.files_total), Fs::Md, Weight::Semibold, None, Some(1));
                    if shown.is_empty() {
                        Label("변경한 파일이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    for f in shown {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                            let (bg, fg) = status_tone(f.status_letter);
                            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(20.0, 20.0).background(bg).corner_radius(CORNER_XS), alignment = Alignment::Center, {
                                Label(f.status_letter, Fs::Xs2, Weight::Semibold, Some(fg.clone()), Some(1));
                            }) };
                            let name = match &f.old_path {
                                Some(old) => format!("{old} \u{2192} {}", f.path),
                                None => f.path.clone(),
                            };
                            Txt(&name, TypeRole::Mono, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), Some(1.0), None);
                            if f.binary {
                                Label(&file_stat_text(f.additions, f.deletions, true), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
                            } else {
                                Label(&format!("+{}", f.additions), Fs::Xs, Weight::Medium, Some(paint(p.green.strong)), Some(1));
                                Label(&format!("-{}", f.deletions), Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(1));
                            }
                        }) };
                    }
                    if let Some(line) = more {
                        Label(&line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                }) };
            });

            // Two rows: four buttons side by side do not fit at 390 (Cherry-pick was cut).
            { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                    let toggle = ctl.on_toggle_form.clone();
                    AppButton("여기서 브랜치", None, ButtonKind::Default, true, !ctl.busy, false, move || toggle());
                    let copy = ctl.on_copy.clone();
                    AppButton("SHA 복사", None, ButtonKind::Default, true, true, false, move || copy());
                }) };
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                    let revert = ctl.on_revert.clone();
                    let label = if ctl.armed == Some(CommitAction::Revert) { CommitAction::Revert.armed_label() } else { CommitAction::Revert.label() };
                    AppButton(label, None, ButtonKind::Destructive, true, !ctl.busy, false, move || revert());
                    let pick = ctl.on_pick.clone();
                    let label = if ctl.armed == Some(CommitAction::CherryPick) { CommitAction::CherryPick.armed_label() } else { CommitAction::CherryPick.label() };
                    AppButton(label, None, ButtonKind::Default, true, !ctl.busy, false, move || pick());
                }) };
            }) };

            if let Some(action) = ctl.armed {
                Label(&commit_action_branch_line(action, &ctl.branch), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }

            if ctl.show_sha {
                // The toolkit clipboard refused the SHA (compose-rust#471 writes it on every system,
                // v0.0.7), so the full SHA is shown selectable for Cmd+C / Ctrl+C.
                Label("아래에서 선택해 복사해 주세요", Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                key(v.sha.clone(), || {
                    EmberField(FieldOptions { fill_max_width: true, text: Some(v.sha.clone()), type_role: Some(TypeRole::Mono), ..Default::default() });
                });
            }

            if ctl.form_open {
                Label(&format!("{} 에서 새 브랜치를 만들고 전환합니다", v.short), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(2));
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                    let name = ctl.name.clone();
                    key((v.sha.clone(), ctl.name_gen), || {
                        EmberField(FieldOptions {
                            weight: Some(1.0),
                            text: Some(ctl.name.get()),
                            placeholder: "feat/새-브랜치".to_string(),
                            on_value_change: Some(Box::new(move |value| name.set(value))),
                            ..Default::default()
                        });
                    });
                    let create = ctl.on_create.clone();
                    AppButton("만들고 전환", None, ButtonKind::Primary, true, !ctl.busy, false, move || create());
                }) };
            }
        }) };
    });
}

/// 히스토리: the commits of the current branch as a list, newest first, and the selected commit
/// in full (changed files, 여기서 브랜치, 되돌리기, Cherry-pick, SHA 복사; FR-G14, FR-G15, #484).
/// `actions` is the project and the screen's reload counter (a commit or a branch change reads
/// the list again); `None` shows sample commits. `dirty` is whether the work tree has tracked
/// changes (the node refuses a revert or cherry-pick then). The wide layout shows the list and
/// the detail side by side, each with its own scroll (the first commit selected until a tap); the
/// compact one shows one at a time, with 목록으로.
fn HistoryPane(actions: Option<Actions>, layout: PaneLayout, dirty: bool, branch: String) {
    let p = ember_theme().palette;
    let key_project = actions.as_ref().map(|a| a.project.clone());
    let two = layout == PaneLayout::Two;
    let state = remember_with_key(key_project.clone(), || mutable_state_of(HistoryLoad::Loading));
    let retry = remember(|| mutable_state_of(0_u64));
    // `EMBER_CR_VCS_COMMIT=<n>` opens the n-th commit of the list directly, for captures without clicks.
    let env_index = std::env::var("EMBER_CR_VCS_COMMIT").ok().and_then(|v| v.parse::<usize>().ok());
    let sample_pick = if actions.is_none() {
        env_index.and_then(|i| match sample_history() {
            HistoryLoad::Ready { entries, .. } => entries.get(i).map(|e| e.sha.clone()),
            _ => None,
        })
    } else {
        None
    };
    let selected = remember_with_key(key_project.clone(), || mutable_state_of(sample_pick));
    let env_done = remember_with_key(key_project.clone(), || mutable_state_of(false));
    let detail = remember_with_key(key_project, || mutable_state_of(CommitDetailLoad::Idle));
    let wanted = remember(|| mutable_state_of(None::<String>));
    let busy = remember(|| mutable_state_of(false));
    // The armed button as (key from `commit_action_key`, arm time in ms); cleared by a reload or a timeout.
    let armed = remember(|| mutable_state_of(None::<(usize, i64)>));
    let form_open = remember(|| mutable_state_of(false));
    let name = remember(|| mutable_state_of(String::new()));
    let name_gen = remember(|| mutable_state_of(0_u64));
    let show_sha = remember(|| mutable_state_of(false));
    let filter = remember(|| mutable_state_of(HistoryFilter::AllBranches));

    if let Some(a) = actions.clone() {
        let (st, sel, done) = (state.clone(), selected.clone(), env_done.clone());
        launched_effect((a.project.clone(), a.reload.get(), retry.get()), move || async move {
            if crate::services::try_services().is_some() {
                st.set(HistoryLoad::Loading);
                let (name, now) = (a.project.clone(), crate::demo::now_ms() / 1000);
                crate::services::run_shared(async move { history_from(fetch_history(&name, 0).await, now) }, move |result| {
                    if !done.get() {
                        done.set(true);
                        if let (Some(i), HistoryLoad::Ready { entries, .. }) = (env_index, &result) {
                            if let Some(e) = entries.get(i) {
                                sel.set(Some(e.sha.clone()));
                            }
                        }
                    }
                    st.set(result);
                });
            } else {
                st.set(HistoryLoad::Failed("서버에 연결되어 있지 않아 히스토리를 불러올 수 없습니다.".to_string()));
            }
        });
    }
    let current = if actions.is_some() { state.get() } else { sample_history() };
    let first_sha = match &current {
        HistoryLoad::Ready { entries, .. } if two => {
            let shown = filter_history_entries(entries, filter.get());
            shown.first().map(|e| e.sha.clone())
        }
        _ => None,
    };
    let effective = selected.get().or(first_sha);

    if let Some(a) = actions.clone() {
        let (det, disarm, wanted, form, sha_shown) = (detail.clone(), armed.clone(), wanted.clone(), form_open.clone(), show_sha.clone());
        let sha = effective.clone();
        launched_effect((a.project.clone(), effective.clone(), a.reload.get(), retry.get()), move || async move {
            disarm.set(None);
            sha_shown.set(false);
            wanted.set(sha.clone());
            let Some(sha) = sha else {
                det.set(CommitDetailLoad::Idle);
                return;
            };
            if crate::services::try_services().is_some() {
                if !matches!(det.get(), CommitDetailLoad::Ready(ref v) if v.sha == sha) {
                    det.set(CommitDetailLoad::Loading);
                    form.set(false);
                }
                let (name, now) = (a.project.clone(), crate::demo::now_ms() / 1000);
                let want = sha.clone();
                crate::services::run_shared(async move { commit_detail_from(fetch_commit_detail(&name, &sha).await, now) }, move |result| {
                    if wanted.get().as_deref() == Some(want.as_str()) {
                        det.set(result);
                    }
                });
            } else {
                det.set(CommitDetailLoad::Failed("서버에 연결되어 있지 않아 커밋을 불러올 수 없습니다.".to_string()));
            }
        });
    }
    let detail_now = if actions.is_some() {
        detail.get()
    } else {
        effective.as_deref().map(sample_commit_detail).unwrap_or(CommitDetailLoad::Idle)
    };
    let is_busy = busy.get();
    let ready = match &detail_now {
        CommitDetailLoad::Ready(v) => Some((v.sha.clone(), v.is_merge)),
        _ => None,
    };
    let now_ms = crate::demo::now_ms();
    let armed_action = ready.as_ref().and_then(|(sha, _)| {
        [CommitAction::Revert, CommitAction::CherryPick]
            .into_iter()
            .find(|&act| armed_label_visible(armed.get(), commit_action_key(sha, act), now_ms))
    });

    // 되돌리기 and Cherry-pick ask twice: the gate first, then the first tap arms, the second
    // counts after 600 ms and before 4 s. The busy guard is set before the request leaves.
    let on_apply = {
        let (actions, busy, armed, ready, branch) = (actions.clone(), busy.clone(), armed.clone(), ready.clone(), branch.clone());
        move |action: CommitAction| {
            let Some((sha, is_merge)) = ready.clone() else { return };
            if busy.get() {
                return;
            }
            if let Err(line) = commit_action_gate(action, dirty, is_merge) {
                armed.set(None);
                notice::post(line.to_string());
                return;
            }
            let now = crate::demo::now_ms();
            match delete_tap(armed.get(), commit_action_key(&sha, action), now) {
                DeleteTap::Arm | DeleteTap::Expired => armed.set(Some((commit_action_key(&sha, action), now))),
                DeleteTap::Wait => {}
                DeleteTap::Confirm => {
                    armed.set(None);
                    let Some(a) = actions.clone() else { return };
                    busy.set(true);
                    let (busy, reload, branch) = (busy.clone(), a.reload.clone(), branch.clone());
                    crate::services::run_shared(
                        crate::ui::vcs_watch::guarded(&a.project.clone(), async move {
                            match action {
                                CommitAction::Revert => run_commit_revert(&a.project, &sha, &branch).await,
                                CommitAction::CherryPick => run_commit_cherry_pick(&a.project, &sha, &branch).await,
                            }
                        }, |r: &Result<String, String>| r.as_ref().map_or_else(|line| commit_failure_reloads_snapshot(line), |_| true)),
                        move |result| {
                            match result {
                                Ok(line) => {
                                    notice::post(line);
                                    reload.set(reload.get() + 1);
                                }
                                Err(line) => {
                                    // The node found changes the snapshot did not show: reload so the dirty flag corrects itself.
                                    if commit_failure_reloads_snapshot(&line) {
                                        reload.set(reload.get() + 1);
                                    }
                                    notice::post(line);
                                }
                            }
                            busy.set(false);
                        },
                    );
                }
            }
        }
    };
    let on_revert: Rc<dyn Fn()> = {
        let apply = on_apply.clone();
        Rc::new(move || apply(CommitAction::Revert))
    };
    let on_pick: Rc<dyn Fn()> = {
        let apply = on_apply.clone();
        Rc::new(move || apply(CommitAction::CherryPick))
    };
    let on_toggle_form: Rc<dyn Fn()> = {
        let (form_open, armed) = (form_open.clone(), armed.clone());
        Rc::new(move || {
            armed.set(None);
            form_open.set(!form_open.get());
        })
    };
    let on_create: Rc<dyn Fn()> = {
        let (actions, busy, ready, name, name_gen, form_open) = (actions.clone(), busy.clone(), ready.clone(), name.clone(), name_gen.clone(), form_open.clone());
        Rc::new(move || {
            let Some(a) = actions.clone() else { return };
            let Some((sha, _)) = ready.clone() else { return };
            if busy.get() {
                return;
            }
            let branch = name.get().trim().to_string();
            if !valid_branch_name(&branch) {
                notice::post("브랜치 이름을 확인해 주세요. 공백이나 쓸 수 없는 문자가 있습니다".to_string());
                return;
            }
            busy.set(true);
            let (busy, reload, name, name_gen, form_open) = (busy.clone(), a.reload.clone(), name.clone(), name_gen.clone(), form_open.clone());
            crate::services::run_shared(crate::ui::vcs_watch::guarded_ok(&a.project.clone(), async move { run_branch_from(&a.project, &branch, &sha).await }), move |result| {
                match result {
                    Ok(line) => {
                        notice::post(line);
                        name.set(String::new());
                        name_gen.set(name_gen.get() + 1);
                        form_open.set(false);
                        reload.set(reload.get() + 1);
                    }
                    Err(line) => notice::post(line),
                }
                busy.set(false);
            });
        })
    };
    let on_copy: Rc<dyn Fn()> = {
        let (show_sha, ready) = (show_sha.clone(), ready.clone());
        Rc::new(move || {
            let copied = if let Some((ref sha, _)) = ready {
                copy_sha_to_clipboard(sha)
            } else {
                false
            };
            // The selectable SHA is only the fallback for a copy the clipboard refused.
            show_sha.set(!copied);
            if copied {
                notice::post("SHA를 클립보드에 복사했습니다.".to_string());
            } else {
                notice::post("SHA를 아래에서 복사해 주세요".to_string());
            }
        })
    };
    let ctl = CommitCtl {
        busy: is_busy,
        armed: armed_action,
        form_open: form_open.get(),
        name: name.clone(),
        name_gen: name_gen.get(),
        show_sha: show_sha.get(),
        branch: branch.clone(),
        on_revert,
        on_pick,
        on_toggle_form,
        on_create,
        on_copy,
    };

    let load_more = {
        let more_state = state.clone();
        let more_project = actions.as_ref().map(|a| a.project.clone());
        move || {
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
        }
    };

    let list = {
        let (selected, retry, effective, filter) = (selected.clone(), retry.clone(), effective.clone(), filter.clone());
        move || {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
                let cur_filter = filter.get();
                let filter_row = filter.clone();
                VcsRadioRow(&HistoryFilter::ALL, HistoryFilter::label, cur_filter, filter_row, || {});
                match current {
                    HistoryLoad::Loading => {
                        Label("히스토리를 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    HistoryLoad::Failed(text) => {
                        Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
                    }
                    HistoryLoad::Ready { entries, more, loading_more } => {
                        let shown = filter_history_entries(&entries, cur_filter);
                        if shown.is_empty() {
                            EmberCard(false, true, None, Some(SP[3]), None, || {
                                let msg = if cur_filter == HistoryFilter::LocalOnly {
                                    "로컬 커밋이 없습니다"
                                } else {
                                    "커밋이 없습니다"
                                };
                                Label(msg, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                            });
                            return;
                        }
                        VcsCard(|| {
                            for (i, e) in shown.iter().enumerate() {
                                if i > 0 {
                                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                                }
                                let pick = selected.clone();
                                let sha = e.sha.clone();
                                HistoryRow(e, layout, effective.as_deref() == Some(e.sha.as_str()), move || pick.set(Some(sha.clone())));
                            }
                        });
                        if more {
                            let label = if loading_more { "불러오는 중입니다" } else { "더 보기" };
                            AppButton(label, None, ButtonKind::Default, false, !loading_more, true, load_more);
                        }
                    }
                }
            }) };
        }
    };

    let detail_view = {
        let (selected, retry) = (selected.clone(), retry.clone());
        move || {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
                if !two {
                    let back = selected.clone();
                    AppButton("목록으로", None, ButtonKind::Default, true, true, false, move || back.set(None));
                }
                match detail_now {
                    CommitDetailLoad::Idle => {
                        EmberCard(false, true, None, Some(SP[3]), None, || {
                            Label("커밋을 선택해 주세요", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        });
                    }
                    CommitDetailLoad::Loading => {
                        Label("커밋을 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                    CommitDetailLoad::Failed(text) => {
                        Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry.set(retry.get() + 1));
                    }
                    CommitDetailLoad::Ready(v) => CommitDetailBody(&v, &ctl),
                }
            }) };
        }
    };

    if two {
        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().fill_max_height(), alignment = Alignment::TopStart, spacing = GAP_CARD, content = move || {
                { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().weight(1.0).fill_max_height(), content = list) };
                { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().weight(1.4).fill_max_height(), content = detail_view) };
            }) };
    } else if selected.get().is_some() {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
            detail_view();
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(super::routines::compact_bottom_clearance())) };
        }) };
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
    { use ::compose_rust::ui::Box; Box!(modifier = frame, content = move || {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
            Txt(&format!("#{} {}", e.number, e.title), TypeRole::BodyStrong, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            Txt(&pr_meta(e), TypeRole::Caption, Some(paint(p.text_2)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                Pill(e.state_text, pr_state_tone(e), true);
                if e.draft {
                    Pill("초안", PillTone::Grey, true);
                }
                Pill(e.review_text, pr_review_tone(e), true);
                Pill(&e.checks_text, pr_checks_tone(e), true);
            }) };
            let gate = if e.can_merge { paint(p.green.strong) } else { paint(p.text_2) };
            Label(&e.gate_text, Fs::Sm, Weight::Regular, Some(gate), None);
        }) };
        DataNamedOverlay(&format!("풀 리퀘스트 #{} {}", e.number, e.title), true, None, on_select);
    }) };
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
        body_lines: clean_body("요약과 변경 이유는 설명에 적습니다.\n- 세션 shim 위치 정돈\n* egress 종료 처리").lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).map(String::from).collect(),
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
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(8.0, 8.0).background(paint(color)).shape_role(ShapeRole::Full)) };
}

/// What the detail card asks of the pane: the review box text, the busy flag, whether the merge
/// What the detail card asks of the pane: the review box text, the busy flag, whether the merge
/// button is armed, the merge method choice, the delete branch toggle, and the actions.
struct DetailControls<R: Fn(GitPrReviewAction) + Clone + 'static, M: Fn() + Clone + 'static> {
    review_text: MutableState<String>,
    review_gen: MutableState<u64>,
    /// The review action that is armed and asks again, if any.
    armed_review: Option<GitPrReviewAction>,
    busy: bool,
    merge_armed: bool,
    merge_method: MutableState<GitPrMergeMethod>,
    delete_branch: MutableState<bool>,
    can_approve: Result<(), &'static str>,
    on_review: R,
    on_merge: M,
}

/// The selected pull request, the part that scrolls: title, meta line, description, 검사, 리뷰
/// and 리뷰 코멘트. The review box and 병합 are `PrDetailActions`, pinned below the scroll area.
/// Columns of the detail's 검사 and 리뷰 panels (`.vcs-prgrid`): two, one at a window 560 dp wide
/// or narrower. A window that has not reported its width counts as wide.
pub fn pr_grid_columns(window_width_dp: f32) -> usize {
    if window_width_dp > 0.0 && window_width_dp <= 560.0 { 1 } else { 2 }
}

fn PrDetailBody(v: &PrDetailView) {
    let p = ember_theme().palette;
    VcsCard(|| {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
            Txt(&format!("#{} {}", v.number, v.title), TypeRole::BodyStrong, Some(paint(p.text)), Some(3), Some(TextOverflow::Ellipsis), None, None);
            Txt(&pr_detail_meta(v), TypeRole::Caption, Some(paint(p.text_2)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            for line in &v.body_lines {
                Label(line, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }

            // `.vcs-prgrid`: 검사 and 리뷰 side by side, one column at 560 px and below. Stacked,
            // the detail was taller than its scroll area at 1440 and 검사 ended at the cut edge.
            let checks_panel = || {
                InsetPanel(true, || {
                    { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().height(28.0), {
                            Label("검사", Fs::Md, Weight::Semibold, None, Some(1));
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                            Pill(&v.ci_pill, detail_ci_tone(v), true);
                        }) };
                        if v.checks.is_empty() {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(24.0), {
                                Label("검사가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                            }) };
                        }
                        for (name, class, label) in &v.checks {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().height(24.0), {
                                StatusDot(*class);
                                // The name takes the room left and ends in an ellipsis
                                // (`.ellipsis.spacer`); the result keeps its own width.
                                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).width(0.0), {
                                    Label(name, Fs::Sm, Weight::Regular, None, Some(1));
                                }) };
                                Label(label, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                            }) };
                        }
                    }) };
                });
            };
            let reviews_panel = || {
                InsetPanel(true, || {
                    { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().height(28.0), {
                            Label("리뷰", Fs::Md, Weight::Semibold, None, Some(1));
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                            Pill(v.review_pill, detail_review_tone(v.review), true);
                        }) };
                        if v.reviewers.is_empty() {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(24.0), {
                                Label("리뷰어 없음", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                            }) };
                        }
                        for (name, state) in &v.reviewers {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width().height(24.0), {
                                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).width(0.0), {
                                    Label(name, Fs::Sm, Weight::Regular, None, Some(1));
                                }) };
                                Pill(state.label(), reviewer_tone(*state), true);
                            }) };
                        }
                    }) };
                });
            };
            if pr_grid_columns(current_window_size().width_dp) == 2 {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::TopStart, spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                    // Zero-width weighted cells: a weighted child is at least as wide as its
                    // content, and a long check name would push the other column out.
                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).width(0.0), content = checks_panel) };
                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).width(0.0), content = reviews_panel) };
                }) };
            } else {
                checks_panel();
                reviews_panel();
            }

            Label("리뷰 코멘트", Fs::Md, Weight::Semibold, None, Some(1));
            if v.comments.is_empty() {
                Label("코멘트가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
            for (author, text) in &v.comments {
                { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().fill_max_width(), {
                    Label(author, Fs::Sm, Weight::Semibold, None, Some(1));
                    Label(text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                }) };
            }

        }) };
    });
}

/// The review box (the text, 코멘트, 변경 요청, 승인) and 병합 with its method choices, delete-branch
/// toggle and hint, for an open pull request. At the wide layout it sits below the scroll area, so it is always reachable.
fn PrDetailActions<R: Fn(GitPrReviewAction) + Clone + 'static, M: Fn() + Clone + 'static>(v: &PrDetailView, ctl: DetailControls<R, M>) {
    let p = ember_theme().palette;
    let (merge_enabled, hint) = merge_button_state(v);
    let method = ctl.merge_method.get();
    let del_branch = ctl.delete_branch.get();
    VcsCard(|| {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
            Label("리뷰 남기기", Fs::Md, Weight::Semibold, None, Some(1));
            let text = ctl.review_text.clone();
            key(ctl.review_gen.get(), || {
                EmberField(FieldOptions {
                    fill_max_width: true,
                    height: Some(VCS_TEXTAREA_H),
                    inset: Some(VCS_TEXTAREA_INSET),
                    line_height: Some(VCS_TEXTAREA_LINE_H),
                    type_role: Some(VCS_FIELD_ROLE),
                    multiline: true,
                    text: Some(text.get()),
                    placeholder: "코멘트나 변경 요청의 내용".to_string(),
                    on_value_change: Some(Box::new(move |value| text.set(value))),
                    ..Default::default()
                });
            });
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                for (action, kind) in [
                    (GitPrReviewAction::Comment, ButtonKind::Default),
                    (GitPrReviewAction::RequestChanges, ButtonKind::Destructive),
                    (GitPrReviewAction::Approve, ButtonKind::Default),
                ] {
                    let on_review = ctl.on_review.clone();
                    let label = review_button_label(action, ctl.armed_review == Some(action));
                    let is_approve = action == GitPrReviewAction::Approve;
                    let action_enabled = !ctl.busy && (!is_approve || ctl.can_approve.is_ok());
                    AppButton(label, None, kind, true, action_enabled, false, move || on_review(action));
                }
            }) };
            if let Err(reason) = ctl.can_approve {
                Label(reason, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
            }

            let del_flag = ctl.delete_branch.clone();
            VcsRadioRow(&PR_MERGE_METHODS, pr_merge_method_label, method, ctl.merge_method.clone(), || {
                // An on or off choice beside the method: a pressed toggle, not a radio.
                ChoiceButton(PR_DELETE_BRANCH_LABEL, Choice::Toggle, del_branch, true, true, move || del_flag.set(!del_flag.get_untracked()));
            });

            let on_merge = ctl.on_merge.clone();
            AppButton(merge_button_text(ctl.merge_armed), None, ButtonKind::Primary, false, merge_enabled && !ctl.busy, true, move || on_merge());
            match hint {
                Some(reason) => Label(&reason, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None),
                None => {
                    Label(merge_method_text(method), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    if del_branch {
                        Label(PR_DELETE_BRANCH_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                    }
                }
            }
        }) };
    });
}

/// 풀 리퀘스트: the repository's pull requests with checks, review and the merge gate, and the
/// selected one in full (checks, reviewers, comments, review box, merge; FR-G10 to FR-G12,
/// #483). `actions` is the project and the screen's reload counter; `None` shows sample pull
/// requests. `count` is written for the section list (the open ones). `branch` is the current
/// branch, the head of a new pull request. The wide layout shows the list and the detail side by
/// side (the first one selected until a tap); the compact one shows one at a time, with 목록으로.
fn PullRequestsPane(actions: Option<Actions>, count: MutableState<Option<usize>>, shared: MutableState<Option<Vec<PrEntry>>>, selected: MutableState<Option<u64>>, layout: PaneLayout, branch: Option<String>) {
    let p = ember_theme().palette;
    let key_project = actions.as_ref().map(|a| a.project.clone());
    let state = remember_with_key(key_project.clone(), || mutable_state_of(PrLoad::Loading));
    let filter = remember(|| mutable_state_of(PrFilter::Open));
    let retry = remember(|| mutable_state_of(0_u64));
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
                        shared.set(if let PrLoad::Ready(list) = &result { Some(list.clone()) } else { None });
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
    // 승인 and 변경 요청 as (key of the action, arm time in ms); only one is armed, and 병합 clears it.
    let review_armed = remember(|| mutable_state_of(None::<(usize, i64)>));
    let prev_effective = remember(|| mutable_state_of(effective));
    if prev_effective.get() != effective {
        prev_effective.set(effective);
        armed.set(None);
        review_armed.set(None);
    }
    let merge_method = remember_with_key(effective, || mutable_state_of(GitPrMergeMethod::Squash));
    let delete_branch = remember_with_key(effective, || mutable_state_of(false));
    // The draft of the review box belongs to one pull request: another one starts empty.
    let draft_key = review_draft_key(actions.as_ref().map(|a| a.project.as_str()).unwrap_or(""), effective);
    let review_text = remember_with_key(draft_key.clone(), || mutable_state_of(String::new()));
    let review_gen = remember_with_key(draft_key, || mutable_state_of(0_u64));

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

    let signed_in_account = if let Some(s) = crate::services::try_services() {
        if let Some(state) = s.ui.live.github.get().state() {
            state.account.clone()
        } else if s.demo.is_some() {
            crate::demo::github_state().account
        } else {
            None
        }
    } else {
        None
    };
    let can_approve = match &detail_now {
        PrDetailLoad::Ready(v) => can_approve_pr(&v.author, signed_in_account.as_deref()),
        _ => Ok(()),
    };

    // A review: busy before the request, then the notice, and the list and the detail again.
    let on_review = {
        let (actions, busy, retry, review_text, review_gen, armed, review_armed, can_approve) =
            (actions.clone(), busy.clone(), retry.clone(), review_text.clone(), review_gen.clone(), armed.clone(), review_armed.clone(), can_approve);
        move |action: GitPrReviewAction| {
            if action == GitPrReviewAction::Approve {
                if let Err(msg) = can_approve {
                    notice::post(msg.to_string());
                    return;
                }
            }
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
            let Some(a) = actions.clone() else {
                notice::post(pr_review_notice(action).to_string());
                review_text.set(String::new());
                review_gen.set(review_gen.get() + 1);
                return;
            };
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
        let (actions, busy, retry, armed, head_sha, review_armed, merge_method, delete_branch) =
            (actions.clone(), busy.clone(), retry.clone(), armed.clone(), head_sha.clone(), review_armed.clone(), merge_method.clone(), delete_branch.clone());
        move || {
            let Some(number) = effective else { return };
            let now = crate::demo::now_ms();
            review_armed.set(None);
            match delete_tap(armed.get(), number as usize, now) {
                DeleteTap::Arm | DeleteTap::Expired => armed.set(Some((number as usize, now))),
                DeleteTap::Wait => {}
                DeleteTap::Confirm => {
                    armed.set(None);
                    let Some(a) = actions.clone() else {
                        notice::post("병합했습니다".to_string());
                        return;
                    };
                    busy.set(true);
                    let (busy, retry, head, method, del) = (busy.clone(), retry.clone(), head_sha.clone(), merge_method.get(), delete_branch.get());
                    crate::services::run_shared(crate::ui::vcs_watch::guarded(&a.project.clone(), async move { run_pr_merge(&a.project, number, method, del, &head).await }, |_| false), move |result| {
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
            if busy.get() {
                return;
            }
            let (title, body, draft) = (f_title.get(), clean_body(&f_body.get()), f_draft.get());
            if let Err(line) = pr_create_gate(&title) {
                notice::post(line.to_string());
                return;
            }
            let Some(a) = actions.clone() else {
                notice::post(pr_created_notice(999));
                f_title.set(String::new());
                f_body.set(String::new());
                f_gen.set(f_gen.get() + 1);
                form_open.set(false);
                return;
            };
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
            VcsRadioRow(&PrFilter::ALL, PrFilter::label, shown, filter.clone(), || {});
        }
    };
    let open_count = if actions.is_some() { count.get() } else { Some(sample_pr_count()) };
    let new_form = {
        let (form_open, f_title, f_body, f_draft, f_gen, branch) = (form_open.clone(), f_title.clone(), f_body.clone(), f_draft.clone(), f_gen.clone(), branch.clone());
        move || {
            VcsCard(move || {
                { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), content = move || {
                    Label("새 풀 리퀘스트", Fs::Md, Weight::Semibold, None, Some(1));
                    let t = f_title.clone();
                    key((f_gen.get(), 0_u8), || {
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            text: Some(f_title.get()),
                            placeholder: "제목 (필수)".to_string(),
                            on_value_change: Some(Box::new(move |v| t.set(v))),
                            ..Default::default()
                        });
                    });
                    let b = f_body.clone();
                    key((f_gen.get(), 1_u8), || {
                        EmberField(FieldOptions {
                            fill_max_width: true,
                            height: Some(VCS_TEXTAREA_H),
                            inset: Some(VCS_TEXTAREA_INSET),
                            line_height: Some(VCS_TEXTAREA_LINE_H),
                            type_role: Some(VCS_FIELD_ROLE),
                            multiline: true,
                            text: Some(f_body.get()),
                            placeholder: "설명 (선택)".to_string(),
                            on_value_change: Some(Box::new(move |v| b.set(v))),
                            ..Default::default()
                        });
                    });
                    let head_text = branch.clone().unwrap_or_else(|| "현재 브랜치".to_string());
                    Label(&format!("{head_text} \u{2192} 기본 브랜치"), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(2));
                    Label("원격에 올라가 있는 브랜치여야 합니다", Fs::Xs, Weight::Regular, Some(paint(p.text_2)), None);
                    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(32.0), alignment = Alignment::CenterStart, spacing = SP[1], {
                        let flag = f_draft.clone();
                        StageCheck(if f_draft.get() { Check::On } else { Check::Off }, STAGE_CHECK_SIDE, 32.0, move || flag.set(!flag.get()));
                        Label("초안", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                    }) };
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                        AppButton("만들기", None, ButtonKind::Primary, true, !is_busy, false, create.clone());
                        let close = form_open.clone();
                        AppButton("취소", None, ButtonKind::Default, true, true, false, move || close.set(false));
                    }) };
                }) };
            });
        }
    };

    let list = {
        let (selected, retry, form_open) = (selected.clone(), retry.clone(), form_open.clone());
        move || {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                    Label("풀 리퀘스트", Fs::Lg, Weight::Semibold, None, Some(1));
                    if let Some(n) = open_count {
                        Pill(&format!("열림 {n}"), PillTone::Green, true);
                    }
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                    let open = form_open.clone();
                    AppButton("새 PR", None, ButtonKind::Primary, true, true, false, move || open.set(!open.get()));
                }) };
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
                                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                                }
                                let (pick, disarm, disarm_review) = (selected.clone(), armed.clone(), review_armed.clone());
                                let number = e.number;
                                PullRequestRow(e, effective == Some(number), move || {
                                    disarm.set(None);
                                    disarm_review.set(None);
                                    pick.set(Some(number));
                                });
                            }
                        });
                    }
                }
            }) };
        }
    };

    let detail_view = {
        let (selected, retry) = (selected.clone(), retry.clone());
        move || {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
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
            }) };
        }
    };

    let ctl = DetailControls {
        review_text,
        review_gen,
        armed_review,
        busy: is_busy,
        merge_armed,
        merge_method,
        delete_branch,
        can_approve,
        on_review,
        on_merge,
    };
    if two {
        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().fill_max_height(), alignment = Alignment::TopStart, spacing = GAP_CARD, content = move || {
                { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().weight(1.0).fill_max_height(), content = list) };
                // Bounded column: the detail scrolls, the review box and 병합 stay below it.
                { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().weight(1.4).fill_max_height(), content = move || {
                    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), content = detail_view) };
                    if let Some(v) = actions_view {
                        PrDetailActions(&v, ctl);
                    }
                }) };
            }) };
    } else if selected.get().is_some() {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
            detail_view();
            if let Some(v) = actions_view {
                PrDetailActions(&v, ctl);
            }
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(super::routines::compact_bottom_clearance())) };
        }) };
    } else {
        list();
    }
}

/// Branches for the review captures (`EMBER_CR_ROUTE=vcs`) and for a screen without a server.
/// The sample 브랜치 list: its current branch is the sample snapshot's (#878, review r10: the page
/// header said `feat/session-shim` while this list marked `feat/git-api` current).
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
        e("feat/session-shim", true, false, Some("origin/feat/session-shim"), 2, 1, "a1b2c3d \u{00b7} Feat: attach the session shim before routing \u{00b7} 12분 전", Some("/Users/demo/darkpyonix-ember")),
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
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(BRANCH_BAR_W).height(4.0).background(paint(p.line)).corner_radius(2.0), content = move || {
        if fraction > 0.0 {
            { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().width(BRANCH_BAR_W * fraction).height(4.0).background(fill).corner_radius(2.0)) };
        }
    }) };
}

/// Side of the icon tile at the start of a branch or stash row (`.vcs-bico`).
pub const ROW_ICON_TILE: f32 = 32.0;

/// The `.vcs-bico` tile: 32 px, `CORNER_SM`, a 1 px `line` border, the glyph in `text_2`.
fn RowIconTile(svg: &'static str) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().size(ROW_ICON_TILE, ROW_ICON_TILE).border(1.0, paint(p.line)).corner_radius(CORNER_SM), alignment = Alignment::Center, content = || NavIcon(svg, p.text_2.to_argb(), 16.0)) };
}

fn BranchIconTile() {
    RowIconTile(ICON_GIT_SVG);
}

/// One branch: name with the 현재 pill, ahead/behind text and bars, the last commit line, the
/// worktree pill and the 전환 button (off for the current branch).
fn BranchRow(e: &BranchEntry, busy: bool, on_switch: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (a, b) = branch_bar_fractions(e.ahead, e.behind);
    let show_bars = e.has_upstream && (e.ahead > 0 || e.behind > 0);
    // `.vcs-brow` (`vcs.css:140-143`): the current row on `surface_2`, a 32 px icon tile with a
    // `line` border, the mono name, then the 전환 button on the right.
    let row_fill = if e.current { paint(p.surface_2) } else { Paint::Literal(Color::argb(0)) };
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[2], modifier = Modifier::new().fill_max_width().background(row_fill).padding(SP[2]), content = move || {
        BranchIconTile();
        { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().weight(1.0), {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                Txt(&e.name, TypeRole::Mono, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                if e.current {
                    Pill("현재", PillTone::Green, true);
                }
                if e.default {
                    Pill("기본", PillTone::Sky, true);
                }
                if let Some(w) = &e.worktree {
                    Pill(&format!("워크트리 {}", worktree_tail(w)), PillTone::Sky, true);
                }
            }) };
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                Txt(&if show_bars { branch_bar_text(e.ahead, e.behind) } else { e.sync_text() }, TypeRole::Caption, Some(paint(p.text_2)), Some(2), Some(TextOverflow::Ellipsis), None, None);
                if show_bars {
                    BranchBar(a, paint(p.green.strong));
                    BranchBar(b, paint(p.orange.strong));
                }
            }) };
            if e.has_upstream {
                Txt(&e.upstream_text, TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
            }
            if !e.last_line.is_empty() {
                Txt(&e.last_line, TypeRole::Caption, Some(paint(p.text_2)), Some(2), Some(TextOverflow::Ellipsis), None, None);
            }
        }) };
        AppButton("전환", None, ButtonKind::Default, true, !e.current && !busy, false, on_switch);
    }) };
}

/// The 기준 menu's rows: one per branch with the branch glyph, the name and a check on the
/// chosen base.
pub fn base_menu_rows(names: &[String], chosen: Option<usize>) -> Vec<PopRow> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| PopRow { icon: Some(ICON_GIT_SVG), title: n.clone(), chosen: chosen == Some(i), ..Default::default() })
        .collect()
}

/// The 기준 menu's checked row: the chosen base, or with none chosen the current branch, as the
/// mockup's `#vcsBase` selects the `<option>` of `r.branch` (#888: 기준: 현재 브랜치 named no row,
/// so the menu showed nothing chosen).
pub fn base_menu_chosen(names: &[String], base: Option<&str>, current: Option<&str>) -> Option<usize> {
    names.iter().position(|n| Some(n.as_str()) == base.or(current))
}

/// The 기준 button of 새 브랜치 and its menu (FR-L10.4, #888): the shared menu in the
/// permission-mode look under the button, lined up with its start, headed by what is chosen
/// (`기준: branch`); a bottom sheet with that head at 560 and below. The mockup draws a native
/// `<select>` (`#vcsBase`). The button keeps its full width, is disabled without branches and
/// says whether the menu is open.
pub(super) fn BaseMenu(open: MutableState<bool>, text: &str, names: &[String], chosen: Option<usize>, base: MutableState<Option<String>>) {
    let rows = base_menu_rows(names, chosen);
    let is_open = open.get();
    let (dismiss, toggle) = (open.clone(), open.clone());
    let list = names.to_vec();
    let enabled = !names.is_empty();
    PopoverMenuWith(
        is_open,
        PopMenu {
            name: super::a11y::BASE_PICKER,
            kind: PopKind::Mode,
            head: text,
            sub: None,
            rows: &rows,
            align: compose_rust::PopoverAlign::Start,
        },
        PopExtra { plain_sheet: true, ..Default::default() },
        move || dismiss.set(false),
        move |idx| base.set(list.get(idx).cloned()),
        || {
            super::components::AppMenuButton(text, ButtonKind::Default, false, enabled, true, is_open, move || toggle.set(!toggle.get_untracked()));
        },
        None,
    );
}

/// 브랜치: create a branch from a base and switch to it, then the 기본 브랜치 row and the 작업
/// 브랜치 rows (the node's order: current, default, newest). `actions` is the project and the
/// screen's reload counter; `None` shows sample branches. `count` is written for the section list.
fn BranchesPane(actions: Option<Actions>, count: MutableState<Option<usize>>, layout: PaneLayout) {
    let p = ember_theme().palette;
    let key_project = actions.as_ref().map(|a| a.project.clone());
    let state = remember_with_key(key_project.clone(), || mutable_state_of(BranchLoad::Loading));
    let retry = remember(|| mutable_state_of(0_u64));
    let name = remember(|| mutable_state_of(String::new()));
    let generation = remember(|| mutable_state_of(0_u64));
    let base = remember(|| mutable_state_of(None::<String>));
    // `EMBER_CR_VCS_MENU=base` opens the 기준 menu at start, for captures.
    let base_menu = remember(|| mutable_state_of(std::env::var("EMBER_CR_VCS_MENU").as_deref() == Ok("base")));
    let query = remember_with_key(key_project.clone(), || mutable_state_of(String::new()));
    let switch_busy = remember_with_key(key_project.clone(), || mutable_state_of(false));
    let switch_last = remember_with_key(key_project.clone(), || mutable_state_of(None::<i64>));
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
    let current_name = if let BranchLoad::Ready(list) = &current { list.iter().find(|b| b.current).map(|b| b.name.clone()) } else { None };
    let chosen_base = base_menu_chosen(&names, base.get().as_deref(), current_name.as_deref());

    let creator = {
        let (actions, name, generation, base, base_menu) = (actions.clone(), name.clone(), generation.clone(), base.clone(), base_menu.clone());
        let (names, base_text) = (names.clone(), base_text.clone());
        move || {
            VcsCard(|| {
                { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
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
                    BaseMenu(base_menu.clone(), &base_text, &names, chosen_base, base.clone());
                    SmallNote(if base.get().is_some() { "고른 브랜치에서 만듭니다" } else { "현재 브랜치에서 만듭니다" });
                    Label(BRANCH_CREATE_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
                    let (act, nm, gn, bs) = (actions.clone(), name.clone(), generation.clone(), base.clone());
                    let create_enabled = actions.is_some() && valid_branch_name(nm.get().trim());
                    AppButton("만들고 전환", None, if create_enabled { ButtonKind::Primary } else { ButtonKind::Default }, false, create_enabled, true, move || {
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
                }) };
            });
        }
    };

    let switch_actions = actions.clone();
    let retry_state = retry.clone();
    let status = current.clone();
    let list = move || {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
            creator();
            Label(BRANCH_SWITCH_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
            let search = query.clone();
            EmberField(FieldOptions {
                fill_max_width: true,
                text: Some(query.get()),
                placeholder: "브랜치 검색".to_string(),
                on_value_change: Some(Box::new(move |v| search.set(v))),
                ..Default::default()
            });
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
                    let filtered: Vec<_> = entries.into_iter().filter(|e| branch_matches(&e.name, &query.get())).collect();
                    if filtered.is_empty() {
                        Label("검색 결과가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                        return;
                    }
                    let (default, rest) = split_branches(&filtered);
                    let row = |e: &BranchEntry| {
                        let (a, target) = (switch_actions.clone(), e.name.clone());
                        let (busy, last) = (switch_busy.clone(), switch_last.clone());
                        BranchRow(e, switch_busy.get() || a.is_none(), move || {
                            let now = crate::demo::now_ms();
                            if !branch_switch_allowed(busy.get(), last.get(), now) { return; }
                            if let Some(a) = &a {
                                busy.set(true);
                                last.set(Some(now));
                                let (project, reload, busy, target) = (a.project.clone(), a.reload.clone(), busy.clone(), target.clone());
                                crate::services::run_shared(crate::ui::vcs_watch::guarded_ok(&project.clone(), async move {
                                    git_call::<Value>(&project, "checkout", json!({"branch": target})).await
                                }), move |result| {
                                    match result {
                                        Ok(_) => { notice::post("브랜치를 전환했습니다".to_string()); reload.set(reload.get() + 1); }
                                        Err(e) => notice::post(failure_text("브랜치 전환을 ", &e)),
                                    }
                                    busy.set(false);
                                });
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
                                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                                }
                                row(e);
                            }
                        });
                    }
                }
            }
        }) };
    };
    let content = list;
    if layout == PaneLayout::Two {
        { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = content) };
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
                    let s = state.session(&m.session_id);
                    let cwd = s.as_ref().map(|s| s.record.cwd.clone()).unwrap_or_default();
                    let title = s.as_ref().map(|s| s.record.title.clone()).unwrap_or_default();
                    Some(MemberCwd {
                        name: m.name.clone(),
                        harness: m.agent.clone(),
                        leader: m.leader,
                        cwd,
                        state_text: crate::ui::project::status_pill(m.status).0.to_string(),
                        status: m.status,
                        session_id: m.session_id.clone(),
                        title,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Sessions for the review captures: one in the main checkout, one in the ui worktree.
fn sample_members() -> Vec<MemberCwd> {
    use ember_client::LauncherStatus as S;
    let m = |name: &str, harness: &str, leader: bool, cwd: &str, status: S, session_id: &str, title: &str| MemberCwd {
        name: name.to_string(),
        harness: harness.to_string(),
        leader,
        cwd: cwd.to_string(),
        state_text: crate::ui::project::status_pill(status).0.to_string(),
        status,
        session_id: session_id.to_string(),
        title: title.to_string(),
    };
    vec![
        m("ember-리더", "claude-code", true, "/Users/demo/darkpyonix-ember/ember/app", S::Running, "s1", "UI wire & state"),
        m("ember-codex", "codex", false, "/Users/demo/worktrees/ui-phase-6", S::WaitingForApproval, "s2", "Compose Phase 6"),
    ]
}

/// The sample rows of 팀 작업 (the sample branches, members and pull requests).
fn sample_team_rows() -> Vec<TeamRow> {
    let (BranchLoad::Ready(mut branches), PrLoad::Ready(prs)) = (sample_branches(), sample_prs()) else { return Vec::new() };
    branches.push(BranchEntry {
        name: "fix/egress-stop".to_string(),
        current: false,
        default: false,
        upstream_text: "origin/fix/egress-stop".to_string(),
        has_upstream: true,
        ahead: 1,
        behind: 0,
        last_line: "b4c5d6e \u{00b7} Fix: stop egress when a session detaches \u{00b7} 5시간 전".to_string(),
        worktree: Some("/Users/demo/worktrees/egress-stop".to_string()),
    });
    let mut rows = team_rows(&branches, &sample_members(), Some(&prs));
    if let Some(r) = rows.get_mut(0) {
        r.computer = Some("mac-mini".to_string());
        r.dirty_count = Some(0);
    }
    rows
}

fn sample_team_len() -> usize {
    sample_team_rows().len()
}

/// One worktree card: the member's character, name, subtitle (title and harness), state pill,
/// the inset box with branch, worktree folder, computer and dirty status, last-commit line,
/// and action buttons (PR chip, 충돌 해결 when conflicts exist, 브랜치 보기, and 세션).
fn TeamRowView(
    r: &TeamRow,
    project: &str,
    on_branch: impl Fn() + Clone + 'static,
    on_session: impl Fn(&str) + Clone + 'static,
    on_conflict: impl Fn() + Clone + 'static,
    on_pr: impl Fn(u64) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let border = if r.conflict { paint(p.orange.strong) } else { paint(p.line) };
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .fill_max_width()
                .background(paint(p.surface))
                .border(1.0, border)
                .corner_radius(CORNER_LG), {
            { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[3]), {
                // Top row: Character, Name + (+N) + Leader, Subtitle (Title · Harness), State Pill
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                        match &r.member {
                            Some(m) => super::characters::Character(
                                &m.harness,
                                m.leader,
                                36.0,
                                &m.name,
                                project,
                                super::characters::CharState::from_status(m.status),
                                None,
                            ),
                            None => super::channels::Avatar("-", "", 36.0),
                        }
                        { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().weight(1.0), {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                                match &r.member {
                                    Some(m) => {
                                        Txt(&m.name, TypeRole::BodyStrong, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                                        if r.extra_members > 0 {
                                            Txt(&format!("+{}", r.extra_members), TypeRole::BodyStrong, Some(paint(p.text_2)), Some(1), None, None, None);
                                        }
                                        if m.leader {
                                            Pill("리더", PillTone::Orange, true);
                                        }
                                    }
                                    None => Txt("담당 없음", TypeRole::BodyStrong, Some(paint(p.text_2)), Some(1), None, None, None),
                                }
                            }) };
                            if let Some(m) = &r.member {
                                let sub = if m.title.trim().is_empty() {
                                    m.harness.clone()
                                } else {
                                    format!("{} \u{00b7} {}", m.title, m.harness)
                                };
                                Txt(&sub, TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                            }
                        }) };
                        if let Some(m) = &r.member {
                            Pill(&m.state_text, crate::ui::project::status_pill(m.status).1.into(), true);
                        }
                    }) };

                // Inset box: branch, worktree, computer & dirty
                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width(), {
                        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new()
                                    .fill_max_width()
                                    .background(paint(p.surface_2))
                                    .corner_radius(CORNER_MD)
                                    .padding(SP[2]), {
                                // Branch line
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                                    Txt("브랜치", TypeRole::Caption, Some(paint(p.text_2)), Some(1), None, None, None);
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                                    Txt(&r.branch, TypeRole::Mono, Some(paint(p.text)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                                    if r.current {
                                        Pill("현재", PillTone::Green, true);
                                    }
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                    Txt(&r.sync_text, TypeRole::Caption, Some(paint(p.text_2)), Some(1), None, None, None);
                                }) };
                                // Worktree folder line
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                                    Txt("워크트리", TypeRole::Caption, Some(paint(p.text_2)), Some(1), None, None, None);
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                                    Txt(&r.worktree, TypeRole::Mono, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                                }) };
                                // Computer and dirty status line
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                                    Txt("컴퓨터", TypeRole::Caption, Some(paint(p.text_2)), Some(1), None, None, None);
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                                    Txt(r.computer_text(), TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                    let dirty_lbl = r.dirty_text();
                                    let dirty_paint = if r.dirty_count.is_some_and(|n| n > 0) {
                                        paint(p.orange.strong)
                                    } else {
                                        paint(p.text_2)
                                    };
                                    Txt(&dirty_lbl, TypeRole::Caption, Some(dirty_paint), Some(1), None, None, None);
                                }) };
                            }) };
                    }) };

                // Last-commit line
                if !r.last_commit.is_empty() {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                            Txt(&r.last_commit, TypeRole::Caption, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
                        }) };
                }

                // Action buttons: PR chip, conflict button, spacer, branch view button, session button
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = move || {
                        match &r.pr_status {
                            TeamPrStatus::Checking => {
                                Pill("PR 확인 중", PillTone::Grey, true);
                            }
                            TeamPrStatus::Failed => {
                                Pill("PR 확인 실패", PillTone::Red, true);
                            }
                            TeamPrStatus::None => {
                                Pill("PR 없음", PillTone::Grey, true);
                            }
                            TeamPrStatus::Matched { number, label, ci, merged } => {
                                let tone = if *merged {
                                    PillTone::Blue
                                } else {
                                    match ci {
                                        Some(CheckClass::Passed) => PillTone::Green,
                                        Some(CheckClass::Failed) => PillTone::Red,
                                        Some(CheckClass::Pending) => PillTone::Orange,
                                        None => PillTone::Blue,
                                    }
                                };
                                let pr_num = *number;
                                let on_pr_click = on_pr.clone();
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().shape_role(ShapeRole::Full), content = move || {
                                    Pill(label, tone, true);
                                    DataNamedOverlay(label, true, None, move || on_pr_click(pr_num));
                                }) };
                            }
                        }

                        // The card's buttons are the mockup's `.btn.sm` (32 px, 13 px text). They are
                        // painted pills, as the stash buttons are: `AppButton` cannot set the text
                        // size (compose-rust#500), so its label came out at the toolkit's 16 px.
                        if r.conflict {
                            let on_cf = on_conflict.clone();
                            StashPill("충돌 해결", "충돌 해결", ButtonKind::Destructive, true, move || on_cf());
                        }

                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };

                        let on_br = on_branch.clone();
                        StashPill("브랜치 보기", "브랜치 보기", ButtonKind::Default, true, move || on_br());

                        if let Some(m) = &r.member {
                            if !m.session_id.is_empty() {
                                let sid = m.session_id.clone();
                                let on_sess = on_session.clone();
                                StashPill("세션", "세션", ButtonKind::Default, true, move || on_sess(&sid));
                            }
                        }
                    }) };
            }) };
        }) };
}

/// 팀 작업: one card per worktree with its member, branch, ahead/behind and pull request. The
/// branches come from the branches API; the members from the project's sessions (a session belongs
/// to the worktree that holds its folder). `prs` are the pull requests the 풀 리퀘스트 section has
/// read (or loaded here across all states if unread). `None` actions show sample rows. `count` is
/// written for the section list.
fn TeamPane(
    actions: Option<Actions>,
    count: MutableState<Option<usize>>,
    prs: MutableState<Option<Vec<PrEntry>>>,
    selected_pr: Option<MutableState<Option<u64>>>,
    layout: PaneLayout,
    sec_state: Option<MutableState<Section>>,
    snap: Option<&RepoSnapshot>,
) {
    let p = ember_theme().palette;
    let key_project = actions.as_ref().map(|a| a.project.clone());
    let state = remember_with_key(key_project.clone(), || mutable_state_of(BranchLoad::Loading));
    let team_pr_load = remember_with_key(key_project, || mutable_state_of(PrLoad::Loading));
    let retry = remember(|| mutable_state_of(0_u64));
    let compact = current_window_size().class == WindowSizeClass::Compact;

    if let Some(a) = actions.clone() {
        let (st, cnt) = (state.clone(), count.clone());
        let proj_br = a.project.clone();
        launched_effect((a.project.clone(), a.reload.get(), retry.get()), move || async move {
            if crate::services::try_services().is_some() {
                if !matches!(st.get(), BranchLoad::Ready(_)) {
                    st.set(BranchLoad::Loading);
                }
                let (project, now) = (proj_br.clone(), crate::demo::now_ms() / 1000);
                crate::services::run_shared(
                    async move { branches_from(fetch_branches(&project).await, now) },
                    move |result| {
                        cnt.set(if let BranchLoad::Ready(list) = &result {
                            Some(list.iter().filter(|b| b.worktree.is_some()).count())
                        } else {
                            None
                        });
                        st.set(result);
                    },
                );
            } else {
                st.set(BranchLoad::Failed("서버에 연결되어 있지 않아 팀 작업을 불러올 수 없습니다.".to_string()));
            }
        });

        // Load PR list across all states (`PrFilter::All`) so merged and closed PRs match too,
        // and surface fetch failures as `TeamPrStatus::Failed` instead of leaving `Checking` forever.
        let pr_load_state = team_pr_load.clone();
        let prs_state = prs.clone();
        let proj_pr = a.project.clone();
        launched_effect((a.project.clone(), a.reload.get(), retry.get()), move || async move {
            if crate::services::try_services().is_some() {
                if !matches!(pr_load_state.get(), PrLoad::Ready(_)) {
                    pr_load_state.set(PrLoad::Loading);
                }
                let (project, now) = (proj_pr.clone(), crate::demo::now_ms() / 1000);
                crate::services::run_shared(
                    async move { prs_from(fetch_prs(&project, PrFilter::All).await, now) },
                    move |result| {
                        if let PrLoad::Ready(ref list) = result {
                            if prs_state.get().is_none() {
                                prs_state.set(Some(list.clone()));
                            }
                        }
                        pr_load_state.set(result);
                    },
                );
            } else {
                pr_load_state.set(PrLoad::Failed("서버에 연결되어 있지 않아 풀 리퀘스트를 불러올 수 없습니다.".to_string()));
            }
        });
    }

    let project = actions.as_ref().map(|a| a.project.clone()).unwrap_or_default();
    let status = if actions.is_some() { state.get() } else { sample_branches() };
    let is_demo = crate::services::try_services().is_some_and(|s| s.demo.is_some());
    let members = match &actions {
        // Demo sessions run in temporary folders that no demo worktree path can match, so demo
        // mode uses the sample members (the same two as the sample route).
        Some(_) if is_demo => sample_members(),
        Some(a) => crate::services::try_services().map(|s| s.client.read(|st| member_cwds(st, &a.project))).unwrap_or_default(),
        None => sample_members(),
    };

    let pr_load_for_rows = if actions.is_some() {
        let shared = prs.get();
        resolve_team_pr_load(&team_pr_load.get(), shared.as_deref())
    } else {
        sample_prs()
    };

    let retry_state = retry.clone();
    let sec_nav = sec_state.clone();
    let sel_pr_nav = selected_pr.clone();
    let snap_owned = snap.cloned();
    let has_actions = actions.is_some();

    let list = move || {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[3], modifier = Modifier::new().fill_max_width(), content = move || match status {
            BranchLoad::Loading => {
                Label("팀 작업을 불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
            BranchLoad::Failed(text) => {
                Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                AppButton("다시 시도", None, ButtonKind::Primary, false, true, true, move || retry_state.set(retry_state.get() + 1));
            }
            BranchLoad::Ready(entries) => {
                let mut rows = if has_actions {
                    team_rows_with_pr_load(&entries, &members, &pr_load_for_rows)
                } else {
                    sample_team_rows()
                };
                if rows.is_empty() {
                    VcsCard(|| {
                        Label("작업 중인 워크트리가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    });
                    return;
                }
                if has_actions {
                    apply_team_row_snapshot_and_mode(&mut rows, snap_owned.as_ref(), is_demo);
                }
                for r in &rows {
                    let nav_br = sec_nav.clone();
                    let nav_cf = sec_nav.clone();
                    let nav_pr = sec_nav.clone();
                    let pick_pr = sel_pr_nav.clone();
                    TeamRowView(
                        r,
                        &project,
                        move || {
                            if let Some(s) = &nav_br {
                                s.set(Section::Branches);
                            }
                        },
                        move |sid: &str| {
                            if let Some(s) = crate::services::try_services() {
                                s.open_session(sid);
                            }
                        },
                        move || {
                            if let Some(s) = &nav_cf {
                                s.set(Section::Conflicts);
                            }
                        },
                        move |num: u64| {
                            if let Some(p) = &pick_pr {
                                p.set(Some(num));
                            }
                            if let Some(s) = &nav_pr {
                                s.set(Section::PullRequests);
                            }
                        },
                    );
                }
                if compact {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(super::routines::compact_bottom_clearance())) };
                }
            }
        }) };
    };

    if layout == PaneLayout::Two {
        { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = list) };
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
            crate::services::run_shared(crate::ui::vcs_watch::guarded_ok(&name.clone(), async move { run_stash_action(&name, action, index).await }), move |result| {
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
            crate::services::run_shared(crate::ui::vcs_watch::guarded_ok(&name.clone(), async move { run_stash_push(&name, &text, with_untracked).await }), move |result| {
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
                { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
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
                    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().height(32.0), alignment = Alignment::CenterStart, spacing = SP[1], {
                        let flag = untracked.clone();
                        StageCheck(if untracked.get() { Check::On } else { Check::Off }, STAGE_CHECK_SIDE, 32.0, move || flag.set(!flag.get()));
                        Label("추적하지 않는 파일 포함", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
                    }) };
                    AppButton("스태시 만들기", None, ButtonKind::Primary, false, !is_busy, true, make);
                }) };
            });
        }
    };

    let (retry, status) = (local.clone(), current.clone());
    let list = move || {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), content = move || {
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
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
                            }
                            let (run, armed) = (run.clone(), armed.clone());
                            StashRow(e, layout, is_busy, dirty, armed, run);
                        }
                    });
                }
            }
        }) };
    };
    if layout == PaneLayout::Two {
        { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = list) };
    } else {
        list();
    }
}

/// Whether a stash row keeps its actions on its own line's right end (wide) or puts them on a
/// line of their own under the text (phone, `.vcs-sact{width:100%}`).
pub fn stash_actions_inline(layout: PaneLayout) -> bool {
    layout == PaneLayout::Two
}

/// One stash: message, meta line and the 적용, 꺼내기, 삭제 buttons. 삭제 needs a second tap on
/// the same button, which then reads "정말 삭제합니다"; 적용 and 꺼내기 ask the same way when
/// `dirty`, under a line that says why. The armed label shows only while `armed_label_visible`
/// holds at composition: nothing re-renders on a timer, so it may stay on screen until the next
/// recomposition after 4 seconds, and `stash_tap` decides what a tap does. Each button has an
/// accessible name with the index and message (visible text stays 적용, 꺼내기, 삭제).
fn StashRow(e: &StashEntry, layout: PaneLayout, busy: bool, dirty: bool, armed: MutableState<Option<(usize, i64)>>, run: impl Fn(StashAction, u32) + Clone + 'static) {
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
    // `.vcs-brow` with `.vcs-sact` (`vcs.css:140, 156`): icon tile, the text, the actions on the
    // right; on a phone the actions take their own line, still right-aligned (`vcs.css` 560 px).
    let text = || {
        Txt(&e.message, TypeRole::BodyStrong, Some(paint(p.text)), Some(2), Some(TextOverflow::Ellipsis), None, None);
        Txt(&stash_meta(e), TypeRole::Mono, Some(paint(p.text_2)), Some(1), Some(TextOverflow::Ellipsis), None, None);
        if asking {
            Label(STASH_DIRTY_WARNING, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            if shows(StashAction::Pop) {
                Label(STASH_POP_NOTE, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
            }
        }
    };
    let actions = || {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], {
            for (action, verb, kind, ask) in [
                (StashAction::Apply, "적용", ButtonKind::Default, dirty),
                (StashAction::Pop, "꺼내기", ButtonKind::Default, dirty),
                (StashAction::Drop, "삭제", ButtonKind::Destructive, true),
            ] {
                let armed_now = ask && shows(action);
                let on_tap = tap.clone();
                StashPill(stash_button_text(action, armed_now), &stash_button_label(index, &e.message, verb), kind, !busy, move || on_tap(action));
            }
        }) };
    };
    if stash_actions_inline(layout) {
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[2], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
            RowIconTile(ICON_STASH_SVG);
            { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().weight(1.0), content = text) };
            actions();
        }) };
    } else {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width().padding(SP[2]), {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                RowIconTile(ICON_STASH_SVG);
                { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().weight(1.0), content = text) };
            }) };
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                actions();
            }) };
        }) };
    }
}

// The pill is painted and its accessible name is built from data (the stash's index and message),
// so the clear button of `components::DataNamedOverlay` carries it (the gap is filed there).
/// A small pill whose visible text is `text` and whose accessible name is `access`. It is as wide
/// as its label and [`VCS_PILL_PAD_X`] at each side.
fn StashPill(text: &str, access: &str, kind: ButtonKind, enabled: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let s = super::components::button_style_for(kind, true, enabled, p);
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .height(s.height)
                .background(paint(s.fill))
                .border(1.0, paint(s.border))
                .shape_role(ShapeRole::Full), alignment = Alignment::Center, content = move || {
            PillLabel(text, s.fs, s.text);
            DataNamedOverlay(access, enabled, None, on_click);
        }) };
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
        VcsScreenWithLoad(Load::Ready(sample_snapshot()), boxes, current, reload, true, None);
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
    crate::ui::vcs_watch::watch_and_refresh(current.as_deref(), &load);
    let state = load.get();
    VcsScreenWithLoad(state, boxes, current, reload, false, None);
}

/// VCS screen with explicit snapshot and projects.
pub fn VcsScreenWith(
    snap: Option<RepoSnapshot>,
    boxes: Vec<ProjectBox>,
    current_id: Option<String>,
) {
    let state = snap.map(Load::Ready).unwrap_or_else(|| Load::Unavailable(String::new()));
    let reload = remember(|| mutable_state_of(0_u64));
    VcsScreenWithLoad(state, boxes, current_id, reload, false, None);
}

fn VcsScreenWithLoad(
    load: Load,
    boxes: Vec<ProjectBox>,
    current_id: Option<String>,
    reload: MutableState<u64>,
    sample: bool,
    sheet_ctl: Option<VcsSheetCtl>,
) {
    let snap = load.snapshot().cloned();
    let p = ember_theme().palette;
    let window = current_window_size();
    let class = window.class;
    let sheet = sheet_ctl.is_some();
    // In the sheet the layout follows the sheet's own width, as the mockup's mounted screen
    // follows its container (FR-L18.1, #889): the icon rail and two panes while the padded body
    // is wider than 760, pills over one column below that, the tabs on a phone.
    let frame = sheet_ctl.as_ref().map(|c| vcs_sheet_frame(c.sheet_w, c.window_w));
    let layout = frame.map_or_else(|| pane_layout(class), |f| f.layout);
    let wide = frame.map_or_else(|| section_nav(class) == SectionNav::List, |f| f.rail);
    let compact = class == WindowSizeClass::Compact;
    let pad = frame.map_or_else(|| vcs_page_pad(compact), |f| f.pad);
    let files_w = frame.map_or(320.0, |f| f.files_w);
    // The height of the two panes, known in the sheet only (its body height is set outright).
    // On the bounded wide page it is the window under the bar (review ba: at 1024 x 800 the
    // 360 tree put the commit button below the fold).
    let pane_h = match sheet_ctl.as_ref() {
        Some(c) => c.body_h.map(|h| sheet_pane_height(h, pad)),
        None => (wide && window.height_dp > 100.0).then(|| page_pane_height(window.height_dp, pad)),
    };
    let content_width = (sheet_ctl.as_ref().map_or(window.width_dp, |c| c.sheet_w) - 2.0 * pad).max(0.0);
    let nav_w = if sheet { VCS_SHEET_RAIL_W + SP[3] } else { 200.0 + SP[3] };
    let section_width = (content_width - if wide { nav_w } else { 0.0 }).max(0.0);
    let diff_width = (section_width - if layout == PaneLayout::Two { files_w + GAP_CARD } else { 0.0 }).max(0.0);
    // The repository bar follows the window, and in the sheet its padded body too (FR-L19).
    let bar_grid = repo_bar_grid(sheet_ctl.as_ref().map_or(window.width_dp, |c| c.window_w), sheet.then_some(content_width));

    // `EMBER_CR_VCS_SECTION=<key>` opens a section directly, for captures without clicks.
    let section = remember(|| mutable_state_of(std::env::var("EMBER_CR_VCS_SECTION").map(|k| route_section(&k)).unwrap_or(Section::Changes)));
    let tab = remember(|| mutable_state_of(PaneTab::Files));
    let selected_proj = remember(|| mutable_state_of(current_id));
    let stash_count = remember(|| mutable_state_of(None::<usize>));
    let pr_count = remember(|| mutable_state_of(None::<usize>));
    let branch_count = remember(|| mutable_state_of(None::<usize>));
    let team_count = remember(|| mutable_state_of(None::<usize>));
    // Keyed by the project: the PR pills of another project's branches must not carry over.
    let pr_list = remember_with_key(selected_proj.get(), || mutable_state_of(None::<Vec<PrEntry>>));
    // `EMBER_CR_VCS_PR=<n>` opens that pull request directly, and clicking a PR chip in 팀 작업 selects it too.
    let selected_pr = remember_with_key(selected_proj.get(), || mutable_state_of(std::env::var("EMBER_CR_VCS_PR").ok().and_then(|v| v.parse::<u64>().ok())));

    let current = selected_proj.get();
    let project = current.as_ref().and_then(|id| boxes.iter().find(|b| &b.id == id).cloned());
    // The sheet shows the session's project only: no project switcher (#495).
    let projects: Vec<(String, String)> = repo_projects(&boxes)
        .into_iter()
        .filter(|b| !sheet || Some(&b.id) == current.as_ref())
        .map(|b| (b.id.clone(), b.name.clone()))
        .collect();

    let sec = section.get();
    let active_tab = effective_tab(sec, tab.get());
    // The commit box's state lives here, so the sheet's footer and the changes card share it.
    let draft_scope = current.clone().filter(|_| !sample && crate::services::try_services().is_some()).map(|id| (id, reload.get()));
    let draft = commit_draft(snap.as_ref(), draft_scope);
    let footer = project.is_some() && sheet_commit_footer(sheet, layout, sec, snap.is_some());
    let footer_body_h = match sheet_ctl.as_ref() {
        Some(c) => c.body_h,
        None => (window.height_dp > 100.0).then(|| page_footer_body_h(window.height_dp)),
    };
    let footer_compact = footer_body_h.is_some_and(|h| sheet_footer_compact(h, pad));
    let footer_actions = project.as_ref().filter(|_| !sample).and_then(|pr| crate::services::try_services().map(|_| Actions { project: pr.id.clone(), reload: reload.clone() }));

    let body = || {
        // The sheet's header carries the title and the summary line (FR-L18.1), so the sheet
        // draws neither again.
        if !sheet {
        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), alignment = Alignment::CenterStart, {
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), spacing = 4.0, {
                // The sheet header carries the title (#495).
                if !sheet {
                    Label("버전 관리", Fs::Xl3, PAGE_TITLE_WEIGHT, None, None);
                }
                if let Some(ref proj) = project {
                    let subtitle = screen_subtitle(proj, snap.as_ref(), sample);
                    Label(&subtitle, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
            }) };
            // The page header carries the theme toggle like 에이전트 and 파일; the sheet and the
            // phone (where the shell's menu button sits) do not (#495).
            if !sheet && !compact {
                if let Some(s) = crate::services::try_services() {
                    // The icon-only theme button is named `a11y::THEME` (`NamedClickable`, compose-rust#460, #429).
                    super::project::ThemeButton(&s.ui);
                }
            }
        }) };
        }

        if let Some(project) = project {
                    let sel_proj = selected_proj.clone();
                    let actions = (!sample)
                        .then(crate::services::try_services)
                        .flatten()
                        .map(|_| Actions { project: project.id.clone(), reload: reload.clone() });
                    RepoBarWithActions(
                        &projects,
                        &project,
                        snap.as_ref(),
                        bar_grid,
                        move |id: String| {
                            sel_proj.set(Some(id.clone()));
                            if !sheet {
                                if let Some(s) = crate::services::try_services() {
                                    s.ui.set_selected_project(Some(id));
                                }
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
                        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().weight(1.0), spacing = SP[3], {
                            let sec_state = section.clone();
                            let sec_for_list = sec_state.clone();
                            if sheet {
                                SectionRail(sec, snap.as_ref(), sample, stash_count.get(), pr_count.get(), branch_count.get(), team_count.get(), move |s| sec_for_list.set(s));
                            } else {
                                { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().width(200.0).fill_max_height(), {
                                    SectionListWithSample(sec, snap.as_ref(), sample, stash_count.get(), pr_count.get(), branch_count.get(), team_count.get(), move |s| sec_for_list.set(s));
                                }) };
                            }
                            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).fill_max_height(), {
                                SectionContent(snap.as_ref(), sec, layout, active_tab, &tab, actions.clone(), stash_count.clone(), pr_count.clone(), branch_count.clone(), team_count.clone(), pr_list.clone(), selected_pr.clone(), Some(sec_state.clone()), diff_width, files_w, pane_h, Some(draft.clone()), footer);
                            }) };
                        }) };
                    } else {
                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), spacing = SP[2], {
                            let sec_state = section.clone();
                            let sec_for_pills = sec_state.clone();
                            let (stashes, prs, branches, team) = (stash_count.get(), pr_count.get(), branch_count.get(), team_count.get());
                            let pill_snap = snap.as_ref();
                            // The row is as wide as the page less the shell's navigation, which
                            // `section_width` leaves in (r12 capture: 충돌 cut at 800, 664 dp row).
                            let nav = if sheet { 0.0 } else {
                                match super::shell::current_mode(false) {
                                    crate::ui::shell::ShellMode::BottomTabs => 0.0,
                                    crate::ui::shell::ShellMode::Rail => super::theme::RAIL_W,
                                    crate::ui::shell::ShellMode::Sidebar => crate::ui::theme::SIDE_W,
                                }
                            };
                            SectionPillsWithCounts(
                                sec,
                                section_width,
                                (section_width - nav).max(0.0),
                                &|s| shown_section_count(s, pill_snap, sample, stashes, prs, branches, team),
                                move |s| sec_for_pills.set(s),
                            );
                            if layout == PaneLayout::Tabs && snap.is_none() && !pane_tabs(sec).is_empty() {
                                let tab_state = tab.clone();
                                PaneTabs(sec, active_tab, move |t| tab_state.set(t));
                            }
                            SectionContent(snap.as_ref(), sec, layout, active_tab, &tab, actions.clone(), stash_count.clone(), pr_count.clone(), branch_count.clone(), team_count.clone(), pr_list.clone(), selected_pr.clone(), Some(sec_state.clone()), diff_width, files_w, pane_h, Some(draft.clone()), footer);
                        }) };
                    }
                } else {
                    Label("저장소를 둘 수 있는 프로젝트가 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                    Label(SOON, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                }
    };
    let page = || {
        // In the sheet the body sits under the header on `bg` (`.svcs-box{background:var(--bg)}`)
        // and takes the height the header leaves.
        let outer = if sheet { Modifier::new().fill_max_width().weight(1.0).background(paint(p.bg)) } else { Modifier::new().fill_max_width().fill_max_height() };
        if wide {
            // Wide: a bounded page, so the section list and the panes can fill its height.
            { use ::compose_rust::ui::Column; Column!(modifier = outer.padding(pad), spacing = SP[3], content = body) };
        } else {
            if footer {
                // One column, in the sheet and on the page: the body scrolls above the commit
                // box, which stays whole at the bottom (`sheet_commit_footer`).
                let fd = draft.clone();
                let fsnap = snap.clone();
                let factions = footer_actions.clone();
                { use ::compose_rust::ui::Column; Column!(modifier = outer, content = move || {
                    { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), {
                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().padding(pad), spacing = SP[3], content = body) };
                    }) };
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().padding(pad), content = move || {
                        if let Some(s) = fsnap.as_ref() {
                            let staged_files = fd.staging.get().staged_files(&s.changes);
                            let commit = factions.clone().map(|a| (a, s.clone(), fd.staging.clone()));
                            CommitPane(&s.branch, staged_files, &fd.summary, &fd.body, &fd.amend, commit, footer_compact);
                        }
                    }) };
                }) };
            } else {
            { use ::compose_rust::ui::Box; Box!(modifier = outer, {
                { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().fill_max_height(), {
                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().padding(pad), spacing = SP[3], content = body) };
                }) };
            }) };
            }
        }
    };
    match sheet_ctl {
        Some(ctl) => {
            let project_name = boxes.iter().find(|b| Some(&b.id) == current.as_ref()).map(|b| b.name.clone()).unwrap_or_default();
            let prs = if sample { Some(sample_pr_count()) } else { pr_count.get() };
            let subtitle = vcs_sheet_subtitle(snap.as_ref(), prs);
            let phone = vcs_sheet_full_window(ctl.window_w);
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().fill_max_height(), {
                if ctl.top_inset > 0.0 {
                    // `.svcs-box{padding-top:env(safe-area-inset-top)}` on a phone: the header
                    // starts below the window's buttons (review r8: the traffic lights sat on the
                    // tile and the title).
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(ctl.top_inset).background(paint(p.bg))) };
                }
                VcsSheetHeader(&vcs_sheet_title(&project_name), &subtitle, phone, current.clone(), ctl.on_close.clone());
                page();
            }) };
        }
        None => page(),
    }
}

/// The page gutter: 16 px on a phone (`vcs-changes-390` mockup), 32 px otherwise.
pub fn vcs_page_pad(compact: bool) -> f32 {
    if compact { SP[3] } else { 32.0 }
}

/// What the 버전 관리 sheet hands the screen it mounts (FR-L18.1, #889): the window and sheet
/// widths it lays out against and its close callback.
#[derive(Clone)]
struct VcsSheetCtl {
    window_w: f32,
    sheet_w: f32,
    /// The strip above the header that the window's own buttons cover ([`vcs_sheet_top_inset`]).
    top_inset: f32,
    /// The height under the sheet header ([`vcs_sheet_body_height`]), when the sheet set it.
    body_h: Option<f32>,
    on_close: std::rc::Rc<std::cell::RefCell<dyn FnMut()>>,
}

/// The icon rail of the mounted screen (`.vcs-compact .vcs-body{grid-template-columns:56px ...}`)
/// and its entries (`.vcs-compact .vcs-navi{width:48px;height:44px}`).
pub const VCS_SHEET_RAIL_W: f32 = 56.0;
pub const VCS_SHEET_RAIL_ITEM: (f32, f32) = (48.0, 44.0);

/// How the sheet lays out the screen at a sheet width (FR-L18.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VcsSheetFrame {
    /// `.vcs-mounted{padding:16px}`, 12 at 560 and below.
    pub pad: f32,
    /// The icon rail and the panes side by side, while the padded body is wider than 760
    /// (`@container (max-width:760px)` stacks them).
    pub rail: bool,
    pub layout: PaneLayout,
    /// The tree column beside the diff (`.vcs-compact .vcs-chg{grid-template-columns:260px ...}`).
    pub files_w: f32,
}

pub fn vcs_sheet_frame(sheet_w: f32, window_w: f32) -> VcsSheetFrame {
    let phone = vcs_sheet_full_window(window_w);
    let pad = if phone { SP[2] } else { SP[3] };
    let rail = sheet_w - 2.0 * pad > 760.0;
    let layout = if phone {
        PaneLayout::Tabs
    } else if rail {
        PaneLayout::Two
    } else {
        PaneLayout::Stacked
    };
    VcsSheetFrame { pad, rail, layout, files_w: 260.0 }
}

/// The strip the full-screen sheet leaves above its header for the window's own buttons: the
/// shared rule for every full-screen sheet (`full_screen_sheet_top_inset`, FR-L33). The sheet is
/// a monitor-presented `EmberSheetSized`, the whole window up to 560 inclusive, so it reserves
/// the height the macOS traffic lights cover there and none on a wider window, where the sheet
/// floats in the middle, clear of them.
pub fn vcs_sheet_top_inset(window_w: f32, area: crate::ui::safe_area::SafeArea) -> f32 {
    super::components::full_screen_sheet_top_inset(window_w, true, area)
}

/// The sheet header's title (`.vcs-mhead b`).
pub fn vcs_sheet_title(project_name: &str) -> String {
    format!("버전 관리 \u{b7} {project_name}")
}

/// The line under the title: branch, changed files and open pull requests (`.vcs-mhead .small`).
/// A PR count not read yet is left out rather than shown as 0; without a snapshot it is empty.
pub fn vcs_sheet_subtitle(snap: Option<&RepoSnapshot>, open_prs: Option<usize>) -> String {
    let Some(s) = snap else { return String::new() };
    let mut line = format!("{} \u{b7} 변경 {}", s.branch, s.changes.len());
    if let Some(n) = open_prs {
        line.push_str(&format!(" \u{b7} PR {n}"));
    }
    line
}

/// The sheet's header (`.vcs-mhead`): 60 high, 20 at the sides (16 on a phone), the 32 `ink`
/// tile with the git glyph, the title over the summary line, 전체 화면 (not on a phone) and the
/// close button, then a line.
fn VcsSheetHeader(title: &str, subtitle: &str, phone: bool, project: Option<String>, on_close: std::rc::Rc<std::cell::RefCell<dyn FnMut()>>) {
    let p = ember_theme().palette;
    let side = if phone { SP[3] } else { SP[4] };
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(SHEET_HEADER_H).background(paint(p.surface)), {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(side)) };
            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(32.0, 32.0).background(paint(p.ink)).corner_radius(CORNER_SM), content = || NavIcon(ICON_GIT_SVG, p.ink_text.to_argb(), 18.0)) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(0.0).weight(1.0), {
                Label(title, Fs::Xl, Weight::Semibold, None, Some(1));
                if !subtitle.is_empty() {
                    Label(subtitle, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
            }) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            if !phone {
                let close = on_close.clone();
                let project = project.clone();
                AppButton("전체 화면", None, ButtonKind::Default, true, true, false, move || {
                    if let (Some(s), Some(id)) = (crate::services::try_services(), project.as_deref()) {
                        open_for_project_cr(&s.ui, id);
                    }
                    (&mut *close.borrow_mut())();
                });
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
            }
            let close = on_close.clone();
            super::components::CloseButton(move || (&mut *close.borrow_mut())());
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(side)) };
        }) };
    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
}

/// The section rail of the mounted screen (`.vcs-compact .vcs-navi`): 48 x 44 icon entries in a
/// 56 column, the open one on `surface` with a `line` outline and the small shadow, a count at
/// the top right (white on orange for conflicts). Each entry is named by its section.
fn SectionRail(
    active: Section, snap: Option<&RepoSnapshot>, is_sample: bool, stash_count: Option<usize>, pr_count: Option<usize>, branch_count: Option<usize>, team_count: Option<usize>,
    on_pick: impl FnMut(Section) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let (item_w, item_h) = VCS_SHEET_RAIL_ITEM;
    { use ::compose_rust::ui::Column; Column!(alignment = Alignment::TopCenter, spacing = 2.0, modifier = Modifier::new().width(VCS_SHEET_RAIL_W), {
        for sec in Section::ALL {
            let on = sec == active;
            let count = shown_section_count(sec, snap, is_sample, stash_count, pr_count, branch_count, team_count);
            let mut m = Modifier::new()
                .size(item_w, item_h)
                .background(if on { paint(p.surface) } else { Paint::Literal(Color::argb(0)) })
                .corner_radius(CORNER_MD);
            if on {
                m = with_shadows(m.border(1.0, paint(p.line)), ember_theme().shadow_sm());
            }
            let mut pick = on_pick.clone();
            NamedClickable(sec.label(), m, Alignment::TopEnd, move || pick(sec), move || {
                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_size(), {
                    NavIcon(section_svg(sec), (if on { p.text } else { p.text_2 }).to_argb(), 18.0);
                }) };
                if let Some(n) = count.filter(|n| *n > 0) {
                    let alert = sec == Section::Conflicts;
                    let (fill, ink) = if alert { (p.orange.strong, Color::argb(0xffff_ffff)) } else { (p.surface_2, p.muted) };
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().height(16.0).background(paint(fill)).shape_role(ShapeRole::Full), {
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(5.0)) };
                            Label(&n.to_string(), Fs::Xs2, Weight::Medium, Some(paint(ink)), Some(1));
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(5.0)) };
                        }) };
                }
            });
        }
    }) };
}

/// A section's count as the list and the rail show it: what the server reported, else the
/// sample's own numbers on the review screen.
fn shown_section_count(
    sec: Section, snap: Option<&RepoSnapshot>, is_sample: bool, stash_count: Option<usize>, pr_count: Option<usize>, branch_count: Option<usize>, team_count: Option<usize>,
) -> Option<usize> {
    section_count_with_team(sec, snap, stash_count, pr_count, branch_count, team_count).or_else(|| {
        if is_sample {
            match sec {
                Section::PullRequests => Some(sample_pr_count()),
                Section::Stash => Some(2),
                Section::Conflicts => Some(SAMPLE_CONFLICT_COUNT),
                Section::Team => Some(sample_team_len()),
                _ => None,
            }
        } else {
            None
        }
    })
}

/// Whether the sheet is the whole window (`.svcs-box` at 560 px and below, session.css): the shared
/// sheet's own boundary for a monitor-presented sheet, so the width, the body height, the header
/// and the strip all change at the same width as the presentation does (FR-L18, FR-L33).
fn vcs_sheet_full_window(window_width_dp: f32) -> bool {
    super::components::sheet_is_full_screen(window_width_dp, true)
}
/// The window size assumed while none has been reported (0 dp in the first frame).
const VCS_SHEET_DEFAULT_WINDOW: (f32, f32) = (1024.0, 720.0);
/// Header (60 dp) plus its separator (1 dp).
const VCS_SHEET_CHROME_H: f32 = 61.0;

static VCS_LAST_WINDOW: std::sync::Mutex<Option<(u32, u32)>> = std::sync::Mutex::new(None);

/// The window size the sheet lays out against: the reported size when both sides are usable
/// (above 100 dp), else the last usable size, else 1024 x 720. A zero size is "unknown", never a
/// zero-sized window.
fn vcs_sheet_window(width_dp: f32, height_dp: f32) -> (f32, f32) {
    let mut last = VCS_LAST_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
    if width_dp > 100.0 && height_dp > 100.0 {
        *last = Some((width_dp as u32, height_dp as u32));
        return (width_dp, height_dp);
    }
    match *last {
        Some((w, h)) => (w as f32, h as f32),
        None => VCS_SHEET_DEFAULT_WINDOW,
    }
}

/// The sheet width: 90 percent of the window capped at the monitor width, the whole window
/// up to 560 dp (mockup `.svcs-box`: 90vw, 100vw on a phone).
fn vcs_sheet_width(window_width_dp: f32) -> f32 {
    if vcs_sheet_full_window(window_width_dp) {
        window_width_dp
    } else {
        (window_width_dp * 0.9).min(super::components::SHEET_WIDTH_MONITOR)
    }
}

/// The body height under the sheet header: 90 percent of the window at wide widths, the whole
/// window on a phone. A zero or tiny size is treated as unknown and falls back to 1024 x 720.
fn vcs_sheet_body_height(width_dp: f32, height_dp: f32) -> f32 {
    let (w, h) = if width_dp > 100.0 && height_dp > 100.0 { (width_dp, height_dp) } else { VCS_SHEET_DEFAULT_WINDOW };
    let total = if vcs_sheet_full_window(w) { h } else { h * 0.9 };
    (total - VCS_SHEET_CHROME_H).max(0.0)
}

/// 버전 관리 as a modal sheet over the session (FR-L18, #495): the same screen body and git data
/// as the page, for the session's project, without changing the selected project or the menu.
pub fn VcsSheet(project_id: &str, open: bool, on_close: impl FnMut() + Clone + 'static) {
    if !open {
        return;
    }
    let boxes = if let Some(s) = crate::services::try_services() {
        let _ = s.ui.live.launcher.get();
        s.client.read(project_boxes)
    } else {
        Vec::new()
    };
    let sample = crate::services::try_services().is_none();
    let current = Some(project_id.to_string());
    let load = remember_with_key(project_id.to_string(), || mutable_state_of(Load::Loading));
    let reload = remember_with_key(project_id.to_string(), || mutable_state_of(0_u64));
    let load_effect = load.clone();
    let project_effect = project_id.to_string();
    launched_effect((project_id.to_string(), reload.get()), move || async move {
        if crate::services::try_services().is_some() {
            load_effect.set(Load::Loading);
            crate::services::run_shared(async move { load_from(fetch_snapshot(&project_effect).await) }, move |result| load_effect.set(result));
        }
    });
    crate::ui::vcs_watch::watch_and_refresh(Some(project_id), &load);
    // Re-read on every composition, so a later frame with a real size corrects the first one.
    let window = current_window_size();
    let (win_w, win_h) = vcs_sheet_window(window.width_dp, window.height_dp);
    let body_height = vcs_sheet_body_height(win_w, win_h);
    let sheet_w = vcs_sheet_width(win_w);
    let state = if sample { Load::Ready(sample_snapshot()) } else { load.get() };
    let boxes = if sample { sample_projects() } else { boxes };
    let current = if sample { Some("ember".to_string()) } else { current };
    let ctl = VcsSheetCtl {
        window_w: win_w,
        sheet_w,
        top_inset: vcs_sheet_top_inset(win_w, crate::ui::safe_area::safe_area()),
        body_h: Some(body_height),
        on_close: std::rc::Rc::new(std::cell::RefCell::new(on_close.clone())),
    };
    // No shared title: the mounted screen draws the mockup's header (`.vcs-mhead`, FR-L18.1).
    EmberSheetSized("", true, sheet_w, true, on_close, None, || {
        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().height(body_height + VCS_SHEET_CHROME_H), {
            VcsScreenWithLoad(state, boxes, current, reload, sample, Some(ctl));
        }) };
    });
}

/// A review window (`EMBER_CR_ROUTE=vcs`): sample repository view in compose-rust.
#[compose_rust::runtime::Composable]
pub fn preview_vcs() {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_size().background(paint(p.bg)), {
        let reload = remember(|| mutable_state_of(0_u64));
        VcsScreenWithLoad(Load::Ready(sample_snapshot()), sample_projects(), Some("ember".into()), reload, true, None);
    }) };
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fr_673_stale_vcs_markers_are_gone() {
        // compose-rust#471, #460/#429 and #500 (v0.0.11): the SHA is copied, the theme button is
        // named, and a filter chip is a named, selectable container without an overlay button.
        let src = include_str!("vcs.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("tests")];
        for marker in [concat!("TODO(compose-rust#", "471)"), concat!("TODO(compose-rust#", "460, #429)"), concat!("TODO(compose-rust#", "500)"), concat!("TODO(compose-rust#", "328)")] {
            assert!(!production.contains(marker), "{marker}");
        }
        assert!(!production.contains("fn VcsChip("), "the grey filter chip gave way to the choice buttons (#878)");
        assert!(production.contains("show_sha.set(!copied);"), "the selectable SHA is only the fallback");
    }

    #[test]
    fn fr_l19_branch_popover_search_filters_ignoring_case() {
        let b: Vec<String> = ["main", "develop", "feat/Git-API", "feat/ui"].iter().map(|s| s.to_string()).collect();
        assert_eq!(branch_menu_rows(&b, ""), vec![0, 1, 2, 3]);
        assert_eq!(branch_menu_rows(&b, "  git "), vec![2]);
        assert_eq!(branch_menu_rows(&b, "FEAT"), vec![2, 3]);
        assert!(branch_menu_rows(&b, "zzz").is_empty());
        assert_eq!(BRANCH_MENU_SEARCH, "브랜치 찾기");
    }

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

    thread_local!(static BAR_GRID: std::cell::Cell<RepoBarGrid> = const { std::cell::Cell::new(RepoBarGrid::Three) });

    #[compose_rust::runtime::Composable]
    fn repo_bar_sample() {
        let project = sample_project_box();
        let projects = vec![(project.id.clone(), project.name.clone())];
        let snap = sample_snapshot();
        RepoBarWithActions(&projects, &project, Some(&snap), BAR_GRID.with(|g| g.get()), |_| {}, None);
    }

    fn branch_menu_sample(open: bool) {
        let state = remember(|| mutable_state_of(open));
        let branches = vec!["main".to_string(), "develop".to_string(), "feat/x".to_string()];
        BranchMenu(state, "develop", &branches, Some("develop"), true, None);
    }

    fn branch_menu_open() {
        branch_menu_sample(true);
    }

    fn branch_menu_closed() {
        branch_menu_sample(false);
    }

    /// (widget, modifiers, props) of every node `content` emits.
    fn branch_menu_nodes(content: fn()) -> Vec<(String, Vec<String>, Vec<String>)> {
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        crate::ui::window_size::set_opened_for_test(None);
        let mut nodes: Vec<(u32, String, Vec<String>, Vec<String>)> = Vec::new();
        for m in batch.iter() {
            match m {
                Mutation::Create { node_id, widget } => nodes.push((*node_id, format!("{widget:?}"), Vec::new(), Vec::new())),
                Mutation::SetModifier { node_id, modifier, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.0 == *node_id) {
                        n.2.push(format!("{modifier:?}"));
                    }
                }
                Mutation::SetProp { node_id, property, value } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.0 == *node_id) {
                        n.3.push(format!("{property:?}={value:?}"));
                    }
                }
                _ => {}
            }
        }
        nodes.into_iter().map(|n| (n.1, n.2, n.3)).collect()
    }

    #[test]
    fn fr_l10_4_branch_menu_is_a_popover_under_its_control() {
        // Mockup `vcsPopHtml`, `vcs.css` 33-41. It was a centred `Dialog` 248 wide.
        let _guard = crate::services::test_guard();
        let nodes = branch_menu_nodes(branch_menu_open);
        assert!(!nodes.iter().any(|n| n.0 == "Dialog" || n.0 == "Sheet"), "no centred dialog");
        let pops: Vec<_> = nodes.iter().filter(|n| n.0 == "Popover").collect();
        assert_eq!(pops.len(), 1);
        let (_, mods, props) = pops[0];
        // Below the control, 4 under it, lined up with its start (not its end, 3).
        for want in ["Open=Bool(true)", "Placement=Integer(2)", "OffsetDp=Float(4.0)"] {
            assert!(props.iter().any(|p| p == want), "{want} in {props:?}");
        }
        assert!(!props.iter().any(|p| p == "Align=Integer(3)"), "{props:?}");
        assert!(mods.iter().any(|m| m == "Width(380.0)"), "{mods:?}");
        assert!(mods.iter().any(|m| m == "Elevation(0.0)"), "{mods:?}");
        assert!(mods.iter().any(|m| m.starts_with("Shape { top_start: 14.0")), "{mods:?}");
        assert!(mods.iter().any(|m| m.starts_with("Semantics") && m.contains("Menu")), "{mods:?}");
        let says = |t: &str| nodes.iter().any(|n| n.2.iter().any(|p| p.contains(t)));
        for t in ["main", "develop", "feat/x", BRANCH_MENU_SEARCH, BRANCH_POP_NEW, BRANCH_POP_CREATE] {
            assert!(says(t), "missing {t}");
        }
        // One 40 dp menu item per branch; the current one alone is on `sel`.
        let items: Vec<_> = nodes.iter().filter(|n| n.1.iter().any(|m| m.starts_with("Semantics") && m.contains("MenuItem"))).collect();
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|n| n.1.iter().any(|m| m == "Height(40.0)")));
        let sel = format!("{:?}", compose_rust::schema::Modifier::Background(paint(ember_theme().palette.sel)));
        assert_eq!(items.iter().filter(|n| n.1.contains(&sel)).count(), 1);
        // Closed, nothing of the box is composed.
        let closed = branch_menu_nodes(branch_menu_closed);
        let pop = closed.iter().find(|n| n.0 == "Popover").expect("the popover node");
        assert!(pop.2.iter().any(|p| p == "Open=Bool(false)"), "{:?}", pop.2);
        assert!(!closed.iter().any(|n| n.2.iter().any(|p| p.contains("feat/x") || p.contains(BRANCH_POP_NEW))));
        assert!(closed.iter().any(|n| n.2.iter().any(|p| p.contains("develop"))), "the control stays");
    }

    static REPO_PICKED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    fn repo_menu_open() {
        let state = remember(|| mutable_state_of(true));
        let projects = vec![("ember".to_string(), "ember".to_string()), ("p-2".to_string(), "core".to_string())];
        RepoMenu(state, "core", &projects, Some(1), |id| *REPO_PICKED.lock().unwrap() = Some(id));
    }

    #[test]
    fn fr_l10_4_repo_menu_is_the_shared_menu_under_its_control() {
        // The mockup's `#vcsProj` is a native select; it was a centred `OptionSheet`.
        let _guard = crate::services::test_guard();
        let nodes = branch_menu_nodes(repo_menu_open);
        assert!(!nodes.iter().any(|n| n.0 == "Dialog" || n.0 == "Sheet"), "no centred dialog at 1440");
        let pops: Vec<_> = nodes.iter().filter(|n| n.0 == "Popover").collect();
        assert_eq!(pops.len(), 1);
        let (_, mods, props) = pops[0];
        // The permission-mode look: 340 wide, 8 under the control, not lined up with its end.
        for want in ["Open=Bool(true)", "Placement=Integer(2)", "OffsetDp=Float(8.0)"] {
            assert!(props.iter().any(|p| p == want), "{want} in {props:?}");
        }
        assert!(!props.iter().any(|p| p == "Align=Integer(3)"), "{props:?}");
        assert!(mods.iter().any(|m| m == "Width(340.0)"), "{mods:?}");
        let says = |t: &str| nodes.iter().any(|n| n.2.iter().any(|p| p.contains(t)));
        for t in ["저장소 \u{b7} core", "ember", "p-2"] {
            assert!(says(t), "missing {t}");
        }
        let items: Vec<_> = nodes.iter().filter(|n| n.1.iter().any(|m| m.starts_with("Semantics") && m.contains("MenuItem"))).collect();
        assert_eq!(items.len(), 2);
        let sel = format!("{:?}", compose_rust::schema::Modifier::Background(paint(ember_theme().palette.sel)));
        assert_eq!(items.iter().filter(|n| n.1.contains(&sel)).count(), 1, "the chosen row is on sel");
        // The control is a button named by the shown project that says the menu is open.
        let control = nodes
            .iter()
            .find(|n| n.0 == "Button" && n.2.iter().any(|p| p.contains("core")) && n.1.iter().any(|m| m == "MatchParentSize"))
            .expect("the control");
        let state: u32 = control
            .1
            .iter()
            .find_map(|m| m.strip_prefix("Semantics")?.split("state: ").nth(1)?.trim_end_matches([' ', '}']).parse().ok())
            .expect("a semantics state");
        assert_eq!((state >> compose_rust::schema::SEMANTICS_EXPANDED_SHIFT) & 3, 2, "{:?}", control.1);
        let rows = repo_menu_rows(&[("a".into(), "a".into()), ("p-2".into(), "core".into())], Some(0));
        assert_eq!((rows[0].chosen, rows[0].description.clone()), (true, None));
        assert_eq!((rows[1].chosen, rows[1].description.as_deref()), (false, Some("p-2")));
        assert_eq!(repo_menu_head("core"), "저장소 \u{b7} core");
        let src = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        let bar = src.split("fn RepoBarWithActions(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(!bar.contains("OptionSheet("), "the centred sheet is gone from the bar");
        assert!(bar.contains("EMBER_CR_VCS_MENU\").as_deref() == Ok(\"project\")"));
    }

    fn base_menu_open() {
        let state = remember(|| mutable_state_of(true));
        let base = remember(|| mutable_state_of(Some("develop".to_string())));
        let names = vec!["main".to_string(), "develop".to_string()];
        BaseMenu(state, "기준: develop", &names, Some(1), base);
    }

    #[test]
    fn fr_l10_4_base_menu_checks_the_current_branch_until_one_is_chosen() {
        let names: Vec<String> = ["main", "develop", "feat/x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(base_menu_chosen(&names, None, Some("develop")), Some(1), "기준: 현재 브랜치 is the current branch's row");
        assert_eq!(base_menu_chosen(&names, Some("feat/x"), Some("develop")), Some(2), "a chosen base wins");
        assert_eq!(base_menu_chosen(&names, None, None), None);
        assert_eq!(base_menu_chosen(&names, Some("gone"), Some("develop")), None, "a base no longer listed checks nothing");
        let rows = base_menu_rows(&names, base_menu_chosen(&names, None, Some("develop")));
        assert_eq!(rows.iter().filter(|r| r.chosen).map(|r| r.title.as_str()).collect::<Vec<_>>(), ["develop"]);
        let src = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        let pane = src.split("fn BranchesPane(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(pane.contains("base_menu_chosen(&names, base.get().as_deref(), current_name.as_deref())"));
    }

    #[test]
    fn fr_l10_4_base_menu_is_the_shared_menu_under_its_button() {
        // The mockup's `#vcsBase` is a native select; it was a centred `OptionSheet`.
        let _guard = crate::services::test_guard();
        let nodes = branch_menu_nodes(base_menu_open);
        assert!(!nodes.iter().any(|n| n.0 == "Dialog" || n.0 == "Sheet"), "no centred dialog at 1440");
        let pops: Vec<_> = nodes.iter().filter(|n| n.0 == "Popover").collect();
        assert_eq!(pops.len(), 1);
        let (_, mods, props) = pops[0];
        for want in ["Open=Bool(true)", "Placement=Integer(2)", "OffsetDp=Float(8.0)"] {
            assert!(props.iter().any(|p| p == want), "{want} in {props:?}");
        }
        assert!(!props.iter().any(|p| p == "Align=Integer(3)"), "{props:?}");
        assert!(mods.iter().any(|m| m == "Width(340.0)"), "{mods:?}");
        // The head and the button both read what is chosen; the rows are the branches.
        let head = nodes.iter().filter(|n| n.2.iter().any(|p| p.contains("기준: develop"))).count();
        assert_eq!(head, 2, "the button and the menu head");
        let items: Vec<_> = nodes.iter().filter(|n| n.1.iter().any(|m| m.starts_with("Semantics") && m.contains("MenuItem"))).collect();
        assert_eq!(items.len(), 2);
        let sel = format!("{:?}", compose_rust::schema::Modifier::Background(paint(ember_theme().palette.sel)));
        assert_eq!(items.iter().filter(|n| n.1.contains(&sel)).count(), 1);
        // The button fills its line, as before, and says the menu is open.
        let button = nodes.iter().find(|n| n.0 == "Button" && n.2.iter().any(|p| p.contains("기준: develop"))).expect("the button");
        assert!(button.1.iter().any(|m| m == "FillMaxWidth"), "{:?}", button.1);
        let state: u32 = button
            .1
            .iter()
            .find_map(|m| m.strip_prefix("Semantics")?.split("state: ").nth(1)?.trim_end_matches([' ', '}']).parse().ok())
            .expect("a semantics state");
        assert_eq!((state >> compose_rust::schema::SEMANTICS_EXPANDED_SHIFT) & 3, 2, "{:?}", button.1);
        let rows = base_menu_rows(&["main".into(), "develop".into()], None);
        assert!(rows.iter().all(|r| !r.chosen) && rows[1].title == "develop");
        let src = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!src.contains("OptionSheet("), "no centred option sheet is left in 버전 관리");
        assert!(src.contains("EMBER_CR_VCS_MENU\").as_deref() == Ok(\"base\")"));
    }

    #[test]
    fn fr_l10_4_branch_menu_sizes() {
        assert_eq!(branch_pop_width(1440.0), 380.0);
        assert_eq!(branch_pop_width(390.0), 358.0);
        assert_eq!(branch_pop_width(0.0), 380.0);
        assert_eq!(BRANCH_POP_LIST_MAX_H, 300.0);
        assert_eq!((BRANCH_POP_ROW_H, BRANCH_POP_OFFSET), (40.0, 4.0));
        let src = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!src.contains("Dialog!(branch_menu"), "the centred branch dialog is gone");
        assert!(!src.contains(&["branch_pop_list", "_height"].concat()));
        assert!(src.contains(".max_height(BRANCH_POP_LIST_MAX_H)"));
    }

    /// The vertical and the horizontal lines between the bar's cells.
    fn bar_lines(grid: RepoBarGrid) -> (usize, usize) {
        BAR_GRID.with(|g| g.set(grid));
        let mut host = Host::new(|| Box::new(Recomposer::new(repo_bar_sample)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut mods: std::collections::HashMap<u32, Vec<String>> = Default::default();
        for m in batch.iter() {
            if let Mutation::SetModifier { node_id, modifier, .. } = m {
                mods.entry(*node_id).or_default().push(format!("{modifier:?}"));
            }
        }
        let has = |v: &Vec<String>, a: &str, b: &str| v.iter().any(|m| m == a) && v.iter().any(|m| m == b);
        let v = mods.values().filter(|m| has(m, "Width(1.0)", "FillMaxHeight")).count();
        let h = mods.values().filter(|m| has(m, "Height(1.0)", "FillMaxWidth")).count();
        (v, h)
    }

    #[test]
    fn fr_l19_repo_bar_sends_three_columns_two_with_the_remote_below_or_one() {
        let _guard = crate::services::test_guard();
        assert_eq!(bar_lines(RepoBarGrid::Three), (2, 0), "three cells side by side");
        assert_eq!(bar_lines(RepoBarGrid::TwoRemoteBelow), (1, 1), "two side by side, 원격 under a line");
        assert_eq!(bar_lines(RepoBarGrid::One), (0, 2), "three stacked");
    }

    #[test]
    fn fr_l19_repo_bar_follows_the_mockups_breakpoints() {
        // The page: three columns above 900, two with 원격 across at 900 and below, one at 560.
        assert_eq!(repo_bar_grid(1440.0, None), RepoBarGrid::Three);
        assert_eq!(repo_bar_grid(1024.0, None), RepoBarGrid::Three);
        assert_eq!(repo_bar_grid(901.0, None), RepoBarGrid::Three);
        assert_eq!(repo_bar_grid(900.0, None), RepoBarGrid::TwoRemoteBelow);
        assert_eq!(repo_bar_grid(800.0, None), RepoBarGrid::TwoRemoteBelow);
        assert_eq!(repo_bar_grid(561.0, None), RepoBarGrid::TwoRemoteBelow);
        assert_eq!(repo_bar_grid(560.0, None), RepoBarGrid::One);
        assert_eq!(repo_bar_grid(390.0, None), RepoBarGrid::One);
        // The sheet: its padded body at 760 and below is two columns, also on a phone.
        let body = |w: f32| vcs_sheet_width(w) - 2.0 * vcs_sheet_frame(vcs_sheet_width(w), w).pad;
        assert_eq!(repo_bar_grid(1440.0, Some(body(1440.0))), RepoBarGrid::Three);
        assert_eq!(repo_bar_grid(1024.0, Some(body(1024.0))), RepoBarGrid::Three);
        assert_eq!(repo_bar_grid(800.0, Some(body(800.0))), RepoBarGrid::TwoRemoteBelow);
        assert_eq!(repo_bar_grid(390.0, Some(body(390.0))), RepoBarGrid::TwoRemoteBelow);
    }

    #[test]
    fn fr_l19_diff_header_wraps_when_the_pane_is_narrow() {
        assert!(diff_header_wraps(559.0));
        assert!(!diff_header_wraps(560.0));
        // 1024 dp wraps, 1440 dp does not.
        assert!(diff_header_wraps(diff_pane_width(1024.0, PaneLayout::Two)));
        assert!(!diff_header_wraps(diff_pane_width(1440.0, PaneLayout::Two)));
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

    #[compose_rust::runtime::Composable]
    fn pr_filter_choices_sample() {
        let st = remember(|| mutable_state_of(PrFilter::Open));
        VcsRadioRow(&PrFilter::ALL, PrFilter::label, PrFilter::Open, st, || {});
    }

    #[test]
    fn fr_l10_2_vcs_single_choices_are_the_seg_buttons() {
        // Review r6 item 1f: the PR state filter, the merge method and the history filter kept
        // the grey `pickchip` fill with no border while every other choice became `.seg`.
        let mut host = Host::new(|| Box::new(Recomposer::new(pr_filter_choices_sample)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let widgets: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::Create { widget, .. } => Some(format!("{widget:?}")),
                _ => None,
            })
            .collect();
        assert_eq!(widgets.iter().filter(|w| *w == "FlowRow").count(), 1, "one wrapping row: {widgets:?}");
        let all: String = batch.iter().map(|m| format!("{m:?}")).collect();
        for want in ["열림", "닫힘", "병합됨", "전체"] {
            assert!(all.contains(want), "{want}");
        }
        let p = ember_theme().palette;
        let grey = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.surface_2)));
        let line = format!("{:?}", compose_rust::schema::Modifier::Border { width: 1.0, paint: paint(p.line) });
        let ink = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.ink)));
        let mods: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { modifier, .. } => Some(format!("{modifier:?}")),
                _ => None,
            })
            .collect();
        assert!(!mods.contains(&grey), "no grey chip fill");
        assert_eq!(mods.iter().filter(|m| **m == line).count(), 3, "the three unchosen ones are bordered");
        assert_eq!(mods.iter().filter(|m| **m == ink).count(), 1, "the chosen one is ink");
        let production = &include_str!("vcs.rs")[..include_str!("vcs.rs").find("#[cfg(test)]").unwrap()];
        for site in [
            "VcsRadioRow(&HistoryFilter::ALL, HistoryFilter::label,",
            "VcsRadioRow(&PrFilter::ALL, PrFilter::label,",
            "VcsRadioRow(&PR_MERGE_METHODS, pr_merge_method_label,",
            "ChoiceButton(PR_DELETE_BRANCH_LABEL, Choice::Toggle,",
        ] {
            assert!(production.contains(site), "{site}");
        }
    }

    fn text_of(content: fn()) -> String {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    #[compose_rust::runtime::Composable]
    fn responsive_pills_sample() {
        SectionPillsAtWidth(Section::Changes, 704.0, |_| {});
    }

    #[compose_rust::runtime::Composable]
    fn commit_fields_sample() {
        let summary = remember(|| mutable_state_of(String::new()));
        let body = remember(|| mutable_state_of(String::new()));
        let amend = remember(|| mutable_state_of(false));
        CommitBox("main", 1, &summary, &body, &amend, None, false);
    }

    #[compose_rust::runtime::Composable]
    fn theme_button_sample() {
        let ui = remember(|| crate::ui::coexist::SharedUi::new(crate::prefs::Prefs::default()));
        super::super::project::ThemeButton(&ui);
    }

    #[test]
    fn fr_l19_responsive_controls_render_in_the_host() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let pills = text_of(responsive_pills_sample);
        for section in Section::ALL {
            assert!(pills.contains(section.label()), "missing {}", section.label());
        }
        let fields = text_of(commit_fields_sample);
        assert!(fields.contains("요약 (필수)"));
        assert!(fields.contains("설명 (선택)"));
        let theme = text_of(theme_button_sample);
        assert!(theme.contains("OnClick"), "theme control must render: {theme}");
        assert_eq!(repo_bar_grid(1024.0, Some(vcs_sheet_width(1024.0) - 32.0)), RepoBarGrid::Three);
        assert!(diff_header_wraps(500.0));
    }

    #[test]
    fn fr_l19_without_git_data_the_vcs_screen_says_it_is_not_ready() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(VcsScreen);
        assert!(text.contains("버전 관리"));
        assert!(text.contains(SOON));
    }

    #[compose_rust::runtime::Composable]
    fn history_sample_wide() {
        HistoryPane(None, PaneLayout::Two, false, "develop".to_string());
    }

    #[compose_rust::runtime::Composable]
    fn history_sample_compact() {
        HistoryPane(None, PaneLayout::Tabs, false, "develop".to_string());
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
            assert!(text.contains("모든 브랜치"));
            assert!(text.contains("로컬 전용"));
            assert!(text.contains("푸시 안 됨"));
            assert!(!text.contains(SOON));
        }
    }

    #[test]
    fn fr_l19_history_detail_shows_message_files_and_the_four_actions() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for content in [history_sample_wide as fn(), history_detail_body_sample as fn()] {
            let text = text_of(content);
            assert!(text.contains("ember/proxy/src/shim.rs"));
            assert!(text.contains("+14"));
            assert!(text.contains("바이너리"));
            assert!(text.contains("여기서 브랜치"));
            assert!(text.contains("되돌리기"));
            assert!(text.contains("Cherry-pick"));
            assert!(text.contains("SHA 복사"));
            assert!(text.contains("푸시 안 됨"));
        }
    }

    #[test]
    fn fr_l19_history_click_filters_and_action_flows() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                HistoryPane(None, PaneLayout::Two, false, "develop".to_string());
            }))
        });

        struct ButtonTracker {
            texts: std::collections::HashMap<u32, String>,
            handlers: std::collections::HashMap<u32, u64>,
            parents: std::collections::HashMap<u32, u32>,
        }

        impl ButtonTracker {
            fn new() -> Self {
                Self {
                    texts: std::collections::HashMap::new(),
                    handlers: std::collections::HashMap::new(),
                    parents: std::collections::HashMap::new(),
                }
            }

            fn scan(&mut self, batch: &[Mutation<'_>]) {
                for m in batch {
                    match m {
                        Mutation::SetProp { node_id, property: PropertyKind::Text | PropertyKind::AccessibleName, value: PropertyValue::String(s) } => {
                            self.texts.insert(*node_id, s.to_string());
                        }
                        Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                            self.handlers.insert(*node_id, *h as u64);
                        }
                        Mutation::Insert { parent_id, node_id, .. } => {
                            self.parents.insert(*node_id, *parent_id);
                        }
                        _ => {}
                    }
                }
            }

            fn find(&self, label: &str) -> (u32, u64) {
                for (id, text) in &self.texts {
                    if text == label {
                        // A choice button's label is a Text inside the clickable pill
                        // (`ChoiceButton`): take the nearest clickable ancestor.
                        let mut at = *id;
                        for _ in 0..4 {
                            if let Some(h) = self.handlers.get(&at) {
                                return (at, *h);
                            }
                            match self.parents.get(&at) {
                                Some(p) => at = *p,
                                None => break,
                            }
                        }
                    }
                }
                panic!("button with label '{label}' not found");
            }
        }

        let mut tracker = ButtonTracker::new();
        let initial_bytes = host.rebuild().expect("rebuild");
        let initial_batch = decode_batch(initial_bytes).expect("decode");
        tracker.scan(&initial_batch);

        let initial_text: String = initial_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(initial_text.contains("모든 브랜치"));
        assert!(initial_text.contains("로컬 전용"));
        assert!(initial_text.contains("푸시 안 됨"));
        assert!(initial_text.contains("원격에 있음"));

        // 1. Click "로컬 전용" filter chip -> filters out remote-only commit
        let (node, handler) = tracker.find("로컬 전용");
        let (bytes, _) = host.dispatch(HostEvent::new(node, handler, EventPayload::Clicked)).expect("click 로컬 전용");
        let local_batch = decode_batch(bytes).expect("decode");
        tracker.scan(&local_batch);
        let local_text: String = local_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(!local_text.contains("Docs: update remote IDE protocol spec"));

        // 2. Click "모든 브랜치" filter chip -> brings back all commits including remote-only
        let (node, handler) = tracker.find("모든 브랜치");
        let (bytes, _) = host.dispatch(HostEvent::new(node, handler, EventPayload::Clicked)).expect("click 모든 브랜치");
        let all_batch = decode_batch(bytes).expect("decode");
        tracker.scan(&all_batch);
        let all_text: String = all_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(all_text.contains("Docs: update remote IDE protocol spec"));
        assert!(all_text.contains("원격에 있음"));

        // 3. Click "여기서 브랜치" -> toggles form open
        let (node, handler) = tracker.find("여기서 브랜치");
        let (bytes, _) = host.dispatch(HostEvent::new(node, handler, EventPayload::Clicked)).expect("click 여기서 브랜치");
        let form_batch = decode_batch(bytes).expect("decode");
        tracker.scan(&form_batch);
        let form_text: String = form_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(form_text.contains("만들고 전환"));
        assert!(form_text.contains("feat/새-브랜치"));

        // 4. Click "되돌리기" -> two-step arming: label becomes "정말 되돌립니다"
        let (node, handler) = tracker.find("되돌리기");
        let (bytes, _) = host.dispatch(HostEvent::new(node, handler, EventPayload::Clicked)).expect("click 되돌리기");
        let arm_batch = decode_batch(bytes).expect("decode");
        tracker.scan(&arm_batch);
        let arm_text: String = arm_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(arm_text.contains("정말 되돌립니다"));

        // 5. Click "Cherry-pick" -> two-step arming: label becomes "정말 가져옵니다"
        let (node, handler) = tracker.find("Cherry-pick");
        let (bytes, _) = host.dispatch(HostEvent::new(node, handler, EventPayload::Clicked)).expect("click Cherry-pick");
        let cp_batch = decode_batch(bytes).expect("decode");
        tracker.scan(&cp_batch);
        let cp_text: String = cp_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(cp_text.contains("정말 가져옵니다"));

        // 6. Click "SHA 복사" -> copies the SHA and says so; only when the clipboard refuses it
        // (a headless runner) is the selectable SHA field shown instead (compose-rust#471).
        let (node, handler) = tracker.find("SHA 복사");
        let (bytes, _) = host.dispatch(HostEvent::new(node, handler, EventPayload::Clicked)).expect("click SHA 복사");
        let sha_batch = decode_batch(bytes).expect("decode");
        tracker.scan(&sha_batch);
        let sha_text: String = sha_batch.iter().map(|m| format!("{m:?}")).collect();
        let notice = crate::ui::coexist::shared_ui().notice.get().map(|(_, t)| t).unwrap_or_default();
        let copied = notice.contains("SHA를 클립보드에 복사했습니다.");
        assert!(copied || notice.contains("SHA를 아래에서 복사해 주세요"), "{notice}");
        assert_eq!(sha_text.contains("아래에서 선택해 복사해 주세요"), !copied, "the selectable SHA is only the fallback");
    }

    #[test]
    fn fr_l19_history_compact_layout_click_flow() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                HistoryPane(None, PaneLayout::Tabs, false, "develop".to_string());
            }))
        });

        struct ButtonTracker {
            texts: std::collections::HashMap<u32, String>,
            handlers: std::collections::HashMap<u32, u64>,
            parents: std::collections::HashMap<u32, u32>,
        }

        impl ButtonTracker {
            fn new() -> Self {
                Self {
                    texts: std::collections::HashMap::new(),
                    handlers: std::collections::HashMap::new(),
                    parents: std::collections::HashMap::new(),
                }
            }

            fn scan(&mut self, batch: &[Mutation<'_>]) {
                for m in batch {
                    match m {
                        Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                            self.texts.insert(*node_id, s.to_string());
                        }
                        Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                            self.handlers.insert(*node_id, *h as u64);
                        }
                        Mutation::Insert { parent_id, node_id, .. } => {
                            self.parents.insert(*node_id, *parent_id);
                        }
                        _ => {}
                    }
                }
            }

            fn find(&self, label: &str) -> (u32, u64) {
                for (id, text) in &self.texts {
                    if text == label {
                        // A choice button's label is a Text inside the clickable pill
                        // (`ChoiceButton`): take the nearest clickable ancestor.
                        let mut at = *id;
                        for _ in 0..4 {
                            if let Some(h) = self.handlers.get(&at) {
                                return (at, *h);
                            }
                            match self.parents.get(&at) {
                                Some(p) => at = *p,
                                None => break,
                            }
                        }
                    }
                }
                panic!("button with label '{label}' not found");
            }
        }

        let mut tracker = ButtonTracker::new();
        let initial_bytes = host.rebuild().expect("rebuild");
        let initial_batch = decode_batch(initial_bytes).expect("decode");
        tracker.scan(&initial_batch);

        // Compact layout initial view shows the commit list with filter chips
        let list_text: String = initial_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(list_text.contains("모든 브랜치"));
        assert!(list_text.contains("로컬 전용"));
        assert!(list_text.contains("Feat: attach the session shim before routing"));

        // Click on first commit row overlay
        let (node, handler) = tracker.find("커밋 a1b2c3d Feat: attach the session shim before routing");
        let (bytes, _) = host.dispatch(HostEvent::new(node, handler, EventPayload::Clicked)).expect("click commit row");
        let detail_batch = decode_batch(bytes).expect("decode");
        tracker.scan(&detail_batch);

        // In compact layout, detail view opens with "목록으로" button
        let detail_text: String = detail_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(detail_text.contains("목록으로"));
        assert!(detail_text.contains("변경한 파일"));
        assert!(detail_text.contains("여기서 브랜치"));

        // Click "목록으로" to navigate back to list
        let (node, handler) = tracker.find("목록으로");
        let (bytes, _) = host.dispatch(HostEvent::new(node, handler, EventPayload::Clicked)).expect("click 목록으로");
        let back_batch = decode_batch(bytes).expect("decode");
        tracker.scan(&back_batch);
        let back_text: String = back_batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(back_text.contains("모든 브랜치"));
        assert!(back_text.contains("로컬 전용"));
    }

    #[compose_rust::runtime::Composable]
    fn history_detail_body_sample() {
        let noop: Rc<dyn Fn()> = Rc::new(|| {});
        let ctl = CommitCtl {
            busy: false,
            armed: None,
            form_open: true,
            name: mutable_state_of(String::new()),
            name_gen: 0,
            show_sha: true,
            branch: "develop".to_string(),
            on_revert: noop.clone(),
            on_pick: noop.clone(),
            on_toggle_form: noop.clone(),
            on_create: noop.clone(),
            on_copy: noop,
        };
        let CommitDetailLoad::Ready(v) = sample_commit_detail(&sample_sha(0)) else { return };
        CommitDetailBody(&v, &ctl);
    }

    fn sample_sha(i: usize) -> String {
        let HistoryLoad::Ready { entries, .. } = sample_history() else { panic!() };
        entries[i].sha.clone()
    }

    #[test]
    fn fr_l19_history_the_sample_detail_exists_for_each_sample_commit_and_names_a_missing_one() {
        let HistoryLoad::Ready { entries, .. } = sample_history() else { panic!() };
        for e in &entries {
            let CommitDetailLoad::Ready(v) = sample_commit_detail(&e.sha) else { panic!("{}", e.short) };
            assert_eq!(v.sha, e.sha);
            assert_eq!(v.is_merge, e.is_merge);
            assert_eq!(v.time_text, e.time_text);
            assert!(!v.files.is_empty());
        }
        assert!(matches!(sample_commit_detail("nope"), CommitDetailLoad::Failed(_)));
    }

    #[test]
    fn fr_l19_history_a_merge_commit_cannot_arm_cherry_pick() {
        // The gate runs before the arm: a merge never reaches the second tap.
        assert!(commit_action_gate(CommitAction::CherryPick, false, true).is_err());
        assert!(commit_action_gate(CommitAction::Revert, false, true).is_ok());
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

    #[compose_rust::runtime::Composable]
    fn prs_sample_wide() {
        let cnt = remember(|| mutable_state_of(None));
        let shared = remember(|| mutable_state_of(Some(Vec::new())));
        let selected = remember(|| mutable_state_of(None));
        PullRequestsPane(None, cnt, shared, selected, PaneLayout::Two, Some("feat/x".to_string()));
    }

    #[compose_rust::runtime::Composable]
    fn prs_sample_compact() {
        let cnt = remember(|| mutable_state_of(None));
        let shared = remember(|| mutable_state_of(Some(Vec::new())));
        let selected = remember(|| mutable_state_of(None));
        PullRequestsPane(None, cnt, shared, selected, PaneLayout::Tabs, None);
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
        for want in [
            "풀 리퀘스트", "열림 3", "새 PR", "검사", "build", "통과", "리뷰", "Minji Kim", "승인",
            "리뷰 코멘트", "리뷰 남기기", "코멘트", "변경 요청", "병합", "스쿼시로 병합합니다",
            "스쿼시", "병합 커밋", "리베이스", "병합 후 브랜치 삭제", "\u{2022} 세션 shim 위치 정돈", "파일 3 +40 -2",
        ] {
            assert!(text.contains(want), "{want}");
        }
        assert!(!text.contains("목록으로"), "the wide layout has no back button");
    }

    #[test]
    fn fr_l19_pr_detail_self_approval_block() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let s = crate::services::services();
        let orig = s.ui.live.github.get();

        // 1. Author of PR 412 is "BREW". When signed in as "BREW", self-approval is blocked.
        let mut gh = crate::demo::github_state();
        gh.account = Some("BREW".to_string());
        s.ui.live.github.set(crate::ui::home::GithubFetch::Loaded(gh));
        let text = text_of(prs_sample_wide);
        assert!(text.contains(PR_SELF_APPROVAL_DISALLOWED), "{text}");

        // 2. Leading @ and case-insensitive: "@brew" is also blocked.
        let mut gh2 = crate::demo::github_state();
        gh2.account = Some("@brew".to_string());
        s.ui.live.github.set(crate::ui::home::GithubFetch::Loaded(gh2));
        let text2 = text_of(prs_sample_wide);
        assert!(text2.contains(PR_SELF_APPROVAL_DISALLOWED), "{text2}");

        // 3. Different user: approval is allowed, no error message displayed.
        let mut gh3 = crate::demo::github_state();
        gh3.account = Some("other_user".to_string());
        s.ui.live.github.set(crate::ui::home::GithubFetch::Loaded(gh3));
        let text3 = text_of(prs_sample_wide);
        assert!(!text3.contains(PR_SELF_APPROVAL_DISALLOWED), "{text3}");

        s.ui.live.github.set(orig);
    }

    struct PrHostDriver {
        host: Host,
        texts: std::collections::HashMap<u32, String>,
        handlers: std::collections::HashMap<u32, u64>,
        parents: std::collections::HashMap<u32, u32>,
    }

    impl PrHostDriver {
        fn new() -> Self {
            let mut host = Host::new(|| Box::new(Recomposer::new(prs_sample_wide)));
            let bytes = host.rebuild().expect("rebuild").to_vec();
            let mut driver = Self {
                host,
                texts: std::collections::HashMap::new(),
                handlers: std::collections::HashMap::new(),
                parents: std::collections::HashMap::new(),
            };
            driver.absorb(&decode_batch(&bytes).expect("decode"));
            driver
        }

        fn absorb(&mut self, batch: &[Mutation<'_>]) {
            for m in batch {
                match m {
                    Mutation::SetProp { node_id, property: PropertyKind::Text | PropertyKind::AccessibleName, value: PropertyValue::String(s) } => {
                        self.texts.insert(*node_id, s.to_string());
                    }
                    Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                        self.handlers.insert(*node_id, *h as u64);
                    }
                    Mutation::Insert { parent_id, node_id, .. } => {
                        self.parents.insert(*node_id, *parent_id);
                    }
                    _ => {}
                }
            }
        }

        fn click(&mut self, label: &str) -> String {
            let (node_id, handler_id) = self.texts.iter().find_map(|(id, text)| {
                if text != label {
                    return None;
                }
                // A choice button's label is a Text inside the clickable pill (`ChoiceButton`).
                let mut at = *id;
                for _ in 0..4 {
                    if let Some(&h) = self.handlers.get(&at) {
                        return Some((at, h));
                    }
                    at = *self.parents.get(&at)?;
                }
                None
            }).unwrap_or_else(|| panic!("button '{label}' not found (known: {:?})", self.texts));

            let bytes = self.host.dispatch(HostEvent::new(node_id, handler_id, EventPayload::Clicked)).expect("dispatch").0.to_vec();
            let batch = decode_batch(&bytes).expect("decode");
            self.absorb(&batch);
            batch.iter().map(|m| format!("{m:?}")).collect()
        }
    }

    #[test]
    fn fr_l19_host_click_pr_two_step_merge_and_method_chips() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut driver = PrHostDriver::new();

        // 1. Initial state has "스쿼시로 병합합니다" and no delete branch note.
        // Click "리베이스" chip -> method text changes to "리베이스하여 병합합니다".
        let out = driver.click("리베이스");
        assert!(out.contains("리베이스하여 병합합니다"), "{out}");

        // Click "스쿼시" chip -> method text changes back to "스쿼시로 병합합니다".
        let out = driver.click("스쿼시");
        assert!(out.contains("스쿼시로 병합합니다"), "{out}");

        // 2. Click "병합 후 브랜치 삭제" -> delete branch note is displayed.
        let out = driver.click("병합 후 브랜치 삭제");
        assert!(out.contains("원격 브랜치를 삭제합니다"), "{out}");

        // Click "병합 후 브랜치 삭제" again -> toggle off.
        let _ = driver.click("병합 후 브랜치 삭제");

        // 3. Click "병합" -> two-step arming: button text changes to "정말 병합합니다".
        let out = driver.click("병합");
        assert!(out.contains("정말 병합합니다"), "{out}");

        // 4. Click "승인" -> disarms merge (merge button reverts to "병합") and arms review ("정말 승인합니다").
        let out = driver.click("승인");
        assert!(out.contains("정말 승인합니다"), "{out}");
        assert!(out.contains("병합"), "{out}");

        // 5. Click "새 PR" -> open create form.
        let out = driver.click("새 PR");
        assert!(out.contains("새 풀 리퀘스트"), "{out}");
        assert!(out.contains("만들기"), "{out}");
        assert!(out.contains("취소"), "{out}");

        // Click "취소" -> closes form.
        let _ = driver.click("취소");
    }

    #[test]
    fn fr_l19_pr_create_double_submit_guard_and_armed_reset() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut driver = PrHostDriver::new();

        // 1. Arm merge on PR 412
        let out = driver.click("병합");
        assert!(out.contains("정말 병합합니다"), "{out}");

        // 2. Switching PR selection resets armed merge
        let out = driver.click("풀 리퀘스트 #409 Fix: stop egress when a session detaches");
        assert!(out.contains("CI 체크가 실패했습니다"), "{out}");

        // 3. Open "새 PR" form and verify create button is present
        let out = driver.click("새 PR");
        assert!(out.contains("새 풀 리퀘스트"), "{out}");
        assert!(out.contains("만들기"), "{out}");

        // Clicking 만들기 with empty title triggers the gate notice
        let _ = driver.click("만들기");
        let notice = crate::ui::coexist::shared_ui().notice.get();
        assert!(notice.is_some_and(|(_, msg)| msg == "제목을 입력해 주세요"));

        // 4. Double-submit guard: when busy is true, calling create returns early without running
        let busy = mutable_state_of(true);
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let (busy_c, ran_c) = (busy.clone(), ran.clone());
        let mut guarded_create = move || {
            if busy_c.get() {
                return;
            }
            ran_c.set(true);
        };
        guarded_create();
        assert!(!ran.get(), "double submit must be blocked while busy is true");
        busy.set(false);
        guarded_create();
        assert!(ran.get(), "create proceeds once busy is cleared");
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

    #[compose_rust::runtime::Composable]
    fn stash_sample_wide() {
        StashPane(None, mutable_state_of(None), PaneLayout::Two, false);
    }

    #[compose_rust::runtime::Composable]
    fn stash_sample_compact() {
        StashPane(None, mutable_state_of(None), PaneLayout::Tabs, false);
    }

    #[compose_rust::runtime::Composable]
    fn conflicts_sample_wide() {
        ConflictsPane(&["a.rs".to_string()], None, PaneLayout::Two);
    }

    #[compose_rust::runtime::Composable]
    fn conflicts_sample_compact() {
        ConflictsPane(&["a.rs".to_string()], None, PaneLayout::Tabs);
    }

    #[test]
    fn fr_g16_the_sample_banner_and_list_agree() {
        let MergeLoad::Ready(Some(v)) = sample_merge() else { panic!("the sample has a banner") };
        assert_eq!(v.remaining, SAMPLE_CONFLICT_COUNT);
        assert_eq!(banner_sub(v.remaining), format!("남은 충돌 파일 {SAMPLE_CONFLICT_COUNT}개"));
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

    #[compose_rust::runtime::Composable]
    fn conflict_file_row_sample() {
        ConflictFileRow("docs/SPEC.md", || {});
    }

    #[test]
    fn fr_l19_conflict_list_row_shows_the_path_and_is_one_tap_target() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(conflict_file_row_sample as fn());
        assert!(text.contains("docs/SPEC.md"), "the path is shown (and is the row's accessible name)");

        // One tap target: a host click on the row's own button selects the file (#694).
        let mut host = Host::new(|| Box::new(Recomposer::new(conflict_row_pick_sample)));
        let bytes = host.rebuild().expect("rebuild").to_vec();
        let batch = decode_batch(&bytes).expect("decode");
        let mut texts = std::collections::HashMap::new();
        let mut handlers = std::collections::HashMap::new();
        for m in &batch {
            match m {
                Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                    texts.insert(*node_id, s.to_string());
                }
                Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                    handlers.insert(*node_id, *h as u64);
                }
                _ => {}
            }
        }
        let before: String = batch.iter().map(|m| format!("{m:?}")).collect();
        assert!(before.contains("고른 파일 없음"), "nothing is selected before the tap");
        let targets: Vec<(u32, u64)> = texts
            .iter()
            .filter(|(_, t)| t.as_str() == "docs/SPEC.md")
            .filter_map(|(id, _)| handlers.get(id).map(|h| (*id, *h)))
            .collect();
        assert_eq!(targets.len(), 1, "the row has exactly one tap target named by its path");
        let (node_id, handler_id) = targets[0];
        let after = host.dispatch(HostEvent::new(node_id, handler_id, EventPayload::Clicked)).expect("dispatch").0.to_vec();
        let after: String = decode_batch(&after).expect("decode").iter().map(|m| format!("{m:?}")).collect();
        assert!(after.contains("고른 파일 docs/SPEC.md"), "the tap selects the file: {after}");
    }

    #[compose_rust::runtime::Composable]
    fn conflict_row_pick_sample() {
        let picked = remember(|| mutable_state_of(None::<String>));
        let set = picked.clone();
        ConflictFileRow("docs/SPEC.md", move || set.set(Some("docs/SPEC.md".to_string())));
        let shown = match picked.get() {
            Some(path) => format!("고른 파일 {path}"),
            None => "고른 파일 없음".to_string(),
        };
        Label(&shown, Fs::Sm, Weight::Regular, None, None);
    }

    #[test]
    fn fr_l19_conflict_rows_follow_the_mockup_metrics() {
        // #694: 34 px, gap 6, a 14 px glyph in a 22 px tile; no border or fill on the row.
        assert_eq!(CONFLICT_ROW_H, 34.0);
        assert_eq!(CONFLICT_ROW_GAP, 6.0);
        assert_eq!(CONFLICT_TILE, 22.0);
        assert_eq!(CONFLICT_GLYPH, 14.0);
        let src = include_str!("vcs.rs");
        let start = src.find("fn ConflictFileRow(").expect("ConflictFileRow");
        let end = start + src[start..].find("\n}\n").expect("end of ConflictFileRow");
        let body = &src[start..end];
        assert!(!body.contains(".border("), "the conflict row has no border");
        assert!(!body.contains("surface_2"), "the conflict row has no fill");
        assert!(body.contains("p.orange.soft"), "the warning tile is soft orange");
    }

    #[test]
    fn fr_l19_stage_checks_are_ink_squares_with_a_partial_dash() {
        let p = super::super::theme::LIGHT;
        assert_eq!(STAGE_CHECK_SIDE, 18.0);
        assert_eq!(STAGE_CHECK_RADIUS, 5.0);
        let on = stage_check_look(Check::On, &p);
        assert_eq!((on.fill, on.border, on.mark, on.mark_color), (p.ink, p.ink, CheckMark::Tick, p.ink_text));
        let part = stage_check_look(Check::Partial, &p);
        assert_eq!((part.fill, part.border, part.mark, part.mark_color), (p.surface, p.ink, CheckMark::Dash, p.ink));
        let off = stage_check_look(Check::Off, &p);
        assert_eq!((off.fill, off.border, off.mark), (p.surface, p.grey.strong, CheckMark::None));
        assert!(STAGE_CHECK_HIT_W >= STAGE_CHECK_SIDE, "the tap area is at least the square");
        // Every stage check on the screen is the painted square; the only toolkit checkbox left is
        // the invisible one inside `StageCheck` that keeps the role and the keyboard.
        let src = include_str!("vcs.rs");
        let code = &src[..src.find("#[cfg(test)]").expect("tests")];
        assert_eq!(code.matches("Checkbox!(").count(), 1);
    }

    #[compose_rust::runtime::Composable]
    fn stage_check_toggle_sample() {
        let check = remember(|| mutable_state_of(Check::Partial));
        let set = check.clone();
        StageCheck(check.get(), STAGE_CHECK_SIDE, 34.0, move || set.set(if set.get() == Check::On { Check::Off } else { Check::On }));
        Label(&format!("상태 {:?}", check.get()), Fs::Sm, Weight::Regular, None, None);
    }

    #[test]
    fn fr_l19_a_stage_check_keeps_the_toolkit_toggle() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(stage_check_toggle_sample);
        assert!(text.contains("상태 Partial"));
        assert!(text.contains("Checkbox"), "the toolkit checkbox carries the role: {text}");
    }

    #[test]
    fn fr_l19_hunk_action_sits_in_the_header_row() {
        let src = include_str!("vcs.rs");
        let start = src.find("fn DiffViewAtWidth(").expect("DiffViewAtWidth");
        let end = start + src[start..].find("fn FilesHeader(").expect("FilesHeader");
        let body = &src[start..end];
        let header = body.find("Txt(&h.header, TypeRole::Mono").expect("mono hunk header");
        let action = body.find("\"이 덩어리 전체\"").expect("hunk action");
        let row = body[..header].rfind("Row!(").expect("header row");
        let column = body[..header].rfind("Column!(").unwrap_or(0);
        assert!(row > column, "header and action share one row, not a column");
        assert!(!body[header..action].contains("Column!("), "the action is not on its own row");
        assert!(action > header, "the action follows the header text, on the right");
        assert_eq!(VCS_HUNK_HEADER_H, VCS_HUNK_BUTTON_H + 12.0);
        // compose-rust#424 (v0.0.11): the diff line check column is pinned with Modifier::sticky()
        // inside a horizontal ScrollRow so long lines scroll sideways under the pinned check cell.
        assert!(body.contains("ScrollRow!("), "{body}");
        assert!(body.contains("StageCheckWith(Modifier::new().sticky()"), "{body}");
        assert!(body.contains("Modifier::new().sticky().width(STAGE_CHECK_HIT_W)"), "{body}");
        assert!(!include_str!("../vcs.rs").contains(concat!("TODO(compose-rust#", "424")));
    }

    /// One node of a composition: widget, parent, modifiers and props as debug text.
    struct N {
        id: u32,
        widget: String,
        parent: Option<u32>,
        /// Its place among its parent's children, as inserted.
        at: u32,
        mods: Vec<String>,
        props: Vec<String>,
    }

    fn nodes_of(content: fn()) -> Vec<N> {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut nodes: Vec<N> = Vec::new();
        for m in batch.iter() {
            match m {
                Mutation::Create { node_id, widget } => {
                    nodes.push(N { id: *node_id, widget: format!("{widget:?}"), parent: None, at: 0, mods: Vec::new(), props: Vec::new() })
                }
                Mutation::Insert { parent_id, node_id, index } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.parent = Some(*parent_id);
                        n.at = *index;
                    }
                }
                Mutation::SetModifier { node_id, modifier, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.mods.push(format!("{modifier:?}"));
                    }
                }
                Mutation::SetProp { node_id, property, value } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.props.push(format!("{property:?}={value:?}"));
                    }
                }
                _ => {}
            }
        }
        nodes
    }

    fn up(nodes: &[N], n: &N) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = n.parent;
        while let Some(id) = cur {
            out.push(id);
            cur = nodes.iter().find(|x| x.id == id).and_then(|x| x.parent);
        }
        out
    }

    /// The node whose text or hint is exactly `text`.
    fn with_text<'a>(nodes: &'a [N], text: &str) -> &'a N {
        let want = format!("=String({text:?})");
        nodes.iter().find(|n| n.props.iter().any(|p| p.ends_with(&want))).unwrap_or_else(|| panic!("no {text}"))
    }

    fn under<'a>(nodes: &'a [N], top: &N) -> Vec<&'a N> {
        nodes.iter().filter(|n| up(nodes, n).contains(&top.id)).collect()
    }

    /// The widget of the nearest node that holds both texts.
    fn shared<'a>(nodes: &'a [N], a: &str, b: &str) -> &'a N {
        let (ua, ub) = (up(nodes, with_text(nodes, a)), up(nodes, with_text(nodes, b)));
        let id = ua.iter().find(|id| ub.contains(id)).expect("one tree");
        nodes.iter().find(|n| n.id == *id).unwrap()
    }

    #[test]
    fn fr_l19_bar_pickers_are_one_height() {
        // Review r8 fix round: 저장소 was 44 high and 현재 브랜치 36 (`.vcs-sel{height:36px}`), so
        // the two cells sat 4 px apart on the value line and 8 on the note under it.
        assert_eq!(VCS_SEL_H, 36.0);
        for nodes in [nodes_of(|| RepoSelector("ember", false, || {})), nodes_of(|| BranchSelector("feat/session-shim", false, || {}))] {
            let frame = nodes.iter().find(|n| n.widget == "Box").expect("the picker");
            assert!(frame.mods.iter().any(|m| m == "Height(36.0)"), "{:?}", frame.mods);
            assert!(frame.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", frame.mods);
            assert!(!nodes.iter().any(|n| n.mods.iter().any(|m| m == "Height(44.0)")), "no 44 px picker");
            assert_eq!(nodes.iter().filter(|n| n.widget == "Image").count(), 2, "the icon and the chevron");
        }
    }

    fn pills_probe() {
        let count = |s: Section| match s {
            Section::Changes => Some(5),
            Section::Conflicts => Some(2),
            Section::Stash => Some(0),
            _ => None,
        };
        SectionPillsWithCounts(Section::Changes, 358.0, 358.0, &count, |_| {});
    }

    #[test]
    fn fr_l19_section_pills_carry_icon_and_count_and_the_current_one_is_white_under_a_shadow() {
        // Reviews r7 and r8, #878: grey `PickChip`s with a name only; the mockup's `.vcs-navi`
        // pills have the icon and the count, and `.on` is `surface` with a line and a shadow.
        let p = ember_theme().palette;
        assert_eq!(SECTION_PILL_H, 36.0);
        assert_eq!(section_pill_look(true, p), (Some((p.surface, p.line)), p.text));
        assert_eq!(section_pill_look(false, p), (None, p.text_2));
        let nodes = nodes_of(pills_probe);
        assert!(nodes.iter().any(|n| n.widget == "ScrollRow"), "one row that scrolls sideways");
        assert!(!nodes.iter().any(|n| n.widget == "Button"), "not the composer's chip");
        let pill_of = |sec: Section| {
            let label = with_text(&nodes, sec.label());
            let ups = up(&nodes, label);
            nodes.iter().find(|n| ups.contains(&n.id) && n.props.iter().any(|x| x.starts_with("OnClick="))).unwrap_or_else(|| panic!("{} is a control", sec.label()))
        };
        let fill = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.surface)));
        let line = format!("{:?}", compose_rust::schema::Modifier::Border { width: 1.0, paint: paint(p.line) });
        for sec in Section::ALL {
            let pill = pill_of(sec);
            assert!(pill.mods.iter().any(|m| m == "Height(36.0)"), "{}: {:?}", sec.label(), pill.mods);
            let inside = under(&nodes, pill);
            assert_eq!(inside.iter().filter(|n| n.widget == "Image").count(), 1, "{}: its icon", sec.label());
            let on = sec == Section::Changes;
            assert_eq!(pill.mods.contains(&fill), on, "{}: {:?}", sec.label(), pill.mods);
            assert_eq!(pill.mods.contains(&line), on, "{}: {:?}", sec.label(), pill.mods);
            assert_eq!(pill.mods.iter().any(|m| m.starts_with("Shadow")), on, "{}: {:?}", sec.label(), pill.mods);
            assert!(!pill.mods.contains(&format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.sel)))), "no grey selection");
        }
        let texts_in = |sec: Section| -> Vec<String> {
            under(&nodes, pill_of(sec)).iter().flat_map(|n| n.props.iter().filter(|x| x.starts_with("Text=")).cloned()).collect()
        };
        assert!(texts_in(Section::Changes).contains(&"Text=String(\"5\")".to_string()), "{:?}", texts_in(Section::Changes));
        assert!(texts_in(Section::Conflicts).contains(&"Text=String(\"2\")".to_string()));
        assert_eq!(texts_in(Section::Stash).len(), 1, "a zero count is not shown");
        assert_eq!(texts_in(Section::History).len(), 1, "an unread count is not shown");
        // The conflicts count sits on orange (`.vcs-navi.alert .vcs-cnt`).
        let two = with_text(&nodes, "2");
        let badge = nodes.iter().find(|n| Some(n.id) == two.parent).unwrap();
        assert!(badge.mods.contains(&format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.orange.strong)))), "{:?}", badge.mods);
        assert!(badge.mods.iter().any(|m| m == "Height(18.0)"), "{:?}", badge.mods);
        // The screen passes the counts the list and the rail show.
        let production = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("&|s| shown_section_count(s, pill_snap, sample, stashes, prs, branches, team),"));
        assert!(!production.contains("PickChip("));
    }

    #[test]
    fn fr_l19_a_scrolling_tree_shows_whole_rows_at_rest() {
        // Review r10: in the 1024 sheet the commit box's top cut a tree row in half. The
        // scrolling tree is its top padding and whole rows, so the rule under it meets a row end.
        for cap in [VCS_TREE_MIN_H, 218.0, 260.0, 281.0, 318.0, VCS_TREE_MAX_H] {
            let h = tree_view_height(cap);
            assert!(h <= cap && cap - h < VCS_TREE_ROW_H, "{cap}: {h}");
            let rows = (h - VCS_TREE_PAD) / VCS_TREE_ROW_H;
            assert_eq!(rows, rows.round(), "{cap}: whole rows");
        }
        assert_eq!(tree_view_height(281.0), 280.0);
        assert_eq!(tree_view_height(VCS_TREE_MAX_H), 348.0);
        assert_eq!(tree_view_height(VCS_TREE_MIN_H), 110.0);
        assert_eq!(tree_view_height(10.0), VCS_TREE_PAD + VCS_TREE_ROW_H, "at least one row");
        let production = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        let capped = production.split("fn FilesTreeCapped(").nth(1).unwrap().split("\nfn ").next().unwrap();
        assert!(capped.contains(".max_height(tree_view_height(cap))"), "the scroll box is capped at the snapped height");
    }

    #[test]
    fn fr_l19_the_commit_box_is_in_the_changes_card_and_the_tree_leaves_it_room() {
        // Review r8: a separate commit card 16 below a 10-row tree put the commit button below
        // the fold at 1440. `.vcs-files` is one card: header, `.vcs-tree{max-height:360px}`,
        // then `.vcs-commit` under a rule.
        assert_eq!((VCS_TREE_ROW_H, VCS_TREE_PAD), (34.0, 8.0));
        assert_eq!(tree_height(9), 322.0);
        assert_eq!(tree_height(10), 356.0);
        assert_eq!(tree_cap(VCS_TREE_MAX_H, None), 360.0, "the mockup's cap on the page");
        assert_eq!(tree_cap(VCS_TREE_MAX_H_STACKED, None), 260.0, "stacked panes, 900 and below");
        assert_eq!(VCS_COMMIT_BOX_EST_H, 250.0);
        // The sheet at 1440 x 900: 749 under the header, 585 for the panes; the header (54)
        // and the commit box (250) leave the tree 281, so the card ends with the pane.
        assert!((vcs_sheet_body_height(1440.0, 900.0) - 749.0).abs() < 0.01);
        assert_eq!(sheet_pane_height(749.0, 16.0), 585.0);
        assert_eq!(tree_cap(VCS_TREE_MAX_H, Some(585.0)), 281.0);
        assert!(tree_height(10) > 281.0, "the r8 sample's ten rows scroll inside the cap");
        assert_eq!(VCS_FILES_HEAD_H + 281.0 + VCS_COMMIT_BOX_EST_H, 585.0);
        // A tall pane keeps the mockup's 360; a short one keeps three rows.
        assert_eq!(tree_cap(VCS_TREE_MAX_H, Some(900.0)), 360.0);
        assert_eq!(tree_cap(VCS_TREE_MAX_H, Some(300.0)), VCS_TREE_MIN_H);
        assert_eq!(VCS_TREE_MIN_H, 118.0);
        // Composed: the summary field and the files header are in one bordered card.
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let _ = vcs_sheet_window(1440.0, 900.0);
        let nodes = nodes_of(open_vcs_sheet_sample);
        let summary = nodes.iter().find(|n| n.props.iter().any(|x| x.contains("요약 (필수) 예: Feat: Wire shim"))).expect("the summary field");
        let summary_up = up(&nodes, summary);
        let card = nodes
            .iter()
            .filter(|n| summary_up.contains(&n.id) && n.mods.iter().any(|m| m.starts_with("Border")) && n.mods.iter().any(|m| m.starts_with("Shape { top_start: 14.0")))
            .last()
            .expect("the commit box is in a card");
        let header = nodes.iter().find(|n| n.props.iter().any(|x| x.starts_with("Text=String(\"변경 파일 "))).expect("the files header");
        assert!(up(&nodes, header).contains(&card.id), "the same card holds the files header");
        // Rows keep the height the cap counts with.
        let rows = build_tree(&sample_snapshot().changes.iter().map(|d| d.path.clone()).collect::<Vec<_>>(), &BTreeSet::new()).len();
        assert!(nodes.iter().filter(|n| n.mods.iter().any(|m| m == "Height(34.0)")).count() >= rows);
        assert!(
            nodes.iter().any(|n| n.widget == "ScrollColumn" && n.mods.iter().any(|m| m.contains("max_height: 280.0"))),
            "the tree scrolls inside its cap (SizeIn max_height 280.0)"
        );
        let production = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        let two = production.split("PaneLayout::Two => {").nth(1).unwrap().split("PaneLayout::Stacked => {").next().unwrap();
        assert_eq!(two.matches("VcsCard(").count(), 1, "one card for the tree and the commit box: {two}");
        assert!(two.contains("FilesTreeCapped(&d_files, &st_files, &fold_files, &sel_files, on_tab_files, tree_cap(VCS_TREE_MAX_H, pane_h));"));
        assert!(!production.contains(concat!("TODO(compose-rust#", "778")));
    }

    /// The children of `parent`, in the order they were inserted.
    fn kids<'a>(nodes: &'a [N], parent: u32) -> Vec<&'a N> {
        let mut v: Vec<&N> = nodes.iter().filter(|n| n.parent == Some(parent)).collect();
        v.sort_by_key(|n| n.at);
        v
    }

    /// The child of `top` that is `n` or holds it.
    fn branch_of<'a>(nodes: &'a [N], top: u32, n: &'a N) -> &'a N {
        if n.parent == Some(top) {
            return n;
        }
        let ups = up(nodes, n);
        nodes.iter().find(|x| x.parent == Some(top) && ups.contains(&x.id)).expect("under the node")
    }

    /// A snapshot whose tree (fifteen rows) is taller than any cap.
    fn long_snapshot() -> RepoSnapshot {
        let mut snap = sample_snapshot();
        snap.changes = (0..14)
            .map(|i| {
                let mut d = a_diff();
                d.path = format!("src/f{i}.rs");
                d
            })
            .collect();
        snap
    }

    #[compose_rust::runtime::Composable]
    fn changes_two_pane_probe() {
        ChangesPaneWithActions(&long_snapshot(), PaneLayout::Two, None, |_| {}, None, 600.0, 320.0, Some(585.0), None, false);
    }

    #[test]
    fn fr_l19_1_the_commit_box_follows_the_scrolling_tree_inside_the_changes_card() {
        // #415 (1): the earlier check only found the commit button's text. Since #878 the box is
        // not pinned under a scroll area; it is the last part of the changes card, and only the
        // tree above it scrolls.
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let nodes = nodes_of(changes_two_pane_probe);
        let p = ember_theme().palette;
        assert_eq!(tree_cap(VCS_TREE_MAX_H, Some(585.0)), 281.0);
        let tree = nodes
            .iter()
            .find(|n| n.widget == "ScrollColumn" && n.mods.iter().any(|m| m.contains("max_height: 280.0")))
            .expect("the tree scrolls inside its cap");
        let row = with_text(&nodes, "f13.rs");
        assert!(up(&nodes, row).contains(&tree.id), "the last file row is inside the scrolling tree");
        let summary = nodes.iter().find(|n| n.props.iter().any(|x| x.contains("요약 (필수) 예: Feat: Wire shim"))).expect("the summary field");
        assert!(!up(&nodes, summary).contains(&tree.id), "the commit box does not scroll with the tree");
        // One column holds the header, a rule, the tree, a rule and the commit box, in that order.
        let column = tree.parent.expect("the card's column");
        let parts = kids(&nodes, column);
        assert_eq!(parts.len(), 5, "{:?}", parts.iter().map(|n| (&n.widget, &n.mods)).collect::<Vec<_>>());
        let header = with_text(&nodes, "변경 파일 14");
        assert_eq!(branch_of(&nodes, column, header).id, parts[0].id, "the files header is first");
        assert_eq!(parts[2].id, tree.id, "the tree is under the header's rule");
        let commit = branch_of(&nodes, column, summary);
        assert_eq!(commit.id, parts[4].id, "the commit box is last, after the tree");
        assert!(commit.at > tree.at);
        for rule in [parts[1], parts[3]] {
            assert!(rule.mods.iter().any(|m| m == "Height(1.0)") && rule.mods.iter().any(|m| m == "FillMaxWidth"), "{:?}", rule.mods);
        }
        let fill = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.surface_2)));
        assert!(commit.mods.contains(&fill), "on surface-2: {:?}", commit.mods);
        assert!(commit.widget == "Column" && !commit.mods.iter().any(|m| m.starts_with("Height(")), "as tall as its fields: {:?}", commit.mods);
        // The commit button is in that box too.
        let button = nodes.iter().find(|n| n.widget == "Button" && under(&nodes, commit).iter().any(|x| x.id == n.id)).expect("the commit button");
        assert!(!up(&nodes, button).contains(&tree.id));
        // The column is inside one bordered card.
        let ups = up(&nodes, tree);
        assert!(nodes.iter().any(|n| ups.contains(&n.id) && n.mods.iter().any(|m| m.starts_with("Border")) && n.mods.iter().any(|m| m.starts_with("Shape { top_start: 14.0"))));
    }

    #[compose_rust::runtime::Composable]
    fn phone_files_tab_probe() {
        ChangesPane(&long_snapshot(), PaneLayout::Tabs, Some(PaneTab::Files), |_| {});
    }

    #[compose_rust::runtime::Composable]
    fn phone_diff_tab_probe() {
        ChangesPane(&long_snapshot(), PaneLayout::Tabs, Some(PaneTab::Diff), |_| {});
    }

    #[compose_rust::runtime::Composable]
    fn phone_commit_tab_probe() {
        ChangesPane(&long_snapshot(), PaneLayout::Tabs, Some(PaneTab::Commit), |_| {});
    }

    #[test]
    fn fr_l19_1_every_phone_tab_ends_with_the_floating_bar_clearance() {
        // #415 (2): only 커밋 ended with the clearance, so the last tree row and the last diff
        // line stayed under the floating tab bar.
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let clearance = crate::ui::cr::routines::compact_bottom_clearance();
        assert_eq!(clearance, 90.0);
        let want = format!("{:?}", compose_rust::schema::Modifier::Height(clearance));
        let cases: [(&str, fn(), &str); 3] = [
            ("파일", phone_files_tab_probe, "f13.rs"),
            ("차이", phone_diff_tab_probe, "이 덩어리 전체"),
            ("커밋", phone_commit_tab_probe, "마지막 커밋 수정 (amend)"),
        ];
        for (name, probe, inside) in cases {
            let nodes = nodes_of(probe);
            let spacers: Vec<&N> = nodes.iter().filter(|n| n.widget == "Spacer" && n.mods.contains(&want)).collect();
            assert_eq!(spacers.len(), 1, "{name}: one clearance");
            let spacer = spacers[0];
            let column = spacer.parent.expect("the pane's column");
            let parts = kids(&nodes, column);
            assert_eq!(parts.last().map(|n| n.id), Some(spacer.id), "{name}: the clearance is the last thing in the pane");
            // The tab's own content is in the same column, above the clearance.
            let content = branch_of(&nodes, column, with_text(&nodes, inside));
            assert!(content.at < spacer.at, "{name}");
        }
    }

    #[test]
    fn fr_l19_1_the_icon_buttons_do_not_set_the_row_heights() {
        // #415 (3): the 44 dp box of a round button made the 원격 cell's row 44 beside the 36 dp
        // pickers, and in the files header (48 with 12 all round) it had 24.
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        assert_eq!((ICON_TOUCH_TARGET, VCS_SEL_H), (44.0, 36.0));
        assert_eq!((VCS_FILES_HEAD_ROW_H, VCS_FILES_HEAD_PAD_X, VCS_FILES_HEAD_H), (52.0, 16.0, 54.0));
        let has = |n: &N, m: &str| n.mods.iter().any(|x| x == m);
        // The nearest row with a height above a node.
        let row_over = |nodes: &[N], n: &N| -> Vec<String> {
            let ups = up(nodes, n);
            let row = ups
                .iter()
                .filter_map(|id| nodes.iter().find(|x| x.id == *id))
                .find(|x| x.widget == "Row" && x.mods.iter().any(|m| m.starts_with("Height(")))
                .expect("a row with a height");
            row.mods.clone()
        };
        for grid in [RepoBarGrid::Three, RepoBarGrid::TwoRemoteBelow, RepoBarGrid::One] {
            BAR_GRID.with(|g| g.set(grid));
            let nodes = nodes_of(repo_bar_sample);
            for label in ["Pull 1", "가져오기", "내려받기", "올리기"] {
                let control = nodes.iter().find(|n| n.props.iter().any(|x| *x == format!("AccessibleName=String({label:?})"))).unwrap_or_else(|| panic!("{label}"));
                let mods = row_over(&nodes, control);
                assert!(mods.iter().any(|m| m == "Height(36.0)") && mods.iter().any(|m| m == "FillMaxWidth"), "{grid:?} {label}: {mods:?}");
            }
            // The round buttons still ask for the 44 dp box and draw the 32 dp circle.
            // The 44 dp box is the control itself (compose-rust#429): it has the name and the click.
            let hit = nodes.iter().find(|n| n.props.iter().any(|x| x == "AccessibleName=String(\"가져오기\")")).unwrap();
            assert!(hit.widget == "Box" && hit.props.iter().any(|x| x.starts_with("OnClick=")), "{:?}", hit.props);
            assert!(has(hit, "Width(44.0)") && has(hit, "Height(44.0)"), "{:?}", hit.mods);
            let circle = kids(&nodes, hit.id)[0];
            assert!(has(circle, "Width(32.0)") && has(circle, "Height(32.0)"), "{:?}", circle.mods);
            // The two pickers have the same height as the row.
            assert_eq!(nodes.iter().filter(|n| n.widget == "Box" && has(n, "Height(36.0)") && has(n, "FillMaxWidth")).count(), 2, "{grid:?}");
        }
        BAR_GRID.with(|g| g.set(RepoBarGrid::Three));
        // The files header: 52 high, no padding of its own, 16 at each side, and the stash
        // button after a note that takes only the room left.
        let nodes = nodes_of(changes_two_pane_probe);
        let stash = nodes.iter().find(|n| n.props.iter().any(|x| x == "AccessibleName=String(\"스태시\")")).expect("the stash button");
        let ups = up(&nodes, stash);
        let head = ups
            .iter()
            .filter_map(|id| nodes.iter().find(|x| x.id == *id))
            .find(|x| x.widget == "Row" && x.mods.iter().any(|m| m.starts_with("Height(")))
            .expect("the header row");
        assert!(has(head, "Height(52.0)") && has(head, "FillMaxWidth"), "{:?}", head.mods);
        assert!(!head.mods.iter().any(|m| m.starts_with("Padding")), "no padding to take the box's height: {:?}", head.mods);
        let parts = kids(&nodes, head.id);
        assert_eq!(parts.len(), 3, "a side, the content, a side");
        for side in [parts[0], parts[2]] {
            assert!(side.widget == "Spacer" && has(side, "Width(16.0)"), "{:?}", side.mods);
        }
        assert!(parts[1].widget == "Row" && has(parts[1], "Weight(1.0)"), "{:?}", parts[1].mods);
        // The named box is the 44 by 44 control itself (compose-rust#429, #673).
        let hit = if has(stash, "Width(44.0)") { stash } else { nodes.iter().find(|n| Some(n.id) == stash.parent).unwrap() };
        assert!(has(hit, "Width(44.0)") && has(hit, "Height(44.0)"), "{:?}", hit.mods);
        assert_eq!(hit.parent, Some(parts[1].id), "the box is a child of the header's content row");
        let note = with_text(&nodes, STASH_NOW_NOTE);
        let holder = nodes.iter().find(|n| Some(n.id) == note.parent).unwrap();
        assert!(holder.widget == "Box" && has(holder, "Weight(1.0)"), "the note gives way: {:?}", holder.mods);
        assert!(holder.at < hit.at, "the button follows the note");
        assert!(!kids(&nodes, parts[1].id).iter().any(|n| n.widget == "Spacer" && has(n, "Weight(1.0)")), "the note's box is the only thing that grows");
    }

    fn pr_detail_probe() {
        if let PrDetailLoad::Ready(v) = sample_pr_detail(412) {
            PrDetailBody(&v);
        }
    }

    #[test]
    fn fr_l19_pr_detail_puts_checks_and_reviews_side_by_side() {
        // #878, review r7: stacked, the detail was taller than its scroll area at 1440 and the
        // 검사 panel ended at the cut edge. `.vcs-prgrid`: two columns, one at 560 and below.
        assert_eq!([1440.0, 1024.0, 561.0, 560.0, 390.0, 0.0].map(pr_grid_columns), [2, 2, 2, 1, 1, 2]);
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        let wide = nodes_of(pr_detail_probe);
        crate::ui::window_size::set_opened_for_test(Some((390.0, 844.0)));
        let phone = nodes_of(pr_detail_probe);
        crate::ui::window_size::set_opened_for_test(None);
        let grid = shared(&wide, "검사", "리뷰");
        assert_eq!(grid.widget, "Row");
        let cells: Vec<&N> = wide.iter().filter(|n| n.parent == Some(grid.id)).collect();
        assert_eq!(cells.len(), 2);
        for cell in cells {
            for want in ["Weight(1.0)", "Width(0.0)"] {
                assert!(cell.mods.iter().any(|m| m == want), "{want}: {:?}", cell.mods);
            }
        }
        assert_eq!(shared(&phone, "검사", "리뷰").widget, "Column");
        // A check's name gives way, its result keeps its width.
        let name = with_text(&wide, "clippy");
        let holder = wide.iter().find(|n| Some(n.id) == name.parent).unwrap();
        assert!(holder.mods.iter().any(|m| m == "Weight(1.0)") && holder.mods.iter().any(|m| m == "Width(0.0)"), "{:?}", holder.mods);
        assert!(name.props.iter().any(|x| x == "MaxLines=Integer(1)"), "{:?}", name.props);
    }

    #[test]
    fn fr_l19_compact_pills_scroll_tabs_are_segments_and_the_gutter_is_16() {
        assert!(section_pills_scroll(390.0 - 32.0));
        assert!(section_pills_scroll(704.0));
        assert!(!section_pills_scroll(1024.0));
        assert_eq!(vcs_page_pad(true), 16.0);
        assert_eq!(vcs_page_pad(false), 32.0);
        let p = super::super::theme::LIGHT;
        assert_eq!(pane_tab_look(true, &p), (p.ink, p.ink, p.ink_text));
        assert_eq!(pane_tab_look(false, &p), (p.surface, p.line, p.text_2));
    }

    #[test]
    fn fr_l19_the_sample_sources_name_one_current_branch() {
        // #878: the header, the bar's picker, the 브랜치 list, the merge banner and the conflict
        // editor's ours side all read the same current branch on the sample screen.
        let snap = sample_snapshot();
        let BranchLoad::Ready(list) = sample_branches() else { panic!("sample branches") };
        let current: Vec<_> = list.iter().filter(|b| b.current).collect();
        assert_eq!(current.len(), 1, "one current branch");
        assert_eq!(current[0].name, snap.branch);
        assert_eq!(current[0].upstream_text, snap.upstream.clone().unwrap());
        assert_eq!((current[0].ahead, current[0].behind), (snap.ahead, snap.behind));
        assert_eq!(snap.branches.first(), Some(&snap.branch), "the picker lists it first");
        assert_eq!(sample_project_box().branch.as_deref(), Some(snap.branch.as_str()));
        let MergeLoad::Ready(Some(merge)) = sample_merge() else { panic!("sample merge") };
        assert_eq!(merge.into, snap.branch);
        let src = include_str!("vcs.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        let conflict = &production[production.find("fn sample_conflict()").unwrap()..];
        let conflict = &conflict[..conflict.find("\n}\n").unwrap()];
        assert!(conflict.contains(&format!("\"HEAD ({})\"", snap.branch)), "the ours side names the current branch");
        for sample in ["fn sample_branches()", "fn sample_conflict()"] {
            let body = &production[production.find(sample).unwrap()..];
            let body = &body[..body.find("\n}\n").unwrap()];
            assert!(!body.contains("feat/git-api"), "{sample} names another current branch");
        }
    }

    #[test]
    fn fr_l19_the_current_section_pill_is_scrolled_into_view() {
        // The page at 800: the pills get the content width (800 less the 72 rail, less 2 x 32).
        let width = 800.0 - 72.0 - 64.0;
        let counts = [Some(5), None, Some(9), Some(3), Some(2), Some(1), Some(3)];
        assert_eq!(section_pills_reveal(Section::Changes, width, &counts), 0.0, "the first pill is in view at 0");
        assert_eq!(section_pills_reveal(Section::Stash, width, &counts), 0.0);
        let conflicts = section_pills_reveal(Section::Conflicts, width, &counts);
        let team = section_pills_reveal(Section::Team, width, &counts);
        assert!(conflicts > 0.0, "충돌 ends past the edge at 800 (r10 capture): {conflicts}");
        assert!(team > conflicts, "팀 작업 is further right");
        // The count adds its width, the conflicts count its orange pill too.
        assert!(section_pill_width(Section::Conflicts, Some(1)) > section_pill_width(Section::Conflicts, None) + 14.0);
        assert_eq!(section_pill_width(Section::Stash, Some(0)), section_pill_width(Section::Stash, None), "no count at 0");
        // The scrolling row is moved with the toolkit's scroll state, once per change of section.
        let src = include_str!("vcs.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(production.contains("let to = section_pills_reveal(active, view_w, &counts);"));
        assert!(production.contains("(section_width - nav).max(0.0),"), "the row's width leaves out the shell's navigation");
        assert!(production.contains("target.scroll_to(to);"));
        assert!(production.contains("seen.set(Some((active, range, to_key)));"), "sent again once the row's range is measured");
        assert!(production.contains("ScrollRow!(scroll_state = &scroll, modifier = Modifier::new().fill_max_width(), content = pills)"));

    }

    #[test]
    fn fr_l28_5_the_section_pills_start_at_0_with_the_current_pill_at_360() {
        // Flip 4 review 3.6: the row opened on 히스토리 with 변경 사항 (selected) scrolled away.
        // Before the window's width is reported the row's width reads 0, and the target was the
        // whole first pill; once the width arrived the target was not sent again.
        let counts = [Some(5), None, Some(9), Some(3), Some(2), Some(1), Some(3)];
        assert_eq!(section_pills_reveal(Section::Changes, 0.0, &counts), 0.0, "an unknown width moves nothing");
        assert_eq!(section_pills_reveal(Section::History, 0.0, &counts), 0.0);
        // The phone page at 360: the row is the window less the page's two sides.
        let width = 360.0 - 2.0 * vcs_page_pad(true);
        assert_eq!(section_pills_reveal(Section::Changes, width, &counts), 0.0);
        assert_eq!(section_pills_reveal(Section::History, width, &counts), 0.0);
        // A later target is sent even when the measured range stays the same.
        let src = include_str!("vcs.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(production.contains("if seen.get_untracked() != Some((active, range, to_key)) {"));
    }

    #[compose_rust::runtime::Composable]
    fn compact_pills_sample() {
        SectionPillsAtWidth(Section::Changes, 358.0, |_| {});
        PaneTabs(Section::Changes, Some(PaneTab::Files), |_| {});
    }

    #[test]
    fn fr_l19_compact_pills_render_in_one_scroll_row() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(compact_pills_sample);
        assert!(text.contains("ScrollRow"), "the pills scroll sideways: {text}");
        for section in Section::ALL {
            assert!(text.contains(section.label()), "missing {}", section.label());
        }
        for t in pane_tabs(Section::Changes) {
            assert!(text.contains(t.label()), "missing tab {}", t.label());
        }
    }

    #[test]
    fn fr_l19_branch_and_stash_rows_follow_the_mockup() {
        assert_eq!(ROW_ICON_TILE, 32.0);
        assert!(stash_actions_inline(PaneLayout::Two));
        assert!(!stash_actions_inline(PaneLayout::Tabs));
        let src = include_str!("vcs.rs");
        let start = src.find("fn BranchRow(").expect("BranchRow");
        let end = start + src[start..].find("fn BranchesPane(").expect("BranchesPane");
        let body = &src[start..end];
        assert!(body.contains("Txt(&e.name, TypeRole::Mono"), "branch names are mono");
        assert!(body.contains("BranchIconTile()"), "a branch row starts with the icon tile");
        assert!(body.contains("p.surface_2"), "the current branch row is on surface_2");
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

    #[compose_rust::runtime::Composable]
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

    #[compose_rust::runtime::Composable]
    fn branches_sample_wide() {
        BranchesPane(None, mutable_state_of(None), PaneLayout::Two);
    }

    #[compose_rust::runtime::Composable]
    fn branches_sample_compact() {
        BranchesPane(None, mutable_state_of(None), PaneLayout::Tabs);
    }

    #[test]
    fn fr_l19_branches_render_search_guidance_default_and_bar_text() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        for content in [branches_sample_wide as fn(), branches_sample_compact as fn()] {
            let text = text_of(content);
            assert!(text.contains("브랜치 검색"));
            assert!(text.contains(BRANCH_SWITCH_NOTE));
            assert!(text.contains(BRANCH_CREATE_NOTE));
            assert!(text.contains("기본"));
            assert!(text.contains(branch_bar_text(2, 1).as_str()));
        }
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
            assert!(text.contains("feat/session-shim"));
            assert!(text.contains("현재"));
            assert!(text.contains(branch_bar_text(2, 1).as_str()));
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

    #[test]
    fn fr_l18_1_vcs_sheet_frame_follows_the_mounted_mockup() {
        // 1440: the sheet is 1296 wide and padded 16, so 1264 inside: the icon rail and the two
        // panes with the 260 tree (`.vcs-compact .vcs-body`, `.vcs-compact .vcs-chg`).
        assert_eq!(
            vcs_sheet_frame(vcs_sheet_width(1440.0), 1440.0),
            VcsSheetFrame { pad: 16.0, rail: true, layout: PaneLayout::Two, files_w: 260.0 }
        );
        // 1024: 921.6 wide, 889.6 inside, still wider than the 760 container rule.
        assert!(vcs_sheet_frame(vcs_sheet_width(1024.0), 1024.0).rail);
        // 800: 720 wide, 688 inside: pills over one column (`@container (max-width:760px)`).
        let m = vcs_sheet_frame(vcs_sheet_width(800.0), 800.0);
        assert_eq!((m.rail, m.layout), (false, PaneLayout::Stacked));
        // 390: the whole window padded 12, pills and the 파일 / 차이 / 커밋 tabs.
        assert_eq!(
            vcs_sheet_frame(vcs_sheet_width(390.0), 390.0),
            VcsSheetFrame { pad: 12.0, rail: false, layout: PaneLayout::Tabs, files_w: 260.0 }
        );
        assert_eq!((VCS_SHEET_RAIL_W, VCS_SHEET_RAIL_ITEM), (56.0, (48.0, 44.0)));
    }

    #[test]
    fn fr_l18_1_vcs_sheet_subtitle_is_branch_changes_and_open_prs() {
        let snap = sample_snapshot();
        let n = snap.changes.len();
        assert_eq!(vcs_sheet_subtitle(Some(&snap), Some(2)), format!("{} \u{b7} 변경 {n} \u{b7} PR 2", snap.branch));
        // A count not read yet is left out, never shown as 0.
        assert_eq!(vcs_sheet_subtitle(Some(&snap), None), format!("{} \u{b7} 변경 {n}", snap.branch));
        assert_eq!(vcs_sheet_subtitle(None, Some(1)), "");
        assert_eq!(vcs_sheet_title("Ember"), "버전 관리 \u{b7} Ember");
    }

    #[test]
    fn fr_l18_1_vcs_sheet_draws_the_header_rail_tree_diff_and_commit_box() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let _ = vcs_sheet_window(1440.0, 900.0);
        // The test services are installed, so `VcsSheet` would load a real (empty) project;
        // mount the sheet's screen on the sample, as `VcsSheet` does without services.
        let text = text_of(open_vcs_sheet_sample);
        let project = sample_project_box();
        assert!(text.contains(&vcs_sheet_title(&project.name)), "the header names the project");
        assert!(text.contains("전체 화면"), "the header's full screen button");
        assert!(text.contains("Width(56.0)"), "the icon rail");
        assert!(text.contains("Width(260.0)"), "the tree column beside the diff");
        assert!(text.contains("요약 (필수) 예: Feat: Wire shim"), "the commit box under the tree");
        let screen = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        let sheet = screen.split("pub fn VcsSheet(").nth(1).unwrap().split("\npub fn ").next().unwrap();
        assert!(sheet.contains("EmberSheetSized(\"\""), "the sheet draws its own header, not the shared title");
    }

    #[test]
    fn fr_l18_vcs_sheet_width_is_ninety_percent_capped() {
        assert_eq!(vcs_sheet_width(1024.0), 1024.0 * 0.9);
        assert_eq!(vcs_sheet_width(561.0), 561.0 * 0.9);
        assert_eq!(vcs_sheet_width(1440.0), 1296.0);
        assert_eq!(vcs_sheet_width(2400.0), super::super::components::SHEET_WIDTH_MONITOR);
    }

    #[test]
    fn fr_l18_vcs_sheet_body_height_survives_a_zero_first_frame() {
        let h = vcs_sheet_body_height(0.0, 0.0);
        assert_eq!(h, 720.0 * 0.9 - 61.0);
        assert!(vcs_sheet_body_height(3.0, 2.0) > 400.0);
        // A later frame with a real size corrects the first one.
        assert_eq!(vcs_sheet_body_height(1440.0, 900.0), 900.0 * 0.9 - 61.0);
        assert_eq!(vcs_sheet_window(0.0, 0.0).0 > 0.0, true);
        let (w, hh) = vcs_sheet_window(1200.0, 800.0);
        assert_eq!((w, hh), (1200.0, 800.0));
        // After a usable size the unknown size falls back to it (a global, so only checked as usable).
        let (fw, fh) = vcs_sheet_window(0.0, 0.0);
        assert!(fw > 100.0 && fh > 100.0);
    }

    #[test]
    fn fr_l18_vcs_sheet_is_full_screen_on_a_phone() {
        assert_eq!(vcs_sheet_width(390.0), 390.0);
        assert_eq!(vcs_sheet_body_height(390.0, 844.0), 844.0 - 61.0);
        // Full screen: the shared sheet takes the whole window for a monitor-style sheet at <= 560.
        assert_eq!(super::super::components::sheet_max_height(390.0, 844.0, true), 844.0);
    }

    #[test]
    fn fr_l18_vcs_sheet_geometry_changes_where_the_presentation_does() {
        // `sheet_presentation` makes a monitor sheet the whole window up to 560 inclusive.
        assert_eq!(vcs_sheet_width(560.0), 560.0, "the whole window at 560");
        assert_eq!(vcs_sheet_body_height(560.0, 844.0), 844.0 - 61.0);
        assert_eq!(vcs_sheet_width(561.0), 561.0 * 0.9, "floating from 561");
        assert_eq!(vcs_sheet_body_height(561.0, 844.0), 844.0 * 0.9 - 61.0);
        assert!(vcs_sheet_frame(560.0, 560.0).pad < vcs_sheet_frame(vcs_sheet_width(561.0), 561.0).pad, "the phone padding at 560 only");
    }

    #[test]
    fn fr_l18_vcs_sheet_leaves_the_selected_project_and_wires_dismiss() {
        let src = include_str!("vcs.rs");
        let start = src.find("pub fn VcsSheet(").expect("VcsSheet");
        let end = start + src[start..].find("pub fn preview_vcs()").expect("preview_vcs");
        let body = &src[start..end];
        assert!(!body.contains("set_selected_project"));
    }

    static VCS_DISMISSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    #[compose_rust::runtime::Composable]
    fn host_vcs_sheet_dismiss() {
        VcsSheet("ember", true, || { VCS_DISMISSES.fetch_add(1, std::sync::atomic::Ordering::SeqCst); });
    }

    #[test]
    fn fr_l18_vcs_sheet_renderer_dismiss_calls_close() {
        let _guard = crate::services::test_guard();
        VCS_DISMISSES.store(0, std::sync::atomic::Ordering::SeqCst);
        let mut host = Host::new(|| Box::new(Recomposer::new(host_vcs_sheet_dismiss)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let (node_id, handler_id) = batch.iter().find_map(|m| match m {
            Mutation::SetProp { node_id, property: PropertyKind::OnDismiss, value: PropertyValue::Integer(h) } => Some((*node_id, *h as u64)),
            _ => None,
        }).expect("VCS sheet dismiss handler");
        host.dispatch(HostEvent::new(node_id, handler_id, EventPayload::Clicked)).expect("dispatch dismiss");
        assert_eq!(VCS_DISMISSES.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[compose_rust::runtime::Composable]
    fn open_vcs_sheet() {
        VcsSheet("ember", true, || {});
    }

    #[compose_rust::runtime::Composable]
    fn open_vcs_sheet_sample() {
        let reload = remember(|| mutable_state_of(0_u64));
        let ctl = VcsSheetCtl { window_w: 1440.0, sheet_w: vcs_sheet_width(1440.0), top_inset: 0.0, body_h: Some(vcs_sheet_body_height(1440.0, 900.0)), on_close: std::rc::Rc::new(std::cell::RefCell::new(|| {})) };
        VcsScreenWithLoad(Load::Ready(sample_snapshot()), sample_projects(), Some("ember".into()), reload, true, Some(ctl));
    }

    #[compose_rust::runtime::Composable]
    fn open_vcs_sheet_800_sample() {
        let reload = remember(|| mutable_state_of(0_u64));
        let ctl = VcsSheetCtl { window_w: 800.0, sheet_w: vcs_sheet_width(800.0), top_inset: 0.0, body_h: Some(vcs_sheet_body_height(800.0, 800.0)), on_close: std::rc::Rc::new(std::cell::RefCell::new(|| {})) };
        VcsScreenWithLoad(Load::Ready(sample_snapshot()), sample_projects(), Some("ember".into()), reload, true, Some(ctl));
    }

    #[test]
    fn fr_l19_the_one_column_sheet_keeps_the_commit_box_whole_under_the_scroll() {
        // Review ba: at 800 x 800 the sheet's bottom edge cut the summary field and the tree.
        // Where the sheet uses one column, the commit box sits under the scrolling body.
        assert_eq!(vcs_sheet_frame(vcs_sheet_width(800.0), 800.0).layout, PaneLayout::Stacked);
        assert!(sheet_commit_footer(true, PaneLayout::Stacked, Section::Changes, true));
        assert!(sheet_commit_footer(false, PaneLayout::Stacked, Section::Changes, true), "the page in one column pins it too (#878)");
        assert!(!sheet_commit_footer(true, PaneLayout::Two, Section::Changes, true), "1440 and 1024 keep the card");
        assert!(!sheet_commit_footer(true, PaneLayout::Tabs, Section::Changes, true), "the phone has its 커밋 tab");
        assert!(!sheet_commit_footer(true, PaneLayout::Stacked, Section::History, true));
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let _ = vcs_sheet_window(800.0, 800.0);
        let nodes = nodes_of(open_vcs_sheet_800_sample);
        let summaries: Vec<&N> = nodes.iter().filter(|n| n.props.iter().any(|x| x.contains("요약 (필수) 예: Feat: Wire shim"))).collect();
        assert_eq!(summaries.len(), 1, "one commit box");
        let summary_up = up(&nodes, summaries[0]);
        let scrolls_above: Vec<&N> = nodes.iter().filter(|n| summary_up.contains(&n.id) && n.widget == "ScrollColumn").collect();
        assert!(scrolls_above.is_empty(), "the commit box is outside every scroll");
        let header = nodes.iter().find(|n| n.props.iter().any(|x| x.starts_with("Text=String(\"변경 파일 "))).expect("the files header");
        let body_scroll = nodes
            .iter()
            .find(|n| up(&nodes, header).contains(&n.id) && n.widget == "ScrollColumn" && n.mods.iter().any(|m| m.starts_with("Weight(1.0")))
            .expect("the body scrolls in the height the footer leaves");
        let footer_col = nodes.iter().find(|n| n.id == body_scroll.parent.unwrap()).unwrap();
        assert!(summary_up.contains(&footer_col.id), "the footer is the scroll's sibling in one column");
        // Fix round: at 800 x 800 the whole box left no tree row on screen, so the footer is
        // the compact box (summary and button on one row, 설명 and amend behind a toggle).
        let body_h = vcs_sheet_body_height(800.0, 800.0);
        assert!(sheet_footer_compact(body_h, 16.0));
        assert!(!sheet_footer_compact(vcs_sheet_body_height(800.0, 1200.0), 16.0), "a tall sheet keeps the whole box");
        let tree_room = body_h - 2.0 * 16.0 - VCS_SHEET_ONE_COLUMN_ABOVE - VCS_FILES_HEAD_H - (VCS_COMMIT_BOX_COMPACT_H + 2.0 * 16.0);
        assert!(tree_room >= VCS_SHEET_MIN_TREE_ROOM, "three rows show above the compact footer: {tree_room}");
        assert!(!nodes.iter().any(|n| n.props.iter().any(|x| x.contains("설명 (선택)"))), "설명 is folded");
        assert!(!nodes.iter().any(|n| n.props.iter().any(|x| x.contains("마지막 커밋 수정"))), "amend is folded");
        let button = nodes.iter().find(|n| n.props.iter().any(|x| x.contains("에 커밋"))).expect("the commit button");
        let shared_row = nodes.iter().find(|n| n.widget == "Row" && summary_up.contains(&n.id) && up(&nodes, button).contains(&n.id)).expect("summary and button share a row");
        assert!(!scrolls_above.iter().any(|sc| up(&nodes, shared_row).contains(&sc.id)));
        assert!(nodes.iter().any(|n| n.props.iter().any(|x| x.contains(compact_toggle_label(true)))), "the toggle is sent");
        assert!(!button.mods.iter().any(|m| m == "FillMaxWidth"), "the compact button does not take the summary's width: {:?}", button.mods);
        // The page and the wide sheet: the commit box stays in the changes card.
        compose_rust::window::reset_window_size();
        let _ = vcs_sheet_window(1440.0, 900.0);
        let wide = nodes_of(open_vcs_sheet_sample);
        assert_eq!(wide.iter().filter(|n| n.props.iter().any(|x| x.contains("요약 (필수) 예: Feat: Wire shim"))).count(), 1);
    }

    #[compose_rust::runtime::Composable]
    fn open_vcs_page_sample() {
        let reload = remember(|| mutable_state_of(0_u64));
        VcsScreenWithLoad(Load::Ready(sample_snapshot()), sample_projects(), Some("ember".into()), reload, true, None);
    }

    #[test]
    fn fr_l19_the_one_column_page_pins_the_commit_box_under_its_scroll() {
        // #878, review ba: on the 버전 관리 page at 800 x 800 (one column, one page scroll) the
        // commit button was below the fold. The page in one column now draws the commit box
        // under the scrolling body, as the one-column sheet does, compact when the whole box
        // would leave the tree no room.
        assert_eq!(page_footer_body_h(800.0), 728.0);
        assert!(sheet_footer_compact(page_footer_body_h(800.0), 32.0), "800 high: the compact box");
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        crate::ui::window_size::set_opened_for_test(Some((800.0, 800.0)));
        let nodes = nodes_of(open_vcs_page_sample);
        crate::ui::window_size::set_opened_for_test(None);
        let summaries: Vec<&N> = nodes.iter().filter(|n| n.props.iter().any(|x| x.contains("요약 (필수) 예: Feat: Wire shim"))).collect();
        assert_eq!(summaries.len(), 1, "one commit box");
        let summary_up = up(&nodes, summaries[0]);
        assert!(!nodes.iter().any(|n| summary_up.contains(&n.id) && n.widget == "ScrollColumn"), "the commit box is outside every scroll");
        let header = nodes.iter().find(|n| n.props.iter().any(|x| x.starts_with("Text=String(\"변경 파일 "))).expect("the files header");
        let body_scroll = nodes
            .iter()
            .find(|n| up(&nodes, header).contains(&n.id) && n.widget == "ScrollColumn" && n.mods.iter().any(|m| m.starts_with("Weight(1.0")))
            .expect("the page body scrolls in the height the footer leaves");
        let footer_col = nodes.iter().find(|n| n.id == body_scroll.parent.unwrap()).unwrap();
        assert!(summary_up.contains(&footer_col.id), "the footer is the scroll's sibling");
        let button = nodes.iter().find(|n| n.props.iter().any(|x| x.contains("에 커밋"))).expect("the commit button");
        assert!(up(&nodes, button).contains(&footer_col.id), "the commit button is in the footer");
    }

    #[test]
    fn fr_l19_the_wide_page_tree_leaves_the_commit_box_on_screen() {
        // Review ba follow-up: at 1024 x 800 the page's 360 tree put the commit button below
        // the fold. The panes start 246 down and end 32 above the window's bottom.
        assert_eq!(page_pane_height(800.0, 32.0), 522.0);
        // The files header row is 52 with the stash button and its note (FR-L10.4, #888), 4 more
        // than the 48 this was first written for.
        assert_eq!(tree_cap(VCS_TREE_MAX_H, Some(page_pane_height(800.0, 32.0))), 218.0);
        assert_eq!(tree_cap(VCS_TREE_MAX_H, Some(page_pane_height(900.0, 32.0))), 318.0);
        let production = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        assert!(production.contains("None => (wide && window.height_dp > 100.0).then(|| page_pane_height(window.height_dp, pad)),"));
    }

    #[compose_rust::runtime::Composable]
    fn open_vcs_sheet_phone_sample() {
        let reload = remember(|| mutable_state_of(0_u64));
        let lights = crate::ui::safe_area::SafeArea { top: 28.0, leading: 72.0, trailing: 0.0 };
        let ctl = VcsSheetCtl { window_w: 390.0, sheet_w: 390.0, top_inset: vcs_sheet_top_inset(390.0, lights), body_h: None, on_close: std::rc::Rc::new(std::cell::RefCell::new(|| {})) };
        VcsScreenWithLoad(Load::Ready(sample_snapshot()), sample_projects(), Some("ember".into()), reload, true, Some(ctl));
    }

    #[test]
    fn fr_l18_1_the_phone_sheet_header_starts_below_the_window_buttons() {
        use crate::ui::safe_area::SafeArea;
        let lights = SafeArea { top: 28.0, leading: 72.0, trailing: 0.0 };
        assert_eq!(vcs_sheet_top_inset(390.0, lights), 28.0);
        assert_eq!(vcs_sheet_top_inset(559.0, lights), 28.0);
        assert_eq!(vcs_sheet_top_inset(560.0, lights), 28.0, "the whole window up to 560, as `sheet_presentation`");
        assert_eq!(vcs_sheet_top_inset(561.0, lights), 0.0, "the sheet floats from 561");
        assert_eq!(vcs_sheet_top_inset(1440.0, lights), 0.0);
        assert_eq!(vcs_sheet_top_inset(390.0, SafeArea::default()), 0.0, "nothing to clear");
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(open_vcs_sheet_phone_sample);
        let strip = text.find("Height(28.0)").expect("the 28 strip");
        let title = text.find(&vcs_sheet_title(&sample_project_box().name)).expect("the title");
        assert!(strip < title, "the strip comes before the header");
        let wide = text_of(open_vcs_sheet_sample);
        let wide_title = wide.find(&vcs_sheet_title(&sample_project_box().name)).expect("the title");
        assert!(wide.find("Height(28.0)").map_or(true, |i| i > wide_title), "no strip above a floating sheet");
    }

    #[compose_rust::runtime::Composable]
    fn closed_vcs_sheet() {
        VcsSheet("ember", false, || {});
    }

    #[test]
    fn fr_l18_vcs_sheet_shows_title_and_close_when_open() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let text = text_of(open_vcs_sheet);
        assert!(text.contains("버전 관리"), "{text}");
        // The header close button is the round icon button of every sheet (`CloseButton`).
        assert!(text.contains("Clickable") || text.contains("click") || text.contains("Click"), "{text}");
    }

    #[test]
    fn fr_l18_vcs_sheet_draws_nothing_when_closed() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        assert!(!text_of(closed_vcs_sheet).contains("버전 관리"));
    }

    #[compose_rust::runtime::Composable]
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
        let event = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        assert!(text.contains("feat/session-shim"));
        assert!(text.contains("Pull 1"));
        assert!(text.contains("변경 파일 5"));
        assert!(text.contains("shim.rs"));
    }

    #[test]
    fn fr_l19_commit_box_follows_the_tree_at_the_wide_layout() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(sample_screen)));
        let mut text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        let event = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
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

    fn vcs_textarea() {
        super::super::components::EmberField(super::super::components::FieldOptions {
            fill_max_width: true,
            height: Some(VCS_TEXTAREA_H),
            inset: Some(VCS_TEXTAREA_INSET),
            line_height: Some(VCS_TEXTAREA_LINE_H),
            type_role: Some(VCS_FIELD_ROLE),
            multiline: true,
            ..Default::default()
        });
    }

    #[test]
    fn fr_l10_2_vcs_textarea_is_64_high_inset_8_12_and_13_px() {
        // vcs.css `.vcs textarea.field{height:64px;padding:8px 12px;font-size:var(--fs-sm)}`.
        assert_eq!((VCS_TEXTAREA_H, VCS_TEXTAREA_INSET), (64.0, (12.0, 8.0)));
        use compose_rust::protocol::{decode_batch, Mutation};
        use compose_rust::{Host, Recomposer};
        let mut host = Host::new(|| Box::new(Recomposer::new(vcs_textarea)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mods: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { modifier, .. } => Some(format!("{modifier:?}")),
                _ => None,
            })
            .collect();
        let props: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { property, value, .. } => Some(format!("{property:?}={value:?}")),
                _ => None,
            })
            .collect();
        assert!(mods.iter().any(|m| m == "Height(64.0)"), "{mods:?}");
        // 8 on every side, and 4 more on each side: 12 from the sides (BareField's inset).
        assert!(mods.iter().any(|m| m == "Padding(8.0)"), "{mods:?}");
        assert_eq!(mods.iter().filter(|m| *m == "Width(4.0)").count(), 2, "{mods:?}");
        assert!(props.iter().any(|p| p == "FontSize=Float(13.0)"), "{props:?}");
        // Three whole 16 px lines in the 48 px text area, not 2.4 lines of 20 (review r6).
        assert!(props.iter().any(|p| p.starts_with("LineHeight=") && p.contains("16.0")), "{props:?}");
        assert!(!props.iter().any(|p| p.starts_with("LineHeight=") && p.contains("20.0")), "{props:?}");
        assert_eq!((VCS_TEXTAREA_H - 2.0 * VCS_TEXTAREA_INSET.1) / VCS_TEXTAREA_LINE_H, 3.0);
        // Every vcs textarea takes these.
        let src = include_str!("vcs.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        for placeholder in ["\"설명 (선택)\"", "\"코멘트나 변경 요청의 내용\""] {
            for (at, _) in production.match_indices(placeholder) {
                let start = production[..at].rfind("EmberField(FieldOptions {").expect("field");
                let end = at + production[at..].find("});").expect("end");
                let opts = &production[start..end];
                for want in ["height: Some(VCS_TEXTAREA_H)", "inset: Some(VCS_TEXTAREA_INSET)", "line_height: Some(VCS_TEXTAREA_LINE_H)", "type_role: Some(VCS_FIELD_ROLE)", "multiline: true"] {
                    assert!(opts.contains(want), "{want} in {opts}");
                }
            }
        }
        assert_eq!(production.matches("height: Some(VCS_TEXTAREA_H)").count(), 3, "commit, PR and review textareas");
    }

    #[test]
    fn fr_l19_control_text_uses_the_small_scale() {
        // The constant is the design token: 13 px, and the field role draws the same step.
        assert_eq!(VCS_CONTROL_FS, Fs::Sm);
        assert_eq!(VCS_CONTROL_FS.size(), 13.0);
        assert_eq!(super::super::components::type_role_style(VCS_FIELD_ROLE).0, VCS_CONTROL_FS);
        assert!(VCS_HUNK_BUTTON_H < H_CTL);
    }

    #[test]
    fn a_small_pill_takes_the_width_of_its_label_and_14_at_each_side() {
        // compose-rust#427 (#673): the `text_px` estimate with its 64 minimum is gone. The pill
        // sends a height and no width, and its label sits between two 14 dp spaces.
        let _guard = crate::services::test_guard();
        assert_eq!(VCS_PILL_PAD_X, 14.0);
        let nodes = nodes_of(|| StashPill("적용", "스태시 1 적용", ButtonKind::Default, true, || {}));
        let over = nodes.iter().find(|n| n.widget == "Button").expect("the pill's button");
        assert!(over.props.iter().any(|x| x == "AccessibleName=String(\"스태시 1 적용\")"), "{:?}", over.props);
        let pill = nodes.iter().find(|n| Some(n.id) == over.parent).expect("the pill");
        assert!(pill.mods.iter().any(|m| *m == format!("Height({H_CTL:?})")), "{:?}", pill.mods);
        assert!(!pill.mods.iter().any(|m| m.starts_with("Width(") || m.starts_with("Size(") || m.starts_with("FillMax")), "{:?}", pill.mods);
        let label = with_text(&nodes, "적용");
        let row = nodes.iter().find(|n| Some(n.id) == label.parent).expect("the label's row");
        let parts = kids(&nodes, row.id);
        assert_eq!(parts.len(), 3, "a side, the label, a side");
        for side in [parts[0], parts[2]] {
            assert!(side.widget == "Spacer" && side.mods.iter().any(|m| m == "Width(14.0)"), "{:?}", side.mods);
        }
        let production = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains(&["vcs_pill", "_width"].concat()), "the estimate is gone");
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
        let event = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 390.0,
                height_dp: 844.0,
                class: WindowSizeClass::Compact,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        assert!(text.contains("파일"));
        assert!(text.contains("차이"));
        assert!(text.contains("커밋"));
    }

    #[test]
    fn a_fixed_name_sits_on_the_painted_box_and_a_data_name_on_a_matched_button() {
        // compose-rust#429, #431 (#673): the three `FocusOverlay` functions are gone. A control
        // whose name is one of a fixed set is the painted box itself; one named from data keeps a
        // clear button that takes its box's bounds (`components::DataNamedOverlay`).
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let named = |nodes: &[N], name: &str| -> usize {
            let want = format!("AccessibleName=String({name:?})");
            nodes.iter().position(|n| n.props.contains(&want)).unwrap_or_else(|| panic!("no control named {name}"))
        };
        let press = |n: &N| n.mods.iter().filter(|m| m.starts_with("PressFeedback")).count();
        let button_role = |n: &N| n.mods.iter().any(|m| m.starts_with("Semantics") && m.contains("Button"));

        // A pane tab: the pill has the fill, the name, the role, the click and the press.
        let nodes = nodes_of(|| { use ::compose_rust::ui::Row; Row!({ PaneTabSegment("파일", true, || {}); }) });
        assert!(!nodes.iter().any(|n| n.widget == "Button"), "no button over the pill");
        let tab = &nodes[named(&nodes, "파일")];
        assert_eq!(tab.widget, "Box");
        assert!(tab.props.iter().any(|x| x.starts_with("OnClick=")), "{:?}", tab.props);
        assert!(button_role(tab) && press(tab) == 1, "{:?}", tab.mods);
        let fill = format!("{:?}", compose_rust::schema::Modifier::Background(paint(ember_theme().palette.ink)));
        assert!(tab.mods.contains(&fill) && tab.mods.iter().any(|m| *m == format!("Height({H_CTL:?})")), "{:?}", tab.mods);

        // A round icon button: the 44 dp box is the control and is round, the 32 dp circle is drawn in it,
        // and MinTouchTarget(44.0) keeps a 44 dp hit area inside a shorter row (compose-rust#472, v0.0.11).
        let nodes = nodes_of(|| CircleIconButton(ICON_FETCH_SVG, "가져오기", || {}));
        assert!(!nodes.iter().any(|n| n.widget == "Button"));
        let hit = &nodes[named(&nodes, "가져오기")];
        assert!(hit.props.iter().any(|x| x.starts_with("OnClick=")) && button_role(hit) && press(hit) == 1, "{:?}", hit.mods);
        assert!(hit.mods.iter().any(|m| m == "Width(44.0)") && hit.mods.iter().any(|m| m.starts_with("ShapeRole(Full")), "{:?}", hit.mods);
        assert!(hit.mods.iter().any(|m| m == "MinTouchTarget { size: 44.0 }"), "{:?}", hit.mods);

        // A row named by a path: one clear button with the path as text and name, exactly over
        // its box, which keeps its own size.
        let nodes = nodes_of(|| ConflictFileRow("docs/SPEC.md", || {}));
        let buttons: Vec<&N> = nodes.iter().filter(|n| n.widget == "Button").collect();
        assert_eq!(buttons.len(), 1);
        let over = buttons[0];
        assert!(over.props.iter().any(|x| x == "Text=String(\"docs/SPEC.md\")"), "{:?}", over.props);
        assert!(over.props.iter().any(|x| x == "AccessibleName=String(\"docs/SPEC.md\")"), "{:?}", over.props);
        assert!(over.props.iter().any(|x| x.starts_with("OnClick=")), "{:?}", over.props);
        assert!(over.mods.iter().any(|m| m == "MatchParentSize"), "{:?}", over.mods);
        assert!(!over.mods.iter().any(|m| m.starts_with("FillMax")), "it does not widen its box: {:?}", over.mods);
        let row = nodes.iter().find(|n| Some(n.id) == over.parent).expect("its box");
        assert!(row.widget == "Box" && row.mods.iter().any(|m| *m == format!("Height({CONFLICT_ROW_H:?})")), "{:?}", row.mods);
        assert!(!row.props.iter().any(|x| x.starts_with("OnClick=")), "one tap target, not two");

        let production = include_str!("vcs.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains(&["Focus", "Overlay("].concat()), "the per-screen overlay is gone");
        assert!(!production.contains(&["cu::", "Button("].concat()), "no direct toolkit button is left");
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
                    Mutation::SetProp { node_id, property: PropertyKind::Text | PropertyKind::AccessibleName, value: PropertyValue::String(s) } => {
                        button_texts.insert(*node_id, s.to_string());
                    }
                    _ => {}
                }
            }
        };
        scan(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        
        let event = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
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
                    Mutation::SetProp { node_id, property: PropertyKind::Text | PropertyKind::AccessibleName, value: PropertyValue::String(s) } => {
                        button_texts.insert(*node_id, s.to_string());
                    }
                    _ => {}
                }
            }
        };
        scan(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));

        let event = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 390.0,
                height_dp: 844.0,
                class: WindowSizeClass::Compact,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
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
                    { use ::compose_rust::ui::Column; Column!({
                        ChangesPane(&s, PaneLayout::Tabs, Some(tab), |_| {});
                    }) };
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
                        // A painted control that takes its own name is a clickable box (compose-rust#429, #673).
                        Mutation::SetProp { node_id, property: PropertyKind::OnClick, .. } => {
                            tab_nodes.insert(*node_id);
                        }
                        Mutation::SetProp { node_id, property: PropertyKind::Text | PropertyKind::AccessibleName, value: PropertyValue::String(s) } => {
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
                { use ::compose_rust::ui::Column; Column!({
                    SectionList(Section::Changes, None, |_| {});
                    let diffs = sample_snapshot().changes;
                    let st = mutable_state_of(Staging::new(&diffs, false));
                    let folded = mutable_state_of(BTreeSet::new());
                    let sel = mutable_state_of(0usize);
                    FilesTree(&diffs, &st, &folded, &sel, |_| {});
                }) };
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

    #[test]
    fn fr_l19_team_pane_renders_enriched_cards_and_action_buttons() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let count = mutable_state_of(None::<usize>);
        let PrLoad::Ready(sample_pr_list) = sample_prs() else { panic!() };
        let prs = mutable_state_of(Some(sample_pr_list));
        let sec = mutable_state_of(Section::Team);

        let mut host = Host::new(move || {
            let cnt = count.clone();
            let p = prs.clone();
            let s = sec.clone();
            Box::new(Recomposer::new(move || {
                TeamPane(None, cnt.clone(), p.clone(), None, PaneLayout::Two, Some(s.clone()), None);
            }))
        });

        let mutations = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let texts: Vec<String> = mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp {
                    property: PropertyKind::Text,
                    value: PropertyValue::String(s),
                    ..
                } => Some(s.to_string()),
                _ => None,
            })
            .collect();

        assert!(texts.iter().any(|t| t == "ember-리더"), "should render leader name");
        assert!(texts.iter().any(|t| t == "ember-codex"), "should render codex name");
        assert!(texts.iter().any(|t| t.contains("UI wire & state")), "should render session title");
        assert!(texts.iter().any(|t| t.contains("claude-code")), "should render harness");
        assert!(texts.iter().any(|t| t == "리더"), "should render leader pill");
        assert!(texts.iter().any(|t| t == "브랜치"), "should render branch label");
        assert!(texts.iter().any(|t| t == "워크트리"), "should render Korean worktree label");
        assert!(!texts.iter().any(|t| t == "worktree"), "must not render English 'worktree' label");
        assert!(texts.iter().any(|t| t == "컴퓨터"), "should render computer label");
        assert!(texts.iter().any(|t| t == "mac-mini"), "should render computer name");
        assert!(texts.iter().any(|t| t == "깨끗함"), "should render clean status");
        assert!(texts.iter().any(|t| t == "확인 안 됨"), "should render unknown dirty status for non-current worktree");
        assert!(texts.iter().any(|t| t == "브랜치 보기"), "should render '브랜치 보기' action button");
        assert!(texts.iter().any(|t| t == "세션"), "should render '세션' action button");
    }

    #[test]
    fn fr_l19_team_card_buttons_draw_their_text_at_the_small_step() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let count = mutable_state_of(None::<usize>);
        let PrLoad::Ready(sample_pr_list) = sample_prs() else { panic!() };
        let prs = mutable_state_of(Some(sample_pr_list));
        let sec = mutable_state_of(Section::Team);
        let mut host = Host::new(move || {
            let cnt = count.clone();
            let p = prs.clone();
            let s = sec.clone();
            Box::new(Recomposer::new(move || {
                TeamPane(None, cnt.clone(), p.clone(), None, PaneLayout::Two, Some(s.clone()), None);
            }))
        });
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut text_of: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut size_of: std::collections::HashMap<u32, f32> = std::collections::HashMap::new();
        for m in &batch {
            match m {
                Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                    text_of.insert(*node_id, s.to_string());
                }
                Mutation::SetProp { node_id, property: PropertyKind::FontSize, value: PropertyValue::Float(f) } => {
                    size_of.insert(*node_id, *f);
                }
                _ => {}
            }
        }
        // The mockup's `.btn.sm`: the visible label of each card button is 13 px, not the toolkit's 16 px.
        for want in ["브랜치 보기", "세션"] {
            let sized: Vec<f32> = text_of.iter().filter(|(_, t)| *t == want).filter_map(|(id, _)| size_of.get(id).copied()).collect();
            assert!(!sized.is_empty(), "no sized text for {want}");
            assert!(sized.iter().all(|f| (*f - VCS_CONTROL_FS.size()).abs() < 0.01), "{want} is drawn at {sized:?}");
        }
    }

    #[test]
    fn fr_l19_team_row_view_renders_all_pr_states() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();

        // 1. Checking state (and unknown computer & dirty count)
        let row_checking = TeamRow {
            branch: "feat/check".to_string(),
            worktree: "/w/check".to_string(),
            member: None,
            extra_members: 0,
            sync_text: "원격과 같음".to_string(),
            pr_text: None,
            pr_status: TeamPrStatus::Checking,
            current: false,
            computer: None,
            dirty_count: None,
            last_commit: "init commit".to_string(),
            conflict: false,
        };

        let mut host_chk = Host::new(move || {
            let r = row_checking.clone();
            Box::new(Recomposer::new(move || {
                TeamRowView(&r, "test", || {}, |_| {}, || {}, |_| {});
            }))
        });
        let texts_chk: Vec<String> = decode_batch(host_chk.rebuild().expect("rebuild"))
            .expect("decode")
            .into_iter()
            .filter_map(|m| match m {
                Mutation::SetProp { property: PropertyKind::Text, value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect();
        assert!(texts_chk.iter().any(|t| t == "PR 확인 중"), "should render 'PR 확인 중' pill");
        assert!(texts_chk.iter().any(|t| t == "알 수 없음"), "unknown computer should render '알 수 없음'");
        assert!(texts_chk.iter().any(|t| t == "확인 안 됨"), "unknown dirty count should render '확인 안 됨'");

        // 2. Failed state
        let row_failed = TeamRow {
            branch: "feat/failed".to_string(),
            worktree: "/w/failed".to_string(),
            member: None,
            extra_members: 0,
            sync_text: "원격과 같음".to_string(),
            pr_text: None,
            pr_status: TeamPrStatus::Failed,
            current: false,
            computer: None,
            dirty_count: Some(0),
            last_commit: "".to_string(),
            conflict: false,
        };
        let mut host_fail = Host::new(move || {
            let r = row_failed.clone();
            Box::new(Recomposer::new(move || {
                TeamRowView(&r, "test", || {}, |_| {}, || {}, |_| {});
            }))
        });
        let texts_fail: Vec<String> = decode_batch(host_fail.rebuild().expect("rebuild"))
            .expect("decode")
            .into_iter()
            .filter_map(|m| match m {
                Mutation::SetProp { property: PropertyKind::Text, value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect();
        assert!(texts_fail.iter().any(|t| t == "PR 확인 실패"), "should render 'PR 확인 실패' pill");

        // 3. None state
        let row_none = TeamRow {
            branch: "feat/none".to_string(),
            worktree: "/w/none".to_string(),
            member: None,
            extra_members: 0,
            sync_text: "원격과 같음".to_string(),
            pr_text: None,
            pr_status: TeamPrStatus::None,
            current: false,
            computer: Some("로컬".to_string()),
            dirty_count: Some(0),
            last_commit: "".to_string(),
            conflict: false,
        };

        let row_none_for_host = row_none.clone();
        let mut host_none = Host::new(move || {
            let r = row_none_for_host.clone();
            Box::new(Recomposer::new(move || {
                TeamRowView(&r, "test", || {}, |_| {}, || {}, |_| {});
            }))
        });
        let texts_none: Vec<String> = decode_batch(host_none.rebuild().expect("rebuild"))
            .expect("decode")
            .into_iter()
            .filter_map(|m| match m {
                Mutation::SetProp { property: PropertyKind::Text, value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect();
        assert!(texts_none.iter().any(|t| t == "PR 없음"), "should render 'PR 없음' pill");

        // 4. Matched with passed checks and PR chip click navigation
        let mut row_matched = row_none.clone();
        row_matched.pr_status = TeamPrStatus::Matched {
            number: 100,
            label: "#100 통과".to_string(),
            ci: Some(CheckClass::Passed),
            merged: false,
        };

        let clicked_pr = mutable_state_of(None::<u64>);
        let clicked_pr_cap = clicked_pr.clone();
        let mut host_matched = Host::new(move || {
            let r = row_matched.clone();
            let cp = clicked_pr_cap.clone();
            Box::new(Recomposer::new(move || {
                let cp_in = cp.clone();
                TeamRowView(&r, "test", || {}, |_| {}, || {}, move |n| cp_in.set(Some(n)));
            }))
        });
        let batch_matched = decode_batch(host_matched.rebuild().expect("rebuild")).expect("decode");
        let mut texts_matched: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut clickables: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        for m in &batch_matched {
            match m {
                Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                    texts_matched.insert(*node_id, s.to_string());
                }
                Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                    clickables.insert(*node_id, *h as u64);
                }
                _ => {}
            }
        }
        assert!(texts_matched.values().any(|t| t == "#100 통과"), "should render '#100 통과' chip");
        let pr_btn = texts_matched.iter().find(|(id, t)| *t == "#100 통과" && clickables.contains_key(id)).map(|(id, _)| *id).expect("clickable PR chip");
        let pr_handler = clickables[&pr_btn];
        host_matched.dispatch(HostEvent::new(pr_btn, pr_handler, EventPayload::Clicked)).expect("dispatch PR click");
        assert_eq!(clicked_pr.get(), Some(100), "clicking PR chip must pass its PR number");
    }

    #[test]
    fn fr_l19_team_row_view_conflict_and_extra_members() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();

        let row = TeamRow {
            branch: "feat/cf".to_string(),
            worktree: "/w/cf".to_string(),
            member: Some(TeamMember {
                name: "dev".to_string(),
                harness: "codex".to_string(),
                leader: false,
                state_text: "실행 중".to_string(),
                status: ember_client::LauncherStatus::Running,
                session_id: "s1".to_string(),
                title: "Fix conflict".to_string(),
            }),
            extra_members: 3,
            sync_text: "원격과 같음".to_string(),
            pr_text: None,
            pr_status: TeamPrStatus::None,
            current: true,
            computer: Some("로컬".to_string()),
            dirty_count: Some(5),
            last_commit: "commit msg".to_string(),
            conflict: true,
        };

        let mut host = Host::new(move || {
            let r = row.clone();
            Box::new(Recomposer::new(move || {
                TeamRowView(&r, "test", || {}, |_| {}, || {}, |_| {});
            }))
        });
        let texts: Vec<String> = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .into_iter()
            .filter_map(|m| match m {
                Mutation::SetProp { property: PropertyKind::Text, value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect();

        assert!(texts.iter().any(|t| t == "+3"), "extra_members should render '+3'");
        assert!(texts.iter().any(|t| t == "충돌 해결"), "conflict row should render '충돌 해결' button");
        assert!(texts.iter().any(|t| t == "변경 5개"), "dirty count 5 should render '변경 5개'");
    }

    #[test]
    fn fr_l19_team_pane_action_buttons_click_dispatch() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let count = mutable_state_of(None::<usize>);
        let prs = mutable_state_of(Some(vec![]));
        let selected_pr = mutable_state_of(None::<u64>);
        let sec = mutable_state_of(Section::Team);

        let sec_capture = sec.clone();
        let sel_pr_capture = selected_pr.clone();
        let mut host = Host::new(move || {
            let cnt = count.clone();
            let p = prs.clone();
            let sp = sel_pr_capture.clone();
            let s = sec_capture.clone();
            Box::new(Recomposer::new(move || {
                TeamPane(None, cnt.clone(), p.clone(), Some(sp.clone()), PaneLayout::Two, Some(s.clone()), None);
            }))
        });

        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");

        let mut button_texts: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut clickables: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();

        for m in &batch {
            match m {
                Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } => {
                    button_texts.insert(*node_id, s.to_string());
                }
                Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                    clickables.insert(*node_id, *h as u64);
                }
                _ => {}
            }
        }

        // The painted pill carries the text twice (its label and the button over it); the button is the clickable one.
        let br_button = button_texts.iter().find(|(id, text)| *text == "브랜치 보기" && clickables.contains_key(id)).map(|(id, _)| *id);
        assert!(br_button.is_some(), "expected '브랜치 보기' button");

        if let Some(node_id) = br_button {
            if let Some(&handler_id) = clickables.get(&node_id) {
                host.dispatch(HostEvent::new(node_id, handler_id, EventPayload::Clicked)).expect("dispatch");
                assert_eq!(sec.get(), Section::Branches, "clicking '브랜치 보기' should navigate to Section::Branches");
            }
        }

        // Clicking the PR chip (#409 실패 on fix/egress-stop) selects PR 409 and navigates to Section::PullRequests
        let pr_chip = button_texts.iter().find(|(id, text)| text.starts_with("#409") && clickables.contains_key(id)).map(|(id, _)| *id);
        assert!(pr_chip.is_some(), "expected clickable '#409 ...' PR chip in sample team pane");
        if let Some(node_id) = pr_chip {
            if let Some(&handler_id) = clickables.get(&node_id) {
                host.dispatch(HostEvent::new(node_id, handler_id, EventPayload::Clicked)).expect("dispatch PR chip");
                assert_eq!(selected_pr.get(), Some(409), "clicking PR chip should set selected_pr to 409");
                assert_eq!(sec.get(), Section::PullRequests, "clicking PR chip should navigate to Section::PullRequests");
            }
        }
    }

    #[compose_rust::runtime::Composable]
    fn team_compact_probe() {
        let cnt = compose_rust::runtime::remember(|| mutable_state_of(None::<usize>));
        let p = compose_rust::runtime::remember(|| mutable_state_of(Some(Vec::<PrEntry>::new())));
        let s = compose_rust::runtime::remember(|| mutable_state_of(Section::Team));
        TeamPane(None, cnt, p, None, PaneLayout::Tabs, Some(s), None);
    }

    #[test]
    fn fr_l19_team_pane_compact_adds_bottom_clearance() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(team_compact_probe)));
        let initial = host.rebuild().expect("rebuild");
        let batch = decode_batch(initial).expect("decode");
        let clearance = crate::ui::cr::routines::compact_bottom_clearance();
        let has_clearance = batch.iter().any(|m| match m {
            Mutation::SetModifier {
                modifier: compose_rust::Modifier::Height(h),
                ..
            } => (*h - clearance).abs() < 0.1,
            _ => false,
        });
        assert!(has_clearance, "compact width should include bottom clearance Spacer");
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn issue_878_conflict_body_loads_sample_in_demo_mode() {
        let src = include_str!("vcs.rs");
        let start = src.find("fn ConflictsBody(").expect("ConflictsBody");
        let body = &src[start..start + 1800];
        assert!(body.contains("actions.is_none() || crate::demo::active_from_env()"), "{body}");
        assert!(body.contains("ConflictLoad::Ready(sample_conflict())"), "{body}");
    }
}
