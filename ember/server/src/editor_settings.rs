//! Editor server presets in the server's settings, inherited and overridden per computer (SPEC
//! FR-W7, FR-W7a, FR-W7b, `INTENT.md` D56, #1121).
//!
//! | Method | Path | Body | Answer |
//! | ------ | ---- | ---- | ------ |
//! | GET | `/api/editor/settings` | none | the server record [`EditorSettings`] |
//! | PUT | `/api/editor/settings` (owner) | the full record | the saved record; 400 for a bad value |
//! | GET | `/api/computers/{id}/editor/settings` | none | `{overrides, effective, applied, reachable}` |
//! | PUT | `/api/computers/{id}/editor/settings` (owner) | [`EditorOverrides`] | the GET body; 400 for a bad value or `local` |
//!
//! **The record.** `editor_server` (`code_ose` by default, `vscode`, `manual_script` with its
//! `script`), `vscode_license_accepted` (required for `vscode`; the server records who accepted
//! it and when, in `vscode_license_accepted_by` and `vscode_license_accepted_at`, and ignores
//! those two fields when a client sends them), `extensions`, `theme` (`DarkPyonix Dark`),
//! `default_ide` (`ember`, `vscode`, `gateway`) and `settings` (later editor defaults). A fresh
//! server has [`EditorSettings::default`].
//!
//! **Per computer.** A registered computer has [`EditorOverrides`]: the same fields, each absent
//! or `null` to inherit. Its effective record is the server record with its present fields on top
//! ([`effective`]). The local computer (`local`, this server's own machine) has no overrides.
//!
//! **Apply.** Saving the server record applies it, in the background, to every computer whose
//! effective record changed (apart from `default_ide`, which is Ember's alone); saving a
//! computer's overrides applies to that computer. A registered computer gets `PUT /editor/settings`
//! on its node, which applies through its DarkPyonix manager
//! ([`ember_node::editor_settings`]); the local computer is applied in this process, against the
//! manager of this machine, with the same function. The report is kept
//! (`computer_editor_settings.applied`). The GET of a node asks the node for a fresher report
//! (short timeout) and falls back to the kept one, with `reachable: false` when the node does not
//! answer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{Extensions, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ember_node::client::ClientError;
use ember_node::editor_manager::{EditorExtension, EditorRelays};
use ember_node::editor_settings::{validate_parts, ApplyReport, Cli, EditorServer, NodeEditorSettings};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::computers::{Computers, LOCAL};
use crate::store::{now_ms, Store};

/// Store migration: the server record (one row) and each computer's overrides and last report.
pub const MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS editor_settings (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS computer_editor_settings (
    computer_id TEXT PRIMARY KEY,
    overrides   TEXT,
    applied     TEXT,
    applied_at  INTEGER
);
";

/// The default colour theme (SPEC FR-W6).
pub const DEFAULT_THEME: &str = "DarkPyonix Dark";
/// Longest wait for a node's apply: an install downloads the editor server (the node's CLI
/// limit is 10 minutes).
pub const NODE_APPLY_TIMEOUT: Duration = Duration::from_secs(11 * 60);
/// Longest wait for a node's fresher report on GET.
pub const NODE_REPORT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a computer's PUT waits for its apply before it answers the report kept so far (the
/// apply goes on).
pub const PUT_APPLY_WAIT: Duration = Duration::from_secs(30);
/// The caller recorded when the server knows no name for it.
pub const UNKNOWN_CALLER: &str = "owner";

/// The IDE the "Open IDE" button launches for a project with no pick (SPEC FR-W7b).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultIde {
    #[default]
    Ember,
    Vscode,
    Gateway,
}

/// The server's editor settings record (SPEC FR-W7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub editor_server: EditorServer,
    pub script: Option<String>,
    pub vscode_license_accepted: bool,
    /// Who accepted the licence (set by the server).
    pub vscode_license_accepted_by: Option<String>,
    /// When the licence was accepted, ms since the epoch (set by the server).
    pub vscode_license_accepted_at: Option<i64>,
    pub extensions: Vec<EditorExtension>,
    pub theme: String,
    pub default_ide: DefaultIde,
    pub settings: Map<String, Value>,
}

