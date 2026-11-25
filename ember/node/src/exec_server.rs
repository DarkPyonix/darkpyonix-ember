//! Bridge to Codex's experimental exec-server (`docs/design/INTERCEPTION.md`, option (c)).
//!
//! Codex can run its tools (shell, unified exec, PTY, `apply_patch`'s file operations) through a
//! remote *exec-server* that its app-server attaches with `environment/add {environmentId,
//! execServerUrl}`. This module puts that executor on the node:
//!
//! - `ember-node exec-server` (see `main.rs`) replaces itself with
//!   `codex exec-server --listen stdio`. It is the one place that decides
//!   which codex binary and flags run, so the daemon and a manual test use the same command.
//! - `GET /exec-server` (WebSocket, bearer-authenticated like every other route) starts one
//!   such codex child per connection and relays it. A missing codex binary is answered before
//!   the upgrade with 503 `codex_not_installed` naming [`CODEX_BIN_ENV`] (#1047).
//!
//! **Protocol (codex-rs/exec-server at rust-v0.167.0, #1047).** On stdio the exec-server reads
//! newline-delimited JSON-RPC (`codex-rs/exec-server/src/connection.rs`, `from_stdio`: one message
//! per line, a trailing CR stripped, blank lines skipped, 64 MiB per line) in the Codex dialect
//! without the `"jsonrpc"` field (`codex-rs/exec-server-protocol/src/rpc.rs`). Methods are the
//! router of `codex-rs/exec-server/src/server/registry.rs`; their parameters are in
//! `codex-rs/exec-server-protocol/src/{protocol,capabilities,environment_config}.rs`. Every path
//! is a `file:` URI (`PathUri`, `codex-rs/utils/path-uri/src/lib.rs`).
//!
//! **Confinement (SPEC FR-X5, FR-X1, #1047).** Client-to-codex bytes are split into lines and
//! each request is screened by [`screen_line`] before it reaches codex:
//!
//! - a request whose method is not in the table [`method_paths`] (the router at rust-v0.167.0)
//!   is refused with JSON-RPC error `-32601`;
//! - every path the method names (see [`method_paths`]) is decoded from its `file:` URI strictly
//!   ([`file_uri_to_path`]) and checked with the node's [`PathPolicy`], the same rule as the file
//!   API and terminal working directories (canonicalised, symlinks resolved, `..` refused,
//!   denied entries refused). Any refusal answers JSON-RPC error `-32602` with `data.code` set to
//!   the file API's error code (`forbidden_path`, `not_found`, `bad_request`) and the request is
//!   not written to codex;
//! - an accepted request is written as the re-serialised JSON value the check read (one line), so
//!   codex parses exactly what was checked (no duplicate keys or other parser differences);
//! - a client notification other than `initialized` is dropped (codex closes the connection on
//!   one); a client response or error (to a codex request such as `network/policyRequest`) is
//!   written unchanged.
//!
//! Codex-to-client bytes pass through unchanged and chunked as codex wrote them. A refusal is
//! injected only between lines of that stream, so the client's line framing stays intact.
//!
//! **Not confined.** The commands `process/start` runs (they touch anything this user can;
//! codex's own sandbox, if requested, is the only limit); what codex reaches below a checked
//! path by itself (`fs/walk`, `fs/readDirectory`, recursive `fs/copy` and `fs/remove`, which can
//! follow links and enter denied entries); the sandbox permission paths a request carries;
//! `http/request` (network, not paths); and the window between the node's check and codex's own
//! open (a symlink swapped in there is not detected, unlike the file API's open-then-check, #468).
//!
//! Closing the socket closes the child's stdin; the stdio transport ends and codex exits
//! and is killed after [`EXIT_GRACE`] if it does not.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::api::{ApiError, Node};
use crate::client::{ClientError, NodeClient};
use crate::policy::{PathPolicy, PolicyError};
use crate::proto::ErrorCode;

