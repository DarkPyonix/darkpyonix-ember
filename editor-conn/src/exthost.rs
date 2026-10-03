//! Extension-host connection: bootstrap and the typed minimal subset of `extHost.protocol.ts`.
//!
//! **Bootstrap** (`remoteExtensionHost.ts` L146-180, `extensionHostProcess.ts` L330-394):
//! after the `ExtensionHost` handshake the server hands the socket to a freshly spawned Node
//! extension host. It sends the 1-byte regular message `Ready` (2); the renderer answers with the
//! JSON `IExtensionHostInitData`; the extension host answers `Initialized` (1). From then on every
//! regular message is an RPC message ([`crate::rpc`]). `Terminate` (3) asks it to exit
//! (`extensionHostProtocol.ts` L123-143).
//!
//! The extension host then blocks extension activation until the renderer has called
//! `ExtHostWorkspace.$initializeWorkspace` (`extHostExtensionService.ts` L218) and
//! `ExtHostConfiguration.$initializeConfiguration` (barrier in `extHostConfiguration.ts` L107-131,
//! awaited in `api/node/extHostExtensionService.ts` L180). Upstream those calls come from the
//! `MainThreadWorkspace` / `MainThreadConfiguration` constructors; here from [`bootstrap_calls`].
//!
//! **Minimal subset** — what Ember's editor core needs for SPEC FR-E2..E4 (diagnostics, CodeLens,
//! hover, inline completions) plus what the extension host needs to run at all. See the table in
//! `docs/design/EDITOR-CONNECTION.md` §4.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::connection::{ConnEvent, Connection};
use crate::management::{extension_id, RemoteAgentEnvironment};
use crate::rpc::{Arg, IncomingRequest, Reply, RpcEvent, RpcPeer};
use crate::uri::UriComponents;
use crate::{Error, Result};

/// `MessageType` bytes of `createMessageOfType` (extensionHostProtocol.ts L129-139).
pub const MSG_INITIALIZED: u8 = 1;
pub const MSG_READY: u8 = 2;
pub const MSG_TERMINATE: u8 = 3;

/// Proxy identifier names used below (resolved to numbers by [`crate::rpc_ids`]).
pub mod proxy {
    pub const EXT_HOST_COMMANDS: &str = "ExtHostCommands";
    pub const EXT_HOST_CONFIGURATION: &str = "ExtHostConfiguration";
    pub const EXT_HOST_DOCUMENTS_AND_EDITORS: &str = "ExtHostDocumentsAndEditors";
    pub const EXT_HOST_DOCUMENTS: &str = "ExtHostDocuments";
    pub const EXT_HOST_EDITORS: &str = "ExtHostEditors";
    pub const EXT_HOST_LANGUAGE_FEATURES: &str = "ExtHostLanguageFeatures";
    pub const EXT_HOST_EXTENSION_SERVICE: &str = "ExtHostExtensionService";
    pub const EXT_HOST_WORKSPACE: &str = "ExtHostWorkspace";
    pub const EXT_HOST_FILE_SYSTEM_INFO: &str = "ExtHostFileSystemInfo";

    pub const MAIN_THREAD_COMMANDS: &str = "MainThreadCommands";
    pub const MAIN_THREAD_DIAGNOSTICS: &str = "MainThreadDiagnostics";
    pub const MAIN_THREAD_LANGUAGE_FEATURES: &str = "MainThreadLanguageFeatures";
    pub const MAIN_THREAD_STORAGE: &str = "MainThreadStorage";
    pub const MAIN_THREAD_WINDOW: &str = "MainThreadWindow";
    pub const MAIN_THREAD_WORKSPACE: &str = "MainThreadWorkspace";
    pub const MAIN_THREAD_EXTENSION_SERVICE: &str = "MainThreadExtensionService";
}

// ---- init data ------------------------------------------------------------------------------

