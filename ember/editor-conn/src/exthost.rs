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
//! awaited in `api/node/extHostExtensionService.ts` L179). Upstream those calls come from the
//! `MainThreadWorkspace` / `MainThreadConfiguration` constructors; here from [`bootstrap_calls`].
//!
//! **Minimal subset**: what Ember's editor core needs for SPEC FR-E2..E4 (diagnostics, CodeLens,
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
    pub const EXT_HOST_SEARCH: &str = "ExtHostSearch";

    pub const MAIN_THREAD_COMMANDS: &str = "MainThreadCommands";
    pub const MAIN_THREAD_DIAGNOSTICS: &str = "MainThreadDiagnostics";
    pub const MAIN_THREAD_LANGUAGE_FEATURES: &str = "MainThreadLanguageFeatures";
    pub const MAIN_THREAD_STORAGE: &str = "MainThreadStorage";
    pub const MAIN_THREAD_WINDOW: &str = "MainThreadWindow";
    pub const MAIN_THREAD_WORKSPACE: &str = "MainThreadWorkspace";
    pub const MAIN_THREAD_EXTENSION_SERVICE: &str = "MainThreadExtensionService";
    pub const MAIN_THREAD_OUTPUT_SERVICE: &str = "MainThreadOutputService";
    pub const MAIN_THREAD_MESSAGE_SERVICE: &str = "MainThreadMessageService";
    pub const MAIN_THREAD_SEARCH: &str = "MainThreadSearch";
}

// ---- init data ------------------------------------------------------------------------------

/// Inputs for `IExtensionHostInitData` (extensionHostProtocol.ts L28-63), as
/// `RemoteExtensionHost._createExtHostInitData` (remoteExtensionHost.ts L206-262) fills it.
#[derive(Debug, Clone)]
pub struct InitDataParams {
    pub version: String,
    pub quality: Option<String>,
    /// Leave `None` unless it is the server's exact commit (the value returned by
    /// [`crate::handshake::verify_server`], i.e. [`crate::PINNED_COMMIT`]): on mismatch the
    /// extension host exits with `VersionMismatch` (extensionHostProcess.ts L340-347).
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

/// `IDocumentFilterDto` (extHost.protocol.ts L438-446).
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

/// `SemanticTokensLegend` (`languages.ts`, sent with `$registerDocumentSemanticTokensProvider`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticTokensLegend {
    #[serde(default)]
    pub token_types: Vec<String>,
    #[serde(default)]
    pub token_modifiers: Vec<String>,
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
    pub(crate) fn new(proxy: &'static str, method: &'static str, args: Vec<Arg>) -> Self {
        Self { proxy, method, args, cancellable: false }
    }
    pub(crate) fn cancellable(mut self) -> Self {
        self.cancellable = true;
        self
    }

    pub fn start(self, peer: &RpcPeer) -> Result<crate::rpc::PendingCall> {
        peer.start_call(self.proxy, self.method, self.args, self.cancellable)
    }
}

pub(crate) fn j<T: Serialize>(v: &T) -> Arg {
    Arg::Json(serde_json::to_value(v).unwrap_or(Value::Null))
}

/// Calls that must precede extension activation (see module docs):
/// `ExtHostConfigurationShape.$initializeConfiguration(data)` (extHost.protocol.ts L2378) and
/// `ExtHostWorkspaceShape.$initializeWorkspace(workspace | null, trusted)` (L2510).
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

/// `ExtHostExtensionService.$activateByEvent(activationEvent, ActivationKind.Normal)`
/// (extHost.protocol.ts L2609; `ActivationKind.Normal = 0`, `services/extensions/common/extensions.ts` L383-386).
/// Ember must send `onLanguage:<id>` when a document of that language opens and
/// `onCommand:<id>` before executing a contributed command (upstream: `languageService.ts` L289,
/// `abstractExtensionService.activateByEvent`).
pub fn activate_by_event(event: &str) -> Call {
    Call::new(proxy::EXT_HOST_EXTENSION_SERVICE, "$activateByEvent", vec![Arg::Json(event.into()), Arg::Json(0.into())])
}

/// `IModelAddedData` (extHost.protocol.ts L2390-2398).
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

/// `ITextEditorAddData` (extHost.protocol.ts L2411-2418). `options` is
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

