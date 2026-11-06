//! The git wrappers (SPEC FR-G4) against a stub of `/api/projects/{id}/git/<op>` that records
//! each body and answers the contract's shapes.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use ember_client::api::Api;
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

async fn handle(
    State(seen): State<Seen>,
    Path((project, op)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    seen.lock().unwrap().push((project.clone(), op.clone(), body.clone()));
    if project == "plain" {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_a_repo" })));
    }
    match op.as_str() {
        "status" => (
            StatusCode::OK,
            Json(json!({
                "branch": "feat/x", "upstream": "origin/feat/x", "ahead": 2, "behind": 1,
                "fetched_age_secs": 90, "branches": ["main", "feat/x"],
                "files": [
                    { "path": "a.rs", "old_path": null, "index": "M", "worktree": "." },
                    { "path": "new.rs", "old_path": "old.rs", "index": "R", "worktree": "M" },
                    { "path": "u.txt", "index": "?", "worktree": "?" }
                ]
            })),
        ),
        "diff" => (StatusCode::OK, Json(json!({ "patch": "diff --git a/a.rs b/a.rs\n" }))),
        "commit" => (StatusCode::OK, Json(json!({ "sha": "abc123" }))),
        "fetch" | "pull" | "push" => (StatusCode::OK, Json(json!({ "output": format!("{op} ok") }))),
        "conflicts" => (StatusCode::OK, Json(json!({ "paths": ["c.rs"] }))),
        "log" => (StatusCode::OK, Json(json!({"commits": [{"sha":"abc", "parents":["def"], "author":"A", "email":"a@example.com", "time":123, "subject":"subject", "refs":[{"name":"HEAD", "kind":"head"}]}]}))),
        "branches" => (StatusCode::OK, Json(json!({"branches": [
            {"name":"feat/x", "is_current":true, "is_default":false, "upstream":"origin/feat/x", "ahead":2, "behind":1,
             "last_sha":"abc1234", "last_subject":"Fix: x", "last_time":"2026-11-05T10:00:00+09:00", "worktree":"/w/ember"},
            {"name":"main", "is_current":false, "is_default":true}]}))),
        "prs" if body["state"] == "closed" => (StatusCode::CONFLICT, Json(json!({"error": "GitHub에 로그인되어 있지 않습니다.\ngh auth login"}))),
        "prs" => (StatusCode::OK, Json(json!({"prs": [{
            "number": 7, "title": "Add the pull request list", "state": "open", "draft": false, "author": "octo",
            "head": "feat/prs", "base": "develop", "updated_at": "2026-11-05T10:00:00Z", "review": "changes_requested",
            "checks": {"passed": 3, "failed": 1, "pending": 0},
            "merge": {"can_merge": false, "reason": "CI 체크가 실패했습니다 (실패 1개)."},
            "url": "https://github.com/o/r/pull/7"}]}))),
        "pr" => (StatusCode::OK, Json(json!({
            "number": 7, "title": "Add the pull request list", "state": "open", "draft": false, "author": "octo",
            "head": "feat/prs", "base": "develop", "updated_at": "2026-11-05T10:00:00Z", "review": "approved",
            "checks": {"passed": 1, "failed": 1, "pending": 1},
            "merge": {"can_merge": false, "reason": "CI 체크가 실패했습니다 (실패 1개)."},
            "url": "https://github.com/o/r/pull/7", "body": "text",
            "check_runs": [{"name":"build", "state":"passed", "detail_url":"https://ci/build"}, {"name":"e2e", "state":"pending"}],
            "reviews": [{"author":"a", "state":"changes_requested", "body":"fix", "submitted_at":"2026-11-05T09:00:00Z"}],
            "comments": [{"author":"b", "body":"hi", "created_at":"2026-11-05T09:30:00Z"}],
            "requested_reviewers": ["rev1"], "additions": 10, "deletions": 3, "changed_files": 2}))),
        "pr/review" => (StatusCode::OK, Json(json!({"number": body["number"], "action": body["action"]}))),
        "pr/merge" if body["number"] == 9 => (StatusCode::CONFLICT, Json(json!({"error": "CI 체크가 실패했습니다 (실패 1개). 통과해야 병합할 수 있습니다."}))),
        "pr/merge" => (StatusCode::OK, Json(json!({"number": body["number"], "merged": true, "method": body["method"]}))),
        "pr/create" => (StatusCode::OK, Json(json!({"number": 42, "url": "https://github.com/o/r/pull/42"}))),
        "stash/list" => (StatusCode::OK, Json(json!([{"index":0, "message":"work", "branch":"main", "time":"2026-01-01"}]))),
        "conflict" => (StatusCode::OK, Json(json!({"blocks":[{"kind":"common", "text":"before"}, {"kind":"conflict", "ours":"a", "theirs":"b", "ours_label":"HEAD", "theirs_label":"other"}]}))),
        "stash/push" | "stash/apply" | "stash/pop" | "stash/drop" | "resolve" => (StatusCode::NO_CONTENT, Json(Value::Null)),
        "stage" | "unstage" | "apply" | "checkout" | "branch" => (StatusCode::NO_CONTENT, Json(Value::Null)),
        _ => (StatusCode::NOT_FOUND, Json(json!({ "error": "unknown" }))),
    }
}

async fn start() -> (Api, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route("/api/projects/{id}/git/{*op}", post(handle))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Api::new(&format!("http://{addr}")).unwrap(), seen)
}

