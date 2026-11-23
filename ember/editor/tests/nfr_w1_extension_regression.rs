//! NFR-W1 (#84): the extension regression list, and the client that makes a real OSE activate it.
//!
//! The tests:
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
//! * `nfr_w1_deep_checks` is the deep client (#929), run by the same command when the harness has
//!   `OSE_REGRESS_MODE=deep` (the activation client skips itself then, and this test skips itself
//!   otherwise). For every entry with a `deep` check it makes that check through the session and
//!   writes `{"deep": pass|fail|not-run, "reason"}` to `$OSE_REGRESS_DEEP/<id>.json`; the harness
//!   compares those with `expected.json`. The webview check runs the view command, waits for the
//!   panel and its HTML, reports the page shown and ready, sends the entry's message and waits for
//!   the answer. The debug check is not built (the client has no debug service, #445): it reports
//!   `not-run`, and `not-run: no python3` when the machine has no `python3`.
//!
//! Environment the harness sets: `OSE_REGRESS_ADDR` (`127.0.0.1:<port>` of the running server),
//! `OSE_REGRESS_WS` (a copy of `regression/workspace/` on the server's machine), `OSE_REGRESS_MODE`
//! (`activation` or `deep`), `OSE_REGRESS_DEEP` (the folder for deep results). Optional:
//! `OSE_REGRESS_SETTLE` (seconds to stay connected after the last open, default 30).

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use ember_editor::webview::{WebviewEvent, WebviewKind, WebviewMessage, WebviewUpdate};
use ember_editor::{EditorSession, SessionConfig, SessionUpdate};
use serde_json::{json, Value};

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
    /// The deeper check of the deep run (#929): `{check: webview, ...}` or `{check: debug, ...}`.
    deep: Option<Value>,
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
            let deep = obj.get("deep").filter(|d| !d.is_null()).cloned();
            Entry { id: text("id").unwrap_or_else(|| panic!("an entry has no id: {e}")), version: text("version"), open: text("open"), deep }
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

/// The `deep` field of an entry (#929).
#[derive(Debug, Clone, PartialEq)]
enum DeepCheck {
    /// Run `command`, wait for its panel and HTML, send `send`, wait for a message whose `command` is `reply`.
    Webview { command: String, send: Value, reply: String },
    /// Debug `program` (relative to the workspace) with a breakpoint at 1-based `line`.
    Debug { program: String, line: u32 },
}

impl DeepCheck {
    fn from_json(v: &Value) -> Result<Self, String> {
        let text = |key: &str| v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
        match v.get("check").and_then(Value::as_str) {
            Some("webview") => Ok(Self::Webview {
                command: text("command").ok_or("webview: `command` is the view command id")?,
                send: v.get("send").filter(|s| s.is_object()).cloned().ok_or("webview: `send` is the JSON object sent to the panel")?,
                reply: text("reply").ok_or("webview: `reply` is the `command` of the expected answer")?,
            }),
            Some("debug") => {
                let program = text("program").filter(|p| relative_inside(p)).ok_or("debug: `program` is a relative path inside the workspace")?;
                let line = v.get("line").and_then(Value::as_u64).filter(|l| *l >= 1 && *l <= u64::from(u32::MAX));
                Ok(Self::Debug { program, line: line.ok_or("debug: `line` is a 1-based line number")? as u32 })
            }
            other => Err(format!("`check` is webview or debug, got {other:?}")),
        }
    }
}

/// Whether a message from the extension to its page is the expected answer: a JSON object whose
/// `command` is `reply` (Git Graph's `ResponseMessage` shape).
fn reply_matches(json_text: &str, reply: &str) -> bool {
    serde_json::from_str::<Value>(json_text).ok().and_then(|v| v.get("command").and_then(Value::as_str).map(|c| c == reply)).unwrap_or(false)
}

/// One deep check's outcome, as `regress.py compare-deep` reads it from `<OSE_REGRESS_DEEP>/<id>.json`.
#[derive(Debug, Clone, PartialEq)]
struct DeepResult {
    status: &'static str,
    reason: String,
}

impl DeepResult {
    fn pass(reason: impl Into<String>) -> Self {
        Self { status: "pass", reason: reason.into() }
    }
    fn fail(reason: impl Into<String>) -> Self {
        Self { status: "fail", reason: reason.into() }
    }
    fn not_run(reason: impl Into<String>) -> Self {
        Self { status: "not-run", reason: reason.into() }
    }
    fn to_json(&self) -> Value {
        json!({"deep": self.status, "reason": self.reason})
    }
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

