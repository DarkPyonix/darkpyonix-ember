//! FR-G8 pull requests over HTTP: temp repositories under `.scratch/` and a fake `gh` script.
//! `EMBER_GH_BIN` names the script; what it prints is read from files in the repository it runs
//! in (`.gh-json`, `.gh-fail`), and the arguments it got are written to `.gh-args`, so tests
//! running in parallel do not share state.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use ember_node::git::git_prs::{check_class, merge_gate, parse_prs, CheckClass, GitPrChecks, GitPrReview, GitPrState};
use ember_node::{api::{self, Node}, config::NodeConfig};
use serde_json::{json, Value};

fn scratch() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-prs-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn install_fake_gh() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let script = scratch().join("fake-gh.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\" > .gh-args\nif [ -f .gh-fail ]; then cat .gh-fail >&2; exit 1; fi\ncat .gh-json\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("EMBER_GH_BIN", &script);
        // A plain directory under `.scratch/` must not find the checkout's own repository.
        std::env::set_var("GIT_CEILING_DIRECTORIES", scratch());
    });
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A repository, a plain (non-repository) directory, the node's URL for `/git/prs` and a client.
struct Fixture { _dir: tempfile::TempDir, repo: PathBuf, plain: tempfile::TempDir, url: String, client: reqwest::Client }

async fn setup() -> Fixture {
    install_fake_gh();
    let dir = tempfile::Builder::new().prefix("p").tempdir_in(scratch()).unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let plain = tempfile::Builder::new().prefix("n").tempdir_in(scratch()).unwrap();
    let roots = vec![repo.clone(), plain.path().canonicalize().unwrap()];
    let node = Node::new(NodeConfig::new("token", roots)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/git/prs", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fixture { _dir: dir, repo, plain, url, client: reqwest::Client::new() }
}

impl Fixture {
    async fn post(&self, body: Value) -> (u16, Value) {
        let response = self.client.post(&self.url).bearer_auth("token").json(&body).send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap())
    }

    fn gh_args(&self) -> Vec<String> {
        std::fs::read_to_string(self.repo.join(".gh-args")).unwrap().lines().map(str::to_owned).collect()
    }
}

fn check(conclusion: &str) -> Value { json!({"__typename":"CheckRun", "status":"COMPLETED", "conclusion":conclusion, "name":"ci"}) }

fn pr(number: u64, review: &str, checks: Vec<Value>) -> Value {
    json!({
        "number": number, "title": format!("change {number}"), "state": "OPEN", "isDraft": false,
        "author": {"login": "octo"}, "headRefName": format!("feat/{number}"), "baseRefName": "develop",
        "updatedAt": "2026-11-05T10:00:00Z", "reviewDecision": review, "statusCheckRollup": checks,
        "mergeable": "MERGEABLE", "url": format!("https://github.com/o/r/pull/{number}")
    })
}

#[tokio::test]
async fn fr_g8_lists_pull_requests_and_passes_arguments() {
    let f = setup().await;
    std::fs::write(f.repo.join(".gh-json"), json!([
        pr(7, "APPROVED", vec![check("SUCCESS"), check("SKIPPED")]),
        pr(8, "REVIEW_REQUIRED", vec![]),
    ]).to_string()).unwrap();
    let (status, body) = f.post(json!({"repo": f.repo})).await;
    assert_eq!(status, 200, "{body}");
    let prs = body["prs"].as_array().unwrap();
    assert_eq!(prs.len(), 2);
    let first = &prs[0];
    assert_eq!(first["number"], 7);
    assert_eq!(first["title"], "change 7");
    assert_eq!(first["state"], "open");
    assert_eq!(first["draft"], false);
    assert_eq!(first["author"], "octo");
    assert_eq!(first["head"], "feat/7");
    assert_eq!(first["base"], "develop");
    assert_eq!(first["updated_at"], "2026-11-05T10:00:00Z");
    assert_eq!(first["review"], "approved");
    assert_eq!(first["checks"], json!({"passed": 2, "failed": 0, "pending": 0}));
    assert_eq!(first["url"], "https://github.com/o/r/pull/7");
    assert_eq!(prs[1]["review"], "review_required");
    let args = f.gh_args();
    assert_eq!(&args[..4], ["pr", "list", "--state", "open"]);
    assert!(args.iter().any(|a| a == "--limit") && args.iter().any(|a| a == "30"));
    assert!(args.iter().any(|a| a == "number,title,state,isDraft,author,headRefName,baseRefName,updatedAt,reviewDecision,statusCheckRollup,mergeable,url"));

    let (status, _) = f.post(json!({"repo": f.repo, "state": "merged", "limit": 5})).await;
    assert_eq!(status, 200);
    let args = f.gh_args();
    assert_eq!(args[3], "merged");
    assert_eq!(args.last().unwrap(), "5");
}

