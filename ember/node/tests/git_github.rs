//! FR-G13 over HTTP: `POST /git/github` with a fake `gh`. `EMBER_GH_BIN` names the script; per
//! subcommand it prints `.gh-out-<kind>` from the repository it runs in, or fails with the text
//! of `.gh-fail-<kind>` (kinds: `repo`, `user`, `prs`, `run`). The arguments it got are appended
//! to `.gh-args`, so tests running in parallel do not share state.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

mod common;

use ember_node::{api::{self, Node}, config::NodeConfig};
use serde_json::{json, Value};

fn scratch() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/git-github-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn install_fake_gh() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let script = scratch().join("fake-gh.sh");
        common::write_exec_ready(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\" >> .gh-args\ncase \"$1\" in repo) k=repo;; api) k=user;; pr) k=prs;; run) k=run;; *) k=other;; esac\nif [ -f \".gh-fail-$k\" ]; then cat \".gh-fail-$k\" >&2; exit 1; fi\ncat \".gh-out-$k\"\n");
        std::env::set_var("EMBER_GH_BIN", &script);
        std::env::set_var("GIT_CEILING_DIRECTORIES", scratch());
    });
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

struct Fixture { _dir: tempfile::TempDir, repo: PathBuf, url: String, client: reqwest::Client }

