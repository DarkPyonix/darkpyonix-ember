//! CI event routines (SPEC FR-L23, FR-A8): a failed GitHub workflow run starts the linked routine.
//!
//! An event schedule is `kind: {"type":"event","on":"ci_failed","repository":"owner/name",
//! "branch"?}`. The [`CiWatcher`] polls a [`CiSource`] for each project that has such a schedule
//! (unpaused), every [`POLL_SECS`] seconds by default (`EMBER_CI_POLL_SECS`). The source answers
//! the project's repository and its recent failed runs; the real one is [`NodeCiSource`], which
//! asks the project's computer `POST /git/github` with `failed_runs: true` through the same path
//! as the Home GitHub card (`api::integrations`, FR-G13), so it uses that computer's `gh` and
//! Ember stores no token. There is no webhook (no public endpoint is assumed) [provisional, #759].
//!
//! A failed run starts a schedule when the repository matches (case-insensitive, URL forms
//! accepted), the branch matches when the schedule names one, and the run was created at or after
//! the schedule (failures from before the routine existed never fire it). Each (schedule, run id)
//! starts at most once: [`Scheduler::fire_event`] stores the pair before sending, so a later poll
//! or a restart does not refire. A re-run attempt keeps GitHub's run id, so it does not fire again.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use super::{timing, EventOn, Schedule, ScheduleKind, ScheduleRun, Scheduler};
use crate::computers::{Computers, LOCAL};
use crate::session::Sessions;

/// Default seconds between polls.
pub const POLL_SECS: u64 = 120;

/// One failed workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiFailure {
    /// GitHub's run id.
    pub id: u64,
    pub branch: Option<String>,
    pub workflow: Option<String>,
    pub url: Option<String>,
    /// When the run was created (Unix ms); a run without a time never fires.
    pub created_at: Option<i64>,
}

/// What a source knows about one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CiReport {
    /// `owner/name` of the project's GitHub repository, if known.
    pub repository: Option<String>,
    /// Recent failed runs, any order.
    pub failures: Vec<CiFailure>,
}

/// Where the watcher reads CI state (tests use a fake).
pub trait CiSource: Send + Sync {
    fn report<'a>(&'a self, project: &'a str) -> BoxFuture<'a, anyhow::Result<CiReport>>;
}

/// `owner/name`, lower case, from `owner/name`, `github.com/owner/name`,
/// `https://github.com/owner/name(.git)`; `None` when it is not that shape.
pub fn normalize_repo(text: &str) -> Option<String> {
    let t = text.trim().trim_end_matches('/');
    let t = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(t);
    let t = t.strip_prefix("github.com/").unwrap_or(t);
    let t = t.strip_suffix(".git").unwrap_or(t);
    let mut parts = t.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    (parts.next().is_none() && ok(owner) && ok(name)).then(|| format!("{}/{}", owner.to_ascii_lowercase(), name.to_ascii_lowercase()))
}

pub struct CiWatcher {
    scheduler: Arc<Scheduler>,
    source: Arc<dyn CiSource>,
    lock: tokio::sync::Mutex<()>,
}

impl CiWatcher {
    pub fn new(scheduler: Arc<Scheduler>, source: Arc<dyn CiSource>) -> Arc<CiWatcher> {
        Arc::new(CiWatcher { scheduler, source, lock: tokio::sync::Mutex::new(()) })
    }

    /// The poll interval from `EMBER_CI_POLL_SECS` (at least 30), else [`POLL_SECS`].
    pub fn interval_from_env() -> Duration {
        let secs = std::env::var("EMBER_CI_POLL_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(POLL_SECS);
        Duration::from_secs(secs.max(30))
    }

    /// Poll every `every` in the background.
    pub fn start(self: &Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.poll().await;
                tokio::time::sleep(every).await;
            }
        })
    }

    /// One poll: ask the source once per project with an unpaused CI routine, start the routines
    /// for failures not seen before. Returns the runs started, oldest failure first.
    pub async fn poll(&self) -> Vec<ScheduleRun> {
        let _guard = self.lock.lock().await;
        let schedules = match self.scheduler.list(None) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("listing schedules for CI routines failed: {e:#}");
                return Vec::new();
            }
        };
        let mut by_project: BTreeMap<String, Vec<Schedule>> = BTreeMap::new();
        for s in schedules {
            if !s.paused && matches!(s.kind, ScheduleKind::Event { on: EventOn::CiFailed, .. }) {
                by_project.entry(s.project.clone()).or_default().push(s);
            }
        }
        let mut started = Vec::new();
        for (project, schedules) in by_project {
            let report = match self.source.report(&project).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(project = %project, "reading CI state failed: {e:#}");
                    continue;
                }
            };
            let Some(repo) = report.repository.as_deref().and_then(normalize_repo) else { continue };
            let mut failures = report.failures.clone();
            failures.sort_by_key(|f| (f.created_at, f.id));
            for s in &schedules {
                let ScheduleKind::Event { repository, branch, .. } = &s.kind else { continue };
                if normalize_repo(repository).as_deref() != Some(repo.as_str()) {
                    continue;
                }
                for f in &failures {
                    let Some(at) = f.created_at else { continue };
                    if at < s.created_at {
                        continue;
                    }
                    if let Some(b) = branch {
                        if f.branch.as_deref() != Some(b.trim()) {
                            continue;
                        }
                    }
                    let context = format!(
                        "GitHub Actions run {} of {repo} failed (workflow {}, branch {}, created {}{}).",
                        f.id,
                        f.workflow.as_deref().unwrap_or("unknown"),
                        f.branch.as_deref().unwrap_or("unknown"),
                        timing::rfc3339(at),
                        f.url.as_deref().map(|u| format!(", {u}")).unwrap_or_default(),
                    );
                    match self.scheduler.fire_event(s, &format!("ci:{repo}:{}", f.id), at, &context).await {
                        Ok(Some(run)) => started.push(run),
                        Ok(None) => {}
                        Err(e) => tracing::error!(schedule = %s.id, "starting a CI routine failed: {e:#}"),
                    }
                }
            }
        }
        started
    }
}

