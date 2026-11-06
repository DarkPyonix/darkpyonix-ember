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

// FR-G13: the GitHub card's summary, with null numbers and defaults.
#[tokio::test]
async fn fr_g13_github_summary_decodes_null_numbers_and_defaults() {
    use ember_client::api::GithubState;
    async fn handler() -> Json<Value> {
        Json(json!({
            "state": "warn", "reason": "gh에 로그인되어 있지 않습니다 (터미널에서 gh auth login)", "account": null,
            "repos": [
                {"name_with_owner": "o/r", "default_branch": "main", "open_prs": null, "review_requested": null,
                 "capped": true, "ci": ["success", "failure", "cancelled", "pending"], "error": "GitHub에 연결하지 못했습니다.", "projects": ["a"]},
                {"name_with_owner": null}
            ],
            "totals": {"repos": 2, "open_prs": null, "review_requested": 3, "ci_pass_percent": null},
            "checked_at": 1760000000
        }))
    }
    let app = Router::new().route("/api/integrations/github", axum::routing::get(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = Api::new(&format!("http://{addr}")).unwrap();
    let g = api.github_summary().await.unwrap();
    assert_eq!(g.state, GithubState::Warn);
    assert!(g.reason.as_deref().unwrap().contains("gh auth login"));
    assert_eq!(g.account, None);
    assert_eq!(g.repos.len(), 2);
    assert_eq!(g.repos[0].open_prs, None);
    assert!(g.repos[0].capped);
    assert_eq!(g.repos[0].ci.as_ref().unwrap().len(), 4);
    assert_eq!(g.repos[1].ci, None);
    assert_eq!(g.totals.open_prs, None);
    assert_eq!(g.totals.review_requested, Some(3));
    assert_eq!(g.totals.ci_pass_percent, None);
    assert_eq!(g.checked_at, 1760000000);
}