fn last(seen: &Seen) -> (String, String, Value) {
    seen.lock().unwrap().last().cloned().unwrap()
}

#[tokio::test]
async fn status_decodes_branch_remote_state_and_files() {
    let (api, seen) = start().await;
    let st = api.git_status("ember").await.unwrap();
    assert_eq!(last(&seen).0, "ember");
    assert_eq!(last(&seen).1, "status");
    assert_eq!(st.branch, "feat/x");
    assert_eq!(st.upstream.as_deref(), Some("origin/feat/x"));
    assert_eq!((st.ahead, st.behind, st.fetched_age_secs), (2, 1, Some(90)));
    assert_eq!(st.branches, vec!["main", "feat/x"]);
    assert_eq!(st.files.len(), 3);
    assert_eq!((st.files[0].index, st.files[0].worktree), ('M', '.'));
    assert_eq!(st.files[1].old_path.as_deref(), Some("old.rs"));
    assert_eq!((st.files[2].index, st.files[2].worktree), ('?', '?'));
    assert_eq!(st.files[2].old_path, None);
}

#[tokio::test]
async fn a_project_that_is_not_a_repository_answers_404() {
    let (api, _) = start().await;
    let err = api.git_status("plain").await.unwrap_err();
    assert_eq!(err.status(), Some(404));
    assert!(err.to_string().contains("not_a_repo"), "{err}");
}

#[tokio::test]
async fn bodies_match_the_contract() {
    let (api, seen) = start().await;

    let d = api.git_diff("ember", Some("a.rs"), true).await.unwrap();
    assert!(d.patch.starts_with("diff --git"));
    assert_eq!(last(&seen).2, json!({ "path": "a.rs", "staged": true }));
    api.git_diff("ember", None, false).await.unwrap();
    assert_eq!(last(&seen).2, json!({ "path": null, "staged": false }));

    // One patch for the work tree with the untracked files in it.
    api.git_diff_untracked("ember").await.unwrap();
    assert_eq!(last(&seen).1, "diff");
    assert_eq!(last(&seen).2, json!({ "path": null, "staged": false, "include_untracked": true }));

    api.git_stage("ember", &["a.rs".to_string(), "b.rs".to_string()]).await.unwrap();
    assert_eq!(last(&seen).1, "stage");
    assert_eq!(last(&seen).2, json!({ "paths": ["a.rs", "b.rs"] }));
    api.git_unstage("ember", &["a.rs".to_string()]).await.unwrap();
    assert_eq!(last(&seen).1, "unstage");

    api.git_apply("ember", "@@ patch", true).await.unwrap();
    assert_eq!(last(&seen).2, json!({ "patch": "@@ patch", "reverse": true }));

    let c = api.git_commit("ember", "Fix: x", Some("why"), false).await.unwrap();
    assert_eq!(c.sha, "abc123");
    assert_eq!(last(&seen).2, json!({ "summary": "Fix: x", "description": "why", "amend": false }));

    api.git_checkout("ember", "main").await.unwrap();
    assert_eq!(last(&seen).2, json!({ "branch": "main" }));
    api.git_branch("ember", "feat/y", None).await.unwrap();
    assert_eq!(last(&seen).2, json!({ "name": "feat/y", "from": null }));
}

