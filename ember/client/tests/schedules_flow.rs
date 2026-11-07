//! The schedules wrapper against a real ember-server router in-process (SPEC FR-A8, FR-L25).

use std::sync::Arc;

use ember_client::api::Api;
use ember_client::wire::{NewSchedule, RunStatus, ScheduleKind, SchedulePatch, ScheduleTarget};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::schedules::{ManualClock, Scheduler};
use ember_server::session::Sessions;
use ember_server::store::Store;

async fn start() -> Api {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), adapters);
    let scheduler = Scheduler::new(sessions, ManualClock::new(1_700_000_000_000));
    let app = ember_server::schedules::api::router(scheduler);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Api::new(&format!("http://{addr}")).unwrap()
}

fn daily(project: &str, prompt: &str) -> NewSchedule {
    NewSchedule {
        project: project.into(),
        agent: Some("scripted".into()),
        account: None,
        computer: None,
        cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        prompt: prompt.into(),
        kind: ScheduleKind::Cron { expr: "0 9 * * *".into(), tz: "UTC".into() },
        target: ScheduleTarget::New { title: None },
        catch_up: false,
        paused: false,
    }
}

#[tokio::test]
async fn create_list_and_filter_by_project() {
    let api = start().await;
    assert!(api.schedules(None).await.unwrap().is_empty());

    let a = api.create_schedule(&daily("acme", "summarise")).await.unwrap();
    api.create_schedule(&daily("other", "lint")).await.unwrap();
    assert_eq!(a.project, "acme");
    assert_eq!(a.agent, "scripted");
    assert!(!a.paused);
    assert!(a.next_run_at.is_some());

    assert_eq!(api.schedules(None).await.unwrap().len(), 2);
    let only = api.schedules(Some("acme")).await.unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].id, a.id);
}

#[tokio::test]
async fn pause_resume_and_update() {
    let api = start().await;
    let s = api.create_schedule(&daily("acme", "summarise")).await.unwrap();

    let paused = api.pause_schedule(&s.id).await.unwrap();
    assert!(paused.paused);
    assert_eq!(paused.next_run_at, None);

    let resumed = api.resume_schedule(&s.id).await.unwrap();
    assert!(!resumed.paused);
    assert!(resumed.next_run_at.is_some());

    let patch = SchedulePatch {
        prompt: Some("new prompt".into()),
        kind: Some(ScheduleKind::Cron { expr: "30 8 * * 1".into(), tz: "UTC".into() }),
        ..SchedulePatch::default()
    };
    let updated = api.update_schedule(&s.id, &patch).await.unwrap();
    assert_eq!(updated.prompt, "new prompt");
    assert_eq!(updated.kind, ScheduleKind::Cron { expr: "30 8 * * 1".into(), tz: "UTC".into() });
}

#[tokio::test]
async fn run_now_records_a_run_in_the_history() {
    let api = start().await;
    let s = api.create_schedule(&daily("acme", "summarise")).await.unwrap();
    assert!(api.schedule_runs(&s.id, None).await.unwrap().is_empty());

    let run = api.run_schedule(&s.id).await.unwrap();
    assert_eq!(run.schedule_id, s.id);
    assert_ne!(run.status, RunStatus::Missed);

    let runs = api.schedule_runs(&s.id, Some(10)).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, run.id);
}

#[tokio::test]
async fn errors_carry_the_server_status_and_delete_removes() {
    let api = start().await;
    let missing = api.pause_schedule("nope").await.unwrap_err();
    assert_eq!(missing.status(), Some(404));

    let mut bad = daily("acme", "x");
    bad.cwd = None;
    let err = api.create_schedule(&bad).await.unwrap_err();
    assert_eq!(err.status(), Some(400));

    let s = api.create_schedule(&daily("acme", "x")).await.unwrap();
    api.delete_schedule(&s.id).await.unwrap();
    assert!(api.schedules(None).await.unwrap().is_empty());
}
