//! FR-L23, FR-A8: event routines fire on a CI failure. A fake CI source stands in for `gh`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::events::AgentEvent;
use ember_server::schedules::ci::{CiFailure, CiReport, CiSource, CiWatcher};
use ember_server::schedules::{EventOn, ManualClock, NewSchedule, RunStatus, ScheduleKind, ScheduleTarget, Scheduler};
use ember_server::session::Sessions;
use ember_server::store::Store;
use futures::future::BoxFuture;

/// 2027-01-15T08:00:00Z.
const T0: i64 = 1_800_000_000_000;

#[derive(Default)]
struct FakeCi {
    reports: Mutex<HashMap<String, CiReport>>,
    calls: Mutex<Vec<String>>,
}

impl FakeCi {
    fn set(&self, project: &str, repo: &str, failures: Vec<CiFailure>) {
        self.reports.lock().unwrap().insert(project.into(), CiReport { repository: Some(repo.into()), failures });
    }
}

impl CiSource for FakeCi {
    fn report<'a>(&'a self, project: &'a str) -> BoxFuture<'a, anyhow::Result<CiReport>> {
        self.calls.lock().unwrap().push(project.to_string());
        let r = self.reports.lock().unwrap().get(project).cloned();
        Box::pin(async move { r.ok_or_else(|| anyhow::anyhow!("no CI for this project")) })
    }
}

fn failure(id: u64, branch: &str, at: i64) -> CiFailure {
    CiFailure { id, branch: Some(branch.into()), workflow: Some("Test".into()), url: Some(format!("https://github.com/o/r/actions/runs/{id}")), created_at: Some(at) }
}

fn ci_routine(project: &str, repo: &str, branch: Option<&str>) -> NewSchedule {
    NewSchedule {
        project: project.into(),
        agent: Some("scripted".into()),
        account: None,
        computer: None,
        cwd: Some("/w/ci".into()),
        prompt: "fix the failing build".into(),
        kind: ScheduleKind::Event { on: EventOn::CiFailed, repository: repo.into(), branch: branch.map(String::from) },
        target: ScheduleTarget::New { title: Some("CI fix".into()) },
        catch_up: false,
        paused: false,
    }
}

fn sessions_on(store: Arc<Store>) -> Arc<Sessions> {
    Sessions::new(store, vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>])
}

fn first_user_message(s: &Sessions, id: &str) -> Option<String> {
    s.store().events_after(id, 0).unwrap().into_iter().find_map(|e| match e.event {
        AgentEvent::UserMessage { text } => Some(text),
        _ => None,
    })
}

