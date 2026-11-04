//! compose-rust version of the 버전 관리 (VCS) screen (FR-L19, INTENT D34, `docs/DESIGN.md` section 6.6).
//!
//! Pure decisions (sections, tabs, ahead/behind text, diff parsing, line/hunk/file staging,
//! file tree, commit validation, merge gate) are re-exported from `crate::ui::vcs`.
//!
//! When `snapshot()` returns None (no server git data exists yet), views show `SOON` ("준비 중입니다").
//! When `std::env::var("EMBER_CR_ROUTE") == Ok("vcs")`, sample data from mockup v8 is used
//! so review captures show the full layout.

#![allow(non_snake_case)]

use std::collections::BTreeSet;

use compose_rust::runtime::{current_window_size, mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{
    Alignment, ButtonVariant, Color, IconRole, Paint, ShapeRole, TextOverflow, TypeRole,
    WindowSizeClass,
};

pub use crate::ui::vcs::{
    ahead_behind_text, bar_fractions, build_tree, check_glyph, check_of, commit_enabled,
    commit_label, effective_tab, fetched_text, folder_check, merge_gate, open_for_project,
    pane_layout, pane_tabs, parse_hunk_header, parse_unified, pick_project, remote_action,
    repo_projects, route_section, section_count, section_nav, snapshot, soon_line,
    tab_after_select, upstream_text, Check, CiState, DiffLine, FileDiff, FileStaging,
    FileStatus, Hunk, LineKind, PaneLayout, PaneTab, RemoteAction, RepoSnapshot, ReviewState,
    Section, SectionNav, Staging, TreeRow, SOON,
};
use crate::ui::coexist::SharedUi;
use crate::ui::project::{display_name, project_boxes, ProjectBox};
use crate::ui::shell::NavMenu;
use crate::ui::Route;

use super::components::{
    AppButton, ButtonKind, EmberCard, EmberField, FieldOptions, IconButton, Label, OptionSheet,
    PickChip, Pill, PillTone, Txt,
};
use super::notice;
use super::theme::*;

/// Open 버전 관리 for a project in compose-rust shell.
pub fn open_for_project_cr(ui: &SharedUi, project: &str) {
    ui.set_selected_project(Some(project.to_string()));
    ui.go(Route::Projects);
    ui.set_menu(NavMenu::VersionControl);
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
            if section == Section::PullRequests {
                Label("병합 조건: CI 통과와 승인이 필요합니다.", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            }
        });
    });
}

fn section_icon(sec: Section) -> Option<IconRole> {
    match sec {
        Section::Changes => Some(IconRole::List),
        Section::History => Some(IconRole::History),
        Section::Branches => Some(IconRole::Sidebar),
        Section::PullRequests => Some(IconRole::Forward),
        Section::Stash => Some(IconRole::Inbox),
        Section::Conflicts => Some(IconRole::Close),
        Section::Team => Some(IconRole::More),
    }
}

