//! FR-G10 to FR-G12 over HTTP: pull request detail, review, merge and create, with temp
//! repositories under `.scratch/` and a fake `gh` script. The script appends one tab-separated
//! line per call to `.gh-calls` in the repository it runs in, answers `pr view` from `.gh-json`
//! `pr create` from `.gh-create` and `pr list` from `.gh-list` (default `[]`), and fails with `.gh-fail` when that file exists.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

mod common;

use ember_node::{api::{self, Node}, config::NodeConfig};
use serde_json::{json, Value};

fn scratch() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-pr-ops-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn install_fake_gh() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let script = scratch().join("fake-gh.sh");
        common::write_exec_ready(&script, "#!/bin/sh\n{ printf CALL; for a in \"$@\"; do printf '\\t%s' \"$a\"; done; printf '\\n'; } >> .gh-calls\nif [ -f .gh-fail ]; then cat .gh-fail >&2; exit 1; fi\ncase \"$2\" in view) cat .gh-json;; create) cat .gh-create;; list) if [ -f .gh-list ]; then cat .gh-list; else echo '[]'; fi;; esac\n");
        std::env::set_var("EMBER_GH_BIN", &script);
        std::env::set_var("GIT_CEILING_DIRECTORIES", scratch());
    });
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

struct Fixture { _dir: tempfile::TempDir, repo: PathBuf, base: String, client: reqwest::Client }