/// Inputs for `IExtensionHostInitData` (extensionHostProtocol.ts L28-63), as
/// `RemoteExtensionHost._createExtHostInitData` (remoteExtensionHost.ts L206-262) fills it.
#[derive(Debug, Clone)]
pub struct InitDataParams {
    pub version: String,
    pub quality: Option<String>,
    /// Leave `None` unless it is the server's exact commit: on mismatch the extension host exits
    /// with `VersionMismatch` (extensionHostProcess.ts L340-347).
    pub commit: Option<String>,
    pub remote_authority: String,
    pub env: RemoteAgentEnvironment,
    pub workspace: Option<WorkspaceData>,
    /// `scanExtensions` result, passed through untouched.
    pub extensions: Vec<Value>,
    pub app_language: String,
    pub session_id: String,
    pub machine_id: String,
    /// `LogLevel`: 0 Off, 1 Trace, 2 Debug, 3 Info, 4 Warning, 5 Error.
    pub log_level: u8,
}

impl InitDataParams {
    pub fn to_json(&self) -> Value {
        let ids: Vec<Value> = self
            .extensions
            .iter()
            .filter_map(extension_id)
            .map(|id| json!({ "value": id, "_lower": id.to_lowercase() }))
            .collect();
        let mut init = json!({
            "version": self.version,
            "quality": self.quality,
            "parentPid": self.env.pid,
            "environment": {
                "isExtensionDevelopmentDebug": false,
                "appName": "Ember",
                "appHost": "desktop",
                "appRoot": self.env.app_root,
                "appLanguage": self.app_language,
                "isExtensionTelemetryLoggingOnly": false,
                "appUriScheme": "ember",
                "globalStorageHome": self.env.global_storage_home,
                "workspaceStorageHome": self.env.workspace_storage_home,
                "extensionLogLevel": [],
            },
            "workspace": self.workspace.as_ref().map(WorkspaceData::static_json),
            "extensions": {
                "versionId": 1,
                "allExtensions": self.extensions,
                "myExtensions": ids,
                "activationEvents": activation_events_map(&self.extensions),
            },
            "telemetryInfo": {
                "sessionId": self.session_id,
                "machineId": self.machine_id,
                "sqmId": "",
                "devDeviceId": self.machine_id,
                "firstSessionDate": "",
            },
            "logLevel": self.log_level,
            "loggers": [],
            "logsLocation": self.env.extension_host_logs_path,
            "autoStart": true,
            "remote": {
                "isRemote": true,
                "authority": self.remote_authority,
                "connectionData": null,
            },
            "consoleForward": { "includeStack": false, "logNative": false },
            // UIKind.Desktop = 1, Web = 2. Ember is a native client.
            "uiKind": 1,
        });
        if let Some(c) = &self.commit {
            init["commit"] = Value::from(c.as_str());
        }
        init
    }
}

/// `ImplicitActivationEvents.createActivationEventsMap` for the contribution points that matter
/// most. Upstream ~20 extension points register generators in the renderer
/// (`implicitActivationEvents.ts` L38-75); this covers commands, languages and views. Extensions
/// without `main`/`browser` get no entry.
pub fn activation_events_map(extensions: &[Value]) -> Value {
    let mut map = Map::new();
    for desc in extensions {
        if desc.get("main").is_none() && desc.get("browser").is_none() {
            continue;
        }
        let Some(id) = extension_id(desc) else { continue };
        let key = id.to_lowercase();
        let mut events: Vec<String> = desc
            .get("activationEvents")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        for e in events.iter_mut() {
            if e == "onUri" {
                *e = format!("onUri:{key}");
            }
        }
        if let Some(c) = desc.get("contributes") {
            for cmd in c.get("commands").map(as_list).unwrap_or_default() {
                if let Some(id) = cmd.get("command").and_then(Value::as_str) {
                    events.push(format!("onCommand:{id}"));
                }
            }
            for lang in c.get("languages").map(as_list).unwrap_or_default() {
                if let Some(id) = lang.get("id").and_then(Value::as_str) {
                    events.push(format!("onLanguage:{id}"));
                }
            }
            if let Some(views) = c.get("views").and_then(Value::as_object) {
                for list in views.values() {
                    for v in as_list(list) {
                        if let Some(id) = v.get("id").and_then(Value::as_str) {
                            events.push(format!("onView:{id}"));
                        }
                    }
                }
            }
        }
        if !events.is_empty() {
            map.insert(key, Value::from(events));
        }
    }
    Value::Object(map)
}

