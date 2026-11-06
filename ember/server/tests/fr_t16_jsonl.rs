//! FR-T16: the per-channel JSONL file of human messages. Layout is [provisional].

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::channels::jsonl::JsonlLog;
use ember_server::channels::Message;
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn app(store: &Arc<Store>, dir: &Path) -> Router {
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    ember_server::channels::api::router_with_dir(store.clone(), sessions, dir)
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A store with projects `core` and `web`, one agent in `core`; returns the store, the agent id
/// and the default channel ids of `core` and `web`.
async fn fixture(dir: &Path) -> (Arc<Store>, String, String, String, Router) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let agent = store.create_session("core", AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap().id;
    store.create_session("web", AgentKind::Scripted, "/tmp/x", None, "beta").unwrap();
    let app = app(&store, dir);
    let (_, list) = call(&app, "GET", "/api/channels", None).await;
    let id_of = |p: &str| list.as_array().unwrap().iter().find(|c| c["project"] == p).unwrap()["id"].as_str().unwrap().to_string();
    let (core, web) = (id_of("core"), id_of("web"));
    (store, agent, core, web, app)
}

fn lines(dir: &Path, channel: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(dir.join("channels").join(format!("{channel}.jsonl"))).unwrap_or_default();
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[tokio::test]
async fn fr_t16_a_user_message_is_one_line_with_the_right_fields_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (_, _, core, _, app) = fixture(dir.path()).await;
    let uri = format!("/api/channels/{core}/messages");
    let (st, m1) = call(&app, "POST", &uri, Some(json!({"text":"hello"}))).await;
    assert_eq!(st, StatusCode::CREATED);
    let (_, m2) = call(&app, "POST", &uri, Some(json!({"text":"second \"quoted\"\nline"}))).await;
    let got = lines(dir.path(), &core);
    assert_eq!(got.len(), 2, "one line per message, a newline inside a text stays escaped");
    assert_eq!(got[0]["seq"], m1["seq"]);
    assert_eq!(got[0]["channel_id"], core.as_str());
    assert_eq!(got[0]["from"], "user");
    assert_eq!(got[0]["text"], "hello");
    assert_eq!(got[0]["at"], m1["at"]);
    assert_eq!(got[1]["seq"], m2["seq"]);
    assert_eq!(got[1]["text"], "second \"quoted\"\nline");
    assert!(got[0]["seq"].as_i64().unwrap() < got[1]["seq"].as_i64().unwrap());
    let keys: Vec<&str> = got[0].as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(keys.len(), 5);
}

#[tokio::test]
async fn fr_t16_an_agent_message_is_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let (store, agent, core, _, app) = fixture(dir.path()).await;
    let uri = format!("/api/channels/{core}/messages");
    let (st, _) = call(&app, "POST", &uri, Some(json!({"text":"done","from":agent}))).await;
    assert_eq!(st, StatusCode::CREATED);
    assert!(lines(dir.path(), &core).is_empty());
    assert!(!dir.path().join("channels").join(format!("{core}.jsonl")).exists());
    // It is still in the SQLite log.
    assert_eq!(store.channel(&core).unwrap().messages.len(), 1);
}

#[tokio::test]
async fn fr_t16_two_channels_write_different_files() {
    let dir = tempfile::tempdir().unwrap();
    let (_, _, core, web, app) = fixture(dir.path()).await;
    call(&app, "POST", &format!("/api/channels/{core}/messages"), Some(json!({"text":"in core"}))).await;
    call(&app, "POST", &format!("/api/channels/{web}/messages"), Some(json!({"text":"in web"}))).await;
    let (c, w) = (lines(dir.path(), &core), lines(dir.path(), &web));
    assert_eq!((c.len(), w.len()), (1, 1));
    assert_eq!(c[0]["text"], "in core");
    assert_eq!(w[0]["text"], "in web");
    assert_eq!(w[0]["channel_id"], web.as_str());
}

fn user_message(seq: i64, text: &str) -> Message {
    Message { id: seq.to_string(), seq, from: "user".into(), text: text.into(), at: 1, unread: 0, mentions: Vec::new(), author_name: "나".into(), author_kind: "person".into() }
}