async fn setup() -> Fixture {
    install_fake_gh();
    let dir = tempfile::Builder::new().prefix("o").tempdir_in(scratch()).unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let node = Node::new(NodeConfig::new("token", vec![repo.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/git/pr", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fixture { _dir: dir, repo, base, client: reqwest::Client::new() }
}

impl Fixture {
    /// `op` is "" for the detail, else "review", "merge" or "create".
    async fn post(&self, op: &str, mut body: Value) -> (u16, Value) {
        body["repo"] = json!(self.repo);
        let url = if op.is_empty() { self.base.clone() } else { format!("{}/{op}", self.base) };
        let response = self.client.post(&url).bearer_auth("token").json(&body).send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap())
    }

    /// Every gh call so far, as its argument list.
    fn calls(&self) -> Vec<Vec<String>> {
        match std::fs::read_to_string(self.repo.join(".gh-calls")) {
            Ok(text) => text.lines().map(|l| l.split('\t').skip(1).map(str::to_owned).collect()).collect(),
            Err(_) => vec![],
        }
    }

    fn set_pr(&self, pr: Value) { std::fs::write(self.repo.join(".gh-json"), pr.to_string()).unwrap(); }
}

fn run(conclusion: &str, name: &str) -> Value {
    json!({"__typename":"CheckRun", "status":"COMPLETED", "conclusion":conclusion, "name":name, "detailsUrl":format!("https://ci/{name}")})
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn detail(review: &str, checks: Vec<Value>) -> Value {
    json!({
        "number": 7, "title": "change 7", "body": "the body", "state": "OPEN", "isDraft": false,
        "author": {"login": "octo"}, "headRefName": "feat/7", "headRefOid": HEAD, "baseRefName": "develop",
        "url": "https://github.com/o/r/pull/7", "reviewDecision": review, "mergeable": "MERGEABLE",
        "updatedAt": "2026-11-05T10:00:00Z", "statusCheckRollup": checks,
        "reviews": [], "comments": [], "reviewRequests": [],
        "additions": 10, "deletions": 3, "changedFiles": 2
    })
}

#[tokio::test]
async fn fr_g10_detail_maps_checks_reviews_and_counts() {
    let f = setup().await;
    let mut pr = detail("APPROVED", vec![
        run("SUCCESS", "build"),
        run("FAILURE", "lint"),
        json!({"status":"IN_PROGRESS", "conclusion":"", "name":"e2e"}),
        json!({"__typename":"StatusContext", "context":"legacy/ci", "state":"SUCCESS", "targetUrl":"https://legacy/ci"}),
    ]);
    pr["reviews"] = json!([
        {"author":{"login":"a"}, "state":"APPROVED", "body":"ok", "submittedAt":"2026-11-05T09:00:00Z"},
        {"author":{"login":"b"}, "state":"CHANGES_REQUESTED", "body":"fix", "submittedAt":"2026-11-05T09:10:00Z"},
        {"author":{"login":"c"}, "state":"COMMENTED", "body":"", "submittedAt":"2026-11-05T09:20:00Z"},
        {"author":{"login":"d"}, "state":"DISMISSED", "body":"", "submittedAt":"2026-11-05T09:30:00Z"},
        {"author":{"login":"e"}, "state":"PENDING", "body":"", "submittedAt":null},
    ]);
    pr["reviewRequests"] = json!([{"login":"rev1"}, {"slug":"team-x"}]);
    f.set_pr(pr);
    let (status, body) = f.post("", json!({"number": 7})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["number"], 7);
    assert_eq!(body["title"], "change 7");
    assert_eq!(body["body"], "the body");
    assert_eq!(body["author"], "octo");
    assert_eq!(body["head"], "feat/7");
    assert_eq!(body["review"], "approved");
    assert_eq!(body["checks"], json!({"passed": 2, "failed": 1, "pending": 1}));
    assert_eq!(body["merge"]["can_merge"], false);
    assert_eq!(body["check_runs"], json!([
        {"name":"build", "state":"passed", "detail_url":"https://ci/build"},
        {"name":"lint", "state":"failed", "detail_url":"https://ci/lint"},
        {"name":"e2e", "state":"pending"},
        {"name":"legacy/ci", "state":"passed", "detail_url":"https://legacy/ci"},
    ]));
    let states: Vec<&str> = body["reviews"].as_array().unwrap().iter().map(|r| r["state"].as_str().unwrap()).collect();
    assert_eq!(states, ["approved", "changes_requested", "commented", "dismissed", "pending"]);
    assert_eq!(body["reviews"][0]["author"], "a");
    assert_eq!(body["reviews"][0]["submitted_at"], "2026-11-05T09:00:00Z");
    assert_eq!(body["requested_reviewers"], json!(["rev1", "team-x"]));
    assert_eq!((body["additions"].clone(), body["deletions"].clone(), body["changed_files"].clone()), (json!(10), json!(3), json!(2)));
    let calls = f.calls();
    assert_eq!(&calls[0][..3], ["pr", "view", "7"]);
    assert!(calls[0].iter().any(|a| a.starts_with("number,title,body,state,isDraft")));
}

#[tokio::test]
async fn fr_g10_long_comment_is_cut_and_comments_are_limited() {
    let f = setup().await;
    let mut pr = detail("", vec![]);
    let mut comments = vec![json!({"author":{"login":"long"}, "body":"x".repeat(5000), "createdAt":"2026-11-01T00:00:00Z"})];
    for i in 0..60 {
        comments.push(json!({"author":{"login":"u"}, "body":format!("c{i}"), "createdAt":format!("2026-11-02T00:00:{i:02}Z")}));
    }
    pr["comments"] = json!(comments);
    pr["reviews"] = json!([{"author":{"login":"r"}, "state":"COMMENTED", "body":"y".repeat(3000), "submittedAt":"2026-11-01T00:00:00Z"}]);
    f.set_pr(pr);
    let (status, body) = f.post("", json!({"number": 7})).await;
    assert_eq!(status, 200, "{body}");
    let comments = body["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 50, "at most 50");
    assert_eq!(comments[0]["body"], "c10", "the newest 50, oldest of them first");
    assert_eq!(comments[49]["body"], "c59", "newest last");
    let review = body["reviews"][0]["body"].as_str().unwrap();
    assert_eq!(review.chars().count(), 2003);
    assert!(review.starts_with(&"y".repeat(2000)) && review.ends_with("..."));

    // The 5000-char comment alone is cut at 2000 characters.
    let mut pr = detail("", vec![]);
    pr["comments"] = json!([{"author":{"login":"long"}, "body":"x".repeat(5000), "createdAt":"t"}]);
    f.set_pr(pr);
    let (_, body) = f.post("", json!({"number": 7})).await;
    let text = body["comments"][0]["body"].as_str().unwrap();
    assert_eq!(text.chars().count(), 2003);
    assert!(text.ends_with("..."));
}

#[tokio::test]
async fn fr_g10_number_range_and_gh_errors() {
    let f = setup().await;
    for n in [0u64, 1_000_000] {
        let (status, _) = f.post("", json!({"number": n})).await;
        assert_eq!(status, 400, "{n}");
    }
    assert!(f.calls().is_empty(), "gh is not run for a bad number");
    std::fs::write(f.repo.join(".gh-fail"), "To get started with GitHub CLI, please run:  gh auth login").unwrap();
    let (status, body) = f.post("", json!({"number": 7})).await;
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap().contains("로그인"));
}

#[tokio::test]
async fn fr_g11_review_approve_passes_the_flag() {
    let f = setup().await;
    let (status, body) = f.post("review", json!({"number": 7, "action": "approve", "confirm": true})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"number": 7, "action": "approve"}));
    assert_eq!(f.calls(), vec![vec!["pr", "review", "7", "--approve"]]);
}

#[tokio::test]
async fn fr_g11_review_request_changes_and_comment_pass_the_body_as_a_value() {
    let f = setup().await;
    let (status, _) = f.post("review", json!({"number": 7, "action": "request_changes", "body": "-fix this", "confirm": true})).await;
    assert_eq!(status, 200);
    let (status, _) = f.post("review", json!({"number": 8, "action": "comment", "body": "hello", "confirm": true})).await;
    assert_eq!(status, 200);
    assert_eq!(f.calls(), vec![
        vec!["pr", "review", "7", "--request-changes", "--body=-fix this"],
        vec!["pr", "review", "8", "--comment", "--body=hello"],
    ]);
}

#[tokio::test]
async fn fr_g11_review_needs_a_body_and_a_confirmation() {
    let f = setup().await;
    for action in ["request_changes", "comment"] {
        for body in [json!(null), json!(""), json!("   ")] {
            let (status, out) = f.post("review", json!({"number": 7, "action": action, "body": body, "confirm": true})).await;
            assert_eq!(status, 400, "{action} {body}");
            assert!(out["error"].as_str().unwrap().contains("내용"), "{out}");
        }
    }
    let (status, out) = f.post("review", json!({"number": 7, "action": "approve"})).await;
    assert_eq!(status, 400);
    assert!(out["error"].as_str().unwrap().contains("확인이 필요합니다"));
    let (status, _) = f.post("review", json!({"number": 7, "action": "approve", "confirm": false})).await;
    assert_eq!(status, 400);
    assert!(f.calls().is_empty(), "nothing reached gh");
}

#[tokio::test]
async fn fr_g11_merge_is_refused_with_the_gate_reason_when_a_check_failed() {
    let f = setup().await;
    f.set_pr(detail("APPROVED", vec![run("SUCCESS", "build"), run("FAILURE", "lint")]));
    let (status, body) = f.post("merge", json!({"number": 7, "method": "squash", "delete_branch": true, "head_sha": HEAD, "confirm": true})).await;
    assert_eq!(status, 409, "{body}");
    assert!(body["error"].as_str().unwrap().contains("CI"), "{body}");
    let calls = f.calls();
    assert_eq!(calls.len(), 1, "only the re-read: {calls:?}");
    assert_eq!(&calls[0][..2], ["pr", "view"]);
    assert!(!calls.iter().any(|c| c.get(1).map(String::as_str) == Some("merge")), "pr merge was not called");
}

#[tokio::test]
async fn fr_g11_merge_squash_with_branch_delete_when_allowed() {
    let f = setup().await;
    f.set_pr(detail("APPROVED", vec![run("SUCCESS", "build")]));
    let (status, body) = f.post("merge", json!({"number": 7, "method": "squash", "delete_branch": true, "head_sha": HEAD, "confirm": true})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"number": 7, "merged": true, "method": "squash"}));
    let calls = f.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1], vec!["pr", "merge", "7", "--squash", &format!("--match-head-commit={HEAD}"), "--delete-branch"]);
    for call in &calls {
        assert!(!call.iter().any(|a| a == "--admin" || a == "--auto"), "{call:?}");
    }
    // Without delete_branch, and with the other methods.
    let (status, _) = f.post("merge", json!({"number": 7, "method": "rebase", "delete_branch": false, "head_sha": HEAD, "confirm": true})).await;
    assert_eq!(status, 200);
    let (status, _) = f.post("merge", json!({"number": 7, "method": "merge", "delete_branch": false, "head_sha": HEAD, "confirm": true})).await;
    assert_eq!(status, 200);
    let calls = f.calls();
    assert_eq!(calls[3], vec!["pr", "merge", "7", "--rebase", &format!("--match-head-commit={HEAD}")]);
    assert_eq!(calls[5], vec!["pr", "merge", "7", "--merge", &format!("--match-head-commit={HEAD}")]);
}