/// `IDocumentsAndEditorsDelta` (extHost.protocol.ts L2453-2459).
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

/// `$acceptDocumentsAndEditorsDelta(delta)` (extHost.protocol.ts L2462).
pub fn accept_documents_and_editors_delta(delta: &DocumentsAndEditorsDelta) -> Call {
    Call::new(proxy::EXT_HOST_DOCUMENTS_AND_EDITORS, "$acceptDocumentsAndEditorsDelta", vec![j(delta)])
}

/// `ITextEditorConfiguration` defaults for `TextEditorAddData::options`.
pub fn default_editor_options(tab_size: u32, insert_spaces: bool) -> Value {
    // TextEditorCursorStyle.Line = 1, RenderLineNumbersType.On = 1
    json!({ "tabSize": tab_size, "indentSize": tab_size, "originalIndentSize": tab_size, "insertSpaces": insert_spaces, "cursorStyle": 1, "lineNumbers": 1 })
}

/// `ExtHostDocuments.$acceptModelChanged(uri, ISerializedModelContentChangedEvent, isDirty)`
/// (extHost.protocol.ts L2404; `ExtHostDocumentsShape` L2399-2405 also holds the three below).
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

/// `$provideHover(handle, resource, position, context | undefined, token)` → `HoverWithId | undefined`
/// (extHost.protocol.ts L2963; `$releaseHover(handle, id)` L2964).
pub fn provide_hover(handle: i64, uri: &UriComponents, pos: Position) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$provideHover", vec![Arg::Json(handle.into()), j(uri), j(&pos), Arg::Undefined])
        .cancellable()
}

pub fn release_hover(handle: i64, id: i64) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$releaseHover", vec![Arg::Json(handle.into()), Arg::Json(id.into())])
}

/// `$provideCodeLenses(handle, resource, token)` → `ICodeLensListDto | undefined`
/// (extHost.protocol.ts L2956; `$resolveCodeLens` L2957, `$releaseCodeLenses` L2958).
pub fn provide_code_lenses(handle: i64, uri: &UriComponents) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$provideCodeLenses", vec![Arg::Json(handle.into()), j(uri)]).cancellable()
}

/// `$resolveCodeLens(handle, ICodeLensDto, token)`; pass the lens exactly as received (with its
/// `cacheId`).
pub fn resolve_code_lens(handle: i64, lens: &Value) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$resolveCodeLens", vec![Arg::Json(handle.into()), Arg::Json(lens.clone())])
        .cancellable()
}

/// `$releaseCodeLenses(handle, cacheId)`: required, or the extension host leaks the list.
pub fn release_code_lenses(handle: i64, cache_id: i64) -> Call {
    Call::new(proxy::EXT_HOST_LANGUAGE_FEATURES, "$releaseCodeLenses", vec![Arg::Json(handle.into()), Arg::Json(cache_id.into())])
}

/// `$provideFoldingRanges(handle, resource, context, token)` → `IFoldingRangeDto[] | undefined`
/// (extHost.protocol.ts, `Eb.from` converts 0-based provider lines to 1-based `{ start, end, kind? }`).
pub fn provide_folding_ranges(handle: i64, uri: &UriComponents) -> Call {
    Call::new(
        proxy::EXT_HOST_LANGUAGE_FEATURES,
        "$provideFoldingRanges",
        vec![Arg::Json(handle.into()), j(uri), Arg::Json(json!({}))],
    )
    .cancellable()
}

/// `$provideDocumentSemanticTokens(handle, resource, previousResultId, token)` → `VSBuffer | null`
/// (`Reply::Buffer`, little-endian u32 words: `id`, `kind` (`1` = full), `data_len`, then 5-tuples).
pub fn provide_document_semantic_tokens(handle: i64, uri: &UriComponents, previous_result_id: i64) -> Call {
    Call::new(
        proxy::EXT_HOST_LANGUAGE_FEATURES,
        "$provideDocumentSemanticTokens",
        vec![Arg::Json(handle.into()), j(uri), Arg::Json(previous_result_id.into())],
    )
    .cancellable()
}

/// `$releaseDocumentSemanticTokens(handle, semanticColoringResultId)` to free the previous result in the extension host.
pub fn release_document_semantic_tokens(handle: i64, result_id: i64) -> Call {
    Call::new(
        proxy::EXT_HOST_LANGUAGE_FEATURES,
        "$releaseDocumentSemanticTokens",
        vec![Arg::Json(handle.into()), Arg::Json(result_id.into())],
    )
}

