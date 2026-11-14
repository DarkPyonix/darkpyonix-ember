//! One repository's GitHub summary for the Home GitHub card (SPEC FR-G13), read through the
//! machine's own `gh` CLI. Same safe runner as FR-G8: argv list, no shell, no prompt, the network
//! deadline and the stdout cap; `EMBER_GH_BIN` names the program in tests.
//!
//! Five calls run one after the other. A call that fails makes only its own fields null and
//! records the first failure as a Korean `error`; it never fails the request. Only a missing `gh`
//! (409) or a repository the policy refuses fails the request. No titles or bodies are read.
use std::ffi::OsString;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{exec_cmd, network_timeout, open, GitError, Out, PathPolicy};

/// `gh pr list --limit`: the open count is reported up to this many.
const PR_LIMIT: usize = 100;
/// Workflow runs read for the strip.
const RUN_LIMIT: usize = 14;
/// Names are cut at this many characters.
const MAX_NAME_CHARS: usize = 100;
/// The recorded error is cut at this many characters.
const MAX_ERROR_CHARS: usize = 200;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitGithubRequest {
    pub repo: String,
    /// Also list the recent failed workflow runs on every branch (`failed_runs`, FR-A8 event
    /// routines). Off for the Home card, so it costs one `gh` call only for the CI watcher.
    #[serde(default)]
    pub failed_runs: bool,
}

/// One failed workflow run (`gh run list --status failure`), for CI event routines (FR-A8).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CiFailedRun {
    /// GitHub's run id (`databaseId`), the key the server deduplicates on.
    pub id: u64,
    pub branch: Option<String>,
    pub workflow: Option<String>,
    pub url: Option<String>,
    /// `createdAt` as GitHub gives it (RFC 3339).
    pub created_at: Option<String>,
}

/// The conclusion of one workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CiRun { Success, Failure, Cancelled, Pending }

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GitGithubResponse {
    /// `owner/name`, or null when `gh repo view` failed.
    pub name_with_owner: Option<String>,
    pub default_branch: Option<String>,
    /// The signed-in login, or null.
    pub account: Option<String>,
    /// Open pull requests (at most 100), or null.
    pub open_prs: Option<u32>,
    /// Open pull requests whose review the account is asked for, or null.
    pub review_requested: Option<u32>,
    /// `open_prs` stopped at the 100 limit.
    pub capped: bool,
    /// Last workflow runs on the default branch, newest first, or null.
    pub ci: Option<Vec<CiRun>>,
    /// `gh` said the account is signed out.
    pub signed_out: bool,
    /// Korean text of the first failed call, at most 200 characters.
    pub error: Option<String>,
    /// Recent failed runs on any branch, newest first; only when the request asked for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_runs: Option<Vec<CiFailedRun>>,
}

/// Failed runs read for `failed_runs`.
const FAILED_RUN_LIMIT: usize = 20;

/// The items of `gh run list --json databaseId,headBranch,workflowName,url,createdAt`; an item
/// without a numeric id is dropped.
pub fn map_failed_runs(items: &[Value]) -> Vec<CiFailedRun> {
    let text = |item: &Value, k: &str| item.get(k).and_then(|v| v.as_str()).map(clean_name).filter(|s| !s.is_empty());
    items
        .iter()
        .take(FAILED_RUN_LIMIT)
        .filter_map(|item| {
            Some(CiFailedRun {
                id: item.get("databaseId")?.as_u64()?,
                branch: text(item, "headBranch"),
                workflow: text(item, "workflowName"),
                url: item.get("url").and_then(|v| v.as_str()).filter(|u| u.starts_with("https://")).map(|u| u.chars().filter(|c| !c.is_control()).take(300).collect()),
                created_at: text(item, "createdAt"),
            })
        })
        .collect()
}

/// Control characters removed, cut at [`MAX_NAME_CHARS`].
pub fn clean_name(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect::<String>().trim().to_string()
}

fn clean_error(text: &str) -> String {
    let spaced: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_ERROR_CHARS).collect()
}

fn text_signed_out(text: &str) -> bool {
    let t = text.to_lowercase();
    ["not logged in", "gh auth login", "http 401", "bad credentials"].iter().any(|n| t.contains(n))
}