impl Default for EditorSettings {
    fn default() -> Self {
        EditorSettings {
            editor_server: EditorServer::CodeOse,
            script: None,
            vscode_license_accepted: false,
            vscode_license_accepted_by: None,
            vscode_license_accepted_at: None,
            extensions: Vec::new(),
            theme: DEFAULT_THEME.to_string(),
            default_ide: DefaultIde::Ember,
            settings: Map::new(),
        }
    }
}

/// One computer's overrides: a present field replaces the server's, an absent or `null` one
/// inherits it (SPEC FR-W7a).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_server: Option<EditorServer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vscode_license_accepted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vscode_license_accepted_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vscode_license_accepted_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<EditorExtension>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_ide: Option<DefaultIde>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<Map<String, Value>>,
}

/// The server record with the present fields of `o` on top. The licence's `by` and `at` come
/// from wherever the licence value comes from.
pub fn effective(server: &EditorSettings, o: &EditorOverrides) -> EditorSettings {
    let mut e = server.clone();
    if let Some(v) = o.editor_server {
        e.editor_server = v;
    }
    if let Some(v) = &o.script {
        e.script = Some(v.clone());
    }
    if let Some(v) = o.vscode_license_accepted {
        e.vscode_license_accepted = v;
        e.vscode_license_accepted_by = o.vscode_license_accepted_by.clone();
        e.vscode_license_accepted_at = o.vscode_license_accepted_at;
    }
    if let Some(v) = &o.extensions {
        e.extensions = v.clone();
    }
    if let Some(v) = &o.theme {
        e.theme = v.clone();
    }
    if let Some(v) = o.default_ide {
        e.default_ide = v;
    }
    if let Some(v) = &o.settings {
        e.settings = v.clone();
    }
    e
}

/// What a node is sent: the effective record without `default_ide` and the licence bookkeeping.
pub fn node_settings(e: &EditorSettings) -> NodeEditorSettings {
    NodeEditorSettings {
        editor_server: e.editor_server,
        script: e.script.clone(),
        vscode_license_accepted: e.vscode_license_accepted,
        extensions: e.extensions.clone(),
        theme: Some(e.theme.clone()),
        settings: e.settings.clone(),
    }
}

/// A record the server keeps: the node's checks ([`validate_parts`]) and the licence for
/// `vscode`.
pub fn validate(e: &EditorSettings) -> Result<(), String> {
    validate_parts(e.editor_server, e.script.as_deref(), &e.extensions, Some(&e.theme))?;
    if e.editor_server == EditorServer::Vscode && !e.vscode_license_accepted {
        return Err("vscode needs vscode_license_accepted: the VS Code Server licence (https://code.visualstudio.com/license/server)".into());
    }
    Ok(())
}

/// The licence's `by` and `at` after a save: kept while it stays accepted, set to `caller` and
/// `now` when it turns accepted, cleared when it is not accepted.
fn stamp(
    was: bool,
    was_by: Option<String>,
    was_at: Option<i64>,
    accepted: bool,
    caller: &str,
    now: i64,
) -> (Option<String>, Option<i64>) {
    match (was, accepted) {
        (_, false) => (None, None),
        (true, true) => (was_by, was_at),
        (false, true) => (Some(caller.to_string()), Some(now)),
    }
}

// ---------------------------------------------------------------------------------------------
// Store

/// The server record; the defaults when none was saved.
pub fn load(store: &Store) -> anyhow::Result<EditorSettings> {
    let conn = store.conn();
    let text: Option<String> =
        conn.query_row("SELECT json FROM editor_settings WHERE id = 1", [], |r| r.get(0)).optional()?;
    Ok(match text {
        Some(t) => serde_json::from_str(&t)?,
        None => EditorSettings::default(),
    })
}