/// Codex binary on the node (default `codex` on `PATH`).
pub const CODEX_BIN_ENV: &str = "EMBER_NODE_CODEX_BIN";
/// How long codex may take to exit after its stdin closed before it is killed.
pub const EXIT_GRACE: Duration = Duration::from_secs(3);
/// Longest client line the relay buffers, the exec-server's own stdio limit
/// (`MAX_STDIO_JSONRPC_MESSAGE_LEN` in `codex-rs/exec-server/src/connection.rs`).
pub const MAX_LINE: usize = 64 * 1024 * 1024;

/// JSON-RPC error codes of the refusals (the exec-server's own: `codex-rs/exec-server/src/rpc.rs`).
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

pub fn codex_bin() -> PathBuf {
    std::env::var_os(CODEX_BIN_ENV).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("codex"))
}

/// The codex binary the node would run, or a message naming [`CODEX_BIN_ENV`] when there is none:
/// a value with a directory part must name an executable file; a bare name is looked up on `PATH`.
pub fn find_codex() -> Result<PathBuf, String> {
    let bin = codex_bin();
    let set = std::env::var_os(CODEX_BIN_ENV).is_some();
    let has_dir = bin.is_absolute() || bin.components().count() > 1;
    if has_dir {
        return if is_executable(&bin) {
            Ok(bin)
        } else {
            Err(format!(
                "codex is not installed on this computer: {CODEX_BIN_ENV}={} is not an executable file",
                bin.display()
            ))
        };
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for candidate in executable_names(&bin) {
            let full = dir.join(&candidate);
            if is_executable(&full) {
                return Ok(full);
            }
        }
    }
    let how = if set { format!("{CODEX_BIN_ENV}={}", bin.display()) } else { format!("`{}`", bin.display()) };
    Err(format!(
        "codex is not installed on this computer: {how} was not found on PATH; install Codex there or set {CODEX_BIN_ENV} to the codex binary"
    ))
}

fn executable_names(bin: &Path) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut exe = bin.as_os_str().to_os_string();
        exe.push(".exe");
        vec![bin.to_path_buf(), PathBuf::from(exe)]
    }
    #[cfg(not(windows))]
    {
        vec![bin.to_path_buf()]
    }
}

fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        m.is_file()
    }
}

/// Arguments after the codex binary (codex-cli 0.155.1). Not `--exit-on-stdin-close`: that flag
/// belongs to remote registration and makes codex demand `--environment-id` and `--remote`; with
/// `--listen stdio`, codex already exits when stdin closes (checked: exit 0).
/// `codex exec-server --help`.
pub fn codex_args() -> [&'static str; 3] {
    ["exec-server", "--listen", "stdio"]
}

/// `ember-node exec-server`: become `codex exec-server` on this process's stdio. Returns only on
/// failure.
pub fn run_stdio() -> anyhow::Error {
    let bin = codex_bin();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&bin).args(codex_args()).exec();
        anyhow::anyhow!("could not run {} {}: {err}", bin.display(), codex_args().join(" "))
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&bin).args(codex_args()).status() {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(err) => anyhow::anyhow!("could not run {} {}: {err}", bin.display(), codex_args().join(" ")),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Screening client requests

/// How a path parameter is checked against the [`PathPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathUse {
    /// A working directory: must exist ([`PathPolicy::resolve_existing`], as terminals).
    Cwd,
    /// A path that may not exist yet, followed if it is a link ([`PathPolicy::resolve_for_write`]).
    Follow,
    /// Like `Follow`, but when the request says `followSymlinks: false` the final component is
    /// not followed ([`PathPolicy::resolve_nofollow`]), as the file API's `lstat` and `remove`.
    Target,
}