/// Korean text for a failed `gh` call.
fn failure_text(o: &Out) -> String {
    let raw = if o.stderr.trim().is_empty() { o.stdout.trim() } else { o.stderr.trim() };
    let t = raw.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    let k = if text_signed_out(raw) {
        "GitHub에 로그인되어 있지 않습니다. 이 컴퓨터에서 gh auth login을 실행하세요."
    } else if has(&["none of the git remotes", "no github", "not a github", "could not determine", "no git remotes", "unable to determine base repo", "no default remote"]) {
        "이 저장소에는 GitHub 원격 저장소가 없습니다."
    } else if has(&["could not resolve host", "error connecting", "connection refused", "network is unreachable", "i/o timeout", "tls handshake"]) {
        "GitHub에 연결하지 못했습니다 (네트워크를 확인하세요)."
    } else if has(&["http 403", "rate limit", "resource not accessible"]) {
        "GitHub가 요청을 거부했습니다 (권한이나 요청 한도를 확인하세요)."
    } else if has(&["http 404", "could not resolve to a repository"]) {
        "GitHub에서 저장소를 찾을 수 없습니다."
    } else {
        "gh 작업이 실패했습니다."
    };
    k.to_string()
}

/// One call's outcome: its stdout, or the Korean failure and whether it meant signed out.
enum Call {
    Ok(String),
    Failed { text: String, signed_out: bool },
}

async fn run_gh(dir: &std::path::Path, args: &[&str]) -> Result<Call, GitError> {
    let program = std::env::var_os("EMBER_GH_BIN").unwrap_or_else(|| OsString::from("gh"));
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_SPINNER_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GH_FORCE_TTY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = match exec_cmd(cmd, "gh", None, Some(network_timeout())).await {
        Ok(o) => o,
        // A missing `gh` fails the request; a timeout is only this call's failure.
        Err(GitError::Timeout(m)) => return Ok(Call::Failed { text: clean_error(&m), signed_out: false }),
        Err(e) => return Err(e),
    };
    if out.truncated {
        return Ok(Call::Failed { text: "gh 출력이 너무 커서 중단했습니다.".into(), signed_out: false });
    }
    if out.code != 0 {
        let raw = format!("{} {}", out.stderr, out.stdout);
        return Ok(Call::Failed { text: failure_text(&out), signed_out: text_signed_out(&raw) });
    }
    Ok(Call::Ok(out.stdout))
}

/// The conclusion of one `gh run list --json conclusion,status` item.
pub fn map_run(item: &Value) -> CiRun {
    let field = |k: &str| item.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_ascii_lowercase();
    if field("status") != "completed" {
        return CiRun::Pending;
    }
    match field("conclusion").as_str() {
        "success" => CiRun::Success,
        "cancelled" | "skipped" => CiRun::Cancelled,
        _ => CiRun::Failure,
    }
}

/// The pull requests of `gh pr list --json number,reviewRequests` whose review `account` is asked
/// for. Team entries (no `login`) are ignored.
pub fn count_review_requested(prs: &[Value], account: &str) -> u32 {
    prs.iter()
        .filter(|pr| {
            pr.get("reviewRequests").and_then(|r| r.as_array()).is_some_and(|rs| {
                rs.iter().any(|r| r.get("login").and_then(|l| l.as_str()).is_some_and(|l| l.eq_ignore_ascii_case(account)))
            })
        })
        .count() as u32
}