pub fn save(store: &Store, e: &EditorSettings) -> anyhow::Result<()> {
    let text = serde_json::to_string(e)?;
    store.conn().execute(
        "INSERT INTO editor_settings (id, json) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        params![text],
    )?;
    Ok(())
}

/// The overrides of computer `id`; none when it has no row.
pub fn overrides(store: &Store, id: &str) -> anyhow::Result<EditorOverrides> {
    let conn = store.conn();
    let text: Option<Option<String>> = conn
        .query_row("SELECT overrides FROM computer_editor_settings WHERE computer_id = ?1", params![id], |r| r.get(0))
        .optional()?;
    Ok(match text.flatten() {
        Some(t) => serde_json::from_str(&t)?,
        None => EditorOverrides::default(),
    })
}

pub fn save_overrides(store: &Store, id: &str, o: &EditorOverrides) -> anyhow::Result<()> {
    let text = serde_json::to_string(o)?;
    store.conn().execute(
        "INSERT INTO computer_editor_settings (computer_id, overrides) VALUES (?1, ?2)
         ON CONFLICT(computer_id) DO UPDATE SET overrides = excluded.overrides",
        params![id, text],
    )?;
    Ok(())
}

/// The last report kept for computer `id`.
pub fn applied(store: &Store, id: &str) -> anyhow::Result<Option<ApplyReport>> {
    let conn = store.conn();
    let text: Option<Option<String>> = conn
        .query_row("SELECT applied FROM computer_editor_settings WHERE computer_id = ?1", params![id], |r| r.get(0))
        .optional()?;
    Ok(match text.flatten() {
        Some(t) => Some(serde_json::from_str(&t)?),
        None => None,
    })
}

pub fn save_applied(store: &Store, id: &str, report: &ApplyReport) -> anyhow::Result<()> {
    let text = serde_json::to_string(report)?;
    store.conn().execute(
        "INSERT INTO computer_editor_settings (computer_id, applied, applied_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(computer_id) DO UPDATE SET applied = excluded.applied, applied_at = excluded.applied_at",
        params![id, text, report.applied_at],
    )?;
    Ok(())
}

/// The effective record of computer `id` (`local` and an unknown id have no overrides).
pub fn effective_for(store: &Store, id: &str) -> anyhow::Result<EditorSettings> {
    let server = load(store)?;
    let o = if id == LOCAL { EditorOverrides::default() } else { overrides(store, id)? };
    Ok(effective(&server, &o))
}

