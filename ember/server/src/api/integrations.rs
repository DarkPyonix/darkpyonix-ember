//! Integrations for the Home 연동 area (SPEC FR-G13, FR-L35): `GET /api/integrations/github`.
//!
//! The server lists the projects, takes each project's repository (the working directory of its
//! newest unarchived session, the way the git relay does; a project without one is skipped), asks
//! that session's computer for `POST /git/github` once per distinct (computer, directory), and
//! aggregates:
//!
//! ```text
//! { state: "on"|"warn"|"off", reason, account,
//!   repos: [{ name_with_owner, default_branch, open_prs, review_requested, capped, ci, error, projects }],
//!   totals: { repos, open_prs, review_requested, ci_pass_percent },
//!   checked_at }
//! ```
//!
//! `off` is no `gh` anywhere (or no project with a repository), `warn` is a signed-out `gh`.
//! A number is null when its call failed; totals sum the non-null ones. One repository appears
//! once however many projects use it. The answer is cached 90 seconds (global, single user) and
//! one refresh runs at a time; callers that wait reuse its result. There is no way to bypass the
//! cache. `EMBER_GITHUB_CACHE_SECS` changes the lifetime (read once at start).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value};

use super::git::{local_call, new_local};
use crate::computers::{Computers, LOCAL};
use crate::session::Sessions;

/// How long an answer is served without asking the nodes again.
pub const CACHE_SECS: u64 = 90;

const NOT_INSTALLED: &str = "gh가 설치되어 있지 않습니다";
const SIGNED_OUT: &str = "gh에 로그인되어 있지 않습니다 (터미널에서 gh auth login)";
const NO_REPO: &str = "저장소가 있는 프로젝트가 없습니다";

#[derive(Clone)]
struct State_ {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    local: Arc<std::sync::Mutex<Option<Router>>>,
    ttl: Duration,
    /// Held across a refresh, so concurrent callers wait and reuse the new value.
    cache: Arc<tokio::sync::Mutex<Option<(Instant, Value)>>>,
}