#[tokio::test]
async fn fr_g11_merge_needs_a_confirmation_and_never_reads_without_it() {
    let f = setup().await;
    f.set_pr(detail("APPROVED", vec![]));
    let (status, body) = f.post("merge", json!({"number": 7, "method": "merge", "delete_branch": false})).await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().contains("확인이 필요합니다"));
    assert!(f.calls().is_empty());
}

#[tokio::test]
async fn fr_g12_create_passes_title_body_draft_and_branches() {
    let f = setup().await;
    std::fs::write(f.repo.join(".gh-create"), "\nCreating pull request for feat/x into develop\nhttps://github.com/o/r/pull/42\n").unwrap();
    let (status, body) = f.post("create", json!({
        "title": "Add x", "body": "why", "base": "develop", "head": "feat/x", "draft": true, "confirm": true
    })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"number": 42, "url": "https://github.com/o/r/pull/42"}));
    let calls = f.calls();
    assert_eq!(calls[0], vec!["pr", "list", "--head=feat/x", "--state", "open", "--json", "number"]);
    assert_eq!(calls[1], vec!["pr", "create", "--title=Add x", "--body=why", "--base=develop", "--head=feat/x", "--draft"]);
}

#[tokio::test]
async fn fr_g12_create_minimal_has_an_empty_body_and_no_optional_flags() {
    let f = setup().await;
    std::fs::write(f.repo.join(".gh-create"), "https://github.com/o/r/pull/9\n").unwrap();
    let (status, body) = f.post("create", json!({"title": "T", "confirm": true})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["number"], 9);
    assert_eq!(f.calls(), vec![vec!["pr", "create", "--title=T", "--body="]]);
}

