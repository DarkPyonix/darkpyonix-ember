//! The Session screen (FR-L5 to FR-L7, FR-L15, FR-L16, FR-L18, FR-L24;
//! `docs/DESIGN.md` section 6.3).
//!
//! Tests first (#162): the implementation follows in the next commit.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide::IdeKind;
    use crate::ui::project::{Member, ProjectBox};
    use dioxus_compose::prelude::WindowSizeClass;
    use ember_client::LauncherStatus;

    fn member(id: &str, status: LauncherStatus, leader: bool, at: i64) -> Member {
        Member {
            session_id: id.into(),
            name: format!("m-{id}"),
            task: format!("task {id}"),
            agent: "claude-code".into(),
            status,
            leader,
            updated_at: at,
        }
    }

    fn project(id: &str, members: Vec<Member>) -> ProjectBox {
        ProjectBox {
            id: id.into(),
            name: id.into(),
            members,
            running: 0,
            waiting: 0,
            computers: vec![],
            top: None,
            repo: None,
            branch: None,
        }
    }

    fn ids(v: &[Member]) -> Vec<&str> {
        v.iter().map(|m| m.session_id.as_str()).collect()
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ---- FR-L18: views -----------------------------------------------------------------------

    #[test]
    fn fr_l18_view_switcher_has_multi_single_status() {
        let labels: Vec<_> = SessionView::ALL.iter().map(|v| v.label()).collect();
        assert_eq!(labels, ["멀티", "싱글", "상태"]);
        for v in SessionView::ALL {
            assert_eq!(SessionView::from_key(v.key()), Some(v));
        }
        assert_eq!(SessionView::from_key("nope"), None);
    }

    #[test]
    fn fr_l18_unknown_or_missing_saved_view_falls_back_to_multi() {
        assert_eq!(view_from_saved(None), SessionView::Multi);
        assert_eq!(view_from_saved(Some("bogus")), SessionView::Multi);
        assert_eq!(view_from_saved(Some("status")), SessionView::Status);
    }

    #[test]
    fn fr_l18_view_and_pane_tabs_survive_a_restart() {
        use crate::prefs::{load, save, Prefs};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let mut p = Prefs::default();
        p.session_view = Some(SessionView::Single.key().to_string());
        let mut panes = ProjectPanes::default();
        panes.add_tab(&strings(&["a", "b"]));
        panes.select(1);
        p.panes.insert("ember".into(), panes.clone());
        save(&path, &p).unwrap();
        let back = load(&path);
        assert_eq!(view_from_saved(back.session_view.as_deref()), SessionView::Single);
        assert_eq!(back.panes.get("ember"), Some(&panes));
        // An older prefs file has neither.
        std::fs::write(&path, br#"{"format":1}"#).unwrap();
        let old = load(&path);
        assert!(old.session_view.is_none() && old.panes.is_empty());
    }

    // ---- FR-L18: layout per size class -------------------------------------------------------

    #[test]
    fn fr_l18_panes_two_per_row_on_expanded_one_at_a_time_otherwise() {
        assert_eq!(pane_layout(WindowSizeClass::Expanded), PaneLayout { per_row: 2, paged: false });
        assert_eq!(pane_layout(WindowSizeClass::Medium), PaneLayout { per_row: 1, paged: true });
        assert_eq!(pane_layout(WindowSizeClass::Compact), PaneLayout { per_row: 1, paged: true });
    }

    #[test]
    fn fr_l18_pane_rows_chunk_by_columns() {
        assert_eq!(pane_rows(5, 2), vec![vec![0, 1], vec![2, 3], vec![4]]);
        assert_eq!(pane_rows(0, 2), Vec::<Vec<usize>>::new());
        assert_eq!(pane_rows(3, 0), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn fr_l18_paging_stops_at_both_ends() {
        assert_eq!(page_step(0, 3, false), 0);
        assert_eq!(page_step(0, 3, true), 1);
        assert_eq!(page_step(2, 3, true), 2);
        assert_eq!(page_step(5, 3, false), 1, "an out-of-range page is clamped first");
        assert_eq!(page_step(0, 0, true), 0);
    }

    #[test]
    fn fr_l18_side_list_is_a_column_on_expanded_and_a_drawer_elsewhere() {
        assert_eq!(side_list_mode(WindowSizeClass::Expanded), SideListMode::Column);
        assert_eq!(side_list_mode(WindowSizeClass::Medium), SideListMode::Drawer);
        assert_eq!(side_list_mode(WindowSizeClass::Compact), SideListMode::Drawer);
    }

    // ---- FR-L18, FR-L24: member grouping and order -------------------------------------------

    #[test]
    fn fr_l18_side_list_groups_by_project_current_first_leader_first() {
        let boxes = vec![
            project("a", vec![member("a1", LauncherStatus::Running, false, 5)]),
            project(
                "b",
                vec![
                    member("b1", LauncherStatus::WaitingForApproval, false, 9),
                    member("b2", LauncherStatus::Idle, true, 1),
                ],
            ),
        ];
        let s = side_sections(&boxes, "b", "");
        assert_eq!(s.iter().map(|x| x.project.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        // The leader comes first even though another member is waiting for approval.
        assert_eq!(ids(&s[0].members), ["b2", "b1"]);
        assert!(s[0].members[0].leader);
    }

    #[test]
    fn fr_l24_default_project_has_no_leader_first_rule_to_apply() {
        let boxes = vec![project(
            "unsorted",
            vec![
                member("u1", LauncherStatus::Idle, false, 1),
                member("u2", LauncherStatus::Running, false, 2),
            ],
        )];
        let s = side_sections(&boxes, "unsorted", "");
        assert_eq!(s[0].name, "기본 프로젝트");
        assert_eq!(ids(&s[0].members), ["u1", "u2"], "the given order is kept");
    }

    #[test]
    fn fr_l18_session_search_matches_member_task_harness_or_project() {
        let boxes = vec![
            project("alpha", vec![member("a1", LauncherStatus::Idle, false, 1)]),
            project("beta", vec![member("b1", LauncherStatus::Idle, false, 1), member("b2", LauncherStatus::Idle, false, 2)]),
        ];
        let by_task = side_sections(&boxes, "alpha", "task b2");
        assert_eq!(by_task.len(), 1);
        assert_eq!(ids(&by_task[0].members), ["b2"]);
        // A project name match keeps the whole section.
        let by_project = side_sections(&boxes, "alpha", "BETA");
        assert_eq!(by_project.len(), 1);
        assert_eq!(by_project[0].members.len(), 2);
        assert!(side_sections(&boxes, "alpha", "zzz").is_empty());
    }

    #[test]
    fn fr_l18_project_resolves_from_focus_then_choice_then_first() {
        let boxes = vec![project("a", vec![]), project("b", vec![])];
        assert_eq!(resolve_project(Some("b"), Some("a"), &boxes).as_deref(), Some("b"));
        assert_eq!(resolve_project(None, Some("b"), &boxes).as_deref(), Some("b"));
        assert_eq!(resolve_project(None, Some("gone"), &boxes).as_deref(), Some("a"));
        assert_eq!(resolve_project(None, None, &boxes).as_deref(), Some("a"));
        assert_eq!(resolve_project(None, None, &[]), None);
    }

    #[test]
    fn fr_l18_single_view_target_is_focus_then_leader_then_first() {
        let ms = vec![
            member("x", LauncherStatus::Idle, false, 1),
            member("l", LauncherStatus::Idle, true, 1),
        ];
        assert_eq!(single_target(&ms, Some("x")).as_deref(), Some("x"));
        assert_eq!(single_target(&ms, Some("elsewhere")).as_deref(), Some("l"));
        assert_eq!(single_target(&ms, None).as_deref(), Some("l"));
        assert_eq!(single_target(&ms[..1], None).as_deref(), Some("x"));
        assert_eq!(single_target(&[], None), None);
    }

    // ---- FR-L18: kanban ----------------------------------------------------------------------

    #[test]
    fn fr_l18_kanban_columns_in_design_order() {
        let labels: Vec<_> = KanbanColumn::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels, ["대기", "실행", "승인 대기", "검토", "완료"]);
    }

    #[test]
    fn fr_l18_kanban_buckets_every_status() {
        use KanbanColumn::*;
        use LauncherStatus as S;
        assert_eq!(kanban_column(S::Idle), Waiting);
        assert_eq!(kanban_column(S::Unknown), Waiting);
        assert_eq!(kanban_column(S::Running), Running);
        assert_eq!(kanban_column(S::WaitingForApproval), Approval);
        assert_eq!(kanban_column(S::FinishedUnread), Review);
        assert_eq!(kanban_column(S::Failed), Review);
        assert_eq!(kanban_column(S::Finished), Done);
    }

    #[test]
    fn fr_l18_kanban_always_has_five_columns_and_keeps_member_order() {
        let ms = vec![
            member("1", LauncherStatus::Running, false, 3),
            member("2", LauncherStatus::Running, false, 2),
            member("3", LauncherStatus::Finished, false, 1),
        ];
        let k = kanban(&ms);
        assert_eq!(k.len(), 5);
        assert_eq!(k[1].0, KanbanColumn::Running);
        assert_eq!(ids(&k[1].1), ["1", "2"]);
        assert_eq!(ids(&k[4].1), ["3"]);
        assert!(k[0].1.is_empty() && k[2].1.is_empty() && k[3].1.is_empty());
    }

    // ---- FR-L7: IDE chooser ------------------------------------------------------------------

    #[test]
    fn fr_l7_ide_chooser_lists_exactly_three() {
        let c = ide_choices(None);
        let labels: Vec<_> = c.iter().map(|x| x.label).collect();
        assert_eq!(labels, ["Ember 에디터", "VS Code", "JetBrains Gateway"]);
        for l in labels {
            for banned in ["Cursor", "Zed", "SSH"] {
                assert!(!l.contains(banned));
            }
        }
    }

    #[test]
    fn fr_l7_remembered_ide_is_listed_first_and_nothing_is_added() {
        let c = ide_choices(Some(IdeKind::Gateway));
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].kind, IdeKind::Gateway);
        let kinds: Vec<_> = c.iter().map(|x| x.kind).collect();
        assert!(kinds.contains(&IdeKind::Ember) && kinds.contains(&IdeKind::VsCode));
    }

    // ---- FR-L18: panes, tabs and the reorder menu --------------------------------------------

    #[test]
    fn fr_l18_move_pane_swaps_with_its_neighbour_and_stops_at_the_ends() {
        let mut o = strings(&["a", "b", "c"]);
        assert!(move_pane(&mut o, "b", PaneMove::Left));
        assert_eq!(o, ["b", "a", "c"]);
        assert!(!move_pane(&mut o, "b", PaneMove::Left), "already first");
        assert!(move_pane(&mut o, "a", PaneMove::Right));
        assert_eq!(o, ["b", "c", "a"]);
        assert!(!move_pane(&mut o, "a", PaneMove::Right), "already last");
        assert!(!move_pane(&mut o, "zz", PaneMove::Left), "unknown pane");
    }

    #[test]
    fn fr_l18_move_availability_matches_position() {
        let o = strings(&["a", "b", "c"]);
        assert_eq!((can_move(&o, "a", PaneMove::Left), can_move(&o, "a", PaneMove::Right)), (false, true));
        assert_eq!((can_move(&o, "c", PaneMove::Left), can_move(&o, "c", PaneMove::Right)), (true, false));
    }

    #[test]
    fn fr_l18_new_tab_shows_all_members_until_edited() {
        let t = PaneTab::all("탭");
        let all = strings(&["a", "b"]);
        assert_eq!(visible_panes(&t, &all), all);
    }

    #[test]
    fn fr_l18_closing_a_pane_in_an_automatic_tab_pins_the_rest() {
        let all = strings(&["a", "b", "c"]);
        let mut t = PaneTab::all("탭");
        close_pane(&mut t, &all, "b");
        assert_eq!(visible_panes(&t, &all), ["a", "c"]);
        // A member created later is not forced into a tab that was edited.
        let later = strings(&["a", "b", "c", "d"]);
        assert_eq!(visible_panes(&t, &later), ["a", "c"]);
    }

    #[test]
    fn fr_l18_adding_from_the_side_list_appends_once() {
        let all = strings(&["a", "b", "c"]);
        let mut t = PaneTab { name: "탭".into(), members: Some(strings(&["a"])) };
        add_pane(&mut t, &all, "c");
        add_pane(&mut t, &all, "c");
        add_pane(&mut t, &all, "gone");
        assert_eq!(visible_panes(&t, &all), ["a", "c"]);
    }

    #[test]
    fn fr_l18_removed_members_drop_out_of_a_saved_tab() {
        let t = PaneTab { name: "탭".into(), members: Some(strings(&["a", "x", "b"])) };
        assert_eq!(visible_panes(&t, &strings(&["a", "b"])), ["a", "b"]);
    }

    #[test]
    fn fr_l18_reordering_an_automatic_tab_saves_the_order() {
        let all = strings(&["a", "b", "c"]);
        let mut t = PaneTab::all("탭");
        assert!(reorder_pane(&mut t, &all, "c", PaneMove::Left));
        assert_eq!(visible_panes(&t, &all), ["a", "c", "b"]);
    }

    #[test]
    fn fr_l18_tabs_add_select_remove_and_always_keep_one() {
        let mut p = ProjectPanes::default();
        assert_eq!(p.tabs.len(), 1);
        assert_eq!(p.active, 0);
        p.add_tab(&strings(&["a"]));
        assert_eq!((p.tabs.len(), p.active), (2, 1), "a new tab is selected");
        assert_eq!(p.tabs[1].members, Some(strings(&["a"])), "it saves the panes shown now");
        p.select(9);
        assert_eq!(p.active, 1, "out of range is ignored");
        p.select(0);
        p.remove_tab(0);
        assert_eq!(p.tabs.len(), 1);
        p.remove_tab(0);
        assert_eq!(p.tabs.len(), 1, "the last tab stays");
        assert!(p.active < p.tabs.len());
    }

    #[test]
    fn fr_l18_removing_the_active_tab_selects_a_valid_one() {
        let mut p = ProjectPanes::default();
        p.add_tab(&[]);
        p.add_tab(&[]);
        p.select(2);
        p.remove_tab(2);
        assert_eq!(p.active, 1);
        assert_eq!(p.active_tab().name, p.tabs[1].name);
    }

    #[test]
    fn fr_l18_tab_names_are_numbered_and_unique() {
        let mut p = ProjectPanes::default();
        p.add_tab(&[]);
        p.add_tab(&[]);
        let mut names: Vec<_> = p.tabs.iter().map(|t| t.name.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 3);
    }

    // ---- FR-L5: leases of the panes ----------------------------------------------------------

    #[test]
    fn fr_l5_panes_open_what_is_missing_and_close_only_their_own() {
        let mine: std::collections::HashSet<String> = strings(&["old", "kept"]).into_iter().collect();
        let wanted = strings(&["kept", "new", "focus"]);
        let (open, close) = lease_plan(&wanted, |id| id == "kept" || id == "focus", &mine, Some("focus"));
        assert_eq!(open, ["new"]);
        assert_eq!(close, ["old"]);
        // The route's own session is never closed by a pane.
        let mine2: std::collections::HashSet<String> = strings(&["focus"]).into_iter().collect();
        let (_, close2) = lease_plan(&[], |_| true, &mine2, Some("focus"));
        assert!(close2.is_empty());
    }

    // ---- FR-L5, FR-L6, FR-L15, FR-L16: composer chips and the pane header -------------------

    #[test]
    fn fr_l15_model_chip_reads_harness_and_model() {
        assert_eq!(model_chip_label("claude-code", Some("opus")), "claude-code · opus");
        assert_eq!(model_chip_label("codex", None), "codex · 기본 모델");
        assert_eq!(model_chip_label("codex", Some("")), "codex · 기본 모델");
    }

    #[test]
    fn fr_l16_mode_chip_follows_approve_everything() {
        assert_eq!(mode_chip_label(false), "승인 요청");
        assert_eq!(mode_chip_label(true), "모두 승인");
        assert!(mode_chip_warns(true) && !mode_chip_warns(false), "bypass is drawn in the warning colour");
    }

    #[test]
    fn fr_l18_pane_toggle_has_conversation_and_terminal() {
        let l: Vec<_> = PaneToggle::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(l, ["대화", "터미널"]);
    }

    #[test]
    fn fr_l5_right_panel_has_four_cards_and_only_environment_has_data() {
        let l: Vec<_> = InfoCard::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(l, ["환경", "컨텍스트", "결과물", "변경 사항"]);
        assert!(InfoCard::Environment.ready());
        assert!(!InfoCard::Context.ready() && !InfoCard::Outputs.ready() && !InfoCard::Changes.ready());
    }

    #[test]
    fn fr_l5_environment_rows_come_from_the_record() {
        let rec: ember_client::wire::SessionRecord = serde_json::from_value(serde_json::json!({
            "id": "s1", "project": "ember", "agent": "codex", "cwd": "/w/ember",
            "model": "o3", "status": "idle", "title": "t",
            "created_at": 1, "updated_at": 2, "last_seq": 3
        }))
        .unwrap();
        let rows = environment_rows(&rec, Some("mac-mini"), true);
        let get = |k: &str| rows.iter().find(|(a, _)| *a == k).map(|(_, v)| v.clone());
        assert_eq!(get("컴퓨터").as_deref(), Some("mac-mini"));
        assert_eq!(get("하네스").as_deref(), Some("codex"));
        assert_eq!(get("모델").as_deref(), Some("o3"));
        assert_eq!(get("권한 모드").as_deref(), Some("모두 승인"));
        assert_eq!(get("작업 폴더").as_deref(), Some("/w/ember"));
        assert_eq!(get("저장소").as_deref(), Some("준비 중입니다"));
        let none = environment_rows(&rec, None, false);
        assert_eq!(none.iter().find(|(a, _)| *a == "컴퓨터").unwrap().1, "확인하는 중입니다");
    }

    // ---- NFR-L4 ------------------------------------------------------------------------------

    #[test]
    fn nfr_l4_session_text_has_no_plain_da_endings() {
        for s in UI_STRINGS {
            let t = s.trim();
            assert!(
                !t.ends_with('다') || t.ends_with("입니다") || t.ends_with("합니다") || t.ends_with("됩니다"),
                "{t}"
            );
            assert!(!t.contains('\u{2014}'), "no em-dash: {t}");
        }
        assert!(UI_STRINGS.contains(&"준비 중입니다"));
    }
}