fn as_list(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

/// Wait for `Ready`, send the init data, wait for `Initialized`, then hand the connection to an
/// [`RpcPeer`]. Upstream allows 60 s for `Ready`.
pub async fn initialize(conn: Connection, init: &Value) -> Result<(RpcPeer, mpsc::UnboundedReceiver<RpcEvent>)> {
    let Connection { handle, mut events } = conn;
    let init_bytes = serde_json::to_vec(init)?;
    let fut = async {
        loop {
            match events.recv().await {
                Some(ConnEvent::Message(m)) if m == [MSG_READY] => handle.send(init_bytes.clone()),
                Some(ConnEvent::Message(m)) if m == [MSG_INITIALIZED] => return Ok(()),
                Some(ConnEvent::Message(m)) => {
                    tracing::warn!("editor-conn exthost: unexpected {}-byte message during init", m.len());
                }
                Some(ConnEvent::Lost(_)) | Some(ConnEvent::Disconnected) | None => return Err(Error::Closed),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(60), fut)
        .await
        .map_err(|_| Error::Timeout("extension host ready/initialized"))??;
    Ok(RpcPeer::start(handle, events))
}

/// Ask the extension host to exit (`RemoteExtensionHost.disconnect`).
pub fn terminate(peer_conn: &crate::connection::ConnectionHandle) {
    peer_conn.send(vec![MSG_TERMINATE]);
    peer_conn.close();
}

// ---- shared DTOs ----------------------------------------------------------------------------

/// `IRange`: 1-based lines, 1-based UTF-16 columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    pub start_line_number: u32,
    pub start_column: u32,
    pub end_line_number: u32,
    pub end_column: u32,
}

/// `IPosition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub line_number: u32,
    pub column: u32,
}

/// `ISelection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub selection_start_line_number: u32,
    pub selection_start_column: u32,
    pub position_line_number: u32,
    pub position_column: u32,
}

/// `IWorkspaceData` (extHost.protocol.ts L112): `IStaticWorkspaceData` + folders.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceData {
    pub id: String,
    pub name: String,
    pub folders: Vec<WorkspaceFolder>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceFolder {
    pub uri: UriComponents,
    pub name: String,
    pub index: u32,
}

impl WorkspaceData {
    /// The `IStaticWorkspaceData` part used in the init data.
    pub fn static_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "configuration": null })
    }
}

/// `IMarkerData` (platform/markers/common/markers.ts). Severity: 1 Hint, 2 Info, 4 Warning, 8 Error.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerData {
    #[serde(default)]
    pub code: Option<Value>,
    pub severity: u8,
    pub message: String,
    #[serde(default)]
    pub source: Option<String>,
    pub start_line_number: u32,
    pub start_column: u32,
    pub end_line_number: u32,
    pub end_column: u32,
    #[serde(default)]
    pub model_version_id: Option<i64>,
    #[serde(default)]
    pub related_information: Option<Value>,
    /// `MarkerTag`: 1 Unnecessary, 2 Deprecated.
    #[serde(default)]
    pub tags: Option<Vec<u8>>,
}

/// `IDocumentFilterDto` (extHost.protocol.ts L436-444).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFilter {
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub scheme: Option<String>,
    /// `string | IRelativePattern` (`{ baseUri, pattern }`).
    #[serde(default)]
    pub pattern: Option<Value>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub notebook_type: Option<String>,
}

// ---- Ember → extension host -----------------------------------------------------------------

/// One outgoing call: target proxy, method, positional args, whether a cancellation token is
/// implied (`…WithCancellation` message type).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub proxy: &'static str,
    pub method: &'static str,
    pub args: Vec<Arg>,
    pub cancellable: bool,
}

impl Call {
    fn new(proxy: &'static str, method: &'static str, args: Vec<Arg>) -> Self {
        Self { proxy, method, args, cancellable: false }
    }
    fn cancellable(mut self) -> Self {
        self.cancellable = true;
        self
    }

    pub fn start(self, peer: &RpcPeer) -> Result<crate::rpc::PendingCall> {
        peer.start_call(self.proxy, self.method, self.args, self.cancellable)
    }
}

fn j<T: Serialize>(v: &T) -> Arg {
    Arg::Json(serde_json::to_value(v).unwrap_or(Value::Null))
}