#[tokio::test]
async fn fr_g8_merge_gate_over_http() {
    let f = setup().await;
    std::fs::write(f.repo.join(".gh-json"), json!([
        pr(1, "APPROVED", vec![check("SUCCESS"), check("SUCCESS")]),
        pr(2, "APPROVED", vec![check("SUCCESS"), check("FAILURE")]),
        pr(3, "REVIEW_REQUIRED", vec![check("SUCCESS")]),
        pr(4, "CHANGES_REQUESTED", vec![check("SUCCESS")]),
        pr(5, "APPROVED", vec![check("SUCCESS"), json!({"status":"IN_PROGRESS", "conclusion":""})]),
        pr(6, "", vec![]),
    ]).to_string()).unwrap();
    let (_, body) = f.post(json!({"repo": f.repo})).await;
    let merge = |i: usize| &body["prs"][i]["merge"];
    assert_eq!(merge(0)["can_merge"], true);
    assert_eq!(merge(1)["can_merge"], false);
    assert!(merge(1)["reason"].as_str().unwrap().contains("CI"));
    assert_eq!(body["prs"][1]["checks"], json!({"passed": 1, "failed": 1, "pending": 0}));
    assert_eq!(merge(2)["can_merge"], false);
    assert!(merge(2)["reason"].as_str().unwrap().contains("승인"));
    assert_eq!(body["prs"][3]["review"], "changes_requested");
    assert!(merge(3)["reason"].as_str().unwrap().contains("승인"));
    assert!(merge(4)["reason"].as_str().unwrap().contains("CI"));
    assert_eq!(body["prs"][4]["checks"]["pending"], 1);
    assert_eq!(body["prs"][5]["review"], "none");
    assert!(merge(5)["reason"].as_str().unwrap().contains("승인"));
}

#[test]
fn fr_g8_check_classes() {
    for c in ["SUCCESS", "NEUTRAL", "SKIPPED"] {
        assert_eq!(check_class(&json!({"status":"COMPLETED", "conclusion": c})), CheckClass::Passed, "{c}");
    }
    for c in ["FAILURE", "ERROR", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED"] {
        assert_eq!(check_class(&json!({"status":"COMPLETED", "conclusion": c})), CheckClass::Failed, "{c}");
    }
    assert_eq!(check_class(&json!({"status":"IN_PROGRESS", "conclusion":""})), CheckClass::Pending);
    assert_eq!(check_class(&json!({"status":"QUEUED"})), CheckClass::Pending);
    // A status context carries `state` only.
    assert_eq!(check_class(&json!({"state":"SUCCESS"})), CheckClass::Passed);
    assert_eq!(check_class(&json!({"state":"FAILURE"})), CheckClass::Failed);
    assert_eq!(check_class(&json!({"state":"PENDING"})), CheckClass::Pending);
}

#[test]
fn fr_g8_parse_tolerates_missing_fields_and_states() {
    let prs = parse_prs(r#"[{"number":3,"state":"MERGED","isDraft":false,"author":null,"statusCheckRollup":null,"reviewDecision":null},
        {"number":4,"state":"CLOSED"},{"number":5,"state":"OPEN","isDraft":true,"mergeable":"CONFLICTING"}]"#).unwrap();
    assert_eq!(prs[0].state, GitPrState::Merged);
    assert_eq!(prs[0].author, "");
    assert_eq!(prs[0].review, GitPrReview::None);
    assert!(!prs[0].merge.can_merge);
    assert_eq!(prs[1].state, GitPrState::Closed);
    assert!(prs[2].draft && !prs[2].merge.can_merge);
    assert!(parse_prs("not json").is_err());
    assert_eq!(parse_prs("[]").unwrap().len(), 0);
}

#[test]
fn fr_g8_merge_gate_order() {
    let green = GitPrChecks { passed: 3, failed: 0, pending: 0 };
    assert!(merge_gate(GitPrState::Open, false, "MERGEABLE", GitPrReview::Approved, green).can_merge);
    assert!(!merge_gate(GitPrState::Open, true, "MERGEABLE", GitPrReview::Approved, green).can_merge);
    assert!(merge_gate(GitPrState::Open, false, "CONFLICTING", GitPrReview::Approved, green).reason.contains("충돌"));
    assert!(!merge_gate(GitPrState::Merged, false, "", GitPrReview::Approved, green).can_merge);
    assert!(!merge_gate(GitPrState::Closed, false, "", GitPrReview::Approved, green).can_merge);
}

#[tokio::test]
async fn fr_g8_gh_failures_answer_409_in_korean() {
    let f = setup().await;
    for (stderr, expect) in [
        ("To get started with GitHub CLI, please run:  gh auth login", "로그인"),
        ("none of the git remotes configured for this repository point to a known GitHub host", "GitHub 원격"),
        ("something odd", "gh 작업이 실패했습니다"),
    ] {
        std::fs::write(f.repo.join(".gh-fail"), stderr).unwrap();
        let (status, body) = f.post(json!({"repo": f.repo})).await;
        assert_eq!(status, 409, "{body}");
        let message = body["error"].as_str().unwrap();
        assert!(message.contains(expect), "{message}");
        assert!(message.contains(stderr), "gh's text stays as detail: {message}");
    }
}

#[tokio::test]
async fn fr_g8_non_repo_is_404_and_bad_limit_400() {
    let f = setup().await;
    let (status, body) = f.post(json!({"repo": f.plain.path().canonicalize().unwrap()})).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], "not_a_repo");
    for limit in [0, 201] {
        let (status, _) = f.post(json!({"repo": f.repo, "limit": limit})).await;
        assert_eq!(status, 400);
    }
}