async fn setup() -> Fixture {
    install_fake_gh();
    let dir = tempfile::Builder::new().prefix("g").tempdir_in(scratch()).unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let node = Node::new(NodeConfig::new("token", vec![repo.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/git/github", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fixture { _dir: dir, repo, url, client: reqwest::Client::new() }
}

impl Fixture {
    fn out(&self, kind: &str, text: impl AsRef<str>) { std::fs::write(self.repo.join(format!(".gh-out-{kind}")), text.as_ref()).unwrap(); }
    fn fail(&self, kind: &str, text: &str) { std::fs::write(self.repo.join(format!(".gh-fail-{kind}")), text).unwrap(); }

    /// A healthy repository: `o/r` on `main`, account `octo`.
    fn healthy(&self) {
        self.out("repo", json!({"nameWithOwner": "o/r", "defaultBranchRef": {"name": "main"}}).to_string());
        self.out("user", "octo\n");
        self.out("prs", json!([
            {"number": 1, "reviewRequests": [{"__typename": "User", "login": "octo"}]},
            {"number": 2, "reviewRequests": [{"__typename": "Team", "name": "core", "slug": "core"}]},
            {"number": 3, "reviewRequests": [{"__typename": "User", "login": "other"}, {"__typename": "User", "login": "Octo"}]},
            {"number": 4, "reviewRequests": []},
        ]).to_string());
        self.out("run", json!([
            {"status": "completed", "conclusion": "success"},
            {"status": "completed", "conclusion": "failure"},
            {"status": "in_progress", "conclusion": ""},
            {"status": "completed", "conclusion": "cancelled"},
            {"status": "completed", "conclusion": "skipped"},
            {"status": "completed", "conclusion": "timed_out"},
            {"status": "queued", "conclusion": null},
        ]).to_string());
    }

    async fn post(&self) -> (u16, Value) {
        let response = self.client.post(&self.url).bearer_auth("token").json(&json!({"repo": self.repo})).send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap())
    }

    fn gh_args(&self) -> Vec<String> {
        std::fs::read_to_string(self.repo.join(".gh-args")).unwrap().lines().map(str::to_owned).collect()
    }
}

#[tokio::test]
async fn fr_g13_full_mapping() {
    let f = setup().await;
    f.healthy();
    let (status, body) = f.post().await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["name_with_owner"], "o/r");
    assert_eq!(body["default_branch"], "main");
    assert_eq!(body["account"], "octo");
    assert_eq!(body["open_prs"], 4);
    assert_eq!(body["capped"], false);
    assert_eq!(body["signed_out"], false);
    assert_eq!(body["error"], Value::Null);
    let args = f.gh_args();
    assert!(args.windows(3).any(|w| w == ["repo", "view", "--json"]));
    assert!(args.iter().any(|a| a == "nameWithOwner,defaultBranchRef"));
    assert!(args.windows(3).any(|w| w == ["api", "user", "--jq"]));
    assert!(args.iter().any(|a| a == "number,reviewRequests") && args.iter().any(|a| a == "100"));
    assert!(args.windows(2).any(|w| w == ["--branch", "main"]) && args.iter().any(|a| a == "14"));
}

#[tokio::test]
async fn fr_g13_review_requested_counts_users_only_and_ignores_teams() {
    let f = setup().await;
    f.healthy();
    let (_, body) = f.post().await;
    // PR 1 (octo) and PR 3 (Octo, case-insensitive); the team entry of PR 2 does not count.
    assert_eq!(body["review_requested"], 2);
}

#[tokio::test]
async fn fr_g13_ci_mapping_order_and_statuses() {
    let f = setup().await;
    f.healthy();
    let (_, body) = f.post().await;
    assert_eq!(body["ci"], json!(["success", "failure", "pending", "cancelled", "cancelled", "failure", "pending"]));
}

#[tokio::test]
async fn fr_g13_open_count_is_capped_at_100() {
    let f = setup().await;
    f.healthy();
    let prs: Vec<Value> = (1..=100).map(|n| json!({"number": n, "reviewRequests": []})).collect();
    f.out("prs", Value::Array(prs).to_string());
    let (_, body) = f.post().await;
    assert_eq!(body["open_prs"], 100);
    assert_eq!(body["capped"], true);
    assert_eq!(body["review_requested"], 0);
}

#[tokio::test]
async fn fr_g13_one_failing_call_nulls_only_its_fields() {
    let f = setup().await;
    f.healthy();
    f.fail("prs", "HTTP 403: Resource not accessible by integration");
    let (status, body) = f.post().await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["open_prs"], Value::Null);
    assert_eq!(body["review_requested"], Value::Null);
    assert_eq!(body["name_with_owner"], "o/r");
    assert_eq!(body["account"], "octo");
    assert_eq!(body["ci"].as_array().unwrap().len(), 7);
    let error = body["error"].as_str().unwrap();
    assert!(error.contains("거부"), "{error}");
    assert!(error.chars().count() <= 200);
    assert_eq!(body["signed_out"], false);

    // A failing run list nulls only `ci`.
    let f = setup().await;
    f.healthy();
    f.fail("run", "could not resolve host: api.github.com");
    let (_, body) = f.post().await;
    assert_eq!(body["ci"], Value::Null);
    assert_eq!(body["open_prs"], 4);
    assert!(body["error"].as_str().unwrap().contains("연결"));
}

#[tokio::test]
async fn fr_g13_signed_out_sets_the_flag() {
    let f = setup().await;
    f.healthy();
    f.fail("repo", "You are not logged in to any GitHub hosts. To log in, run: gh auth login");
    let (status, body) = f.post().await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["signed_out"], true);
    assert_eq!(body["open_prs"], Value::Null);
    assert_eq!(body["ci"], Value::Null);
    assert!(body["error"].as_str().unwrap().contains("로그인"));
}

#[tokio::test]
async fn fr_g13_names_are_cleaned() {
    let f = setup().await;
    f.healthy();
    let long = "y".repeat(300);
    f.out("repo", json!({"nameWithOwner": format!("o/\u{1b}[31m{long}\u{7}"), "defaultBranchRef": {"name": "main"}}).to_string());
    f.out("user", "oc\u{0}to\n");
    let (_, body) = f.post().await;
    let name = body["name_with_owner"].as_str().unwrap();
    assert!(!name.chars().any(|c| c.is_control()));
    assert_eq!(name.chars().count(), 100);
    assert_eq!(body["account"], "octo");
}
