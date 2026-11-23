//! The node's check on Codex's exec-server protocol (SPEC FR-X5, FR-X1; #1047).
//!
//! `/exec-server` runs `codex exec-server --listen stdio` on this computer for a Codex session on
//! the main server ([`crate::exec_server`]). Without this check the node relayed raw bytes, so the
//! session could read, write and run anywhere the node's user can, past the allowed folders that
//! confine the node's own file API. The node now reads the client-to-codex stream line by line and
//! judges every message with [`check_line`] before codex sees it.
//!
//! **The protocol** (codex-rs/exec-server and codex-rs/exec-server-protocol at tag rust-v0.167.0,
//! commit 086ccc833bc9cf01024ad2dd6af7dbd421b6bf9a): newline-delimited JSON-RPC on stdio without
//! the `"jsonrpc"` field (`exec-server/src/connection.rs`, `read_line`, 64 MiB per message). A
//! message with `method` and `id` is a request, with `method` only a notification, without
//! `method` a reply (`exec-server-protocol/src/rpc.rs`). The methods the server routes
//! (`exec-server/src/server/registry.rs`) and the paths they carry
//! (`exec-server-protocol/src/protocol.rs`, all `PathUri`, a `file:` URI string):
//!
//! | Method | Paths checked here |
//! |-|-|
//! | `fs/readFile`, `fs/open`, `fs/writeFile`, `fs/createDirectory`, `fs/getMetadata`, `fs/canonicalize`, `fs/readDirectory`, `fs/remove` | `path` |
//! | `fs/walk` | `path`; `options.followDirectorySymlinks` is forced to `false` |
//! | `fs/copy` | `sourcePath`, `destinationPath` |
//! | `fs/watchMissing` | every `missing[]`, and `present` |
//! | `capabilityRoots/discoverV1` | every `roots[].path` |
//! | `capabilities/discoverV2`, `environmentConfig/read`, `process/start` | `cwd` (must exist) |
//! | `initialize`, `environment/info`, `environment/status`, `process/read`, `process/write`, `process/signal`, `process/terminate`, `fs/readBlock`, `fs/writeBlock`, `fs/close`, `http/request` | none (handles, process ids, or no file) |
//!
//! Any other request is refused, so a method added by a later codex fails closed until it is
//! listed here. A notification other than `initialized` is dropped. A reply (the client answering
//! a request codex sent) is passed on.
//!
//! **The rule** is the node's file API's ([`PathPolicy`]): canonicalised, symlinks resolved,
//! inside an allowed root and outside every denied entry, else refused with the same answer. A
//! path is read the way codex reads it: parsed as a WHATWG URL (the `url` crate, as
//! `codex-utils-path-uri` does), which removes `..` segments (also `%2e%2e`) lexically **before**
//! the filesystem sees the path. Judging the raw text through the filesystem instead would follow
//! a symlink before the `..` and check a different place than codex opens. Only a local `file:`
//! URL with no query or fragment is accepted. A request with `followSymlinks: false` checks the
//! entry itself (its parent canonicalised, [`PathPolicy::resolve_nofollow`]); otherwise the target
//! is checked and may not exist yet ([`PathPolicy::resolve_for_write`]). An allowed root itself
//! cannot be removed.
//!
//! **What codex receives** is the checked message serialised again, not the bytes the client
//! sent, so a message with duplicate keys or odd spacing cannot mean one thing here and another
//! in codex.
//!
//! **Not confined by this check** (see SPEC FR-X5): the commands `process/start` runs (they run as
//! the node's user, like `/exec`, D37: only their working directory is checked); files codex reads
//! for itself (its own `CODEX_HOME` config, skills and plugins under the user's home); outbound
//! HTTP from the node (`http/request`); and a symlink swapped in between this check and codex's
//! own open (the node cannot check codex's file handles, unlike its own file API, #468).

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::policy::{PathPolicy, PolicyError};