    // Deep checks (#929): the same ids carry a check in extensions.json and an expectation here.
    let listed_deep: BTreeSet<String> = entries.iter().filter(|e| e.deep.is_some()).map(|e| e.id.to_lowercase()).collect();
    let expected_deep: BTreeSet<String> = results.iter().filter(|(_, v)| v.get("deep").is_some()).map(|(k, _)| k.to_lowercase()).collect();
    assert_eq!(listed_deep, expected_deep, "extensions.json `deep` and expected.json `deep` name the same ids");
    for e in &entries {
        if let Some(deep) = &e.deep {
            if let Err(problem) = DeepCheck::from_json(deep) {
                panic!("{}: {problem}", e.id);
            }
            if let Ok(DeepCheck::Debug { program, .. }) = DeepCheck::from_json(deep) {
                assert!(regression_dir().join("workspace").join(&program).is_file(), "{}: debug program {program} does not exist", e.id);
            }
        }
    }
    for (id, spec) in results {
        let Some(deep) = spec.get("deep") else { continue };
        match deep.get("expect").and_then(Value::as_str) {
            Some("pass") => {}
            Some("known-fail" | "not-built") => {
                let reason = deep.get("reason").and_then(Value::as_str).unwrap_or_default();
                assert!(!reason.trim().is_empty(), "{id}: a deep known-fail or not-built entry says why");
            }
            other => panic!("{id}: deep.expect is pass, known-fail or not-built, got {other:?}"),
        }
    }
}