/// Calls that must precede extension activation (see module docs).
pub fn bootstrap_calls(workspace: Option<&WorkspaceData>, trusted: bool, configuration: &Value) -> Vec<Call> {
    vec![
        Call::new(proxy::EXT_HOST_CONFIGURATION, "$initializeConfiguration", vec![Arg::Json(configuration.clone())]),
        Call::new(
            proxy::EXT_HOST_WORKSPACE,
            "$initializeWorkspace",
            vec![workspace.map(j).unwrap_or(Arg::Json(Value::Null)), Arg::Json(Value::Bool(trusted))],
        ),
    ]
}

/// `IConfigurationInitData` (extHost.protocol.ts L116) from defaults + user settings. Values are
/// nested objects keyed by dotted paths split on `.` (as `ConfigurationModel.contents`).
pub fn configuration_init_data(defaults: &[(String, Value, Option<u8>)], user: &[(String, Value)]) -> Value {
    fn insert_path(obj: &mut Map<String, Value>, parts: &[&str], v: Value) {
        match parts {
            [] => {}
            [last] => {
                obj.insert((*last).to_owned(), v);
            }
            [head, rest @ ..] => {
                let child = obj.entry((*head).to_owned()).or_insert_with(|| Value::Object(Map::new()));
                if !child.is_object() {
                    *child = Value::Object(Map::new());
                }
                if let Some(m) = child.as_object_mut() {
                    insert_path(m, rest, v);
                }
            }
        }
    }
    fn model(entries: &[(String, Value)]) -> Value {
        let mut contents = Map::new();
        for (k, v) in entries {
            let parts: Vec<&str> = k.split('.').collect();
            insert_path(&mut contents, &parts, v.clone());
        }
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        json!({ "contents": contents, "keys": keys, "overrides": [] })
    }
    let empty = model(&[]);
    let default_entries: Vec<(String, Value)> = defaults.iter().map(|(k, v, _)| (k.clone(), v.clone())).collect();
    let scopes: Vec<Value> = defaults.iter().map(|(k, _, s)| json!([k, s])).collect();
    json!({
        "defaults": model(&default_entries),
        "policy": empty,
        "application": empty,
        "userLocal": model(user),
        "userRemote": empty,
        "workspace": empty,
        "folders": [],
        "configurationScopes": scopes,
    })
}

/// Collect `(key, default, scope)` from extensions' `contributes.configuration`.
/// `ConfigurationScope`: 1 application, 2 machine, 3 application-machine, 4 window (default),
/// 5 resource, 6 language-overridable, 7 machine-overridable.
pub fn contributed_configuration_defaults(extensions: &[Value]) -> Vec<(String, Value, Option<u8>)> {
    let mut out = Vec::new();
    for desc in extensions {
        let Some(cfg) = desc.get("contributes").and_then(|c| c.get("configuration")) else { continue };
        for section in as_list(cfg) {
            let Some(props) = section.get("properties").and_then(Value::as_object) else { continue };
            for (key, schema) in props {
                let scope = schema.get("scope").and_then(Value::as_str).map(|s| match s {
                    "application" => 1,
                    "machine" => 2,
                    "application-machine" => 3,
                    "resource" => 5,
                    "language-overridable" => 6,
                    "machine-overridable" => 7,
                    _ => 4,
                });
                let default = schema.get("default").cloned().unwrap_or(Value::Null);
                out.push((key.clone(), default, scope));
            }
        }
    }
    out
}

/// `ExtHostExtensionService.$activateByEvent(activationEvent, ActivationKind.Normal)`.
/// Ember must send `onLanguage:<id>` when a document of that language opens and
/// `onCommand:<id>` before executing a contributed command (upstream: `languageService.ts` L289,
/// `abstractExtensionService.activateByEvent`).
pub fn activate_by_event(event: &str) -> Call {
    Call::new(proxy::EXT_HOST_EXTENSION_SERVICE, "$activateByEvent", vec![Arg::Json(event.into()), Arg::Json(0.into())])
}

/// `IModelAddedData` (extHost.protocol.ts L397-405).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAddedData {
    pub uri: UriComponents,
    pub version_id: u64,
    pub lines: Vec<String>,
    #[serde(rename = "EOL")]
    pub eol: String,
    pub language_id: String,
    pub is_dirty: bool,
    pub encoding: String,
}