#[tokio::test]
async fn fr_g12_create_rejects_bad_input_and_a_dash_body_is_a_value() {
    let f = setup().await;
    for title in [json!(""), json!("   "), json!("x".repeat(257)), json!("two\nlines")] {
        let (status, _) = f.post("create", json!({"title": title, "confirm": true})).await;
        assert_eq!(status, 400, "{title}");
    }
    let (status, _) = f.post("create", json!({"title": "T"})).await;
    assert_eq!(status, 400, "confirm is required");
    assert!(f.calls().is_empty());

    std::fs::write(f.repo.join(".gh-create"), "https://github.com/o/r/pull/10\n").unwrap();
    let (status, _) = f.post("create", json!({"title": "-T", "body": "--admin and more", "confirm": true})).await;
    assert_eq!(status, 200);
    assert_eq!(f.calls(), vec![vec!["pr", "create", "--title=-T", "--body=--admin and more"]]);
}

#[tokio::test]
async fn fr_g12_create_shows_gh_errors_in_korean_and_needs_a_url() {
    let f = setup().await;
    std::fs::write(f.repo.join(".gh-fail"), "none of the git remotes configured for this repository point to a known GitHub host").unwrap();
    let (status, body) = f.post("create", json!({"title": "T", "confirm": true})).await;
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap().contains("GitHub 원격"));
    std::fs::remove_file(f.repo.join(".gh-fail")).unwrap();
    std::fs::write(f.repo.join(".gh-create"), "done, no url\n").unwrap();
    let (status, _) = f.post("create", json!({"title": "T", "confirm": true})).await;
    assert_eq!(status, 500);
}

#[tokio::test]
async fn fr_g11_merge_with_a_stale_head_sha_is_409_and_never_calls_merge() {
    let f = setup().await;
    f.set_pr(detail("APPROVED", vec![run("SUCCESS", "build")]));
    let stale = "f".repeat(40);
    let (status, body) = f.post("merge", json!({"number": 7, "method": "squash", "head_sha": stale, "confirm": true})).await;
    assert_eq!(status, 409, "{body}");
    assert!(body["error"].as_str().unwrap().contains("풀 리퀘스트가 바뀌었습니다"), "{body}");
    assert!(!f.calls().iter().any(|c| c.get(1).map(String::as_str) == Some("merge")), "pr merge was not called");
    let (status, _) = f.post("merge", json!({"number": 7, "method": "squash", "head_sha": "not-hex", "confirm": true})).await;
    assert_eq!(status, 400);
    assert!(!f.calls().iter().any(|c| c.get(1).map(String::as_str) == Some("merge")));
}