/// JSON-RPC error code for a path outside the allowed folders. Codex maps -32600 to
/// `io::ErrorKind::InvalidInput` and does not retry it (`exec-server/src/remote_file_system.rs`,
/// `map_remote_error`).
pub const FORBIDDEN_CODE: i64 = -32600;
/// JSON-RPC error code for a method the node does not relay (method not found).
pub const METHOD_NOT_ALLOWED_CODE: i64 = -32601;
/// JSON-RPC error code for a path that is not a local absolute `file:` URI (invalid params).
pub const BAD_PATH_CODE: i64 = -32602;
/// JSON-RPC error code for a working directory that does not exist (codex's own not found).
pub const NOT_FOUND_CODE: i64 = -32004;

/// What to do with one line the client sent toward codex.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Write this line (newline-terminated) to codex's stdin.
    Forward(Vec<u8>),
    /// Do not forward; send this JSON-RPC error line (newline-terminated) back to the client.
    Refuse(Vec<u8>),
    /// Neither: a message that cannot be answered (not JSON, a notification codex does not take
    /// from a client, an id of the wrong type). Dropping it fails closed.
    Drop,
}

/// How a path of a request is checked.
#[derive(Clone, Copy)]
enum Check {
    /// The target, following symlinks; it may not exist yet.
    Target,
    /// The entry itself when the request says `followSymlinks: false`, else [`Check::Target`].
    TargetOrEntry,
    /// An existing directory to run or look in.
    Cwd,
}

/// Judge one line (without its newline; a trailing `\r` and surrounding blanks are fine).
pub fn check_line(policy: &PathPolicy, line: &[u8]) -> Verdict {
    let Ok(mut msg) = serde_json::from_slice::<Value>(line) else {
        return Verdict::Drop;
    };
    let Some(obj) = msg.as_object_mut() else {
        return Verdict::Drop;
    };
    let Some(method) = obj.get("method").cloned() else {
        // A reply to a request codex sent to the client: nothing of the node's is named.
        return forward(&msg);
    };
    let Some(id) = obj.get("id").cloned() else {
        return match method.as_str() {
            Some("initialized") => forward(&msg),
            _ => Verdict::Drop,
        };
    };
    if !(id.is_string() || id.is_i64()) {
        return Verdict::Drop;
    }
    let Some(method) = method.as_str() else {
        return refuse(&id, METHOD_NOT_ALLOWED_CODE, "ember node: the method must be a string".into());
    };
    let empty = Value::Object(Map::new());
    let params = obj.get("params").unwrap_or(&empty);
    let result = match method {
        "initialize" | "environment/info" | "environment/status" | "process/read" | "process/write"
        | "process/signal" | "process/terminate" | "fs/readBlock" | "fs/writeBlock" | "fs/close"
        | "http/request" => Ok(()),
        "fs/readFile" | "fs/open" | "fs/writeFile" | "fs/createDirectory" | "fs/getMetadata"
        | "fs/canonicalize" | "fs/readDirectory" | "fs/walk" => {
            check_field(policy, params, "path", Check::TargetOrEntry).map(drop)
        }
        "fs/remove" => check_field(policy, params, "path", Check::TargetOrEntry).and_then(|c| {
            if policy.is_root(&c) {
                Err(forbidden("path", &c))
            } else {
                Ok(())
            }
        }),
        "fs/copy" => check_field(policy, params, "sourcePath", Check::Target)
            .and_then(|_| check_field(policy, params, "destinationPath", Check::Target))
            .map(drop),
        "fs/watchMissing" => check_watch_missing(policy, params),
        "capabilityRoots/discoverV1" => check_discover_roots(policy, params),
        "capabilities/discoverV2" | "environmentConfig/read" | "process/start" => {
            check_field(policy, params, "cwd", Check::Cwd).map(drop)
        }
        other => Err((METHOD_NOT_ALLOWED_CODE, format!("ember node: exec-server method `{other}` is not relayed"))),
    };
    if let Err((code, message)) = result {
        return refuse(&id, code, message);
    }
    if method == "fs/walk" {
        // A walk that follows directory links would list (and hand back paths of) folders outside
        // the roots without any further request the node could check.
        if let Some(opts) = obj.get_mut("params").and_then(|p| p.get_mut("options")).and_then(Value::as_object_mut) {
            opts.insert("followDirectorySymlinks".into(), Value::Bool(false));
        }
    }
    forward(&msg)
}

type Refusal = (i64, String);