/// `ITextEditorAddData` (extHost.protocol.ts L418-425). `options` is
/// `IResolvedTextEditorConfiguration`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEditorAddData {
    pub id: String,
    pub document_uri: UriComponents,
    pub options: Value,
    pub selections: Vec<Selection>,
    pub visible_ranges: Vec<Range>,
    /// `EditorGroupColumn`: 0-based view column, or `None`.
    pub editor_position: Option<i32>,
}

/// `IDocumentsAndEditorsDelta` (extHost.protocol.ts L460-466).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentsAndEditorsDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_documents: Option<Vec<UriComponents>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_documents: Option<Vec<ModelAddedData>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_editors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_editors: Option<Vec<TextEditorAddData>>,
    /// `Some(None)` = no active editor; `None` = unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_active_editor: Option<Option<String>>,
}

pub fn accept_documents_and_editors_delta(delta: &DocumentsAndEditorsDelta) -> Call {
    Call::new(proxy::EXT_HOST_DOCUMENTS_AND_EDITORS, "$acceptDocumentsAndEditorsDelta", vec![j(delta)])
}

/// `ITextEditorConfiguration` defaults for `TextEditorAddData::options`.
pub fn default_editor_options(tab_size: u32, insert_spaces: bool) -> Value {
    // TextEditorCursorStyle.Line = 1, RenderLineNumbersType.On = 1
    json!({ "tabSize": tab_size, "indentSize": tab_size, "originalIndentSize": tab_size, "insertSpaces": insert_spaces, "cursorStyle": 1, "lineNumbers": 1 })
}

/// `ExtHostDocuments.$acceptModelChanged(uri, ISerializedModelContentChangedEvent, isDirty)`.
pub fn accept_model_changed(uri: &UriComponents, event: &crate::document::ModelContentChangedEvent, is_dirty: bool) -> Call {
    Call::new(
        proxy::EXT_HOST_DOCUMENTS,
        "$acceptModelChanged",
        vec![j(uri), j(event), Arg::Json(Value::Bool(is_dirty))],
    )
}

pub fn accept_model_saved(uri: &UriComponents) -> Call {
    Call::new(proxy::EXT_HOST_DOCUMENTS, "$acceptModelSaved", vec![j(uri)])
}

pub fn accept_dirty_state_changed(uri: &UriComponents, is_dirty: bool) -> Call {
    Call::new(proxy::EXT_HOST_DOCUMENTS, "$acceptDirtyStateChanged", vec![j(uri), Arg::Json(is_dirty.into())])
}

pub fn accept_model_language_changed(uri: &UriComponents, language_id: &str) -> Call {
    Call::new(proxy::EXT_HOST_DOCUMENTS, "$acceptModelLanguageChanged", vec![j(uri), Arg::Json(language_id.into())])
}

/// `$provideHover(handle, resource, position, context | undefined, token)` → `HoverWithId | undefined`.
pub fn provide_hover(handle: i64, uri: &UriComponents, pos: Position) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$provideHover", vec![Arg::Json(handle.into()), j(uri), j(&pos), Arg::Undefined])
        .cancellable()
}

pub fn release_hover(handle: i64, id: i64) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$releaseHover", vec![Arg::Json(handle.into()), Arg::Json(id.into())])
}

/// `$provideCodeLenses(handle, resource, token)` → `ICodeLensListDto | undefined`.
pub fn provide_code_lenses(handle: i64, uri: &UriComponents) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$provideCodeLenses", vec![Arg::Json(handle.into()), j(uri)]).cancellable()
}

/// `$resolveCodeLens(handle, ICodeLensDto, token)`; pass the lens exactly as received (with its
/// `cacheId`).
pub fn resolve_code_lens(handle: i64, lens: &Value) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$resolveCodeLens", vec![Arg::Json(handle.into()), Arg::Json(lens.clone())])
        .cancellable()
}

/// `$releaseCodeLenses(handle, cacheId)` — required, or the extension host leaks the list.
pub fn release_code_lenses(handle: i64, cache_id: i64) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$releaseCodeLenses", vec![Arg::Json(handle.into()), Arg::Json(cache_id.into())])
}

/// `InlineCompletionContext` (languages.ts L767-777).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineCompletionContext {
    /// 0 Automatic, 1 Explicit.
    pub trigger_kind: u8,
    pub selected_suggestion_info: Option<Value>,
    pub request_uuid: String,
    pub include_inline_edits: bool,
    pub include_inline_completions: bool,
    pub request_issued_date_time: f64,
    pub earliest_shown_date_time: f64,
}