#[tokio::test]
async fn remote_operations_return_git_output() {
    let (api, seen) = start().await;
    assert_eq!(api.git_fetch("ember", None).await.unwrap().output, "fetch ok");
    assert_eq!(api.git_pull("ember", Some("origin")).await.unwrap().output, "pull ok");
    assert_eq!(last(&seen).2, json!({ "remote": "origin" }));
    assert_eq!(api.git_push("ember", None).await.unwrap().output, "push ok");
    assert_eq!(api.git_conflicts("ember").await.unwrap().paths, vec!["c.rs"]);
}

#[tokio::test]
async fn fr_g4_history_stash_and_conflict_calls_match_node_contract() {
    use ember_client::api::{GitConflictBlock, GitLogRefKind};
    let (api, seen) = start().await;
    let log = api.git_log("ember", 25, 3, Some("main")).await.unwrap();
    assert_eq!(last(&seen).2, json!({"limit":25, "skip":3, "branch":"main"}));
    assert_eq!(log.commits[0].parents, vec!["def"]);
    assert_eq!(log.commits[0].refs[0].kind, GitLogRefKind::Head);
    let stash = api.git_stash_list("ember").await.unwrap();
    assert_eq!(last(&seen).1, "stash/list");
    assert_eq!((stash[0].index, stash[0].message.as_str(), stash[0].branch.as_str(), stash[0].time.as_str()), (0, "work", "main", "2026-01-01"));
    api.git_stash_push("ember", Some("save"), true).await.unwrap();
    assert_eq!(last(&seen).2, json!({"message":"save", "include_untracked":true}));
    api.git_stash_apply("ember", 2).await.unwrap();
    assert_eq!((last(&seen).1, last(&seen).2), ("stash/apply".into(), json!({"index":2})));
    api.git_stash_pop("ember", 1).await.unwrap();
    assert_eq!((last(&seen).1, last(&seen).2), ("stash/pop".into(), json!({"index":1})));
    api.git_stash_drop("ember", 0).await.unwrap();
    assert_eq!((last(&seen).1, last(&seen).2), ("stash/drop".into(), json!({"index":0})));
    let conflict = api.git_conflict("ember", "a.txt").await.unwrap();
    assert_eq!(last(&seen).2, json!({"path":"a.txt"}));
    assert!(matches!(&conflict.blocks[0], GitConflictBlock::Common { text } if text == "before"));
    assert!(matches!(&conflict.blocks[1], GitConflictBlock::Conflict { ours, theirs, ours_label, theirs_label } if ours == "a" && theirs == "b" && ours_label == "HEAD" && theirs_label == "other"));
    api.git_resolve("ember", "a.txt", "resolved").await.unwrap();
    assert_eq!(last(&seen).2, json!({"path":"a.txt", "content":"resolved"}));
}

#[tokio::test]
async fn fr_g8_prs_call_decodes_the_nodes_shape_and_carries_gh_errors() {
    use ember_client::api::{GitPrReview, GitPrState};
    let (api, seen) = start().await;
    let prs = api.git_prs("ember", "open", 30).await.unwrap();
    assert_eq!((last(&seen).1, last(&seen).2), ("prs".into(), json!({"state":"open", "limit":30})));
    let pr = &prs.prs[0];
    assert_eq!((pr.number, pr.state, pr.draft), (7, GitPrState::Open, false));
    assert_eq!((pr.author.as_str(), pr.head.as_str(), pr.base.as_str()), ("octo", "feat/prs", "develop"));
    assert_eq!(pr.review, GitPrReview::ChangesRequested);
    assert_eq!((pr.checks.passed, pr.checks.failed, pr.checks.pending), (3, 1, 0));
    assert!(!pr.merge.can_merge && pr.merge.reason.contains("CI"));
    let err = api.git_prs("ember", "closed", 30).await.unwrap_err();
    assert!(err.to_string().contains("gh auth login"), "{err}");
}