fn forward(msg: &Value) -> Verdict {
    match serde_json::to_vec(msg) {
        Ok(mut line) => {
            line.push(b'\n');
            Verdict::Forward(line)
        }
        Err(_) => Verdict::Drop,
    }
}

fn refuse(id: &Value, code: i64, message: String) -> Verdict {
    let mut line = serde_json::to_vec(&json!({"id": id, "error": {"code": code, "message": message}}))
        .unwrap_or_default();
    line.push(b'\n');
    Verdict::Refuse(line)
}

fn forbidden(field: &str, path: &Path) -> Refusal {
    (
        FORBIDDEN_CODE,
        format!("ember node: forbidden_path: {field} {} is outside this computer's allowed folders", path.display()),
    )
}

/// The local path a `file:` URI names, read the way codex reads it.
pub fn file_uri_path(uri: &str) -> Result<PathBuf, String> {
    let url = url::Url::parse(uri).map_err(|e| format!("not a file URI ({e}): {uri}"))?;
    if url.scheme() != "file" {
        return Err(format!("not a file URI: {uri}"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("a file URI with a query or fragment is not accepted: {uri}"));
    }
    if url.host_str().is_some_and(|h| !h.is_empty()) {
        return Err(format!("a file URI on another host is not accepted: {uri}"));
    }
    let path = url.to_file_path().map_err(|()| format!("not a local file URI: {uri}"))?;
    if !path.is_absolute() {
        return Err(format!("not an absolute path: {uri}"));
    }
    Ok(path)
}

fn check_path(policy: &PathPolicy, field: &str, raw: Option<&Value>, check: Check, follow: Option<bool>) -> Result<PathBuf, Refusal> {
    let Some(uri) = raw.and_then(Value::as_str) else {
        return Err((BAD_PATH_CODE, format!("ember node: {field} must be a file URI string")));
    };
    let path = file_uri_path(uri).map_err(|e| (BAD_PATH_CODE, format!("ember node: {field}: {e}")))?;
    let resolved = match check {
        Check::Cwd => policy.resolve_existing(&path),
        Check::TargetOrEntry if follow == Some(false) => policy.resolve_nofollow(&path),
        Check::TargetOrEntry | Check::Target => policy.resolve_for_write(&path),
    };
    resolved.map_err(|e| match e {
        PolicyError::Outside(_) => forbidden(field, &path),
        PolicyError::NotAbsolute(_) => (BAD_PATH_CODE, format!("ember node: {field}: {e}")),
        PolicyError::Io(_, io) if io.kind() == std::io::ErrorKind::NotFound => {
            (NOT_FOUND_CODE, format!("ember node: {field} {} does not exist", path.display()))
        }
        // Anything the node cannot resolve (a NUL byte, a loop, no permission) is refused: the
        // node cannot tell where it leads.
        PolicyError::Io(..) => (FORBIDDEN_CODE, format!("ember node: {field}: {e}")),
    })
}

fn check_field(policy: &PathPolicy, params: &Value, field: &str, check: Check) -> Result<PathBuf, Refusal> {
    let follow = params.get("followSymlinks").and_then(Value::as_bool);
    check_path(policy, field, params.get(field), check, follow)
}

fn check_watch_missing(policy: &PathPolicy, params: &Value) -> Result<(), Refusal> {
    match params.get("missing") {
        Some(Value::Array(items)) => {
            for item in items {
                check_path(policy, "missing", Some(item), Check::Target, None)?;
            }
        }
        None | Some(Value::Null) => {}
        Some(_) => return Err((BAD_PATH_CODE, "ember node: missing must be a list of file URIs".into())),
    }
    match params.get("present") {
        None | Some(Value::Null) => Ok(()),
        p => check_path(policy, "present", p, Check::Target, None).map(drop),
    }
}

fn check_discover_roots(policy: &PathPolicy, params: &Value) -> Result<(), Refusal> {
    let Some(Value::Array(roots)) = params.get("roots") else {
        return Err((BAD_PATH_CODE, "ember node: roots must be a list".into()));
    };
    for root in roots {
        check_path(policy, "roots[].path", root.get("path"), Check::Target, None)?;
    }
    Ok(())
}