/// `$provideInlineCompletions(handle, resource, position, context, token)` →
/// `IdentifiableInlineCompletions | undefined` (`{ items: [{ insertText, range?, command?, pid,
/// idx, … }], pid, languageId, … }`).
pub fn provide_inline_completions(handle: i64, uri: &UriComponents, pos: Position, ctx: &InlineCompletionContext) -> Call {
    Call::new(
        proxy::EXT_HOST_LANGUAGE_FEATURES,
        "$provideInlineCompletions",
        vec![Arg::Json(handle.into()), j(uri), j(&pos), j(ctx)],
    )
    .cancellable()
}

pub fn handle_inline_completion_did_show(handle: i64, pid: i64, idx: i64, updated_insert_text: &str) -> Call {
    Call::new(
        proxy::EXT_HOST_LANGUAGE_FEATURES,
        "$handleInlineCompletionDidShow",
        vec![Arg::Json(handle.into()), Arg::Json(pid.into()), Arg::Json(idx.into()), Arg::Json(updated_insert_text.into())],
    )
}

/// `$handleInlineCompletionEndOfLifetime(handle, pid, idx, reason)`; `reason.kind`: 0 Accepted
/// (`alternativeAction`), 1 Rejected, 2 Ignored (`userTypingDisagreed`, `supersededBy?`).
pub fn handle_inline_completion_end_of_lifetime(handle: i64, pid: i64, idx: i64, reason: Value) -> Call {
    Call::new(
        proxy::EXT_HOST_LANGUAGE_FEATURES,
        "$handleInlineCompletionEndOfLifetime",
        vec![Arg::Json(handle.into()), Arg::Json(pid.into()), Arg::Json(idx.into()), Arg::Json(reason)],
    )
}

/// `$freeInlineCompletionsList(handle, pid, { kind })`, kind ∈ lostRace | tokenCancellation |
/// other | empty | notTaken. Required to release the list in the extension host.
pub fn free_inline_completions_list(handle: i64, pid: i64, kind: &str) -> Call {
    Call::new(
        proxy::EXT_HOST_LANGUAGE_FEATURES,
        "$freeInlineCompletionsList",
        vec![Arg::Json(handle.into()), Arg::Json(pid.into()), Arg::Json(json!({ "kind": kind }))],
    )
}

/// `ExtHostCommands.$executeContributedCommand(id, ...args)` — e.g. the `command` of an accepted
/// inline completion or a clicked CodeLens.
pub fn execute_contributed_command(id: &str, args: &[Value]) -> Call {
    let mut a = vec![Arg::Json(id.into())];
    a.extend(args.iter().cloned().map(Arg::Json));
    Call::new(proxy::EXT_HOST_COMMANDS, "$executeContributedCommand", a)
}

// ---- extension host → Ember -----------------------------------------------------------------

/// Decoded `MainThread*` calls the editor core acts on. Everything else is
/// [`MainThreadCall::Other`] and gets [`default_reply`].
#[derive(Debug, Clone, PartialEq)]
pub enum MainThreadCall {
    DiagnosticsChangeMany { owner: String, entries: Vec<(UriComponents, Option<Vec<MarkerData>>)> },
    DiagnosticsClear { owner: String },
    RegisterHoverProvider { handle: i64, selector: Vec<DocumentFilter> },
    RegisterCodeLensSupport { handle: i64, selector: Vec<DocumentFilter>, event_handle: Option<i64> },
    EmitCodeLensEvent { event_handle: i64 },
    /// 17 positional args at the pinned commit (extHost.protocol.ts L556-574); only the stable
    /// head is decoded, the rest kept raw.
    RegisterInlineCompletionsSupport { handle: i64, selector: Vec<DocumentFilter>, extension_id: String, raw: Vec<Value> },
    EmitInlineCompletionsChange { handle: i64 },
    Unregister { handle: i64 },
    RegisterCommand { id: String },
    UnregisterCommand { id: String },
    ExecuteCommand { id: String, args: Value },
    Other { proxy: Option<&'static str>, method: String },
}

fn arg_json(args: &[Arg], i: usize) -> Value {
    args.get(i).and_then(Arg::as_json).cloned().unwrap_or(Value::Null)
}

fn arg_i64(args: &[Arg], i: usize) -> Result<i64> {
    arg_json(args, i).as_i64().ok_or_else(|| Error::Malformed(format!("arg {i} is not an integer")))
}

fn arg_str(args: &[Arg], i: usize) -> Result<String> {
    arg_json(args, i)
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Malformed(format!("arg {i} is not a string")))
}

