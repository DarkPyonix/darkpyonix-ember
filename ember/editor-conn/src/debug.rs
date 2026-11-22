//! `ExtHostDebugService`: the calls Ember makes to the extension host for debugging, and the
//! Debug Adapter Protocol message type that crosses `$sendDAMessage` and `$acceptDAMessage`.
//!
//! Code-OSS keeps debug adapters in the extension host (`extHostDebugService.ts`). The renderer
//! side (`mainThreadDebugService.ts`) resolves a configuration through the providers the
//! extensions registered, asks the extension host to create the adapter
//! (`$startDASession`) and relays every DAP message as an RPC call. Line numbers below are
//! `extHost.protocol.ts` at [`crate::PINNED_COMMIT`].

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::exthost::{j, Call};
use crate::rpc::Arg;
use crate::uri::UriComponents;

/// The proxy these calls go to (`ExtHostDebugService`, rpc id 92).
pub const PROXY: &str = "ExtHostDebugService";

// ---- DAP messages -----------------------------------------------------------------------------

/// A `DebugProtocol.ProtocolMessage`: a request, a response or an event, told apart by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DapMessage {
    Request {
        seq: i64,
        command: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        arguments: Value,
    },
    Response {
        #[serde(default)]
        seq: i64,
        request_seq: i64,
        success: bool,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        body: Value,
    },
    Event {
        seq: i64,
        event: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        body: Value,
    },
}

impl DapMessage {
    pub fn request(seq: i64, command: &str, arguments: Value) -> Self {
        Self::Request {
            seq,
            command: command.to_owned(),
            arguments,
        }
    }

    /// The answer to a request of the adapter (a reverse request such as `runInTerminal`).
    pub fn response(request_seq: i64, command: &str, error: Option<&str>, body: Value) -> Self {
        Self::Response {
            seq: 0,
            request_seq,
            success: error.is_none(),
            command: command.to_owned(),
            message: error.map(str::to_owned),
            body,
        }
    }

    pub fn from_value(v: &Value) -> Result<Self, String> {
        serde_json::from_value(v.clone()).map_err(|e| format!("not a DAP message: {e}"))
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

// ---- session DTOs -----------------------------------------------------------------------------

/// `IDebugSessionFullDto` (L3295).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    pub id: String,
    #[serde(rename = "type")]
    pub debug_type: String,
    pub name: String,
    pub parent: Option<String>,
    pub folder_uri: Option<UriComponents>,
    pub configuration: Value,
}

/// `IDebugSessionDto` (L3304): the whole session the first time, only its id once the extension
/// host said `$sessionCached` for it.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionRef {
    Full(SessionDto),
    Id(String),
}

impl SessionRef {
    fn arg(&self) -> Arg {
        match self {
            Self::Full(dto) => j(dto),
            Self::Id(id) => Arg::Json(Value::String(id.clone())),
        }
    }
}

/// One breakpoint of an `IBreakpointsDeltaDto` (L3250-3260 and neighbours).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum BreakpointDto {
    #[serde(rename = "source", rename_all = "camelCase")]
    Source {
        id: String,
        enabled: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hit_condition: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        log_message: Option<String>,
        uri: UriComponents,
        /// 0-based.
        line: u32,
        /// 0-based.
        character: u32,
    },
    #[serde(rename = "function", rename_all = "camelCase")]
    Function {
        id: String,
        enabled: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hit_condition: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        log_message: Option<String>,
        function_name: String,
    },
}

/// `IBreakpointsDeltaDto` (L3280).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct BreakpointsDelta {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<BreakpointDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<BreakpointDto>,
}