/// The real source: the project's computer's `gh`, through `POST /git/github` (FR-G13). The
/// project's repository is the working directory of its newest unarchived session on that
/// session's computer, as in `api::integrations`.
pub struct NodeCiSource {
    sessions: Arc<Sessions>,
    computers: Arc<Computers>,
    local: Arc<std::sync::Mutex<Option<axum::Router>>>,
}

impl NodeCiSource {
    pub fn new(sessions: Arc<Sessions>, computers: Arc<Computers>) -> NodeCiSource {
        NodeCiSource { sessions, computers, local: crate::api::git::new_local() }
    }

    async fn ask(&self, project: &str) -> anyhow::Result<CiReport> {
        let store = self.sessions.store();
        let Some(p) = store.projects()?.into_iter().find(|p| p.name == project && !p.archived) else {
            return Ok(CiReport::default());
        };
        let Some(rec) = store.sessions(Some(&p.name))?.into_iter().find(|r| !r.archived) else {
            return Ok(CiReport::default());
        };
        let computer_id = match self.computers.registry().session_computer(&rec.id)? {
            Some(sc) => sc.computer_id,
            None => p.computers.iter().find(|c| *c != LOCAL).cloned().unwrap_or_else(|| LOCAL.to_string()),
        };
        let body = json!({ "repo": rec.cwd, "failed_runs": true });
        let path = "/git/github";
        let (status, bytes) = if computer_id == LOCAL {
            crate::api::git::local_call(&self.local, path, &body).await?
        } else {
            self.computers
                .client(&computer_id)?
                .post_raw(path, &body)
                .await
                .map_err(|e| anyhow::anyhow!("computer {computer_id}: {e}"))?
        };
        anyhow::ensure!(status == 200, "node answered {status}");
        parse_node_answer(&serde_json::from_slice(&bytes)?)
    }
}

impl CiSource for NodeCiSource {
    fn report<'a>(&'a self, project: &'a str) -> BoxFuture<'a, anyhow::Result<CiReport>> {
        Box::pin(self.ask(project))
    }
}

/// The node's `/git/github` answer as a report. A node too old to list failed runs is an error,
/// not "no failures".
pub fn parse_node_answer(v: &Value) -> anyhow::Result<CiReport> {
    let runs = v["failed_runs"].as_array().ok_or_else(|| anyhow::anyhow!("the node did not list failed runs (update ember-node)"))?;
    let text = |r: &Value, k: &str| r[k].as_str().map(String::from);
    Ok(CiReport {
        repository: v["name_with_owner"].as_str().map(String::from),
        failures: runs
            .iter()
            .filter_map(|r| {
                Some(CiFailure {
                    id: r["id"].as_u64()?,
                    branch: text(r, "branch"),
                    workflow: text(r, "workflow"),
                    url: text(r, "url"),
                    created_at: r["created_at"].as_str().and_then(|t| timing::parse_rfc3339(t).ok()),
                })
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l23_repository_forms_match() {
        assert_eq!(normalize_repo("Owner/Repo").as_deref(), Some("owner/repo"));
        assert_eq!(normalize_repo("github.com/owner/repo").as_deref(), Some("owner/repo"));
        assert_eq!(normalize_repo("https://github.com/owner/repo.git").as_deref(), Some("owner/repo"));
        assert_eq!(normalize_repo("owner"), None);
        assert_eq!(normalize_repo("a/b/c"), None);
        assert_eq!(normalize_repo("a b/c"), None);
    }

    #[test]
    fn fr_l23_node_answer_parses_and_old_nodes_are_an_error() {
        let r = parse_node_answer(&json!({
            "name_with_owner": "o/r",
            "failed_runs": [{"id": 9, "branch": "main", "workflow": "Test", "url": null, "created_at": "2026-11-13T00:00:00Z"}, {"branch": "x"}]
        }))
        .unwrap();
        assert_eq!(r.repository.as_deref(), Some("o/r"));
        assert_eq!(r.failures.len(), 1);
        assert_eq!(r.failures[0].created_at, Some(timing::parse_rfc3339("2026-11-13T00:00:00Z").unwrap()));
        assert!(parse_node_answer(&json!({"name_with_owner": "o/r"})).is_err());
    }
}