impl MainThreadCall {
    pub fn parse(r: &IncomingRequest) -> Result<Self> {
        let a = &r.args;
        let m = r.method.as_str();
        Ok(match (r.proxy, m) {
            (Some(proxy::MAIN_THREAD_DIAGNOSTICS), "$changeMany") => Self::DiagnosticsChangeMany {
                owner: arg_str(a, 0)?,
                entries: serde_json::from_value(arg_json(a, 1))?,
            },
            (Some(proxy::MAIN_THREAD_DIAGNOSTICS), "$clear") => Self::DiagnosticsClear { owner: arg_str(a, 0)? },
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$registerHoverProvider") => Self::RegisterHoverProvider {
                handle: arg_i64(a, 0)?,
                selector: serde_json::from_value(arg_json(a, 1))?,
            },
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$registerCodeLensSupport") => Self::RegisterCodeLensSupport {
                handle: arg_i64(a, 0)?,
                selector: serde_json::from_value(arg_json(a, 1))?,
                event_handle: arg_json(a, 2).as_i64(),
            },
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$emitCodeLensEvent") => {
                Self::EmitCodeLensEvent { event_handle: arg_i64(a, 0)? }
            }
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$registerInlineCompletionsSupport") => {
                Self::RegisterInlineCompletionsSupport {
                    handle: arg_i64(a, 0)?,
                    selector: serde_json::from_value(arg_json(a, 1))?,
                    extension_id: arg_json(a, 3).as_str().unwrap_or_default().to_owned(),
                    raw: a.iter().map(|x| x.as_json().cloned().unwrap_or(Value::Null)).collect(),
                }
            }
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$emitInlineCompletionsChange") => {
                Self::EmitInlineCompletionsChange { handle: arg_i64(a, 0)? }
            }
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$unregister") => Self::Unregister { handle: arg_i64(a, 0)? },
            (Some(proxy::MAIN_THREAD_COMMANDS), "$registerCommand") => Self::RegisterCommand { id: arg_str(a, 0)? },
            (Some(proxy::MAIN_THREAD_COMMANDS), "$unregisterCommand") => Self::UnregisterCommand { id: arg_str(a, 0)? },
            (Some(proxy::MAIN_THREAD_COMMANDS), "$executeCommand") => {
                Self::ExecuteCommand { id: arg_str(a, 0)?, args: arg_json(a, 1) }
            }
            (proxy, _) => Self::Other { proxy, method: r.method.clone() },
        })
    }
}