/// `InlineCompletionContext` (languages.ts L767-795).
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

/// `$provideInlineCompletions(handle, resource, position, context, token)` (extHost.protocol.ts
/// L2995; `$handleInlineCompletionDidShow` L2996, `…EndOfLifetime` L2998, `$freeInlineCompletionsList` L3000) →
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

/// `ExtHostCommands.$executeContributedCommand(id, ...args)` (extHost.protocol.ts L2373), e.g. the `command` of an accepted
/// inline completion or a clicked CodeLens.
pub fn execute_contributed_command(id: &str, args: &[Value]) -> Call {
    let mut a = vec![Arg::Json(id.into())];
    a.extend(args.iter().cloned().map(Arg::Json));
    Call::new(proxy::EXT_HOST_COMMANDS, "$executeContributedCommand", a)
}

// ---- extension host → Ember -----------------------------------------------------------------

/// Decoded `MainThread*` calls the editor core acts on. Everything else is
/// [`MainThreadCall::Other`] and gets [`default_reply`]. Signatures (extHost.protocol.ts at
/// [`crate::PINNED_COMMIT`]): `MainThreadCommandsShape` L135-139, `MainThreadDiagnosticsShape`
/// L254-257, `MainThreadLanguageFeaturesShape` `$unregister` L529, `$registerCodeLensSupport` /
/// `$emitCodeLensEvent` L531-532, `$registerHoverProvider` L537,
/// `$registerInlineCompletionsSupport` L558-576, `$emitInlineCompletionsChange` L577.
#[derive(Debug, Clone, PartialEq)]
pub enum MainThreadCall {
    DiagnosticsChangeMany { owner: String, entries: Vec<(UriComponents, Option<Vec<MarkerData>>)> },
    DiagnosticsClear { owner: String },
    RegisterHoverProvider { handle: i64, selector: Vec<DocumentFilter> },
    RegisterCodeLensSupport { handle: i64, selector: Vec<DocumentFilter>, event_handle: Option<i64> },
    EmitCodeLensEvent { event_handle: i64 },
    /// `MainThreadLanguageFeatures.$registerFoldingRangeProvider(handle, selector, extensionId, eventHandle)` (FR-E6).
    RegisterFoldingRangeProvider { handle: i64, selector: Vec<DocumentFilter>, event_handle: Option<i64> },
    /// `MainThreadLanguageFeatures.$emitFoldingRangeEvent(eventHandle, ranges?)` (FR-E6).
    EmitFoldingRangeEvent { event_handle: i64 },
    /// `MainThreadLanguageFeatures.$registerDocumentSemanticTokensProvider(handle, selector, legend, eventHandle)` (FR-E6).
    RegisterDocumentSemanticTokensProvider { handle: i64, selector: Vec<DocumentFilter>, legend: SemanticTokensLegend, event_handle: Option<i64> },
    /// `MainThreadLanguageFeatures.$emitDocumentSemanticTokensEvent(eventHandle)` (FR-E6).
    EmitDocumentSemanticTokensEvent { event_handle: i64 },
    /// 17 positional args at the pinned commit (extHost.protocol.ts L558-576); only the stable
    /// head is decoded, the rest kept raw.
    RegisterInlineCompletionsSupport { handle: i64, selector: Vec<DocumentFilter>, extension_id: String, raw: Vec<Value> },
    EmitInlineCompletionsChange { handle: i64 },
    /// A registration of [`crate::language`]: completion, definition, references, rename, code actions,
    /// formatting, signature help or document symbols (FR-L31.13).
    RegisterLanguageFeature { handle: i64, selector: Vec<DocumentFilter>, feature: crate::language::LanguageFeature },
    Unregister { handle: i64 },
    RegisterCommand { id: String },
    UnregisterCommand { id: String },
    ExecuteCommand { id: String, args: Value },
    /// MainThreadOutputService calls
    OutputRegister { label: String, file: Option<UriComponents>, language_id: Option<String>, extension_id: Option<String> },
    OutputUpdate { channel_id: String },
    OutputAppend { channel_id: String, value: String },
    OutputClear { channel_id: String, till: Option<i64> },
    OutputReveal { channel_id: String, preserve_focus: bool },
    OutputDispose { channel_id: String },
    /// MainThreadMessageService.$showMessage
    ShowMessage { severity: u8, message: String, options: Value, commands: Vec<Value> },
    /// `MainThreadSearch.$registerTextSearchProvider(handle, scheme)` (FR-E7).
    RegisterTextSearchProvider { handle: i64, scheme: String },
    /// `MainThreadSearch.$unregisterProvider(handle)`: any search provider kind.
    UnregisterSearchProvider { handle: i64 },
    /// `MainThreadSearch.$handleTextMatch(handle, session, IRawFileMatch2[])`: matches of a running search.
    HandleTextMatch { handle: i64, session: i64, matches: Vec<crate::search::FileMatch> },
    Other { proxy: Option<&'static str>, method: String },
}

pub(crate) fn arg_json(args: &[Arg], i: usize) -> Value {
    args.get(i).and_then(Arg::as_json).cloned().unwrap_or(Value::Null)
}

pub(crate) fn arg_i64(args: &[Arg], i: usize) -> Result<i64> {
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
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$registerFoldingRangeProvider") => Self::RegisterFoldingRangeProvider {
                handle: arg_i64(a, 0)?,
                selector: serde_json::from_value(arg_json(a, 1))?,
                event_handle: arg_json(a, 3).as_i64(),
            },
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$emitFoldingRangeEvent") => {
                Self::EmitFoldingRangeEvent { event_handle: arg_i64(a, 0)? }
            }
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$registerDocumentSemanticTokensProvider") => {
                Self::RegisterDocumentSemanticTokensProvider {
                    handle: arg_i64(a, 0)?,
                    selector: serde_json::from_value(arg_json(a, 1))?,
                    legend: serde_json::from_value(arg_json(a, 2))?,
                    event_handle: arg_json(a, 3).as_i64(),
                }
            }
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$emitDocumentSemanticTokensEvent") => {
                Self::EmitDocumentSemanticTokensEvent { event_handle: arg_i64(a, 0)? }
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
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), m) if crate::language::is_registration(m) => {
                match crate::language::parse_registration(m, a)? {
                    Some((handle, selector, feature)) => Self::RegisterLanguageFeature { handle, selector, feature },
                    None => Self::Other { proxy: r.proxy, method: r.method.clone() },
                }
            }
            (Some(proxy::MAIN_THREAD_LANGUAGE_FEATURES), "$unregister") => Self::Unregister { handle: arg_i64(a, 0)? },
            (Some(proxy::MAIN_THREAD_COMMANDS), "$registerCommand") => Self::RegisterCommand { id: arg_str(a, 0)? },
            (Some(proxy::MAIN_THREAD_COMMANDS), "$unregisterCommand") => Self::UnregisterCommand { id: arg_str(a, 0)? },
            (Some(proxy::MAIN_THREAD_COMMANDS), "$executeCommand") => {
                Self::ExecuteCommand { id: arg_str(a, 0)?, args: arg_json(a, 1) }
            }
            (Some(proxy::MAIN_THREAD_OUTPUT_SERVICE), "$register") => Self::OutputRegister {
                label: arg_str(a, 0)?,
                file: serde_json::from_value(arg_json(a, 1)).ok(),
                language_id: arg_json(a, 2).as_str().map(str::to_owned),
                extension_id: arg_json(a, 3).as_str().map(str::to_owned),
            },
            (Some(proxy::MAIN_THREAD_OUTPUT_SERVICE), "$update") => Self::OutputUpdate {
                channel_id: arg_str(a, 0)?,
            },
            (Some(proxy::MAIN_THREAD_OUTPUT_SERVICE), "$append") => Self::OutputAppend {
                channel_id: arg_str(a, 0)?,
                value: arg_str(a, 1)?,
            },
            (Some(proxy::MAIN_THREAD_OUTPUT_SERVICE), "$clear") => Self::OutputClear {
                channel_id: arg_str(a, 0)?,
                till: arg_json(a, 1).as_i64(),
            },
            (Some(proxy::MAIN_THREAD_OUTPUT_SERVICE), "$reveal") => Self::OutputReveal {
                channel_id: arg_str(a, 0)?,
                preserve_focus: arg_json(a, 1).as_bool().unwrap_or(false),
            },
            (Some(proxy::MAIN_THREAD_OUTPUT_SERVICE), "$dispose" | "$close") => Self::OutputDispose {
                channel_id: arg_str(a, 0)?,
            },
            (Some(proxy::MAIN_THREAD_MESSAGE_SERVICE), "$showMessage") => Self::ShowMessage {
                severity: arg_i64(a, 0).map(|s| s as u8).unwrap_or(1),
                message: arg_str(a, 1)?,
                options: arg_json(a, 2),
                commands: arg_json(a, 3).as_array().cloned().unwrap_or_default(),
            },
            (Some(proxy::MAIN_THREAD_SEARCH), "$registerTextSearchProvider") => {
                Self::RegisterTextSearchProvider { handle: arg_i64(a, 0)?, scheme: arg_str(a, 1)? }
            }
            (Some(proxy::MAIN_THREAD_SEARCH), "$unregisterProvider") => Self::UnregisterSearchProvider { handle: arg_i64(a, 0)? },
            (Some(proxy::MAIN_THREAD_SEARCH), "$handleTextMatch") => Self::HandleTextMatch {
                handle: arg_i64(a, 0)?,
                session: arg_i64(a, 1)?,
                matches: crate::search::parse_file_matches(&arg_json(a, 2))?,
            },
            (proxy, _) => Self::Other { proxy, method: r.method.clone() },
        })
    }
}