pub fn router(sessions: Arc<Sessions>, computers: Arc<Computers>) -> Router {
    let secs = std::env::var("EMBER_GITHUB_CACHE_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(CACHE_SECS);
    router_with_ttl(sessions, computers, Duration::from_secs(secs))
}

/// [`router`] with an explicit cache lifetime (tests).
pub fn router_with_ttl(sessions: Arc<Sessions>, computers: Arc<Computers>, ttl: Duration) -> Router {
    Router::new()
        .route("/api/integrations/github", get(github))
        .with_state(State_ { sessions, computers, local: new_local(), ttl, cache: Arc::new(tokio::sync::Mutex::new(None)) })
}

fn error(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

async fn github(State(s): State<State_>) -> Response {
    let mut cache = s.cache.lock().await;
    if let Some((at, value)) = cache.as_ref() {
        if at.elapsed() < s.ttl {
            return Json(value.clone()).into_response();
        }
    }
    match refresh(&s).await {
        Ok(value) => {
            *cache = Some((Instant::now(), value.clone()));
            Json(value).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

/// What one node call came to.
enum Outcome {
    Summary(Value),
    NotInstalled,
    Failed(String),
}

#[derive(Serialize, Clone)]
struct Repo {
    name_with_owner: Option<String>,
    default_branch: Option<String>,
    open_prs: Option<u64>,
    review_requested: Option<u64>,
    capped: bool,
    ci: Option<Vec<String>>,
    error: Option<String>,
    projects: Vec<String>,
}

async fn refresh(s: &State_) -> anyhow::Result<Value> {
    let store = s.sessions.store();
    // (computer, cwd) -> projects using it, in first-seen order.
    let mut targets: Vec<((String, String), Vec<String>)> = Vec::new();
    for project in store.projects()?.into_iter().filter(|p| !p.archived) {
        let Some(rec) = store.sessions(Some(&project.name))?.into_iter().find(|r| !r.archived) else { continue };
        let computer_id = match s.computers.registry().session_computer(&rec.id)? {
            Some(sc) => sc.computer_id,
            None => project.computers.iter().find(|c| *c != LOCAL).cloned().unwrap_or_else(|| LOCAL.to_string()),
        };
        let key = (computer_id, rec.cwd);
        match targets.iter_mut().find(|(k, _)| *k == key) {
            Some((_, names)) => names.push(project.name.clone()),
            None => targets.push((key, vec![project.name.clone()])),
        }
    }

    let calls = targets.iter().map(|((computer_id, cwd), _)| call_node(s, computer_id, cwd));
    let outcomes = futures::future::join_all(calls).await;

    let mut repos: Vec<Repo> = Vec::new();
    let mut account: Option<String> = None;
    let mut signed_out = false;
    let mut installed = 0usize;
    for (outcome, (_, projects)) in outcomes.into_iter().zip(targets.iter()) {
        let repo = match outcome {
            Outcome::NotInstalled => {
                continue;
            }
            Outcome::Failed(text) => Repo { name_with_owner: None, default_branch: None, open_prs: None, review_requested: None, capped: false, ci: None, error: Some(text), projects: projects.clone() },
            Outcome::Summary(v) => {
                installed += 1;
                signed_out |= v["signed_out"].as_bool().unwrap_or(false);
                if account.is_none() {
                    account = v["account"].as_str().map(String::from);
                }
                Repo {
                    name_with_owner: v["name_with_owner"].as_str().map(String::from),
                    default_branch: v["default_branch"].as_str().map(String::from),
                    open_prs: v["open_prs"].as_u64(),
                    review_requested: v["review_requested"].as_u64(),
                    capped: v["capped"].as_bool().unwrap_or(false),
                    ci: v["ci"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()),
                    error: v["error"].as_str().map(String::from),
                    projects: projects.clone(),
                }
            }
        };
        // The same repository reached through two directories counts once; the first is kept.
        match repo.name_with_owner.as_ref().and_then(|n| repos.iter_mut().find(|r| r.name_with_owner.as_ref() == Some(n))) {
            Some(first) => first.projects.extend(repo.projects),
            None => repos.push(repo),
        }
    }

    let (state, reason) = if targets.is_empty() {
        ("off", Some(NO_REPO))
    } else if installed == 0 && repos.is_empty() {
        ("off", Some(NOT_INSTALLED))
    } else if signed_out {
        ("warn", Some(SIGNED_OUT))
    } else {
        ("on", None)
    };

    let sum = |f: fn(&Repo) -> Option<u64>| repos.iter().filter_map(f).sum::<u64>();
    let (ok, bad) = repos.iter().flat_map(|r| r.ci.iter().flatten()).fold((0u64, 0u64), |(ok, bad), c| match c.as_str() {
        "success" => (ok + 1, bad),
        "failure" => (ok, bad + 1),
        _ => (ok, bad),
    });
    let ci_pass_percent = (ok + bad > 0).then(|| ((100.0 * ok as f64) / (ok + bad) as f64).round() as u64);
    let checked_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Ok(json!({
        "state": state,
        "reason": reason,
        "account": account,
        "totals": {
            "repos": repos.len(),
            "open_prs": sum(|r| r.open_prs),
            "review_requested": sum(|r| r.review_requested),
            "ci_pass_percent": ci_pass_percent,
        },
        "repos": repos,
        "checked_at": checked_at,
    }))
}

async fn call_node(s: &State_, computer_id: &str, cwd: &str) -> Outcome {
    let body = json!({ "repo": cwd });
    let path = "/git/github";
    let result: Result<(u16, Vec<u8>), String> = if computer_id == LOCAL {
        local_call(&s.local, path, &body).await.map_err(|e| format!("{e:#}"))
    } else {
        match s.computers.client(computer_id) {
            Ok(c) => c.post_raw(path, &body).await.map_err(|e| format!("computer {computer_id}: {e}")),
            Err(e) => Err(format!("{e:#}")),
        }
    };
    match result {
        Ok((200, bytes)) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => Outcome::Summary(v),
            Err(e) => Outcome::Failed(format!("노드의 응답을 읽지 못했습니다: {e}")),
        },
        Ok((409, bytes)) => {
            let text = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v["error"].as_str().map(String::from)).unwrap_or_default();
            if text.contains("gh") && text.contains("설치") {
                Outcome::NotInstalled
            } else {
                Outcome::Failed(text.chars().take(200).collect())
            }
        }
        Ok((status, bytes)) => {
            let text = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v["error"].as_str().map(String::from)).unwrap_or_else(|| format!("노드가 {status}로 답했습니다"));
            Outcome::Failed(text.chars().take(200).collect())
        }
        Err(text) => Outcome::Failed(text.chars().take(200).collect()),
    }
}