/// The effective `default_ide` of computer `id` (SPEC FR-W7b): the node's override, else the
/// server's; `ember` when the store cannot be read.
pub fn effective_default_ide(store: &Store, id: &str) -> DefaultIde {
    match effective_for(store, id) {
        Ok(e) => e.default_ide,
        Err(e) => {
            tracing::warn!("could not read the editor settings of {id}: {e:#}");
            DefaultIde::Ember
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Apply

/// Applies the effective settings to computers and keeps their reports.
pub struct EditorSettingsService {
    store: Arc<Store>,
    computers: Arc<Computers>,
    /// The manager of this machine (the local computer).
    local: Arc<EditorRelays>,
    /// The DarkPyonix CLI of this machine.
    cli: Cli,
    /// Where the local computer's script is written.
    local_state_dir: Option<PathBuf>,
    /// One apply per computer at a time; each reads the effective record when it starts, so the
    /// last one applies the latest record.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl EditorSettingsService {
    pub fn new(store: Arc<Store>, computers: Arc<Computers>, local: Arc<EditorRelays>) -> Self {
        EditorSettingsService {
            store,
            computers,
            local,
            cli: Cli::from_env(),
            local_state_dir: None,
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Writes the local computer's script under `dir` (the local node's state dir).
    pub fn with_local_state_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.local_state_dir = dir;
        self
    }

    /// Runs `cli` for the local computer (tests).
    pub fn with_cli(mut self, cli: Cli) -> Self {
        self.cli = cli;
        self
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    fn lock_of(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().unwrap_or_else(|e| e.into_inner()).entry(id.to_string()).or_default().clone()
    }

    /// The ids of every computer: `local` first, then the registry.
    fn all_ids(&self) -> anyhow::Result<Vec<String>> {
        let mut ids = vec![LOCAL.to_string()];
        ids.extend(self.computers.registry().list().map_err(|e| anyhow::anyhow!("{e:#}"))?.into_iter().map(|c| c.id));
        Ok(ids)
    }

    /// Applies the effective record of computer `id` now and keeps the report.
    pub async fn apply_to(&self, id: &str) -> ApplyReport {
        let lock = self.lock_of(id);
        let _one = lock.lock().await;
        let body = match effective_for(&self.store, id) {
            Ok(e) => node_settings(&e),
            Err(e) => return ApplyReport::all_failed(format!("에디터 설정을 읽을 수 없습니다: {e:#}")),
        };
        let report = if id == LOCAL {
            ember_node::editor_settings::apply(self.local.source(), &self.cli, self.local_state_dir.as_deref(), &body).await
        } else {
            match self.computers.client(id) {
                Ok(node) => match node.with_deadline(NODE_APPLY_TIMEOUT).put_editor_settings(&body).await {
                    Ok(r) => r,
                    Err(ClientError::Api { status: 404 | 405, .. }) => ApplyReport::all_failed(
                        "이 컴퓨터의 ember-node가 에디터 설정을 받지 못합니다. ember-node를 업데이트해야 합니다",
                    ),
                    Err(ClientError::Api { body, .. }) => ApplyReport::all_failed(body.error),
                    Err(e) => ApplyReport::all_failed(format!("노드에 연결할 수 없습니다: {e}")),
                },
                Err(e) => ApplyReport::all_failed(format!("노드에 연결할 수 없습니다: {e:#}")),
            }
        };
        // A computer removed meanwhile keeps no row.
        let known = id == LOCAL || matches!(self.computers.registry().get(id), Ok(Some(_)));
        if known {
            if let Err(e) = save_applied(&self.store, id, &report) {
                tracing::warn!("could not keep the editor settings report of {id}: {e:#}");
            }
        }
        if !report.all_applied() {
            tracing::info!(computer = id, "editor settings not fully applied");
        }
        report
    }

    /// Applies the effective record to every computer registered from now on (by url or by peer,
    /// the API and the enrol registrar alike), in the background, so an enrolled node gets its
    /// settings without a save (FR-W7a). The listener holds the service weakly.
    pub fn apply_on_registration(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.computers.add_register_listener(Arc::new(move |id: &str| {
            let Some(service) = weak.upgrade() else { return };
            // Registration outside a Tokio runtime (a synchronous caller) applies nothing.
            if tokio::runtime::Handle::try_current().is_ok() {
                service.apply_in_background(id);
            } else {
                tracing::warn!(computer = id, "registered outside a runtime: editor settings not applied");
            }
        }));
    }

    /// [`Self::apply_to`] in the background.
    pub fn apply_in_background(self: &Arc<Self>, id: &str) -> tokio::task::JoinHandle<ApplyReport> {
        let this = self.clone();
        let id = id.to_string();
        tokio::spawn(async move { this.apply_to(&id).await })
    }

    /// The GET body of computer `id`: `{overrides, effective, applied, reachable}`. With `refresh`,
    /// a node is asked for a fresher report.
    async fn computer_body(&self, id: &str, refresh: bool) -> anyhow::Result<Value> {
        let o = if id == LOCAL { EditorOverrides::default() } else { overrides(&self.store, id)? };
        let e = effective(&load(&self.store)?, &o);
        let mut report = applied(&self.store, id)?;
        let mut reachable = true;
        if id != LOCAL && refresh {
            let fresh = match self.computers.client(id) {
                Ok(node) => tokio::time::timeout(NODE_REPORT_TIMEOUT, node.with_deadline(NODE_REPORT_TIMEOUT).editor_settings())
                    .await
                    .ok()
                    .and_then(Result::ok),
                Err(_) => None,
            };
            match fresh {
                Some(state) => {
                    if let Some(r) = state.applied {
                        if report.as_ref().is_none_or(|kept| r.applied_at > kept.applied_at) {
                            let _ = save_applied(&self.store, id, &r);
                            report = Some(r);
                        }
                    }
                }
                None => reachable = false,
            }
        }
        Ok(json!({ "overrides": o, "effective": e, "applied": report, "reachable": reachable }))
    }
}

// ---------------------------------------------------------------------------------------------
// Routes

pub fn router(service: Arc<EditorSettingsService>) -> Router {
    Router::new()
        .route("/api/editor/settings", get(get_server).put(put_server))
        .route("/api/computers/{id}/editor/settings", get(get_computer).put(put_computer))
        .with_state(service)
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

/// The name the server knows the caller by: the owner key's claimant on the local listener, the
/// device over the transport, else [`UNKNOWN_CALLER`].
pub fn caller_name(ext: &Extensions) -> String {
    if let Some(o) = ext.get::<crate::owner::OwnerCaller>() {
        if !o.name.trim().is_empty() {
            return o.name.clone();
        }
    }
    if let Some(c) = ext.get::<crate::devices::scope::Caller>() {
        return format!("device {}", c.peer.fmt_short());
    }
    UNKNOWN_CALLER.to_string()
}

// An axum Response is the natural error for a handler helper; its size does not matter here.
#[allow(clippy::result_large_err)]
async fn read_body(req: Request) -> Result<(Extensions, Bytes), Response> {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 2 * 1024 * 1024)
        .await
        .map_err(|e| error(StatusCode::BAD_REQUEST, format!("bad request body: {e}")))?;
    Ok((parts.extensions, bytes))
}

async fn get_server(State(s): State<Arc<EditorSettingsService>>) -> Response {
    match load(&s.store) {
        Ok(e) => Json(e).into_response(),
        Err(e) => internal(e),
    }
}

async fn put_server(State(s): State<Arc<EditorSettingsService>>, req: Request) -> Response {
    let (ext, bytes) = match read_body(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let mut new: EditorSettings = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("bad editor settings: {e}")),
    };
    if let Err(m) = validate(&new) {
        return error(StatusCode::BAD_REQUEST, m);
    }
    let prev = match load(&s.store) {
        Ok(p) => p,
        Err(e) => return internal(e),
    };
    (new.vscode_license_accepted_by, new.vscode_license_accepted_at) = stamp(
        prev.vscode_license_accepted,
        prev.vscode_license_accepted_by.clone(),
        prev.vscode_license_accepted_at,
        new.vscode_license_accepted,
        &caller_name(&ext),
        now_ms(),
    );
    // What each computer is sent before and after, to apply only where it changed.
    let ids = match s.all_ids() {
        Ok(ids) => ids,
        Err(e) => return internal(e),
    };
    let mut before = Vec::with_capacity(ids.len());
    for id in &ids {
        let o = if id == LOCAL { Ok(EditorOverrides::default()) } else { overrides(&s.store, id) };
        match o {
            Ok(o) => before.push((id.clone(), o)),
            Err(e) => return internal(e),
        }
    }
    if let Err(e) = save(&s.store, &new) {
        return internal(e);
    }
    for (id, o) in before {
        if node_settings(&effective(&prev, &o)) != node_settings(&effective(&new, &o)) {
            s.apply_in_background(&id);
        }
    }
    tracing::info!(editor_server = new.editor_server.as_str(), "editor settings saved");
    Json(new).into_response()
}

/// `local`, or a registered computer; else the 404 to answer.
#[allow(clippy::result_large_err)]
fn known_computer(s: &EditorSettingsService, id: &str) -> Result<(), Response> {
    if id == LOCAL {
        return Ok(());
    }
    match s.computers.registry().get(id) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(error(StatusCode::NOT_FOUND, format!("unknown computer: {id}"))),
        Err(e) => Err(internal(e)),
    }
}

async fn get_computer(State(s): State<Arc<EditorSettingsService>>, Path(id): Path<String>) -> Response {
    if let Err(r) = known_computer(&s, &id) {
        return r;
    }
    match s.computer_body(&id, true).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => internal(e),
    }
}

async fn put_computer(State(s): State<Arc<EditorSettingsService>>, Path(id): Path<String>, req: Request) -> Response {
    if id == LOCAL {
        return error(StatusCode::BAD_REQUEST, "the local computer has no overrides: change the server's editor settings");
    }
    if let Err(r) = known_computer(&s, &id) {
        return r;
    }
    let (ext, bytes) = match read_body(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let mut new: EditorOverrides = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("bad editor overrides: {e}")),
    };
    let (server, prev) = match (load(&s.store), overrides(&s.store, &id)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return internal(e),
    };
    if new.vscode_license_accepted.is_some() {
        (new.vscode_license_accepted_by, new.vscode_license_accepted_at) = stamp(
            prev.vscode_license_accepted == Some(true),
            prev.vscode_license_accepted_by.clone(),
            prev.vscode_license_accepted_at,
            new.vscode_license_accepted == Some(true),
            &caller_name(&ext),
            now_ms(),
        );
    } else {
        (new.vscode_license_accepted_by, new.vscode_license_accepted_at) = (None, None);
    }
    // Each present field on its own (a theme or an id the server would refuse), then the record
    // it makes.
    if let Some(t) = &new.theme {
        if let Err(m) = validate_parts(EditorServer::CodeOse, None, &[], Some(t)) {
            return error(StatusCode::BAD_REQUEST, m);
        }
    }
    if let Err(m) = validate(&effective(&server, &new)) {
        return error(StatusCode::BAD_REQUEST, m);
    }
    if let Err(e) = save_overrides(&s.store, &id, &new) {
        return internal(e);
    }
    let mut task = s.apply_in_background(&id);
    let _ = tokio::time::timeout(PUT_APPLY_WAIT, &mut task).await;
    match s.computer_body(&id, true).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => internal(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_w7_defaults_hold_and_overrides_inherit_when_absent_or_null() {
        let d = EditorSettings::default();
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            json!({
                "editor_server": "code_ose", "script": null, "vscode_license_accepted": false,
                "vscode_license_accepted_by": null, "vscode_license_accepted_at": null,
                "extensions": [], "theme": "DarkPyonix Dark", "default_ide": "ember", "settings": {}
            })
        );
        let o: EditorOverrides = serde_json::from_value(json!({ "theme": null, "default_ide": "gateway" })).unwrap();
        let e = effective(&d, &o);
        assert_eq!(e.theme, DEFAULT_THEME);
        assert_eq!(e.default_ide, DefaultIde::Gateway);
        assert_eq!(serde_json::to_value(EditorOverrides::default()).unwrap(), json!({}));
    }

    #[test]
    fn fr_w7_licence_stamp_is_set_kept_and_cleared() {
        assert_eq!(stamp(false, None, None, true, "me", 5), (Some("me".into()), Some(5)));
        assert_eq!(stamp(true, Some("a".into()), Some(1), true, "me", 5), (Some("a".into()), Some(1)));
        assert_eq!(stamp(true, Some("a".into()), Some(1), false, "me", 5), (None, None));
    }

    #[test]
    fn fr_w7a_node_body_leaves_out_the_ide_choice() {
        let mut a = EditorSettings::default();
        let mut b = a.clone();
        b.default_ide = DefaultIde::Vscode;
        assert_eq!(node_settings(&a), node_settings(&b), "only default_ide differs: nothing to apply");
        a.theme = "Other".into();
        assert_ne!(node_settings(&a), node_settings(&b));
        let v = serde_json::to_value(node_settings(&b)).unwrap();
        assert!(v.get("default_ide").is_none() && v.get("vscode_license_accepted_by").is_none(), "{v}");
    }
}