/// Where a method's paths sit in its `params`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// `params.<name>`, required.
    One(&'static str),
    /// `params.<name>`, optional (absent or null is fine).
    Optional(&'static str),
    /// `params.<name>[*]`, every element a path.
    Each(&'static str),
    /// `params.<list>[*].<name>`.
    EachMember(&'static str, &'static str),
}

/// The client-to-codex request methods of `codex exec-server` at rust-v0.167.0
/// (`codex-rs/exec-server/src/server/registry.rs`) and the path parameters each carries
/// (`codex-rs/exec-server-protocol/src/protocol.rs`, `capabilities.rs`, `environment_config.rs`).
/// `None`: not a method of that version, refused.
fn method_paths(method: &str) -> Option<&'static [(Field, PathUse)]> {
    use Field::*;
    use PathUse::*;
    let fields: &'static [(Field, PathUse)] = match method {
        // No path parameters.
        "initialize" | "environment/info" | "environment/status" | "process/read" | "process/write"
        | "process/signal" | "process/terminate" | "fs/readBlock" | "fs/writeBlock" | "fs/close"
        | "http/request" => &[],
        // A working directory.
        "process/start" | "environmentConfig/read" | "capabilities/discoverV2" => &[(One("cwd"), Cwd)],
        // One path.
        "fs/readFile" | "fs/writeFile" | "fs/getMetadata" | "fs/remove" => &[(One("path"), Target)],
        "fs/open" | "fs/createDirectory" | "fs/canonicalize" | "fs/readDirectory" | "fs/walk" => {
            &[(One("path"), Follow)]
        }
        // Several.
        "fs/copy" => &[(One("sourcePath"), Follow), (One("destinationPath"), Follow)],
        "fs/watchMissing" => &[(Each("missing"), Follow), (Optional("present"), Follow)],
        "capabilityRoots/discoverV1" => &[(EachMember("roots", "path"), Follow)],
        _ => return None,
    };
    Some(fields)
}

/// What the relay does with one client line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screened {
    /// Write these bytes (one line, newline included) to codex.
    Forward(Vec<u8>),
    /// Do not write anything to codex; send this JSON-RPC error line (newline included) back.
    Refuse(Vec<u8>),
    /// Neither (a blank line, or a notification codex does not accept).
    Drop,
}

fn error_line(id: Value, code: i64, message: String, data: Option<Value>) -> Vec<u8> {
    let mut error = json!({ "code": code, "message": message });
    if let Some(d) = data {
        error["data"] = d;
    }
    let mut out = serde_json::to_vec(&json!({ "id": id, "error": error })).expect("json");
    out.push(b'\n');
    out
}

/// Screen one client-to-codex line (newline included or not) against `policy`. See the module
/// documentation for the rules.
pub fn screen_line(policy: &PathPolicy, line: &[u8]) -> Screened {
    let body = line.strip_suffix(b"\n").unwrap_or(line);
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Screened::Drop;
    }
    let malformed = |why: String| Screened::Refuse(error_line(json!(-1), INVALID_REQUEST, why, None));
    let value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return malformed(format!("malformed JSON-RPC message: {e}")),
    };
    let Some(obj) = value.as_object() else {
        return malformed("expected a JSON-RPC object".into());
    };
    let Some(method) = obj.get("method") else {
        // A response or error to a request codex sent: unchanged.
        if obj.contains_key("id") && (obj.contains_key("result") || obj.contains_key("error")) {
            let mut out = body.to_vec();
            out.push(b'\n');
            return Screened::Forward(out);
        }
        return malformed("not a JSON-RPC request, notification or response".into());
    };
    let Some(method) = method.as_str() else {
        return malformed("method must be a string".into());
    };
    let Some(id) = obj.get("id").cloned() else {
        // A notification: codex accepts only `initialized` and closes on any other.
        if method == "initialized" {
            return Screened::Forward(reserialise(&value));
        }
        tracing::warn!(target: "ember_node::exec_server", "dropped client notification `{method}`");
        return Screened::Drop;
    };
    let Some(fields) = method_paths(method) else {
        return Screened::Refuse(error_line(
            id,
            METHOD_NOT_FOUND,
            format!("the node does not relay `{method}` to codex exec-server"),
            None,
        ));
    };
    let params = obj.get("params").unwrap_or(&Value::Null);
    let follow = params.get("followSymlinks").and_then(Value::as_bool);
    for (field, how) in fields {
        let uris = match collect(params, *field) {
            Ok(u) => u,
            Err(why) => return Screened::Refuse(error_line(id, INVALID_PARAMS, why, None)),
        };
        for uri in uris {
            if let Err((code, why)) = check_uri(policy, uri, *how, follow) {
                return Screened::Refuse(error_line(id, INVALID_PARAMS, why, Some(json!({ "code": code }))));
            }
        }
    }
    Screened::Forward(reserialise(&value))
}