#[test]
fn fr_t16_a_restarted_writer_appends_and_never_truncates() {
    let dir = tempfile::tempdir().unwrap();
    JsonlLog::new(dir.path()).append("chan-1", &user_message(1, "before")).unwrap();
    // A new writer on the same directory, as after a restart of the main server.
    JsonlLog::new(dir.path()).append("chan-1", &user_message(2, "after")).unwrap();
    let got = lines(dir.path(), "chan-1");
    assert_eq!(got.iter().map(|l| l["text"].as_str().unwrap()).collect::<Vec<_>>(), vec!["before", "after"]);
}

#[test]
fn fr_t16_an_unsafe_channel_id_is_not_written_outside_the_dir() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let log = JsonlLog::new(&data);
    for bad in ["../escape", "a/b", "..", "", "a\\b", "/etc/x", "x\0y", "a.b"] {
        assert!(log.append(bad, &user_message(1, "x")).is_err(), "{bad:?} must be refused");
    }
    assert!(!root.path().join("escape.jsonl").exists());
    let mut all = Vec::new();
    for e in walk(root.path()) {
        all.push(e);
    }
    assert!(all.iter().all(|p| !p.to_string_lossy().ends_with(".jsonl")), "nothing written: {all:?}");
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test]
async fn fr_t16_a_failed_append_is_500_and_leaves_no_row() {
    let dir = tempfile::tempdir().unwrap();
    // `channels` is a file, so the directory cannot be created.
    std::fs::write(dir.path().join("channels"), b"in the way").unwrap();
    let (store, _, core, _, app) = fixture(dir.path()).await;
    let (st, e) = call(&app, "POST", &format!("/api/channels/{core}/messages"), Some(json!({"text":"lost"}))).await;
    assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(e["code"], "internal");
    assert!(store.channel(&core).unwrap().messages.is_empty(), "the SQLite row is rolled back");
}

// ---- Start-up recovery and hardening (FR-T16, provisional 2026-11-07) ----

fn file_of(dir: &Path, channel: &str) -> std::path::PathBuf {
    dir.join("channels").join(format!("{channel}.jsonl"))
}

fn post(store: &Store, channel: &str, text: &str) -> Message {
    store.post_channel_message(channel, "user", text).unwrap()
}

/// The exact bytes `append` writes for the human rows of `channel` with `seq <= upto`.
fn expected(store: &Store, channel: &str, upto: i64) -> String {
    let d = tempfile::tempdir().unwrap();
    let log = JsonlLog::new(d.path());
    for m in store.channel_messages_after(channel, 0, None, false).unwrap() {
        if m.seq <= upto {
            log.append(channel, &m).unwrap();
        }
    }
    std::fs::read_to_string(file_of(d.path(), channel)).unwrap_or_default()
}

fn write_file(dir: &Path, channel: &str, content: &[u8]) {
    std::fs::create_dir_all(dir.join("channels")).unwrap();
    std::fs::write(file_of(dir, channel), content).unwrap();
}

#[tokio::test]
async fn fr_t16_recovery_cuts_a_partial_last_line_and_re_appends_from_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    let m1 = post(&store, &core, "one");
    let m2 = post(&store, &core, "two");
    let m3 = post(&store, &core, "three");
    let full = expected(&store, &core, m3.seq);
    let upto1 = expected(&store, &core, m1.seq);
    let fragment = &expected(&store, &core, m2.seq)[upto1.len()..upto1.len() + 15];
    write_file(dir.path(), &core, format!("{upto1}{fragment}").as_bytes());
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!(report.truncated_bytes, 15);
    assert_eq!(report.repaired_lines, 2);
    assert_eq!(report.skipped, 0);
    assert_eq!(std::fs::read_to_string(file_of(dir.path(), &core)).unwrap(), full);
}

#[tokio::test]
async fn fr_t16_recovery_appends_the_rows_missing_at_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    let m1 = post(&store, &core, "one");
    post(&store, &core, "two");
    let m3 = post(&store, &core, "three");
    write_file(dir.path(), &core, expected(&store, &core, m1.seq).as_bytes());
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!((report.repaired_lines, report.truncated_bytes), (2, 0));
    assert_eq!(std::fs::read_to_string(file_of(dir.path(), &core)).unwrap(), expected(&store, &core, m3.seq));
}