#[tokio::test]
async fn fr_g11_merge_passes_match_head_commit() {
    let f = setup().await;
    f.set_pr(detail("APPROVED", vec![run("SUCCESS", "build")]));
    let (status, body) = f.post("merge", json!({"number": 7, "method": "merge", "head_sha": HEAD, "confirm": true})).await;
    assert_eq!(status, 200, "{body}");
    let calls = f.calls();
    assert_eq!(calls[1], vec!["pr", "merge", "7", "--merge", &format!("--match-head-commit={HEAD}")]);
    let (_, body) = f.post("", json!({"number": 7})).await;
    assert_eq!(body["head_sha"], HEAD);
}

#[tokio::test]
async fn fr_g12_bad_refs_are_400() {
    let f = setup().await;
    for bad in ["a..b", "a@{1}", "-x", "x/", "x.lock", "a b", "a\tb", "a~1", "a^", "a?", "a*", "a[b", "a\\b", "x/.hidden", "a//b"] {
        let (status, _) = f.post("create", json!({"title": "T", "head": bad, "confirm": true})).await;
        assert_eq!(status, 400, "head {bad:?}");
        let (status, _) = f.post("create", json!({"title": "T", "base": bad, "confirm": true})).await;
        assert_eq!(status, 400, "base {bad:?}");
    }
    let (status, _) = f.post("create", json!({"title": "T", "head": "o:a:b", "confirm": true})).await;
    assert_eq!(status, 400);
    let (status, _) = f.post("create", json!({"title": "T", "head": "-o:b", "confirm": true})).await;
    assert_eq!(status, 400);
    assert!(!f.calls().iter().any(|c| c.get(1).map(String::as_str) == Some("create")));
}

#[tokio::test]
async fn fr_g12_owner_colon_branch_is_allowed_for_head() {
    let f = setup().await;
    std::fs::write(f.repo.join(".gh-create"), "https://github.com/o/r/pull/11\n").unwrap();
    let (status, body) = f.post("create", json!({"title": "T", "head": "some-user.x:feat/y", "confirm": true})).await;
    assert_eq!(status, 200, "{body}");
    let (status, _) = f.post("create", json!({"title": "T", "base": "some-user:feat", "confirm": true})).await;
    assert_eq!(status, 400, "the owner form is for head only");
}

#[tokio::test]
async fn fr_g12_control_characters_in_a_body_are_400() {
    let f = setup().await;
    let (status, body) = f.post("create", json!({"title": "T", "body": "a\u{0}b", "confirm": true})).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap().contains("제어 문자"));
    let (status, _) = f.post("review", json!({"number": 7, "action": "comment", "body": "x\u{7}", "confirm": true})).await;
    assert_eq!(status, 400);
    assert!(f.calls().is_empty());
    std::fs::write(f.repo.join(".gh-create"), "https://github.com/o/r/pull/12\n").unwrap();
    let (status, _) = f.post("create", json!({"title": "T", "body": "line1\r\n\tline2", "confirm": true})).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn fr_g12_create_with_an_open_pr_for_the_head_is_409() {
    let f = setup().await;
    std::fs::write(f.repo.join(".gh-list"), "[{\"number\": 33}]").unwrap();
    let (status, body) = f.post("create", json!({"title": "T", "head": "feat/x", "confirm": true})).await;
    assert_eq!(status, 409, "{body}");
    assert!(body["error"].as_str().unwrap().contains("이미 열린 풀 리퀘스트가 있습니다: #33"), "{body}");
    assert!(!f.calls().iter().any(|c| c.get(1).map(String::as_str) == Some("create")));
    // Without an explicit head the check is skipped.
    std::fs::write(f.repo.join(".gh-create"), "https://github.com/o/r/pull/13\n").unwrap();
    let (status, _) = f.post("create", json!({"title": "T", "confirm": true})).await;
    assert_eq!(status, 200);
    assert!(!f.calls().iter().skip(1).any(|c| c.get(1).map(String::as_str) == Some("list")));
}

#[tokio::test]
async fn fr_g11_merge_without_a_head_sha_is_400_and_never_calls_gh() {
    let f = setup().await;
    f.set_pr(detail("APPROVED", vec![run("SUCCESS", "build")]));
    for body in [
        json!({"number": 7, "method": "merge", "confirm": true}),
        json!({"number": 7, "method": "merge", "head_sha": "", "confirm": true}),
        json!({"number": 7, "method": "merge", "head_sha": "  ", "confirm": true}),
    ] {
        let (status, out) = f.post("merge", body).await;
        assert_eq!(status, 400, "{out}");
        assert_eq!(out["error"], "확인한 헤드가 필요합니다.");
    }
    assert!(f.calls().is_empty(), "nothing reached gh");
}