fn reserialise(v: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec(v).expect("json");
    out.push(b'\n');
    out
}

/// The path strings of `field` in `params`.
fn collect(params: &Value, field: Field) -> Result<Vec<&str>, String> {
    fn str_at<'a>(v: Option<&'a Value>, name: &str) -> Result<&'a str, String> {
        v.and_then(Value::as_str).ok_or_else(|| format!("`{name}` must be a file: URI string"))
    }
    match field {
        Field::One(name) => Ok(vec![str_at(params.get(name), name)?]),
        Field::Optional(name) => match params.get(name) {
            None | Some(Value::Null) => Ok(vec![]),
            v => Ok(vec![str_at(v, name)?]),
        },
        Field::Each(name) => {
            let list = params.get(name).and_then(Value::as_array).ok_or_else(|| format!("`{name}` must be a list"))?;
            list.iter().map(|v| str_at(Some(v), name)).collect()
        }
        Field::EachMember(list_name, name) => {
            let list = params
                .get(list_name)
                .and_then(Value::as_array)
                .ok_or_else(|| format!("`{list_name}` must be a list"))?;
            list.iter().map(|v| str_at(v.get(name), name)).collect()
        }
    }
}

/// Check one `file:` URI. `Err((file API error code, message))`.
fn check_uri(policy: &PathPolicy, uri: &str, how: PathUse, follow: Option<bool>) -> Result<PathBuf, (&'static str, String)> {
    let path = file_uri_to_path(uri).map_err(|why| ("forbidden_path", format!("{uri}: {why}")))?;
    let checked = match how {
        PathUse::Cwd => policy.resolve_existing(&path),
        PathUse::Follow => policy.resolve_for_write(&path),
        PathUse::Target if follow == Some(false) => policy.resolve_nofollow(&path),
        PathUse::Target => policy.resolve_for_write(&path),
    };
    checked.map_err(|e| {
        let code = match &e {
            PolicyError::Outside(_) => "forbidden_path",
            PolicyError::NotAbsolute(_) => "bad_request",
            PolicyError::Io(_, io) if io.kind() == std::io::ErrorKind::NotFound => "not_found",
            PolicyError::Io(..) => "bad_request",
        };
        (code, format!("{uri}: {e}"))
    })
}

/// Decode a `file:` URI to a host path, **strictly**: codex's parser (`url`, WHATWG) removes dot
/// segments lexically and treats `\` as a separator, while the node's policy resolves `..` through
/// the filesystem, so the two could disagree about where a URI with such parts points. Every
/// spelling where they could disagree is refused instead of emulated:
///
/// - the scheme must be `file` and the authority empty or `localhost`;
/// - no ASCII control character, space, `\`, `?` or `#` (codex would strip, convert or cut them);
/// - every `%` starts a valid escape; no segment decodes to `.` or `..`, and no escape decodes
///   to `/`, `\` or NUL.
///
/// Codex itself sends canonical URIs (dot segments already removed), which pass unchanged.
pub fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let (scheme, rest) = uri.split_once(':').ok_or("not a file: URI")?;
    if !scheme.eq_ignore_ascii_case("file") {
        return Err("only file: URIs are accepted".into());
    }
    if uri.bytes().any(|b| b <= b' ' || b == 0x7f || matches!(b, b'\\' | b'?' | b'#')) {
        return Err("the URI has characters the node does not accept (control, space, \\, ? or #)".into());
    }
    let rest = rest.strip_prefix("//").ok_or("a file: URI must start with file://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => return Err("a file: URI needs an absolute path".into()),
    };
    if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
        return Err("a file: URI with a host is not a path on this computer".into());
    }
    let mut segments: Vec<Vec<u8>> = Vec::new();
    for raw in path.split('/').skip(1) {
        let seg = percent_decode(raw)?;
        if seg == b"." || seg == b".." {
            return Err("`.` and `..` segments are refused".into());
        }
        if seg.iter().any(|b| matches!(b, b'/' | b'\\' | 0)) {
            return Err("an escaped `/`, `\\` or NUL is refused".into());
        }
        segments.push(seg);
    }
    segments_to_path(segments)
}