#[tokio::test]
async fn fr_g9_branches_call_decodes_the_nodes_shape_with_defaults() {
    let (api, seen) = start().await;
    let list = api.git_branches("ember").await.unwrap();
    assert_eq!((last(&seen).1, last(&seen).2), ("branches".into(), json!({})));
    let a = &list.branches[0];
    assert_eq!((a.name.as_str(), a.is_current, a.is_default), ("feat/x", true, false));
    assert_eq!((a.upstream.as_deref(), a.ahead, a.behind), (Some("origin/feat/x"), 2, 1));
    assert_eq!((a.last_sha.as_str(), a.last_subject.as_str()), ("abc1234", "Fix: x"));
    assert_eq!((a.last_time.as_deref(), a.worktree.as_deref()), (Some("2026-11-05T10:00:00+09:00"), Some("/w/ember")));
    let m = &list.branches[1];
    assert!(m.is_default && m.upstream.is_none() && m.worktree.is_none() && (m.ahead, m.behind) == (0, 0));
}

#[tokio::test]
async fn fr_g10_pr_detail_call_decodes_the_nodes_shape() {
    use ember_client::api::{GitPrCheckState, GitPrReviewState};
    let (api, seen) = start().await;
    let d = api.git_pr("ember", 7).await.unwrap();
    assert_eq!((last(&seen).1, last(&seen).2), ("pr".into(), json!({"number":7})));
    assert_eq!((d.pr.number, d.pr.head.as_str(), d.body.as_str()), (7, "feat/prs", "text"));
    assert_eq!((d.pr.checks.passed, d.pr.checks.failed, d.pr.checks.pending), (1, 1, 1));
    assert_eq!(d.check_runs[0].state, GitPrCheckState::Passed);
    assert_eq!(d.check_runs[0].detail_url.as_deref(), Some("https://ci/build"));
    assert_eq!((d.check_runs[1].state, d.check_runs[1].detail_url.clone()), (GitPrCheckState::Pending, None));
    assert_eq!((d.reviews[0].state, d.reviews[0].author.as_str()), (GitPrReviewState::ChangesRequested, "a"));
    assert_eq!(d.comments[0].body, "hi");
    assert_eq!(d.requested_reviewers, vec!["rev1"]);
    assert_eq!((d.additions, d.deletions, d.changed_files), (10, 3, 2));
}

#[tokio::test]
async fn fr_g11_pr_review_and_merge_calls_send_confirm_and_carry_the_gate_reason() {
    use ember_client::api::{GitPrMergeMethod, GitPrReviewAction};
    let (api, seen) = start().await;
    let r = api.git_pr_review("ember", 7, GitPrReviewAction::RequestChanges, Some("fix")).await.unwrap();
    assert_eq!(r.action, GitPrReviewAction::RequestChanges);
    assert_eq!((last(&seen).1, last(&seen).2), ("pr/review".into(), json!({"number":7, "action":"request_changes", "body":"fix", "confirm":true})));
    let m = api.git_pr_merge("ember", 7, GitPrMergeMethod::Squash, true).await.unwrap();
    assert!(m.merged && m.method == GitPrMergeMethod::Squash);
    assert_eq!(last(&seen).2, json!({"number":7, "method":"squash", "delete_branch":true, "confirm":true}));
    let err = api.git_pr_merge("ember", 9, GitPrMergeMethod::Merge, false).await.unwrap_err();
    assert!(err.to_string().contains("CI"), "{err}");
}

#[tokio::test]
async fn fr_g12_pr_create_call_sends_the_fields_and_decodes_number_and_url() {
    let (api, seen) = start().await;
    let c = api.git_pr_create("ember", "Add x", Some("why"), Some("develop"), Some("feat/x"), true).await.unwrap();
    assert_eq!((c.number, c.url.as_str()), (42, "https://github.com/o/r/pull/42"));
    assert_eq!((last(&seen).1, last(&seen).2), ("pr/create".into(), json!({"title":"Add x", "body":"why", "base":"develop", "head":"feat/x", "draft":true, "confirm":true})));
}