#[tokio::test]
async fn fr_t16_recovery_leaves_an_up_to_date_file_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    post(&store, &core, "one");
    let m2 = post(&store, &core, "two");
    let full = expected(&store, &core, m2.seq);
    write_file(dir.path(), &core, full.as_bytes());
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!((report.repaired_lines, report.truncated_bytes, report.skipped), (0, 0, 0));
    assert_eq!(std::fs::read(file_of(dir.path(), &core)).unwrap(), full.as_bytes());
}

#[tokio::test]
async fn fr_t16_recovery_writes_everything_to_a_missing_or_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, web, _) = fixture(dir.path()).await;
    post(&store, &core, "one");
    let last = post(&store, &core, "two");
    let w = post(&store, &web, "web one");
    write_file(dir.path(), &web, b"");
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!(report.repaired_lines, 3);
    assert_eq!(std::fs::read_to_string(file_of(dir.path(), &core)).unwrap(), expected(&store, &core, last.seq));
    assert_eq!(std::fs::read_to_string(file_of(dir.path(), &web)).unwrap(), expected(&store, &web, w.seq));
    // A second run changes nothing.
    assert_eq!(JsonlLog::new(dir.path()).recover(&store).repaired_lines, 0);
}

#[tokio::test]
async fn fr_t16_recovery_does_not_repair_a_gap_in_the_middle() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    let m1 = post(&store, &core, "one");
    post(&store, &core, "two");
    let m3 = post(&store, &core, "three");
    let (a, b) = (expected(&store, &core, m1.seq), expected(&store, &core, m3.seq));
    let without_middle = format!("{a}{}", &b[b.rfind("{\"seq\":").unwrap()..]);
    write_file(dir.path(), &core, without_middle.as_bytes());
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!((report.repaired_lines, report.truncated_bytes), (0, 0));
    assert_eq!(std::fs::read_to_string(file_of(dir.path(), &core)).unwrap(), without_middle);
}

#[cfg(unix)]
#[tokio::test]
async fn fr_t16_a_symlinked_file_is_skipped_by_recovery_and_refused_by_append() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    let m = post(&store, &core, "one");
    let target = dir.path().join("target.txt");
    std::fs::write(&target, b"keep\n").unwrap();
    std::fs::create_dir_all(dir.path().join("channels")).unwrap();
    std::os::unix::fs::symlink(&target, file_of(dir.path(), &core)).unwrap();
    let log = JsonlLog::new(dir.path());
    let report = log.recover(&store);
    assert_eq!((report.skipped, report.repaired_lines), (1, 0));
    assert_eq!(std::fs::read(&target).unwrap(), b"keep\n");
    assert!(log.append(&core, &m).is_err(), "O_NOFOLLOW refuses the symlink");
    assert_eq!(std::fs::read(&target).unwrap(), b"keep\n");
}

#[tokio::test]
async fn fr_t16_a_directory_named_like_the_file_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    post(&store, &core, "one");
    std::fs::create_dir_all(file_of(dir.path(), &core)).unwrap();
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!((report.skipped, report.repaired_lines), (1, 0));
    assert!(file_of(dir.path(), &core).is_dir());
}

#[tokio::test]
async fn fr_t16_a_file_with_an_unparseable_last_line_is_skipped_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, web, _) = fixture(dir.path()).await;
    post(&store, &core, "one");
    post(&store, &web, "web");
    write_file(dir.path(), &core, b"{\"seq\":1}\nnot json\n");
    write_file(dir.path(), &web, b"{\"seq\":1}\n{\"no_seq\":true}\n");
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!((report.skipped, report.repaired_lines, report.truncated_bytes), (2, 0, 0));
    assert_eq!(std::fs::read(file_of(dir.path(), &core)).unwrap(), b"{\"seq\":1}\nnot json\n");
    assert_eq!(std::fs::read(file_of(dir.path(), &web)).unwrap(), b"{\"seq\":1}\n{\"no_seq\":true}\n");
}