fn percent_decode(s: &str) -> Result<Vec<u8>, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3).ok_or("a truncated % escape")?;
            let hex = std::str::from_utf8(hex).map_err(|_| "a bad % escape")?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| "a bad % escape")?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Ok(out)
}

#[cfg(unix)]
fn segments_to_path(segments: Vec<Vec<u8>>) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = Vec::new();
    for s in segments {
        bytes.push(b'/');
        bytes.extend_from_slice(&s);
    }
    if bytes.is_empty() {
        bytes.push(b'/');
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}

#[cfg(not(unix))]
fn segments_to_path(segments: Vec<Vec<u8>>) -> Result<PathBuf, String> {
    // `file:///C:/dir/file` on Windows. A drive is required; `:` elsewhere and `|` are refused.
    let mut parts = Vec::new();
    for s in segments {
        parts.push(String::from_utf8(s).map_err(|_| "a Windows path must be UTF-8")?);
    }
    let drive = parts.first().filter(|d| d.len() == 2 && d.as_bytes()[0].is_ascii_alphabetic() && d.ends_with(':'));
    let Some(drive) = drive else { return Err("a Windows file: URI must start with a drive (file:///C:/...)".into()) };
    let mut out = format!("{drive}\\");
    let rest = &parts[1..];
    if rest.iter().any(|p| p.contains(':') || p.contains('|')) {
        return Err("`:` or `|` inside a Windows path is refused".into());
    }
    out.push_str(&rest.join("\\"));
    Ok(PathBuf::from(out))
}

// ---------------------------------------------------------------------------------------------
// The relay

/// Route handler for `GET /exec-server`.
pub async fn ws(State(node): State<Node>, ws: WebSocketUpgrade) -> Response {
    let bin = match find_codex() {
        Ok(b) => b,
        Err(msg) => {
            tracing::warn!("exec-server: {msg}");
            return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::CodexNotInstalled, msg).into_response();
        }
    };
    ws.on_upgrade(move |socket| bridge(node, bin, socket))
}