/// The bar of three: repository, current branch, remote origin in ONE split card.
pub fn RepoBar(
    projects: &[(String, String)],
    project: &ProjectBox,
    snap: Option<&RepoSnapshot>,
    compact: bool,
    mut on_pick: impl FnMut(String) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let project_menu = remember(|| mutable_state_of(false));
    let branch_menu = remember(|| mutable_state_of(false));

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
    let remote_line = match snap {
        Some(s) => format!(
            "{} \u{00b7} {}",
            ahead_behind_text(s.ahead, s.behind),
            fetched_text(s.fetched_age_secs)
        ),
        None => SOON.to_string(),
    };
    let branches = snap.map(|s| s.branches.clone()).unwrap_or_default();
    let current_branch = snap.map(|s| s.branch.clone());

    let proj_id = project.id.clone();
    let proj_name = display_name(&project.id);
    let repo_line = project.repo_line();
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
        EmberCard(false, true, None, Some(0.0), None, || {
            if compact {
                cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
                    let p_open = project_menu_c.clone();
                    let p_name = proj_name.clone();
                    let r_line = repo_line.clone();
                    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[2])).spacing(SP[1]).content(move || {
                        SmallNote("저장소");
                        cu::Button(format!("{p_name}  \u{25be}"))
                            .variant(ButtonVariant::Text)
                            .color(paint(p.text))
                            .modifier(Modifier::new().height(36.0))
                            .on_click(move || p_open.set(true));
                        SmallNote(&r_line);
                    });

                    cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));

                    let b_open = branch_menu_c.clone();
                    let b_text = branch_text.clone();
                    let u_text = upstream.clone();
                    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[2])).spacing(SP[1]).content(move || {
                        SmallNote("현재 브랜치");
                        cu::Button(format!("{b_text}  \u{25be}"))
                            .variant(ButtonVariant::Text)
                            .color(paint(p.text))
                            .modifier(Modifier::new().height(36.0))
                            .on_click(move || b_open.set(true));
                        SmallNote(&u_text);
                    });

                    cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));

                    let r_line2 = remote_line.clone();
                    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[2])).spacing(SP[1]).content(move || {
                        SmallNote("원격 origin");
                        cu::Row().modifier(Modifier::new().fill_max_width()).alignment(Alignment::CenterStart).spacing(SP[1]).content(move || {
                            AppButton(&action.label(), None, ButtonKind::Primary, true, has, false, move || {
                                notice::post(soon_line("원격 동작"));
                            });
                            cu::Row().spacing(4.0).content(|| {
                                IconButton(IconRole::History, true, || notice::post(soon_line("Fetch")));
                                IconButton(IconRole::Back, true, || notice::post(soon_line("Pull")));
                                IconButton(IconRole::Forward, true, || notice::post(soon_line("Push")));
                            });
                        });
                        SmallNote(&r_line2);
                    });
                });
            } else {
                cu::Row().modifier(Modifier::new().fill_max_width().height(114.0)).alignment(Alignment::CenterStart).content(move || {
                    let p_open = project_menu_c.clone();
                    let p_name = proj_name.clone();
                    let r_line = repo_line.clone();
                    cu::Column().modifier(Modifier::new().weight(1.1).fill_max_height().padding(SP[3])).spacing(SP[1]).content(move || {
                        SmallNote("저장소");
                        cu::Button(format!("{p_name}  \u{25be}"))
                            .variant(ButtonVariant::Text)
                            .color(paint(p.text))
                            .modifier(Modifier::new().height(36.0))
                            .on_click(move || p_open.set(true));
                        SmallNote(&r_line);
                    });

                    cu::Box().modifier(Modifier::new().width(1.0).fill_max_height().background(paint(p.line)));

                    let b_open = branch_menu_c.clone();
                    let b_text = branch_text.clone();
                    let u_text = upstream.clone();
                    cu::Column().modifier(Modifier::new().weight(1.1).fill_max_height().padding(SP[3])).spacing(SP[1]).content(move || {
                        SmallNote("현재 브랜치");
                        cu::Button(format!("{b_text}  \u{25be}"))
                            .variant(ButtonVariant::Text)
                            .color(paint(p.text))
                            .modifier(Modifier::new().height(36.0))
                            .on_click(move || b_open.set(true));
                        SmallNote(&u_text);
                    });

                    cu::Box().modifier(Modifier::new().width(1.0).fill_max_height().background(paint(p.line)));

                    let r_line2 = remote_line.clone();
                    cu::Column().modifier(Modifier::new().weight(1.0).fill_max_height().padding(SP[3])).spacing(SP[1]).content(move || {
                        SmallNote("원격 origin");
                        cu::Row().modifier(Modifier::new().fill_max_width()).alignment(Alignment::CenterStart).spacing(SP[1]).content(move || {
                            AppButton(&action.label(), None, ButtonKind::Primary, true, has, false, move || {
                                notice::post(soon_line("원격 동작"));
                            });
                            cu::Row().spacing(4.0).content(|| {
                                IconButton(IconRole::History, true, || notice::post(soon_line("Fetch")));
                                IconButton(IconRole::Back, true, || notice::post(soon_line("Pull")));
                                IconButton(IconRole::Forward, true, || notice::post(soon_line("Push")));
                            });
                        });
                        SmallNote(&r_line2);
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

        OptionSheet(
            branch_menu.get(),
            &branch_opts,
            chosen_br,
            240.0,
            move || b_dismiss.set(false),
            move |_| {
                notice::post(soon_line("브랜치 전환"));
            },
        );
    });
}

/// Section list on the left (wide windows) with counts.
pub fn SectionList(
    active: Section,
    snap: Option<&RepoSnapshot>,
    mut on_pick: impl FnMut(Section) + Clone + 'static,
) {
    let p = ember_theme().palette;
    cu::Column().modifier(Modifier::new().width(200.0)).spacing(2.0).content(|| {
        for sec in Section::ALL {
            let is_active = sec == active;
            let bg = if is_active { paint(p.surface) } else { Paint::Literal(Color::argb(0)) };
            let fg = if is_active { p.text } else { p.muted };
            let count = section_count(sec, snap);
            let mut pick = on_pick.clone();

            let label_text = if let Some(n) = count {
                format!("{}  {n}", sec.label())
            } else {
                sec.label().to_string()
            };

            let mut m = Modifier::new()
                .fill_max_width()
                .height(40.0)
                .background(bg)
                .corner_radius(CORNER_SM);
            if is_active {
                m = m.border(1.0, paint(p.line));
            }

            let mut btn = cu::Button(label_text)
                .variant(ButtonVariant::Text)
                .color(paint(fg))
                .modifier(m)
                .on_click(move || pick(sec));
            if let Some(icon) = section_icon(sec) {
                btn = btn.icon(icon);
            }
            drop(btn);
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
            // Hunk header row (45 px)
            cu::Row().modifier(Modifier::new().fill_max_width().height(45.0).background(paint(p.surface_2)).padding(SP[2])).alignment(Alignment::CenterStart).content(|| {
                Label(&h.header, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                let st_hunk = staging.clone();
                let f_hunk = file.clone();
                cu::Button("이 덩어리 전체")
                    .variant(ButtonVariant::Text)
                    .color(paint(p.text))
                    .modifier(
                        Modifier::new()
                            .width(102.0)
                            .height(32.0)
                            .border(2.0, paint(p.ink))
                            .background(paint(p.surface))
                            .shape_role(ShapeRole::Full),
                    )
                    .on_click(move || {
                        let mut s = st_hunk.get();
                        s.file_mut(&f_hunk.path).toggle_hunk(&f_hunk, hi);
                        st_hunk.set(s);
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
                            cu::Box().modifier(Modifier::new().width(26.0).height(21.0)).alignment(Alignment::Center).content(|| {
                                cu::Checkbox(is_line_staged)
                                    .modifier(Modifier::new().size(14.0, 14.0))
                                    .on_change(move |_| {
                                        let mut s = st_line.get();
                                        s.file_mut(&f_line.path).toggle_line(&f_line, hi, li);
                                        st_line.set(s);
                                    });
                            });
                        } else {
                            cu::Spacer().modifier(Modifier::new().width(26.0));
                        }

                        let old_str = l.old_no.map(|n| format!("{:>4}", n)).unwrap_or_else(|| "    ".to_string());
                        let new_str = l.new_no.map(|n| format!("{:>4}", n)).unwrap_or_else(|| "    ".to_string());
                        let sign_str = match l.kind {
                            LineKind::Add => "+",
                            LineKind::Del => "-",
                            LineKind::Context => " ",
                        };

                        cu::Box().modifier(Modifier::new().width(36.0)).alignment(Alignment::CenterEnd).content(|| {
                            Label(&old_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                        cu::Box().modifier(Modifier::new().width(36.0)).alignment(Alignment::CenterEnd).content(|| {
                            Label(&new_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                        cu::Box().modifier(Modifier::new().width(16.0)).alignment(Alignment::Center).content(|| {
                            Label(sign_str, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                        });
                        cu::Spacer().modifier(Modifier::new().width(4.0));
                        Txt(
                            &l.text,
                            TypeRole::Mono,
                            Some(paint(p.text)),
                            Some(1),
                            Some(TextOverflow::Ellipsis),
                            None,
                            None,
                        );
                    });
                }
            });
        }
    });
}

fn FilesHeader(
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
        IconButton(IconRole::Inbox, true, move || {
            notice::post(soon_line("스태시"));
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
                        let chev = if open { "\u{25be} " } else { "\u{25b8} " };
                        let indent = if depth > 0 { "  ".repeat(depth) } else { String::new() };
                        let dir_label = format!("{indent}{chev}{label}");
                        cu::Button(dir_label)
                            .variant(ButtonVariant::Text)
                            .color(paint(p.text_2))
                            .modifier(Modifier::new().fill_max_width().height(34.0))
                            .on_click(move || {
                                let mut f = f_state.get();
                                if !f.remove(&pth) {
                                    f.insert(pth.clone());
                                }
                                f_state.set(f);
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

                        cu::Row().modifier(Modifier::new().fill_max_width().height(34.0).background(bg).corner_radius(CORNER_XS)).alignment(Alignment::CenterStart).content(move || {
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
                            let file_btn_name = name.clone();
                            cu::Button(file_btn_name)
                                .variant(ButtonVariant::Text)
                                .color(paint(p.text))
                                .modifier(Modifier::new().weight(1.0).height(34.0))
                                .on_click(move || {
                                    sel_state.set(index);
                                    on_tab_file(tab_after_select(PaneTab::Files));
                                });
                            cu::Box().modifier(Modifier::new().size(18.0, 18.0).background(paint(tone_fill)).corner_radius(CORNER_XS)).alignment(Alignment::Center).content(|| {
                                Label(letter, Fs::Xs2, Weight::Semibold, Some(paint(tone_text)), Some(1));
                            });
                            cu::Spacer().modifier(Modifier::new().width(SP[1]));
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

    EmberCard(false, true, None, Some(0.0), None, move || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
            FilesHeader(diffs_len, &all_checks, &st_clone, &d_clone);
            cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
            FilesTree(&d_clone, &st_clone, &fold_clone, &sel_clone, on_tab);
        });
    });
}

fn DiffPane(diffs: &[FileDiff], selected: usize, staging: &MutableState<Staging>) {
    let p = ember_theme().palette;
    let selected_file = diffs.get(selected).cloned();
    let st_diff = staging.clone();
    EmberCard(false, true, None, Some(0.0), None, move || {
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
    amend: &MutableState<bool>,
) {
    let p = ember_theme().palette;
    let sum_state = summary.clone();
    let amend_state = amend.clone();
    let is_amend = amend.get();
    let branch_name = branch.to_string();
    let can_commit = commit_enabled(&summary.get(), staged_files, is_amend);

    cu::Column().modifier(Modifier::new().fill_max_width().padding(SP[2])).spacing(SP[2]).content(move || {
        EmberField(FieldOptions {
            fill_max_width: true,
            placeholder: "요약 (필수) 예: Feat: Wire shim".to_string(),
            on_value_change: Some(Box::new(move |v: String| {
                sum_state.set(v);
            })),
            ..Default::default()
        });

        // Amend checkbox
        cu::Row().modifier(Modifier::new().fill_max_width().height(32.0)).alignment(Alignment::CenterStart).spacing(SP[1]).content(|| {
            let am = amend_state.clone();
            cu::Checkbox(is_amend)
                .modifier(Modifier::new().size(18.0, 18.0))
                .on_change(move |v| am.set(v));
            Label("마지막 커밋 수정 (amend)", Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
        });

        AppButton(
            &commit_label(&branch_name, staged_files),
            Some(IconRole::Check),
            ButtonKind::Primary,
            false,
            can_commit,
            true,
            move || {
                notice::post(soon_line("커밋"));
            },
        );

        let note_text = if can_commit {
            SOON.to_string()
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
    amend: &MutableState<bool>,
) {
    EmberCard(false, true, None, Some(0.0), None, move || {
        CommitBox(branch, staged_files, summary, amend);
    });
}

/// Changes pane: tree, diff and commit box.
pub fn ChangesPane(
    snap: &RepoSnapshot,
    layout: PaneLayout,
    tab: Option<PaneTab>,
    on_tab: impl FnMut(PaneTab) + Clone + 'static,
) {
    let p = ember_theme().palette;
    let diffs = snap.changes.clone();
    let staging = remember(|| mutable_state_of(Staging::new(&diffs, false)));
    let folded = remember(|| mutable_state_of(BTreeSet::<String>::new()));
    let selected = remember(|| mutable_state_of(0usize));
    let summary = remember(|| mutable_state_of(String::new()));
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
            let am_files = amend.clone();

            cu::Row().modifier(Modifier::new().fill_max_width().fill_max_height()).spacing(GAP_CARD).content(move || {
                // Left card: file tree + commit box
                cu::Column().modifier(Modifier::new().width(320.0).fill_max_height()).content(move || {
                    EmberCard(false, true, None, Some(0.0), None, move || {
                        cu::Column().modifier(Modifier::new().fill_max_width().fill_max_height()).content(move || {
                            FilesHeader(diffs_len, &all_checks, &st_files, &d_files);
                            cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                            cu::ScrollColumn().modifier(Modifier::new().fill_max_width().weight(1.0)).content(move || {
                                FilesTree(&d_files, &st_files, &fold_files, &sel_files, on_tab_files);
                            });
                            cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                            CommitBox(&branch, staged_files, &sum_files, &am_files);
                        });
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
            let am_files = amend.clone();

            cu::Column().modifier(Modifier::new().fill_max_width()).spacing(GAP_CARD).content(move || {
                EmberCard(false, true, None, Some(0.0), None, move || {
                    cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
                        FilesHeader(diffs_len, &all_checks, &st_files, &d_files);
                        cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                        FilesTree(&d_files, &st_files, &fold_files, &sel_files, on_tab_files);
                        cu::Box().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                        CommitBox(&branch, staged_files, &sum_files, &am_files);
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
                    PaneTab::Commit => CommitPane(&snap.branch, staged_files, &summary, &amend),
                    _ => {
                        EmberCard(false, true, None, Some(0.0), None, move || {
                            cu::Column().modifier(Modifier::new().fill_max_width()).content(move || {
                                FilesHeader(diffs_len, &all_checks, &st_files, &d_files);
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
) {
    match (snap, sec) {
        (Some(s), Section::Changes) => {
            let tab_state = tab.clone();
            ChangesPane(
                s,
                layout,
                active_tab,
                move |t| tab_state.set(t),
            );
        }
        _ => Unavailable(sec),
    }
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
    let snap = if is_sample_route {
        Some(sample_snapshot())
    } else {
        current.as_deref().and_then(snapshot)
    };
    VcsScreenWith(snap, boxes, current);
}

/// VCS screen with explicit snapshot and projects.
pub fn VcsScreenWith(
    snap: Option<RepoSnapshot>,
    boxes: Vec<ProjectBox>,
    current_id: Option<String>,
) {
    let p = ember_theme().palette;
    let window = current_window_size();
    let class = window.class;
    let layout = pane_layout(class);
    let wide = section_nav(class) == SectionNav::List;
    let compact = class == WindowSizeClass::Compact;
    let pad = if compact { SP[2] } else { 32.0 };

    let section = remember(|| mutable_state_of(Section::Changes));
    let tab = remember(|| mutable_state_of(PaneTab::Files));
    let selected_proj = remember(|| mutable_state_of(current_id));

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
                let repo_name = proj.repo.as_deref().unwrap_or("darkpyonix-ember");
                let repo_short = repo_name.split('/').last().unwrap_or(repo_name);
                let branch_name = snap.as_ref().map_or("feat/session-shim", |s| &s.branch);
                let changes_cnt = snap.as_ref().map_or(0, |s| s.changes.len());
                let subtitle = format!("{repo_short} \u{00b7} {branch_name} \u{00b7} 변경 {changes_cnt} \u{00b7} PR 4");
                Label(&subtitle, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
        });

        if let Some(project) = project {
                    let sel_proj = selected_proj.clone();
                    RepoBar(
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
                    );

                    if snap.is_none() {
                        Label(&soon_line("저장소 정보"), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                    }

                    if wide {
                        cu::Row().modifier(Modifier::new().fill_max_width().weight(1.0)).spacing(SP[3]).content(|| {
                            let sec_state = section.clone();
                            cu::ScrollColumn().modifier(Modifier::new().width(200.0).fill_max_height()).content(|| {
                                SectionList(sec, snap.as_ref(), move |s| sec_state.set(s));
                            });
                            cu::Column().modifier(Modifier::new().weight(1.0).fill_max_height()).content(|| {
                                SectionContent(snap.as_ref(), sec, layout, active_tab, &tab);
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
                            SectionContent(snap.as_ref(), sec, layout, active_tab, &tab);
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
        VcsScreenWith(Some(sample_snapshot()), sample_projects(), Some("ember".into()));
    });
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, HostEvent};
    use compose_rust::{EventPayload, Host, Recomposer, WindowHeightClass, WindowSizeClass};

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
        assert!(snapshot("ember").is_none());
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
        assert_eq!(section_count(Section::Stash, Some(&snap)), None);
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
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        assert!(text.contains("파일"));
        assert!(text.contains("차이"));
        assert!(text.contains("커밋"));
    }
}