#[tokio::test]
async fn fr_t16_a_fragment_longer_than_the_window_is_skipped_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    post(&store, &core, "one");
    let mut content = b"{\"seq\":1}\n".to_vec();
    content.extend(std::iter::repeat(b'x').take(1_200_000));
    write_file(dir.path(), &core, &content);
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!((report.skipped, report.repaired_lines, report.truncated_bytes), (1, 0, 0));
    assert_eq!(std::fs::read(file_of(dir.path(), &core)).unwrap(), content);
}

#[tokio::test]
async fn fr_t16_a_file_that_is_one_short_fragment_is_truncated_to_zero_and_refilled() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    let m = post(&store, &core, "one");
    write_file(dir.path(), &core, b"{\"seq\":1,\"chan");
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!((report.truncated_bytes, report.repaired_lines), (14, 1));
    assert_eq!(std::fs::read_to_string(file_of(dir.path(), &core)).unwrap(), expected(&store, &core, m.seq));
}

#[tokio::test]
async fn fr_t16_recovery_never_writes_agent_or_system_messages() {
    let dir = tempfile::tempdir().unwrap();
    let (store, agent, core, web, _) = fixture(dir.path()).await;
    store.post_channel_message(&core, &agent, "agent says").unwrap();
    store.post_channel_system_message(&core, "not reached").unwrap();
    let m = post(&store, &core, "human");
    store.post_channel_message(&web, &agent, "only an agent").unwrap();
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!(report.repaired_lines, 1);
    assert_eq!(std::fs::read_to_string(file_of(dir.path(), &core)).unwrap(), expected(&store, &core, m.seq));
    assert!(!file_of(dir.path(), &web).exists(), "a channel with no human row gets no file");
}

#[tokio::test]
async fn fr_t16_recovery_does_not_fail_when_the_channels_dir_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("channels"), b"in the way").unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    post(&store, &core, "one");
    let report = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!(report.repaired_lines, 0);
    assert!(report.skipped >= 1);
    assert_eq!(std::fs::read(dir.path().join("channels")).unwrap(), b"in the way");
}

#[tokio::test]
async fn fr_t16_over_the_cap_append_returns_false_and_the_row_stays() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _, core, _, _) = fixture(dir.path()).await;
    let log = JsonlLog::with_max_file_bytes(dir.path(), 10);
    let m1 = post(&store, &core, "first line is longer than ten bytes");
    let m2 = post(&store, &core, "second");
    assert!(log.append(&core, &m1).unwrap(), "an empty file takes the first line");
    let size = std::fs::metadata(file_of(dir.path(), &core)).unwrap().len();
    assert!(size > 10);
    assert!(!log.append(&core, &m2).unwrap(), "over the cap: Ok(false)");
    assert_eq!(std::fs::metadata(file_of(dir.path(), &core)).unwrap().len(), size, "nothing was written");
    assert_eq!(store.channel(&core).unwrap().messages.len(), 2, "the rows stay in SQLite");
    // Recovery leaves a file over the cap alone too.
    let report = JsonlLog::with_max_file_bytes(dir.path(), 10).recover(&store);
    assert_eq!((report.skipped, report.repaired_lines), (1, 0));
}

#[cfg(unix)]
#[test]
fn fr_t16_a_new_file_is_mode_0600_and_a_new_directory_0700() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    JsonlLog::new(dir.path()).append("chan-1", &user_message(1, "x")).unwrap();
    let mode = |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(dir.path().join("channels")), 0o700);
    assert_eq!(mode(file_of(dir.path(), "chan-1")), 0o600);
}

#[cfg(unix)]
#[test]
fn fr_t16_an_existing_directory_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let channels = dir.path().join("channels");
    std::fs::create_dir(&channels).unwrap();
    std::fs::set_permissions(&channels, std::fs::Permissions::from_mode(0o755)).unwrap();
    JsonlLog::new(dir.path()).append("chan-1", &user_message(1, "x")).unwrap();
    assert_eq!(std::fs::metadata(&channels).unwrap().permissions().mode() & 0o777, 0o755);
}