async fn bridge(node: Node, bin: PathBuf, ws: WebSocket) {
    // Same arguments as `ember-node exec-server`, spawned directly so the daemon does not depend
    // on finding its own executable.
    let mut cmd = Command::new(&bin);
    cmd.args(codex_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Start inside the first allowed root, so codex's default cwd is a project directory.
    let policy = node.policy().clone();
    if let Some(root) = policy.roots().first() {
        cmd.current_dir(root);
    }
    let (mut sink, mut stream) = ws.split();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let reason = format!("could not start {} ({CODEX_BIN_ENV}): {e}", bin.display());
            tracing::warn!("exec-server: {reason}");
            let mut reason = reason;
            // A close reason is at most 123 bytes.
            while reason.len() > 123 {
                reason.pop();
            }
            let _ = sink.send(Message::Close(Some(CloseFrame { code: 1011, reason: reason.into() }))).await;
            return;
        }
    };
    let mut stdin = child.stdin.take().expect("piped");
    let mut stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "ember_node::exec_server", "{line}");
        }
    });

    // Refusals go back through the downstream task, which places them between codex's lines.
    let (refuse_tx, mut refuse_rx) = mpsc::channel::<Vec<u8>>(64);

    // Two independent directions, so a full pipe one way never stalls the other.
    let up = async move {
        let mut buf: Vec<u8> = Vec::new();
        let mut scanned = 0usize;
        'frames: while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Binary(b)) => buf.extend_from_slice(&b),
                Ok(Message::Text(t)) => buf.extend_from_slice(t.as_str().as_bytes()),
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue,
            }
            while let Some(i) = buf[scanned..].iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=scanned + i).collect();
                scanned = 0;
                let p = policy.clone();
                // The policy check touches the filesystem; keep it off the async workers.
                let verdict = match tokio::task::spawn_blocking(move || screen_line(&p, &line)).await {
                    Ok(v) => v,
                    Err(_) => break 'frames,
                };
                match verdict {
                    Screened::Forward(out) => {
                        if stdin.write_all(&out).await.is_err() || stdin.flush().await.is_err() {
                            break 'frames;
                        }
                    }
                    Screened::Refuse(reply) => {
                        if refuse_tx.send(reply).await.is_err() {
                            break 'frames;
                        }
                    }
                    Screened::Drop => {}
                }
            }
            scanned = buf.len();
            if buf.len() > MAX_LINE {
                tracing::warn!("exec-server: a client line exceeds {MAX_LINE} bytes; closing");
                break;
            }
        }
        // Dropping stdin here ends codex's stdio transport, and codex exits.
    };
    let down = async move {
        enum Event {
            Out(std::io::Result<usize>),
            Refusal(Option<Vec<u8>>),
        }
        let mut buf = vec![0u8; 64 * 1024];
        // Whether the last byte sent to the client ended a line, and refusals waiting for one.
        let mut at_line_start = true;
        let mut pending: Vec<Vec<u8>> = Vec::new();
        let mut refusals_open = true;
        'events: loop {
            let event = tokio::select! {
                r = stdout.read(&mut buf) => Event::Out(r),
                reply = refuse_rx.recv(), if refusals_open => Event::Refusal(reply),
            };
            match event {
                Event::Out(Ok(0)) | Event::Out(Err(_)) => break,
                Event::Out(Ok(n)) => {
                    let chunk = &buf[..n];
                    // With refusals waiting, cut after the chunk's last newline and put them there.
                    let cut = if pending.is_empty() { None } else { chunk.iter().rposition(|b| *b == b'\n') };
                    let mut frames: Vec<Vec<u8>> = Vec::new();
                    match cut {
                        None => {
                            frames.push(chunk.to_vec());
                            at_line_start = chunk.ends_with(b"\n");
                        }
                        Some(i) => {
                            frames.push(chunk[..=i].to_vec());
                            frames.append(&mut pending);
                            at_line_start = true;
                            if i + 1 < n {
                                frames.push(chunk[i + 1..].to_vec());
                                at_line_start = false;
                            }
                        }
                    }
                    for f in frames {
                        if sink.send(Message::Binary(f.into())).await.is_err() {
                            break 'events;
                        }
                    }
                }
                Event::Refusal(Some(reply)) if at_line_start && pending.is_empty() => {
                    if sink.send(Message::Binary(reply.into())).await.is_err() {
                        break;
                    }
                }
                Event::Refusal(Some(reply)) => pending.push(reply),
                Event::Refusal(None) => refusals_open = false,
            }
        }
        let _ = sink.close().await;
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
    if tokio::time::timeout(EXIT_GRACE, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

/// Client side of `/exec-server`: a byte stream to a fresh codex exec-server on the node.
/// Over TCP or the transport alike ([`crate::client::NodeIo`]).
pub type ExecServerStream = tokio_tungstenite::WebSocketStream<crate::client::NodeIo>;

impl NodeClient {
    /// Open `/exec-server`. Send newline-delimited JSON-RPC as Binary (or Text) frames; codex's
    /// stdout and the node's refusals arrive as Binary frames.
    pub async fn exec_server(&self) -> Result<ExecServerStream, ClientError> {
        self.websocket("/exec-server").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_x5_file_uris_decode_strictly() {
        #[cfg(unix)]
        {
            assert_eq!(file_uri_to_path("file:///a/b%20c").unwrap(), PathBuf::from("/a/b c"));
            assert_eq!(file_uri_to_path("file://localhost/a").unwrap(), PathBuf::from("/a"));
            assert_eq!(file_uri_to_path("FILE:///a").unwrap(), PathBuf::from("/a"));
        }
        for bad in [
            "file:///a/../b",
            "file:///a/%2e%2E/b",
            "file:///a/.%2e/b",
            "file:///a/./b",
            "file:///a%2f..%2fb",
            "file:///a%5Cb",
            "file:///a%00b",
            "file:///a\\..\\b",
            "file:///a?x",
            "file:///a#x",
            "file:///a\tb",
            "file://host/a",
            "file:a",
            "http:///a",
            "file:///a%zz",
            "file:///a%2",
            "/a/b",
        ] {
            assert!(file_uri_to_path(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn fr_x5_every_method_at_rust_v0_167_0_is_known_and_others_are_not() {
        for m in [
            "initialize", "process/start", "process/read", "process/write", "process/signal",
            "process/terminate", "environment/info", "environment/status", "environmentConfig/read",
            "capabilities/discoverV2", "capabilityRoots/discoverV1", "http/request", "fs/readFile",
            "fs/open", "fs/readBlock", "fs/writeBlock", "fs/close", "fs/writeFile",
            "fs/createDirectory", "fs/getMetadata", "fs/watchMissing", "fs/canonicalize",
            "fs/readDirectory", "fs/walk", "fs/remove", "fs/copy",
        ] {
            assert!(method_paths(m).is_some(), "{m}");
        }
        for m in ["fs/rename", "process/kill", "", "FS/READFILE"] {
            assert!(method_paths(m).is_none(), "{m}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn fr_x5_screen_line_reads_every_path_of_a_request() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("root");
        std::fs::create_dir(&root).unwrap();
        let policy = PathPolicy::new([root.clone()]).unwrap();
        let inside = format!("file://{}/x", root.display());
        let outside = format!("file://{}/y", dir.path().canonicalize().unwrap().display());
        let ok = |l: String| matches!(screen_line(&policy, l.as_bytes()), Screened::Forward(_));
        assert!(ok(format!(r#"{{"id":1,"method":"fs/copy","params":{{"sourcePath":"{inside}","destinationPath":"{inside}","recursive":false}}}}"#)));
        assert!(!ok(format!(r#"{{"id":1,"method":"fs/copy","params":{{"sourcePath":"{inside}","destinationPath":"{outside}","recursive":false}}}}"#)));
        assert!(!ok(format!(r#"{{"id":1,"method":"fs/watchMissing","params":{{"watchId":1,"missing":["{inside}","{outside}"],"present":null}}}}"#)));
        assert!(ok(format!(r#"{{"id":1,"method":"fs/watchMissing","params":{{"watchId":1,"missing":["{inside}"],"present":null}}}}"#)));
        assert!(!ok(format!(r#"{{"id":1,"method":"capabilityRoots/discoverV1","params":{{"roots":[{{"id":"a","path":"{outside}"}}]}}}}"#)));
        // A missing path parameter cannot be checked: refused.
        assert!(!ok(r#"{"id":1,"method":"fs/readFile","params":{}}"#.into()));
        // Responses to codex's own requests pass unchanged; unknown notifications are dropped.
        let resp = b"{\"id\":7,\"result\":{\"z\":1,\"a\":2}}\r\n";
        assert_eq!(screen_line(&policy, resp), Screened::Forward(b"{\"id\":7,\"result\":{\"z\":1,\"a\":2}}\n".to_vec()));
        assert_eq!(screen_line(&policy, b"{\"method\":\"other\"}\n"), Screened::Drop);
        assert_eq!(screen_line(&policy, b"  \n"), Screened::Drop);
        assert!(matches!(screen_line(&policy, b"{not json\n"), Screened::Refuse(_)));
    }
}