/// The reply Ember gives to `MainThread*` calls it does not implement, chosen so the extension
/// host keeps running. Most upstream methods are `void` or `Promise<void>` (→ `undefined`).
/// Line numbers are extHost.protocol.ts at [`crate::PINNED_COMMIT`].
/// The exceptions below would otherwise block or mislead extensions:
///
/// * `MainThreadStorage.$initializeExtensionStorage` (L850): awaited during every activation
///   (`undefined` = no stored state).
/// * `MainThreadWindow.$getInitialState` (L2301): `{ isFocused, isActive }`.
/// * `MainThreadWorkspace.$checkExists` (L1976): `workspaceContains:` activation; `false` until Ember
///   implements glob search over the remote filesystem.
/// * `MainThreadWorkspace.$isResourceTrusted` / `$requestWorkspaceTrust` (L1986 / L1985): trusted.
/// * `MainThreadCommands.$getCommands` (L139): empty list.
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
    fn fr_e7_parses_text_search_registration_and_matches() {
        let r = req(proxy::MAIN_THREAD_SEARCH, "$registerTextSearchProvider", json!([1, "vscode-remote"]));
        assert_eq!(
            MainThreadCall::parse(&r).unwrap(),
            MainThreadCall::RegisterTextSearchProvider { handle: 1, scheme: "vscode-remote".into() }
        );
        let r = req(proxy::MAIN_THREAD_SEARCH, "$unregisterProvider", json!([1]));
        assert_eq!(MainThreadCall::parse(&r).unwrap(), MainThreadCall::UnregisterSearchProvider { handle: 1 });
        let r = req(
            proxy::MAIN_THREAD_SEARCH,
            "$handleTextMatch",
            json!([1, 3, [{"resource": {"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/w/a.rs"},
                "results": [{"previewText": "foo", "rangeLocations": [{
                    "source": {"startLineNumber": 0, "startColumn": 0, "endLineNumber": 0, "endColumn": 3},
                    "preview": {"startLineNumber": 0, "startColumn": 0, "endLineNumber": 0, "endColumn": 3}}]}]}]]),
        );
        match MainThreadCall::parse(&r).unwrap() {
            MainThreadCall::HandleTextMatch { handle, session, matches } => {
                assert_eq!((handle, session), (1, 3));
                assert_eq!(matches[0].matches[0].preview_text, "foo");
            }
            other => panic!("{other:?}"),
        }
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

    #[test]
    fn parses_output_and_message_service_calls() {
        let r1 = req(
            proxy::MAIN_THREAD_OUTPUT_SERVICE,
            "$register",
            json!(["Git", null, "git", "vscode.git"]),
        );
        match MainThreadCall::parse(&r1).unwrap() {
            MainThreadCall::OutputRegister { label, file, language_id, extension_id } => {
                assert_eq!(label, "Git");
                assert!(file.is_none());
                assert_eq!(language_id.as_deref(), Some("git"));
                assert_eq!(extension_id.as_deref(), Some("vscode.git"));
            }
            other => panic!("{other:?}"),
        }

        let r2 = req(
            proxy::MAIN_THREAD_OUTPUT_SERVICE,
            "$append",
            json!(["ember-output-1", "hello world\n"]),
        );
        assert_eq!(
            MainThreadCall::parse(&r2).unwrap(),
            MainThreadCall::OutputAppend { channel_id: "ember-output-1".into(), value: "hello world\n".into() }
        );

        let r3 = req(
            proxy::MAIN_THREAD_OUTPUT_SERVICE,
            "$clear",
            json!(["ember-output-1"]),
        );
        assert_eq!(
            MainThreadCall::parse(&r3).unwrap(),
            MainThreadCall::OutputClear { channel_id: "ember-output-1".into(), till: None }
        );

        let r4 = req(
            proxy::MAIN_THREAD_OUTPUT_SERVICE,
            "$reveal",
            json!(["ember-output-1", true]),
        );
        assert_eq!(
            MainThreadCall::parse(&r4).unwrap(),
            MainThreadCall::OutputReveal { channel_id: "ember-output-1".into(), preserve_focus: true }
        );

        let r5 = req(
            proxy::MAIN_THREAD_OUTPUT_SERVICE,
            "$dispose",
            json!(["ember-output-1"]),
        );
        assert_eq!(
            MainThreadCall::parse(&r5).unwrap(),
            MainThreadCall::OutputDispose { channel_id: "ember-output-1".into() }
        );

        let r6 = req(
            proxy::MAIN_THREAD_MESSAGE_SERVICE,
            "$showMessage",
            json!([3, "Build failed", {"source": "Rust Analyzer"}, []]),
        );
        match MainThreadCall::parse(&r6).unwrap() {
            MainThreadCall::ShowMessage { severity, message, options, commands } => {
                assert_eq!(severity, 3);
                assert_eq!(message, "Build failed");
                assert_eq!(options["source"], "Rust Analyzer");
                assert!(commands.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fr_e6_parses_folding_and_semantic_tokens_registrations() {
        let rf = req(
            proxy::MAIN_THREAD_LANGUAGE_FEATURES,
            "$registerFoldingRangeProvider",
            json!([11, [{"language": "rust"}], {"value": "rust-lang.rust-analyzer"}, 12]),
        );
        assert_eq!(
            MainThreadCall::parse(&rf).unwrap(),
            MainThreadCall::RegisterFoldingRangeProvider {
                handle: 11,
                selector: vec![DocumentFilter {
                    language: Some("rust".into()),
                    scheme: None,
                    pattern: None,
                    exclusive: None,
                    notebook_type: None,
                }],
                event_handle: Some(12),
            }
        );
        let ef = req(proxy::MAIN_THREAD_LANGUAGE_FEATURES, "$emitFoldingRangeEvent", json!([12]));
        assert_eq!(MainThreadCall::parse(&ef).unwrap(), MainThreadCall::EmitFoldingRangeEvent { event_handle: 12 });

        let rs = req(
            proxy::MAIN_THREAD_LANGUAGE_FEATURES,
            "$registerDocumentSemanticTokensProvider",
            json!([21, [{"language": "rust"}], {"tokenTypes": ["namespace", "type", "function"], "tokenModifiers": ["declaration"]}, 22]),
        );
        assert_eq!(
            MainThreadCall::parse(&rs).unwrap(),
            MainThreadCall::RegisterDocumentSemanticTokensProvider {
                handle: 21,
                selector: vec![DocumentFilter {
                    language: Some("rust".into()),
                    scheme: None,
                    pattern: None,
                    exclusive: None,
                    notebook_type: None,
                }],
                legend: SemanticTokensLegend {
                    token_types: vec!["namespace".into(), "type".into(), "function".into()],
                    token_modifiers: vec!["declaration".into()],
                },
                event_handle: Some(22),
            }
        );
        let es = req(proxy::MAIN_THREAD_LANGUAGE_FEATURES, "$emitDocumentSemanticTokensEvent", json!([22]));
        assert_eq!(MainThreadCall::parse(&es).unwrap(), MainThreadCall::EmitDocumentSemanticTokensEvent { event_handle: 22 });
    }
}