pub async fn summarize(policy: &PathPolicy, r: &GitGithubRequest) -> Result<GitGithubResponse, GitError> {
    let dir = open(policy, &r.repo).await?;
    let mut res = GitGithubResponse {
        name_with_owner: None,
        default_branch: None,
        account: None,
        open_prs: None,
        review_requested: None,
        capped: false,
        ci: None,
        signed_out: false,
        error: None,
        failed_runs: None,
    };
    let fail = |res: &mut GitGithubResponse, text: String, signed_out: bool| {
        res.signed_out |= signed_out;
        if res.error.is_none() {
            res.error = Some(clean_error(&text));
        }
    };

    // 1. The repository. A missing `gh` surfaces here as the request's 409.
    match run_gh(&dir, &["repo", "view", "--json", "nameWithOwner,defaultBranchRef"]).await? {
        Call::Ok(text) => match serde_json::from_str::<Value>(text.trim()) {
            Ok(v) => {
                res.name_with_owner = v.get("nameWithOwner").and_then(|x| x.as_str()).map(clean_name).filter(|s| !s.is_empty());
                res.default_branch = v.pointer("/defaultBranchRef/name").and_then(|x| x.as_str()).map(clean_name).filter(|s| !s.is_empty());
            }
            Err(_) => fail(&mut res, "gh 출력 형식이 잘못되었습니다.".into(), false),
        },
        Call::Failed { text, signed_out } => fail(&mut res, text, signed_out),
    }
    // Signed out: the other calls would fail the same way, so they are not run.
    if res.signed_out {
        return Ok(res);
    }

    // 2. The account.
    match run_gh(&dir, &["api", "user", "--jq", ".login"]).await? {
        Call::Ok(text) => res.account = Some(clean_name(text.trim())).filter(|s| !s.is_empty()),
        Call::Failed { text, signed_out } => fail(&mut res, text, signed_out),
    }
    if res.signed_out {
        return Ok(res);
    }

    // 3. Open pull requests and the reviews asked of the account.
    let limit = PR_LIMIT.to_string();
    match run_gh(&dir, &["pr", "list", "--state", "open", "--json", "number,reviewRequests", "--limit", &limit]).await? {
        Call::Ok(text) => match serde_json::from_str::<Vec<Value>>(text.trim()) {
            Ok(prs) => {
                res.open_prs = Some(prs.len() as u32);
                res.capped = prs.len() >= PR_LIMIT;
                res.review_requested = res.account.as_deref().map(|a| count_review_requested(&prs, a));
            }
            Err(_) => fail(&mut res, "gh 출력 형식이 잘못되었습니다.".into(), false),
        },
        Call::Failed { text, signed_out } => fail(&mut res, text, signed_out),
    }

    // 4. The last runs on the default branch (needs the branch from call 1).
    if let Some(branch) = res.default_branch.clone() {
        let limit = RUN_LIMIT.to_string();
        match run_gh(&dir, &["run", "list", "--branch", &branch, "--limit", &limit, "--json", "conclusion,status"]).await? {
            Call::Ok(text) => match serde_json::from_str::<Vec<Value>>(text.trim()) {
                Ok(runs) => res.ci = Some(runs.iter().take(RUN_LIMIT).map(map_run).collect()),
                Err(_) => fail(&mut res, "gh 출력 형식이 잘못되었습니다.".into(), false),
            },
            Call::Failed { text, signed_out } => fail(&mut res, text, signed_out),
        }
    }

    // 5. Failed runs on any branch, only when asked (the CI watcher, FR-A8).
    if r.failed_runs {
        let limit = FAILED_RUN_LIMIT.to_string();
        let args = ["run", "list", "--status", "failure", "--limit", &limit, "--json", "databaseId,headBranch,workflowName,url,createdAt"];
        match run_gh(&dir, &args).await? {
            Call::Ok(text) => match serde_json::from_str::<Vec<Value>>(text.trim()) {
                Ok(items) => res.failed_runs = Some(map_failed_runs(&items)),
                Err(_) => fail(&mut res, "gh 출력 형식이 잘못되었습니다.".into(), false),
            },
            Call::Failed { text, signed_out } => fail(&mut res, text, signed_out),
        }
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fr_g13_names_are_cleaned_and_cut() {
        assert_eq!(clean_name("o/\u{1b}[31mr\n"), "o/[31mr");
        assert_eq!(clean_name(&"x".repeat(300)).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn fr_g13_run_mapping() {
        assert_eq!(map_run(&json!({"status":"completed","conclusion":"success"})), CiRun::Success);
        assert_eq!(map_run(&json!({"status":"completed","conclusion":"skipped"})), CiRun::Cancelled);
        assert_eq!(map_run(&json!({"status":"in_progress","conclusion":""})), CiRun::Pending);
        assert_eq!(map_run(&json!({"status":"completed","conclusion":"timed_out"})), CiRun::Failure);
    }

    #[test]
    fn fr_a8_failed_runs_mapping() {
        let runs = map_failed_runs(&[
            json!({"databaseId": 42, "headBranch": "main\n", "workflowName": "Test", "url": "https://github.com/o/r/actions/runs/42", "createdAt": "2026-11-13T01:00:00Z"}),
            json!({"headBranch": "x"}),
            json!({"databaseId": 7, "url": "javascript:alert(1)"}),
        ]);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, 42);
        assert_eq!(runs[0].branch.as_deref(), Some("main"));
        assert_eq!(runs[0].created_at.as_deref(), Some("2026-11-13T01:00:00Z"));
        assert_eq!(runs[1].url, None);
    }

    #[test]
    fn fr_a8_request_defaults_to_no_failed_runs() {
        let r: GitGithubRequest = serde_json::from_value(json!({"repo": "/x"})).unwrap();
        assert!(!r.failed_runs);
    }
}