impl BreakpointsDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// A breakpoint an extension registered with `$registerBreakpoints` (`debug.addBreakpoints`).
/// Positions are as the extension host sends them: 0-based.
#[derive(Debug, Clone, PartialEq)]
pub enum RegisteredBreakpoint {
    Source {
        id: String,
        uri: UriComponents,
        line: u32,
        character: u32,
        enabled: bool,
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
    },
    Function {
        id: Option<String>,
        name: String,
        enabled: bool,
        condition: Option<String>,
        hit_condition: Option<String>,
        log_message: Option<String>,
    },
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn number(v: &Value, key: &str) -> u32 {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// Read the DTOs of `$registerBreakpoints` (`ISourceMultiBreakpointDto`, `IFunctionBreakpointDto`;
/// data breakpoints need an adapter that supports them and are skipped). A DTO that does not
/// parse is skipped.
pub fn parse_registered_breakpoints(dtos: &[Value]) -> Vec<RegisteredBreakpoint> {
    let mut out = Vec::new();
    for dto in dtos {
        match dto.get("type").and_then(Value::as_str) {
            Some("sourceMulti") => {
                let Some(uri) = dto
                    .get("uri")
                    .and_then(|u| serde_json::from_value::<UriComponents>(u.clone()).ok())
                else {
                    continue;
                };
                for l in dto
                    .get("lines")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(id) = text(l, "id") else { continue };
                    out.push(RegisteredBreakpoint::Source {
                        id,
                        uri: uri.clone(),
                        line: number(l, "line"),
                        character: number(l, "character"),
                        enabled: l.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                        condition: text(l, "condition"),
                        hit_condition: text(l, "hitCondition"),
                        log_message: text(l, "logMessage"),
                    });
                }
            }
            Some("function") => {
                let Some(name) = text(dto, "functionName") else {
                    continue;
                };
                out.push(RegisteredBreakpoint::Function {
                    id: text(dto, "id"),
                    name,
                    enabled: dto.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                    condition: text(dto, "condition"),
                    hit_condition: text(dto, "hitCondition"),
                    log_message: text(dto, "logMessage"),
                });
            }
            _ => {}
        }
    }
    out
}

// ---- calls ------------------------------------------------------------------------------------

fn folder_arg(folder: Option<&UriComponents>) -> Arg {
    match folder {
        Some(uri) => j(uri),
        None => Arg::Undefined,
    }
}

/// `$startDASession(handle, session)` (L3323): the extension host creates the adapter of the
/// session's type and starts it. The reply resolves once the adapter runs.
pub fn start_da_session(handle: i64, session: &SessionRef) -> Call {
    Call::new(
        PROXY,
        "$startDASession",
        vec![Arg::Json(handle.into()), session.arg()],
    )
}

/// `$stopDASession(handle)` (L3324).
pub fn stop_da_session(handle: i64) -> Call {
    Call::new(PROXY, "$stopDASession", vec![Arg::Json(handle.into())])
}

/// `$sendDAMessage(handle, message)` (L3325): one DAP message to the adapter.
pub fn send_da_message(handle: i64, message: &DapMessage) -> Call {
    Call::new(
        PROXY,
        "$sendDAMessage",
        vec![Arg::Json(handle.into()), Arg::Json(message.to_value())],
    )
}

/// `$resolveDebugConfiguration(handle, folder, config, token)` (L3326). The reply is the
/// configuration, or `null` / `undefined` when the provider cancels the launch.
pub fn resolve_debug_configuration(
    handle: i64,
    folder: Option<&UriComponents>,
    config: &Value,
) -> Call {
    Call::new(
        PROXY,
        "$resolveDebugConfiguration",
        vec![
            Arg::Json(handle.into()),
            folder_arg(folder),
            Arg::Json(config.clone()),
        ],
    )
    .cancellable()
}

/// `$resolveDebugConfigurationWithSubstitutedVariables(handle, folder, config, token)` (L3327).
pub fn resolve_debug_configuration_with_substituted_variables(
    handle: i64,
    folder: Option<&UriComponents>,
    config: &Value,
) -> Call {
    Call::new(
        PROXY,
        "$resolveDebugConfigurationWithSubstitutedVariables",
        vec![
            Arg::Json(handle.into()),
            folder_arg(folder),
            Arg::Json(config.clone()),
        ],
    )
    .cancellable()
}

/// `$provideDebugConfigurations(handle, folder, token)` (L3328): the configurations a provider
/// offers for the "add configuration" and dynamic-configuration lists.
pub fn provide_debug_configurations(handle: i64, folder: Option<&UriComponents>) -> Call {
    Call::new(
        PROXY,
        "$provideDebugConfigurations",
        vec![Arg::Json(handle.into()), folder_arg(folder)],
    )
    .cancellable()
}

/// `$acceptDebugSessionStarted(session)` (L3330).
pub fn accept_debug_session_started(session: &SessionRef) -> Call {
    Call::new(PROXY, "$acceptDebugSessionStarted", vec![session.arg()])
}

/// `$acceptDebugSessionTerminated(session)` (L3331).
pub fn accept_debug_session_terminated(session: &SessionRef) -> Call {
    Call::new(PROXY, "$acceptDebugSessionTerminated", vec![session.arg()])
}

/// `$acceptDebugSessionActiveChanged(session | undefined)` (L3332).
pub fn accept_debug_session_active_changed(session: Option<&SessionRef>) -> Call {
    Call::new(
        PROXY,
        "$acceptDebugSessionActiveChanged",
        session.map(|s| vec![s.arg()]).unwrap_or_default(),
    )
}

/// `$acceptDebugSessionCustomEvent(session, event)` (L3333): a DAP event the renderer does not
/// know, for `debug.onDidReceiveDebugSessionCustomEvent`.
pub fn accept_debug_session_custom_event(session: &SessionRef, event: &Value) -> Call {
    Call::new(
        PROXY,
        "$acceptDebugSessionCustomEvent",
        vec![session.arg(), Arg::Json(event.clone())],
    )
}

/// `$acceptBreakpointsDelta(delta)` (L3334): keeps `debug.breakpoints` in step.
pub fn accept_breakpoints_delta(delta: &BreakpointsDelta) -> Call {
    Call::new(PROXY, "$acceptBreakpointsDelta", vec![j(delta)])
}

/// `$acceptDebugSessionNameChanged(session, name)` (L3335).
pub fn accept_debug_session_name_changed(session: &SessionRef, name: &str) -> Call {
    Call::new(
        PROXY,
        "$acceptDebugSessionNameChanged",
        vec![session.arg(), Arg::Json(name.into())],
    )
}

/// `$acceptStackFrameFocus(focus | undefined)` (L3336): `IThreadFocusDto` or `IStackFrameFocusDto`.
pub fn accept_stack_frame_focus(session_id: &str, thread_id: i64, frame_id: Option<i64>) -> Call {
    let mut focus = Map::new();
    focus.insert(
        "kind".into(),
        json!(if frame_id.is_some() {
            "stackFrame"
        } else {
            "thread"
        }),
    );
    focus.insert("sessionId".into(), json!(session_id));
    focus.insert("threadId".into(), json!(thread_id));
    if let Some(frame) = frame_id {
        focus.insert("frameId".into(), json!(frame));
    }
    Call::new(
        PROXY,
        "$acceptStackFrameFocus",
        vec![Arg::Json(Value::Object(focus))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_42_dap_message_round_trips_as_the_wire_spells_it() {
        let event = json!({"type": "event", "seq": 3, "event": "stopped", "body": {"reason": "breakpoint", "threadId": 1}});
        let parsed = DapMessage::from_value(&event).unwrap();
        assert_eq!(
            parsed,
            DapMessage::Event {
                seq: 3,
                event: "stopped".into(),
                body: json!({"reason": "breakpoint", "threadId": 1})
            }
        );
        assert_eq!(parsed.to_value(), event);

        let response = json!({"type": "response", "request_seq": 2, "success": false, "command": "launch", "message": "no"});
        let DapMessage::Response {
            seq,
            request_seq,
            success,
            message,
            body,
            ..
        } = DapMessage::from_value(&response).unwrap()
        else {
            panic!("not a response")
        };
        assert_eq!(
            (seq, request_seq, success, message.as_deref(), body),
            (0, 2, false, Some("no"), Value::Null)
        );

        let request = DapMessage::request(7, "threads", Value::Null);
        assert_eq!(
            request.to_value(),
            json!({"type": "request", "seq": 7, "command": "threads"})
        );
        assert!(DapMessage::from_value(&json!({"type": "nonsense"})).is_err());
    }

    #[test]
    fn fr_l31_42_calls_name_the_pinned_methods() {
        let sent = send_da_message(4, &DapMessage::request(1, "initialize", json!({})));
        assert_eq!(
            (sent.proxy, sent.method, sent.cancellable),
            ("ExtHostDebugService", "$sendDAMessage", false)
        );
        assert!(crate::rpc_ids::id_of(PROXY).is_some());
        let resolve = resolve_debug_configuration(2, None, &json!({"type": "x"}));
        assert!(resolve.cancellable);
        assert_eq!(
            resolve.args[1],
            Arg::Undefined,
            "no folder is `undefined`, not `null`"
        );
        let focus = accept_stack_frame_focus("s", 1, Some(9));
        assert_eq!(focus.args[0].as_json().unwrap()["kind"], "stackFrame");
        assert!(accept_debug_session_active_changed(None).args.is_empty());
    }

    #[test]
    fn fr_l31_42_registered_breakpoints_are_read_from_the_dtos() {
        let uri =
            json!({"$mid": 1, "scheme": "vscode-remote", "authority": "a", "path": "/w/m.py"});
        let dtos = vec![
            json!({"type": "sourceMulti", "uri": uri, "lines": [
                {"id": "b1", "enabled": true, "line": 1, "character": 0, "condition": "x > 1"},
                {"id": "b2", "enabled": false, "line": 4, "character": 2, "logMessage": "hi"},
            ]}),
            json!({"type": "function", "id": "f1", "enabled": true, "functionName": "main"}),
            json!({"type": "data", "dataId": "d", "label": "l", "canPersist": false, "enabled": true, "accessType": "write"}),
        ];
        let parsed = parse_registered_breakpoints(&dtos);
        assert_eq!(parsed.len(), 3);
        assert!(
            matches!(&parsed[0], RegisteredBreakpoint::Source { id, line: 1, condition: Some(c), .. } if id == "b1" && c == "x > 1")
        );
        assert!(
            matches!(&parsed[1], RegisteredBreakpoint::Source { enabled: false, character: 2, log_message: Some(m), .. } if m == "hi")
        );
        assert!(
            matches!(&parsed[2], RegisteredBreakpoint::Function { name, .. } if name == "main")
        );
    }

    #[test]
    fn fr_l31_42_breakpoint_delta_serialises_like_the_dto() {
        let delta = BreakpointsDelta {
            added: vec![BreakpointDto::Source {
                id: "b".into(),
                enabled: true,
                condition: None,
                hit_condition: Some("3".into()),
                log_message: None,
                uri: UriComponents::remote("a", "/w/m.py"),
                line: 1,
                character: 0,
            }],
            removed: vec!["old".into()],
            changed: Vec::new(),
        };
        let v = serde_json::to_value(&delta).unwrap();
        assert_eq!(v["added"][0]["type"], "source");
        assert_eq!(v["added"][0]["hitCondition"], "3");
        assert_eq!(v["added"][0]["uri"]["path"], "/w/m.py");
        assert!(v["added"][0].get("condition").is_none());
        assert_eq!(v["removed"], json!(["old"]));
        assert!(v.get("changed").is_none());
    }
}
