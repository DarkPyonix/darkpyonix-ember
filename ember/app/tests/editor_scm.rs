//! Integration tests for Editor SCM parity (FR-E10, Issue #444).
//!
//! Covers:
//! - Left-stripe switching (Project, Commit, Git)
//! - Staged vs working tree change grouping and badge mapping
//! - Line diff computation (compute_line_diffs and diff_gutter_decorations)
//! - End-to-end local git repository workflow (status, diff gutter, staging, commit, log)

use compose_rust::{ColorRole, GutterDecorationKind};
use ember_app::ui::editor::{
    compute_line_diffs, diff_gutter_decorations, first_changed_line_in_repo, first_diff_line,
    git_action_local, git_log_local, git_show_head_local, git_status_local,
    parse_git_status_porcelain_v1, scm_paths, Side, Stripes, ToolWindow,
};
use serde_json::json;

#[test]
fn fr_e10_left_stripes_switch_between_project_commit_and_git() {
    let mut stripes = Stripes::default();
    assert_eq!(stripes.selected(Side::Left), Some(ToolWindow::Project));

    // Toggle to Commit
    stripes.toggle(ToolWindow::Commit);
    assert_eq!(stripes.selected(Side::Left), Some(ToolWindow::Commit));
    assert!(stripes.is_selected(ToolWindow::Commit));

    // Toggle to Git
    stripes.toggle(ToolWindow::Git);
    assert_eq!(stripes.selected(Side::Left), Some(ToolWindow::Git));
    assert!(stripes.is_selected(ToolWindow::Git));

    // Toggle Git off: hides left panel
    stripes.toggle(ToolWindow::Git);
    assert_eq!(stripes.selected(Side::Left), None);

    // Explicit hide
    stripes.toggle(ToolWindow::Project);
    assert_eq!(stripes.selected(Side::Left), Some(ToolWindow::Project));
    stripes.hide(Side::Left);
    assert_eq!(stripes.selected(Side::Left), None);
}

#[test]
fn fr_e10_staged_vs_worktree_grouping_and_badge_mapping() {
    let porcelain = "\
## feat/editor-scm...origin/feat/editor-scm [ahead 3, behind 1]
MM src/both.rs
M  src/staged_only.rs
 M src/worktree_only.rs
A  src/added.rs
 D src/deleted_in_worktree.rs
D  src/deleted_in_index.rs
R  src/old.rs -> src/renamed.rs
?? untracked.txt
";

    let status = parse_git_status_porcelain_v1(porcelain);
    assert_eq!(status.branch, "feat/editor-scm");
    assert_eq!(status.upstream.as_deref(), Some("origin/feat/editor-scm"));
    assert_eq!(status.ahead, 3);
    assert_eq!(status.behind, 1);
    assert_eq!(status.files.len(), 8);

    // Staged files (index is not '.', ' ', or '?')
    let staged = scm_paths(&status, true);
    assert_eq!(
        staged,
        vec![
            "src/both.rs",
            "src/staged_only.rs",
            "src/added.rs",
            "src/deleted_in_index.rs",
            "src/renamed.rs"
        ]
    );
    assert!(staged.contains(&"src/both.rs"));
    assert!(staged.contains(&"src/staged_only.rs"));
    assert!(staged.contains(&"src/added.rs"));
    assert!(staged.contains(&"src/deleted_in_index.rs"));
    assert!(staged.contains(&"src/renamed.rs"));
    assert!(!staged.contains(&"src/worktree_only.rs"));
    assert!(!staged.contains(&"untracked.txt"));

    // Working tree files (worktree is not '.' or ' ')
    let worktree = scm_paths(&status, false);
    assert!(worktree.contains(&"src/both.rs"));
    assert!(worktree.contains(&"src/worktree_only.rs"));
    assert!(worktree.contains(&"src/deleted_in_worktree.rs"));
    assert!(worktree.contains(&"untracked.txt"));
    assert!(!worktree.contains(&"src/staged_only.rs"));
    assert!(!worktree.contains(&"src/added.rs"));
}

#[test]
fn fr_e10_compute_line_diffs_and_gutter_decorations() {
    let head = "fn main() {\n    println!(\"one\");\n    println!(\"two\");\n    println!(\"three\");\n}\n";
    // Line 1 modified ("one" -> "one modified")
    // Line 2 deleted ("two" removed)
    // Line after "three" added
    let current = "fn main() {\n    println!(\"one modified\");\n    println!(\"three\");\n    println!(\"four added\");\n}\n";

    let markers = compute_line_diffs(head, current);
    assert!(!markers.is_empty());
    assert_eq!(first_diff_line(head, current), Some(1));

    // Check diff gutter decorations conversion
    let version = 5;
    let gutter_decs = diff_gutter_decorations(version, &markers);
    assert!(!gutter_decs.is_empty());

    let decs = gutter_decs.decorations();
    assert_eq!(decs.len(), markers.len());
    for d in &decs {
        assert_eq!(d.version, version);
        assert!(matches!(
            d.color,
            Some(ColorRole::DiffAdded | ColorRole::DiffModified | ColorRole::DiffRemoved)
        ));
    }

    // Verify individual kinds
    assert!(decs.iter().any(|d| d.color == Some(ColorRole::DiffModified) && d.kind == GutterDecorationKind::Bar));
    assert!(decs.iter().any(|d| d.color == Some(ColorRole::DiffAdded) && d.kind == GutterDecorationKind::Bar));
}