#[test]
fn nfr_w1_deep_check_shapes() {
    let web = json!({"check": "webview", "command": "git-graph.view", "send": {"command": "loadRepos", "check": false}, "reply": "loadRepos"});
    assert_eq!(
        DeepCheck::from_json(&web),
        Ok(DeepCheck::Webview { command: "git-graph.view".into(), send: json!({"command": "loadRepos", "check": false}), reply: "loadRepos".into() })
    );
    assert_eq!(
        DeepCheck::from_json(&json!({"check": "debug", "program": "sample.py", "line": 2})),
        Ok(DeepCheck::Debug { program: "sample.py".into(), line: 2 })
    );
    for bad in [
        json!({"check": "other"}),
        json!({"check": "webview", "command": "a.b", "reply": "x"}),
        json!({"check": "webview", "command": "", "send": {}, "reply": "x"}),
        json!({"check": "debug", "program": "sample.py", "line": 0}),
        json!({"check": "debug", "program": "../x.py", "line": 1}),
    ] {
        assert!(DeepCheck::from_json(&bad).is_err(), "{bad} is refused");
    }
    // A reply matches on its `command` field only.
    assert!(reply_matches(r#"{"command":"loadRepos","repos":{}}"#, "loadRepos"));
    assert!(!reply_matches(r#"{"command":"refresh"}"#, "loadRepos"));
    assert!(!reply_matches("not json", "loadRepos"));
    // The result file the harness reads.
    assert_eq!(DeepResult::fail("no html").to_json(), json!({"deep": "fail", "reason": "no html"}));
    assert_eq!(DeepResult::pass("12 KB of HTML").to_json(), json!({"deep": "pass", "reason": "12 KB of HTML"}));
    assert_eq!(DeepResult::not_run("no python3").to_json(), json!({"deep": "not-run", "reason": "no python3"}));
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
    if std::env::var("OSE_REGRESS_MODE").as_deref() == Ok("deep") {
        eprintln!("[regress-client] OSE_REGRESS_MODE=deep: the deep client runs instead; skipping");
        return;
    }
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

    let mut gaps = Gaps::default();
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
                let update = rx.recv().await;
                if let Some(u) = &update {
                    gaps.observe(u);
                }
                match update {
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
            if let Some(u) = &update {
                gaps.observe(u);
            }
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
    if let Some(path) = std::env::var_os("OSE_REGRESS_GAPS").map(PathBuf::from) {
        let json = gaps.to_json();
        println!("[regress-client] {} protocol gap(s) recorded in {}", gaps.0.len(), path.display());
        std::fs::write(&path, format!("{json:#}\n")).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(!lost, "the connection to the OSE was lost before the settle time ended; the log comparison would not be trustworthy");
    println!("[regress-client] done after {settle} s of settle time");
}

/// The FR-E9 records of one run: per `todo` method with a value, the session's call count and the
/// extensions that named themselves in an argument.
#[derive(Default)]
struct Gaps(std::collections::BTreeMap<(String, String), (u32, BTreeSet<String>)>);

impl Gaps {
    fn observe(&mut self, u: &SessionUpdate) {
        if let SessionUpdate::ProtocolGap { proxy, method, extension, count } = u {
            let entry = self.0.entry((proxy.clone(), method.clone())).or_default();
            entry.0 = entry.0.max(*count);
            entry.1.extend(extension.clone());
        }
    }

    fn to_json(&self) -> Value {
        Value::Array(
            self.0
                .iter()
                .map(|((actor, method), (count, extensions))| {
                    json!({ "actor": actor, "method": method, "count": count, "extensions": extensions })
                })
                .collect(),
        )
    }
}

#[test]
fn fr_e9_regression_client_keeps_the_records_per_method() {
    let gap = |method: &str, extension: Option<&str>, count| SessionUpdate::ProtocolGap {
        proxy: "MainThreadSecretState".into(),
        method: method.into(),
        extension: extension.map(String::from),
        count,
    };
    let mut gaps = Gaps::default();
    gaps.observe(&gap("$getPassword", Some("b.ext"), 1));
    gaps.observe(&gap("$getPassword", None, 2));
    gaps.observe(&gap("$getPassword", Some("a.ext"), 3));
    gaps.observe(&gap("$getKeys", None, 1));
    gaps.observe(&SessionUpdate::ConnectionLost { reason: "not a gap".into() });
    assert_eq!(
        gaps.to_json(),
        json!([
            { "actor": "MainThreadSecretState", "method": "$getKeys", "count": 1, "extensions": [] },
            { "actor": "MainThreadSecretState", "method": "$getPassword", "count": 3, "extensions": ["a.ext", "b.ext"] },
        ])
    );
}

/// What the editor client still needs before the debug check can run (#445). Kept in one place so
/// the reason in `deep.json` and the README list stay the same.
const DEBUG_NOT_BUILT: &str = "the Ember editor client has no debug service yet: MainThreadDebugService \
($registerDebugTypes, $registerDebugConfigurationProvider, $registerDebugAdapterDescriptorFactory, $acceptDAMessage) \
gets default replies and ExtHostDebugService ($resolveDebugConfiguration, $startDASession, $sendDAMessage, $stopDASession) \
is never called, so no DAP request reaches debugpy (#445)";

fn has_python3() -> bool {
    std::process::Command::new("python3").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Wait for a webview update matching `pick`, returning what `pick` returns. Commands that the
/// extension host has not registered come back as `RunCommand` or `CommandFailed` and end the wait.
async fn wait_webview<T>(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionUpdate>,
    secs: u64,
    mut pick: impl FnMut(&WebviewUpdate) -> Option<T>,
) -> Result<T, String> {
    let until = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        match tokio::time::timeout_at(until, rx.recv()).await {
            Err(_) => return Err(format!("nothing within {secs} s")),
            Ok(None) => return Err("session ended".into()),
            Ok(Some(SessionUpdate::ConnectionLost { reason })) => return Err(format!("connection lost: {reason}")),
            Ok(Some(SessionUpdate::RunCommand { command })) => return Err(format!("unregistered command {}", command.id)),
            Ok(Some(SessionUpdate::CommandFailed { id, error })) => return Err(format!("command {id} failed: {error}")),
            Ok(Some(SessionUpdate::Webview(u))) => {
                if let Some(t) = pick(&u) {
                    return Ok(t);
                }
            }
            Ok(Some(_)) => {}
        }
    }
}

/// The webview check: the view command opens a panel, its HTML arrives, and one message goes to
/// the extension and one comes back.
async fn webview_check(
    session: &EditorSession,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionUpdate>,
    command: &str,
    send: &Value,
    reply: &str,
) -> DeepResult {
    // The extension registers its command while it activates; until then the session hands the
    // command to the window (`RunCommand`). Try again for up to a minute.
    let mut handle = None;
    let mut last = String::new();
    for attempt in 1..=12 {
        if let Err(e) = session.execute_command(command, Vec::new()) {
            return DeepResult::fail(format!("{command} not sent: {e}"));
        }
        let created = wait_webview(rx, 20, |u| match u {
            WebviewUpdate::Created { handle, kind: WebviewKind::Panel { view_type }, .. } => Some((handle.clone(), view_type.clone())),
            _ => None,
        })
        .await;
        match created {
            Ok((h, view_type)) => {
                println!("[regress-deep] {command}: panel {view_type} ({h}) after {attempt} attempt(s)");
                handle = Some(h);
                break;
            }
            Err(e) if e.starts_with("connection lost") || e == "session ended" => return DeepResult::fail(e),
            Err(e) => {
                last = e;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    let Some(handle) = handle else { return DeepResult::fail(format!("{command} opened no webview panel: {last}")) };

    let html = match wait_webview(rx, 60, |u| match u {
        WebviewUpdate::Html { handle: h, html, .. } if *h == handle => Some(html.clone()),
        _ => None,
    })
    .await
    {
        Ok(html) if !html.trim().is_empty() => html,
        Ok(_) => return DeepResult::fail("the panel's HTML is empty"),
        Err(e) => return DeepResult::fail(format!("no HTML for the panel: {e}")),
    };
    println!("[regress-deep] {command}: {} bytes of HTML", html.len());

    // Play the page: shown and active, ready, then the page's own first request.
    let events = [
        WebviewEvent::ViewState { handle: handle.clone(), visible: true, active: true, position: 0 },
        WebviewEvent::Ready { handle: handle.clone() },
        WebviewEvent::Message { handle: handle.clone(), message: WebviewMessage::text(send.to_string()) },
    ];
    for event in events {
        if let Err(e) = session.webview(event) {
            return DeepResult::fail(format!("webview event not sent: {e}"));
        }
    }
    match wait_webview(rx, 60, |u| match u {
        WebviewUpdate::Post { handle: h, message } if *h == handle && reply_matches(&message.json, reply) => Some(message.json.len()),
        _ => None,
    })
    .await
    {
        Ok(len) => DeepResult::pass(format!("{} bytes of HTML; {reply} answered with {len} bytes", html.len())),
        Err(e) => DeepResult::fail(format!("HTML arrived, but no {reply} answer to {send}: {e}")),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "the deep client of ember/ose/regress.sh (OSE_REGRESS_MODE=deep): needs OSE_REGRESS_ADDR and OSE_REGRESS_WS"]
async fn nfr_w1_deep_checks() {
    if std::env::var("OSE_REGRESS_MODE").as_deref() != Ok("deep") {
        eprintln!("[regress-deep] OSE_REGRESS_MODE is not deep; skipping");
        return;
    }
    let (Some(addr), Some(ws)) = (std::env::var("OSE_REGRESS_ADDR").ok(), std::env::var_os("OSE_REGRESS_WS").map(PathBuf::from)) else {
        eprintln!("[regress-deep] OSE_REGRESS_ADDR or OSE_REGRESS_WS not set; skipping");
        return;
    };
    let out = std::env::var_os("OSE_REGRESS_DEEP").map(PathBuf::from);
    let ws = std::fs::canonicalize(&ws).unwrap_or_else(|e| panic!("OSE_REGRESS_WS {}: {e}", ws.display()));
    let checks: Vec<(String, DeepCheck)> = entries()
        .into_iter()
        .filter_map(|e| e.deep.as_ref().map(|d| (e.id.clone(), DeepCheck::from_json(d).unwrap_or_else(|p| panic!("{}: {p}", e.id)))))
        .collect();

    let config = SessionConfig::new(addr.clone(), ws.to_str().expect("a UTF-8 workspace path"));
    let (session, mut rx) = tokio::time::timeout(Duration::from_secs(180), EditorSession::connect_tcp(config))
        .await
        .expect("connect timed out")
        .unwrap_or_else(|e| panic!("connect to {addr}: {e}"));
    println!("[regress-deep] connected to {addr}, workspace {}", ws.display());

    for (id, check) in &checks {
        let result = match check {
            DeepCheck::Webview { command, send, reply } => webview_check(&session, &mut rx, command, send, reply).await,
            // Python is the runner's (the check needs an interpreter for debugpy's adapter and the
            // program); without it the check cannot run at all.
            DeepCheck::Debug { .. } if !has_python3() => DeepResult::not_run("no python3"),
            DeepCheck::Debug { program, line } => {
                let _ = (program, line);
                DeepResult::not_run(DEBUG_NOT_BUILT)
            }
        };
        println!("[regress-deep] {id}: {} ({})", result.status, result.reason);
        if let Some(dir) = &out {
            std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
            let file = dir.join(format!("{}.json", id.to_lowercase()));
            std::fs::write(&file, format!("{}\n", result.to_json())).unwrap_or_else(|e| panic!("write {}: {e}", file.display()));
        }
    }
    session.shutdown().await;
}
