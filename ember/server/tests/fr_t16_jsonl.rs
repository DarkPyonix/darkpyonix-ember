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
    // Only the empty id is refused; every other id is written under an encoded name (#987).
    assert!(log.append("", &user_message(1, "x")).is_err(), "the empty id must be refused");
    let odd = ["../escape", "a/b", "..", "a\\b", "/etc/x", "x\0y", "a.b"];
    for id in odd {
        assert!(log.append(id, &user_message(1, "x")).unwrap(), "{id:?} is written under an encoded name");
    }
    assert!(!root.path().join("escape.jsonl").exists());
    let all = walk(root.path());
    assert_eq!(all.len(), odd.len(), "one file per id: {all:?}");
    let channels = data.join("channels");
    assert!(all.iter().all(|p| p.parent() == Some(channels.as_path())), "every file is directly in channels/: {all:?}");
}

/// `s` as one URL path segment: every byte outside `A-Za-z0-9-_` percent-encoded.
fn segment(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// The file name `JsonlLog` gives `channel` (FR-T16, #987).
fn name_of(log: &JsonlLog, channel: &str) -> String {
    log.path_of(channel).unwrap().file_name().unwrap().to_str().unwrap().to_string()
}

#[tokio::test]
async fn fr_t16_a_default_channel_of_a_project_named_in_korean_or_with_a_space_takes_a_post() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let projects = ["프로젝트", "my project"];
    for p in projects {
        store.create_session(p, AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap();
    }
    let app = app(&store, dir.path());
    let (_, list) = call(&app, "GET", "/api/channels", None).await;
    for p in projects {
        let id = list.as_array().unwrap().iter().find(|c| c["project"] == p).unwrap()["id"].as_str().unwrap().to_string();
        assert_eq!(id, format!("default-{p}"));
        let (st, m) = call(&app, "POST", &format!("/api/channels/{}/messages", segment(&id)), Some(json!({"text":"안녕하세요"}))).await;
        assert_eq!(st, StatusCode::CREATED, "{p}: {m}");
        let path = JsonlLog::new(dir.path()).path_of(&id).unwrap();
        assert_eq!(path.parent().unwrap(), dir.path().join("channels"));
        let text = std::fs::read_to_string(&path).unwrap();
        let got: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(got.len(), 1, "{p}: one line");
        assert_eq!(got[0]["channel_id"], id.as_str());
        assert_eq!(got[0]["seq"], m["seq"]);
        assert_eq!(got[0]["text"], "안녕하세요");
        // The row stays in SQLite (the append did not fail and roll it back).
        assert_eq!(store.channel(&id).unwrap().messages.len(), 1);
    }
}

#[test]
fn fr_t16_file_names_keep_safe_ids_and_encode_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let log = JsonlLog::new(dir.path());
    // Safe ids keep their names, upper case included.
    for id in ["chan-1", "default-core", "default-Web", "3f2b9c1e-0d4a-4c55-9f1e-2a7b6c8d9e01", "a_b"] {
        assert_eq!(name_of(&log, id), format!("{id}.jsonl"));
    }
    let max = "a".repeat(128);
    assert_eq!(name_of(&log, &max), format!("{max}.jsonl"));
    // Every other id: `~`, then each byte outside a-z 0-9 - _ as %XX.
    assert_eq!(name_of(&log, "default-프로젝트"), "~default-%ED%94%84%EB%A1%9C%EC%A0%9D%ED%8A%B8.jsonl");
    assert_eq!(name_of(&log, "default-my project"), "~default-my%20project.jsonl");
    assert_eq!(name_of(&log, "default-My Project"), "~default-%4Dy%20%50roject.jsonl");
    assert_eq!(name_of(&log, ".."), "~%2E%2E.jsonl");
    assert_eq!(name_of(&log, "~a"), "~%7Ea.jsonl");
    let long = "a".repeat(129);
    assert_eq!(name_of(&log, &long), format!("~{long}.jsonl"));
    // Longer than 200 bytes encoded: `~~` and the SHA-256 of the id in lower-case hex.
    let hashed = name_of(&log, &"가".repeat(100));
    assert!(hashed.starts_with("~~") && hashed.ends_with(".jsonl"), "{hashed}");
    let hex = &hashed[2..hashed.len() - ".jsonl".len()];
    assert_eq!(hex.len(), 64);
    assert!(hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{hashed}");
    // The empty id is refused.
    assert!(log.path_of("").is_err());
}

#[test]
fn fr_t16_encoded_file_names_are_distinct_and_stay_in_the_dir() {
    let dir = tempfile::tempdir().unwrap();
    let log = JsonlLog::new(dir.path());
    let ids: Vec<String> = [
        "..", ".", "../escape", "a/b", "a\\b", "/etc/x", "x\0y", "a.b", "a b", "A b", "a%20b", "~a", "%41", "a:b",
        "default-프로젝트", "default-my project", "chan-1", "default-core",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(["x".repeat(300), format!("{}y", "x".repeat(299)), "가".repeat(100), "나".repeat(100)])
    .collect();
    let mut seen = std::collections::HashSet::new();
    for id in &ids {
        let path = log.path_of(id).unwrap();
        assert_eq!(path.parent().unwrap(), dir.path().join("channels"), "{id:?}");
        let name = name_of(&log, id);
        let stem = name.strip_suffix(".jsonl").unwrap();
        assert!(!stem.is_empty() && stem != "." && stem != "..", "{id:?} -> {name}");
        assert!(!stem.contains(['/', '\\', '.', ':', '\0']), "{id:?} -> {name}");
        assert!(name.len() <= 255, "{id:?} -> {} bytes", name.len());
        // Distinct even on a case-insensitive file system.
        assert!(seen.insert(name.to_ascii_lowercase()), "{id:?} collides: {name}");
    }
    // Writing each one creates exactly one file per id, all in channels/.
    for id in &ids {
        assert!(log.append(id, &user_message(1, "x")).unwrap(), "{id:?}");
    }
    let all = walk(dir.path());
    assert_eq!(all.len(), ids.len(), "{all:?}");
    assert!(all.iter().all(|p| p.parent() == Some(dir.path().join("channels").as_path())), "{all:?}");
}

#[tokio::test]
async fn fr_t16_recovery_writes_the_file_of_an_encoded_channel_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    store.create_session("프로젝트", AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap();
    let id = "default-프로젝트";
    post(&store, id, "one");
    post(&store, id, "two");
    let log = JsonlLog::new(dir.path());
    let report = log.recover(&store);
    assert_eq!(report.repaired_lines, 2);
    assert_eq!(report.skipped, 0);
    let text = std::fs::read_to_string(log.path_of(id).unwrap()).unwrap();
    let got: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(got.iter().map(|l| l["text"].as_str().unwrap()).collect::<Vec<_>>(), vec!["one", "two"]);
    assert!(got.iter().all(|l| l["channel_id"] == id));
    // Up to date now: a second start changes nothing.
    let again = JsonlLog::new(dir.path()).recover(&store);
    assert_eq!((again.repaired_lines, again.truncated_bytes, again.skipped), (0, 0, 0));
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
    // `expected` is cumulative (every human row up to the seq): the file without the middle line is
    // the first line plus the last line of the cumulative text.
    let last = b.lines().last().unwrap();
    let without_middle = format!("{a}{last}\n");
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
    content.extend(std::iter::repeat_n(b'x', 1_200_000));
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
    // The agent is a member of the first channel only; the other channel gets a system message.
    store.post_channel_system_message(&web, "only a system message").unwrap();
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