/// The reply Ember gives to `MainThread*` calls it does not implement, chosen so the extension
/// host keeps running. Most upstream methods are `void` or `Promise<void>` (→ `undefined`).
/// The exceptions below would otherwise block or mislead extensions:
///
/// * `MainThreadStorage.$initializeExtensionStorage` — awaited during every activation
///   (`undefined` = no stored state).
/// * `MainThreadWindow.$getInitialState` — `{ isFocused, isActive }`.
/// * `MainThreadWorkspace.$checkExists` — `workspaceContains:` activation; `false` until Ember
///   implements glob search over the remote filesystem.
/// * `MainThreadWorkspace.$isResourceTrusted` / `$requestWorkspaceTrust` — trusted.
/// * `MainThreadCommands.$getCommands` — empty list.
pub fn default_reply(proxy: Option<&str>, method: &str) -> Reply {
    match (proxy, method) {
        (Some(proxy::MAIN_THREAD_WINDOW), "$getInitialState") => Reply::Json(json!({ "isFocused": true, "isActive": true })),
        (Some(proxy::MAIN_THREAD_WORKSPACE), "$checkExists") => Reply::Json(Value::Bool(false)),
        (Some(proxy::MAIN_THREAD_WORKSPACE), "$isResourceTrusted" | "$requestWorkspaceTrust") => Reply::Json(Value::Bool(true)),
        (Some(proxy::MAIN_THREAD_COMMANDS), "$getCommands") => Reply::Json(json!([])),
        _ => Reply::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(proxy: &'static str, method: &str, args: Value) -> IncomingRequest {
        IncomingRequest {
            req: 1,
            rpc_id: crate::rpc_ids::id_of(proxy).unwrap(),
            proxy: Some(proxy),
            method: method.into(),
            args: args.as_array().unwrap().iter().cloned().map(Arg::Json).collect(),
            cancellable: false,
        }
    }

    #[test]
    fn parses_change_many() {
        let r = req(
            proxy::MAIN_THREAD_DIAGNOSTICS,
            "$changeMany",
            json!(["rustc", [[{"$mid":1,"scheme":"vscode-remote","authority":"h","path":"/w/a.rs"},
                [{"severity":8,"message":"mismatched types","startLineNumber":3,"startColumn":5,"endLineNumber":3,"endColumn":9,"source":"rustc"}]],
                [{"$mid":1,"scheme":"vscode-remote","authority":"h","path":"/w/b.rs"}, null]]]),
        );
        match MainThreadCall::parse(&r).unwrap() {
            MainThreadCall::DiagnosticsChangeMany { owner, entries } => {
                assert_eq!(owner, "rustc");
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].1.as_ref().unwrap()[0].start_column, 5);
                assert!(entries[1].1.is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_hover_registration() {
        let r = req(
            proxy::MAIN_THREAD_LANGUAGE_FEATURES,
            "$registerHoverProvider",
            json!([4, [{"$serialized":true,"language":"rust","scheme":"vscode-remote"}]]),
        );
        assert_eq!(
            MainThreadCall::parse(&r).unwrap(),
            MainThreadCall::RegisterHoverProvider {
                handle: 4,
                selector: vec![DocumentFilter {
                    language: Some("rust".into()),
                    scheme: Some("vscode-remote".into()),
                    pattern: None,
                    exclusive: None,
                    notebook_type: None,
                }],
            }
        );
    }

    #[test]
    fn hover_call_is_cancellable_with_undefined_context() {
        let c = provide_hover(4, &UriComponents::remote("h", "/w/a.rs"), Position { line_number: 1, column: 2 });
        assert!(c.cancellable);
        assert_eq!(c.args.len(), 4);
        assert_eq!(c.args[3], Arg::Undefined);
    }

    #[test]
    fn activation_events_include_implicit_ones() {
        let exts = vec![json!({
            "identifier": {"value": "Pub.Ext", "_lower": "pub.ext"},
            "main": "./out/main.js",
            "activationEvents": ["onStartupFinished", "onUri"],
            "contributes": {
                "commands": [{"command": "ext.run", "title": "Run"}],
                "languages": [{"id": "foo"}],
                "views": {"explorer": [{"id": "ext.view", "name": "V"}]}
            }
        }), json!({"identifier": {"value": "theme.only"}, "contributes": {}})];
        let m = activation_events_map(&exts);
        assert_eq!(
            m,
            json!({"pub.ext": ["onStartupFinished", "onUri:pub.ext", "onCommand:ext.run", "onLanguage:foo", "onView:ext.view"]})
        );
    }

    #[test]
    fn configuration_model_nests_dotted_keys() {
        let data = configuration_init_data(
            &[("editor.tabSize".into(), json!(4), Some(6)), ("rust-analyzer.check.command".into(), json!("check"), None)],
            &[],
        );
        assert_eq!(data["defaults"]["contents"]["editor"]["tabSize"], json!(4));
        assert_eq!(data["defaults"]["contents"]["rust-analyzer"]["check"]["command"], json!("check"));
        assert_eq!(data["configurationScopes"][0], json!(["editor.tabSize", 6]));
        assert_eq!(data["configurationScopes"][1], json!(["rust-analyzer.check.command", null]));
    }

    #[test]
    fn storage_init_defaults_to_undefined() {
        assert_eq!(default_reply(Some(proxy::MAIN_THREAD_STORAGE), "$initializeExtensionStorage"), Reply::Empty);
        assert_eq!(
            default_reply(Some(proxy::MAIN_THREAD_WINDOW), "$getInitialState"),
            Reply::Json(json!({"isFocused": true, "isActive": true}))
        );
    }
}