#[test]
fn fr_e10_end_to_end_local_git_repo_workflow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();

    // 1. git init and identity config
    let init_out = std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo_dir)
        .output()
        .expect("git init");
    assert!(init_out.status.success());

    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "Ember Test"])
        .current_dir(repo_dir)
        .output();
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "test@darkpyonix.dev"])
        .current_dir(repo_dir)
        .output();

    // 2. Initial commit
    std::fs::create_dir_all(repo_dir.join("src")).expect("mkdir src");
    let file_path = repo_dir.join("src/lib.rs");
    let initial_content = "pub fn greet() -> &'static str {\n    \"hello\"\n}\n";
    std::fs::write(&file_path, initial_content).expect("write initial file");

    let stage_res = git_action_local(repo_dir, "stage", &json!({"paths": ["src/lib.rs"]}));
    assert!(stage_res.is_ok());

    let commit_res = git_action_local(repo_dir, "commit", &json!({"summary": "Initial commit"}));
    assert!(commit_res.is_ok());

    // Verify repo is clean after commit
    let st0 = git_status_local(repo_dir).expect("status clean");
    assert!(st0.files.is_empty());
    let head_text = git_show_head_local(repo_dir, "src/lib.rs").expect("head text");
    assert_eq!(head_text, initial_content);

    let clean_markers = compute_line_diffs(&head_text, initial_content);
    assert!(clean_markers.is_empty());
    let clean_decs = diff_gutter_decorations(1, &clean_markers);
    assert!(clean_decs.is_empty());

    // 3. Modify the file in working tree
    let modified_content = "pub fn greet() -> &'static str {\n    \"hello ember\"\n}\n\npub fn helper() -> i32 {\n    42\n}\n";
    std::fs::write(&file_path, modified_content).expect("write modified file");

    let st1 = git_status_local(repo_dir).expect("status modified");
    assert_eq!(st1.files.len(), 1);
    assert_eq!(st1.files[0].path, "src/lib.rs");
    assert_eq!(st1.files[0].worktree, 'M');
    assert_eq!(st1.files[0].index, ' ');

    // 4. Verify diff against HEAD detects modified and added lines
    let head_text1 = git_show_head_local(repo_dir, "src/lib.rs").expect("head text");
    let markers1 = compute_line_diffs(&head_text1, modified_content);
    assert!(!markers1.is_empty());
    assert_eq!(first_diff_line(&head_text1, modified_content), Some(1));
    assert_eq!(first_changed_line_in_repo(repo_dir, "src/lib.rs"), Some(1));

    let gutter_decs1 = diff_gutter_decorations(2, &markers1);
    assert!(!gutter_decs1.is_empty());
    let decs1 = gutter_decs1.decorations();
    assert!(decs1.iter().any(|d| d.color == Some(ColorRole::DiffModified) && d.line == 1));
    assert!(decs1.iter().any(|d| d.color == Some(ColorRole::DiffAdded)));

    // 5. Stage the modified file
    let stage_res2 = git_action_local(repo_dir, "stage", &json!({"paths": ["src/lib.rs"]}));
    assert!(stage_res2.is_ok());

    let st2 = git_status_local(repo_dir).expect("status staged");
    assert_eq!(st2.files.len(), 1);
    assert_eq!(st2.files[0].index, 'M');
    assert_eq!(scm_paths(&st2, true), vec!["src/lib.rs"]);

    // 6. Commit the staged change
    let commit_res2 = git_action_local(repo_dir, "commit", &json!({"summary": "Add helper function"}));
    assert!(commit_res2.is_ok());

    // 7. Verify working tree is clean and diff gutter clears after commit
    let st3 = git_status_local(repo_dir).expect("status clean again");
    assert!(st3.files.is_empty());

    let new_head = git_show_head_local(repo_dir, "src/lib.rs").expect("new head");
    assert_eq!(new_head, modified_content);

    let markers3 = compute_line_diffs(&new_head, modified_content);
    assert!(markers3.is_empty());
    let gutter_decs3 = diff_gutter_decorations(3, &markers3);
    assert!(gutter_decs3.is_empty());

    // 8. Verify history log
    let commits = git_log_local(repo_dir, 10).expect("git log");
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "Add helper function");
    assert_eq!(commits[1].subject, "Initial commit");
}
