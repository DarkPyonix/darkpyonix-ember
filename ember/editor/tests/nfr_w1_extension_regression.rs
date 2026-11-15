//! NFR-W1 (#84): the extension regression list, and the client that makes a real OSE activate it.
//!
//! Two tests:
//!
//! * `nfr_w1_regression_data_is_consistent` runs everywhere. It reads
//!   `ember/ose/regression/extensions.json` and `expected.json` and checks what
//!   `ember/ose/regress.sh` relies on: the same ids in both, a pinned version or `null`, and an
//!   `open` file that exists in `ember/ose/regression/workspace/`.
//! * `nfr_w1_regression_client` is the activation client of `ember/ose/regress.sh`. The extension
//!   host of an OSE only starts when a client connects, so the harness starts the server with the
//!   listed extensions installed and then runs this test, which connects an `EditorSession`,
//!   opens every `open` file (that sends `$activateByEvent("onLanguage:<id>")`) and waits. It
//!   decides nothing: the harness reads the extension host log afterwards and compares it with
//!   `expected.json`. Ignored by default and gated on `OSE_REGRESS_ADDR`:
//!
//! ```sh
//! export OSE_REGRESS_CLIENT='cargo test --locked --manifest-path ember/editor/Cargo.toml --test nfr_w1_extension_regression -- --ignored --nocapture'
//! ember/ose/regress.sh "$(dirname "$(dirname "$EMBER_OSE_SERVER")")"
//! ```
//!
//! Environment the harness sets: `OSE_REGRESS_ADDR` (`127.0.0.1:<port>` of the running server),
//! `OSE_REGRESS_WS` (a copy of `regression/workspace/` on the server's machine). Optional:
//! `OSE_REGRESS_SETTLE` (seconds to stay connected after the last open, default 30).

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use ember_editor::{EditorSession, SessionConfig, SessionUpdate};
use serde_json::Value;

fn regression_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ose/regression")
}