async fn wait_message(s: &Sessions, id: &str) -> String {
    for _ in 0..1000 {
        if let Some(m) = first_user_message(s, id) {
            return m;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no message in {id}");
}

#[tokio::test]
async fn fr_l23_ci_failure_starts_the_routine_once() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = sessions_on(store);
    let clock = ManualClock::new(T0);
    let sched = Scheduler::new(sessions.clone(), clock.clone());
    let ci = Arc::new(FakeCi::default());
    let watcher = CiWatcher::new(sched.clone(), ci.clone());

    let routine = sched.create(ci_routine("acme", "github.com/O/R", Some("main")), None).unwrap();
    assert_eq!(routine.next_run_at, None, "an event routine has no planned trigger");
    assert!(sched.tick().await.is_empty(), "time never fires it");

    // A failure from before the routine existed, one on another branch, one that counts.
    ci.set("acme", "o/r", vec![failure(1, "main", T0 - 1), failure(2, "dev", T0 + 10), failure(3, "main", T0 + 20)]);
    let runs = watcher.poll().await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    let run = &runs[0];
    assert_eq!((run.schedule_id.as_str(), run.scheduled_at, run.status), (routine.id.as_str(), T0 + 20, RunStatus::Running));
    let session = run.session_id.clone().expect("a new session");
    let rec = sessions.store().session(&session).unwrap().unwrap();
    assert_eq!((rec.project.as_str(), rec.title.as_str(), rec.agent), ("acme", "CI fix", AgentKind::Scripted));
    let msg = wait_message(&sessions, &session).await;
    assert!(msg.starts_with("[ember schedule] Event routine"), "{msg}");
    assert!(msg.contains("run 3 of o/r failed") && msg.ends_with("fix the failing build"), "{msg}");

    // The same failure seen again does not start it again.
    assert!(watcher.poll().await.is_empty());
    assert_eq!(sched.runs(&routine.id, 10).unwrap().len(), 1);
    assert_eq!(sched.get(&routine.id).unwrap().last_run_at, Some(T0 + 20));

    // A new failure does.
    ci.set("acme", "o/r", vec![failure(3, "main", T0 + 20), failure(4, "main", T0 + 30)]);
    let runs = watcher.poll().await;
    assert_eq!(runs.iter().map(|r| r.scheduled_at).collect::<Vec<_>>(), [T0 + 30]);

    // Paused: not asked, not fired. Another repository: not fired.
    sched.pause(&routine.id).unwrap();
    ci.calls.lock().unwrap().clear();
    ci.set("acme", "o/r", vec![failure(5, "main", T0 + 40)]);
    assert!(watcher.poll().await.is_empty());
    assert!(ci.calls.lock().unwrap().is_empty(), "no unpaused routine, no CI call");
    sched.resume(&routine.id).unwrap();
    assert_eq!(sched.get(&routine.id).unwrap().next_run_at, None);
    let other = sched.create(ci_routine("acme", "o/other", None), None).unwrap();
    let runs = watcher.poll().await;
    assert!(runs.iter().all(|r| r.schedule_id != other.id), "{runs:?}");
    assert_eq!(runs.len(), 1, "the resumed routine takes run 5");

    // A source error is skipped, not fatal.
    ci.reports.lock().unwrap().clear();
    assert!(watcher.poll().await.is_empty());
}

#[tokio::test]
async fn fr_l23_ci_failures_do_not_refire_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ember.db");
    let ci = Arc::new(FakeCi::default());
    ci.set("acme", "o/r", vec![failure(7, "main", T0 + 5)]);
    let id = {
        let sessions = sessions_on(Arc::new(Store::open(&path).unwrap()));
        let sched = Scheduler::new(sessions, ManualClock::new(T0));
        let routine = sched.create(ci_routine("acme", "o/r", None), None).unwrap();
        assert_eq!(CiWatcher::new(sched.clone(), ci.clone()).poll().await.len(), 1);
        routine.id
    };
    let sessions = sessions_on(Arc::new(Store::open(&path).unwrap()));
    let sched = Scheduler::new(sessions, ManualClock::new(T0 + 60_000));
    assert!(CiWatcher::new(sched.clone(), ci.clone()).poll().await.is_empty(), "run 7 was already handled");
    assert_eq!(sched.runs(&id, 10).unwrap().len(), 1);
}

#[tokio::test]
async fn fr_l23_event_routines_validate_and_round_trip() {
    let sessions = sessions_on(Arc::new(Store::open_in_memory().unwrap()));
    let sched = Scheduler::new(sessions.clone(), ManualClock::new(T0));
    assert!(sched.create(ci_routine("acme", "not-a-repo", None), None).is_err());
    assert!(sched.create(ci_routine("acme", "o/r", Some(" ")), None).is_err());
    let s = sched.create(ci_routine("acme", "o/r", None), None).unwrap();
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["kind"], serde_json::json!({"type": "event", "on": "ci_failed", "repository": "o/r"}));
    let parsed: ScheduleKind = serde_json::from_value(serde_json::json!({"type": "event", "on": "ci_failed", "repository": "o/r", "branch": "main"})).unwrap();
    assert_eq!(parsed, ScheduleKind::Event { on: EventOn::CiFailed, repository: "o/r".into(), branch: Some("main".into()) });
    // run-now still works for an event routine (a manual test run).
    let run = sched.run_now(&s.id).await.unwrap();
    assert!(run.session_id.is_some());
}