fn read_json(name: &str) -> Value {
    let path = regression_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// One entry of `extensions.json`.
#[derive(Debug)]
struct Entry {
    id: String,
    version: Option<String>,
    open: Option<String>,
}

fn entries() -> Vec<Entry> {
    let doc = read_json("extensions.json");
    let list = doc.get("extensions").and_then(Value::as_array).expect("extensions.json has an `extensions` array");
    list.iter()
        .map(|e| {
            let obj = e.as_object().unwrap_or_else(|| panic!("every entry is an object {{id, version, open}}: {e}"));
            let text = |key: &str| match obj.get(key) {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(other) => panic!("`{key}` is a string or null: {other}"),
            };
            Entry { id: text("id").unwrap_or_else(|| panic!("an entry has no id: {e}")), version: text("version"), open: text("open") }
        })
        .collect()
}

/// `publisher.name`, as the server route of FR-W4 accepts it.
fn valid_id(id: &str) -> bool {
    let part = |s: &str| !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    matches!(id.split_once('.'), Some((publisher, name)) if part(publisher) && part(name))
}

/// `1.2.3` with an optional `-suffix` or `+suffix`: an exact version, never a range or a tag.
fn valid_version(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or_default();
    let nums: Vec<&str> = core.split('.').collect();
    nums.len() == 3 && nums.iter().all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// A path that stays inside the workspace copy.
fn relative_inside(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}

#[test]
fn nfr_w1_regression_data_is_consistent() {
    let entries = entries();
    assert!(!entries.is_empty(), "the list is not empty");

    let mut ids = BTreeSet::new();
    for e in &entries {
        assert!(valid_id(&e.id), "{}: not a publisher.name id", e.id);
        assert!(ids.insert(e.id.to_lowercase()), "{}: listed twice", e.id);
        if let Some(v) = &e.version {
            assert!(valid_version(v), "{}: version {v:?} is not an exact x.y.z version", e.id);
        }
        if let Some(open) = &e.open {
            assert!(relative_inside(open), "{}: open {open:?} must be a relative path inside the workspace", e.id);
            let file = regression_dir().join("workspace").join(open);
            assert!(file.is_file(), "{}: open file {} does not exist", e.id, file.display());
        }
    }

    let expected = read_json("expected.json");
    let results = expected.get("results").and_then(Value::as_object).expect("expected.json has a `results` object");
    let expected_ids: BTreeSet<String> = results.keys().map(|k| k.to_lowercase()).collect();
    assert_eq!(expected_ids, ids, "extensions.json and expected.json list the same ids");
    for (id, spec) in results {
        match spec.get("expect").and_then(Value::as_str) {
            Some("pass") => {}
            Some("known-fail") => {
                let reason = spec.get("reason").and_then(Value::as_str).unwrap_or_default();
                assert!(!reason.trim().is_empty(), "{id}: a known-fail entry says why");
            }
            other => panic!("{id}: expect is \"pass\" or \"known-fail\", got {other:?}"),
        }
    }
}

#[test]
fn nfr_w1_id_and_version_rules() {
    assert!(valid_id("redhat.vscode-yaml") && valid_id("golang.Go"));
    assert!(!valid_id("vscode-yaml") && !valid_id("a.b.c") && !valid_id("-a.b") && !valid_id("a.b@1.0.0"));
    assert!(valid_version("2.0.0") && valid_version("1.2.3-beta.1") && valid_version("2026.4.0"));
    assert!(!valid_version("latest") && !valid_version("1.2") && !valid_version("^1.2.3") && !valid_version("1.2.x"));
    assert!(relative_inside("src/main.rs") && !relative_inside("../x") && !relative_inside("/etc/passwd") && !relative_inside(""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "the activation client of ember/ose/regress.sh: needs OSE_REGRESS_ADDR and OSE_REGRESS_WS"]
async fn nfr_w1_regression_client() {
    let (Some(addr), Some(ws)) = (std::env::var("OSE_REGRESS_ADDR").ok(), std::env::var_os("OSE_REGRESS_WS").map(PathBuf::from)) else {
        eprintln!("[regress-client] OSE_REGRESS_ADDR or OSE_REGRESS_WS not set; skipping");
        return;
    };
    let settle = std::env::var("OSE_REGRESS_SETTLE").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(30);
    // The server resolves symlinks in what it reports (macOS `/var` is `/private/var`).
    let ws = std::fs::canonicalize(&ws).unwrap_or_else(|e| panic!("OSE_REGRESS_WS {}: {e}", ws.display()));
    let entries = entries();

    let config = SessionConfig::new(addr.clone(), ws.to_str().expect("a UTF-8 workspace path"));
    let (session, mut rx) = tokio::time::timeout(Duration::from_secs(180), EditorSession::connect_tcp(config))
        .await
        .expect("connect timed out")
        .unwrap_or_else(|e| panic!("connect to {addr}: {e}"));
    println!("[regress-client] connected to {addr}, workspace {}", ws.display());

    // What Ember's own scan sees, and what it handed to the extension host. Printed only: the
    // harness decides from the extension host log.
    for e in &entries {
        let scanned = session.installed_extensions().iter().find(|x| x.id.eq_ignore_ascii_case(&e.id)).map(|x| x.version.clone());
        let running = session.running_extensions().iter().any(|x| x.id.eq_ignore_ascii_case(&e.id));
        println!(
            "[regress-client] {}: pinned {}, scanned {}, given to the extension host: {running}",
            e.id,
            e.version.as_deref().unwrap_or("none"),
            scanned.as_deref().unwrap_or("not in the scan"),
        );
    }

    let mut lost = false;
    for e in &entries {
        let Some(open) = &e.open else { continue };
        let path = ws.join(open);
        let uri = session.uri(path.to_str().expect("a UTF-8 path"));
        if let Err(err) = session.open(uri.clone()) {
            println!("[regress-client] {}: open {open} not sent: {err}", e.id);
            continue;
        }
        // Wait for the answer so the opens do not pile up; a failed open is reported, not fatal.
        let answer = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match rx.recv().await {
                    Some(SessionUpdate::Opened { uri: u, language_id, .. }) if u.path == uri.path => return Ok(language_id),
                    Some(SessionUpdate::OpenFailed { uri: u, error }) if u.path == uri.path => return Err(error),
                    Some(SessionUpdate::ConnectionLost { reason }) => return Err(format!("connection lost: {reason}")),
                    Some(_) => {}
                    None => return Err("session ended".to_string()),
                }
            }
        })
        .await;
        match answer {
            Ok(Ok(language)) => println!("[regress-client] {}: opened {open} as {language}", e.id),
            Ok(Err(error)) => {
                println!("[regress-client] {}: open {open} failed: {error}", e.id);
                if error.starts_with("connection lost") || error == "session ended" {
                    lost = true;
                    break;
                }
            }
            Err(_) => println!("[regress-client] {}: no answer to open {open} within 30 s", e.id),
        }
    }

    // Stay connected while the extensions activate (language servers start, downloads finish).
    if !lost {
        let until = tokio::time::Instant::now() + Duration::from_secs(settle);
        while let Ok(update) = tokio::time::timeout_at(until, rx.recv()).await {
            match update {
                Some(SessionUpdate::ConnectionLost { reason }) => {
                    println!("[regress-client] connection lost while settling: {reason}");
                    lost = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
    }
    session.shutdown().await;
    assert!(!lost, "the connection to the OSE was lost before the settle time ended; the log comparison would not be trustworthy");
    println!("[regress-client] done after {settle} s of settle time");
}
