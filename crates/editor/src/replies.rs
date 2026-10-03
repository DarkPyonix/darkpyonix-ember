//! Replies to extension-host requests that no bridge handles.
//!
//! [`ember_editor_conn::exthost::default_reply`] covers what the extension host needs to run at
//! all. The live test against OSE (`crates/editor-conn/tests/live_ose.rs` `live_reply`) found two more
//! that real extensions need, which the session now owns:
//!
//! * `MainThreadLanguages.$getLanguages` → `string[]` (`languages.getLanguages()`; `undefined`
//!   throws in callers that iterate it). Ember answers with every language id contributed by the
//!   scanned extensions.
//! * `MainThreadOutputService.$register(label, file, languageId, extensionId)` → the channel id
//!   string (`window.createOutputChannel`, used by every language client for its log).
//!
//! `MainThreadDocuments.$tryOpenDocument` (`workspace.openTextDocument`) and
//! `MainThreadCommands.$executeCommand` need state and are answered by the session core.

use ember_editor_conn::exthost::default_reply;
use ember_editor_conn::rpc::Reply;
use serde_json::{json, Value};

/// The reply for an unhandled request. `seq` makes output-channel ids unique.
pub fn session_reply(proxy: Option<&str>, method: &str, seq: u64, language_ids: &[String]) -> Reply {
    match (proxy, method) {
        (Some("MainThreadLanguages"), "$getLanguages") => Reply::Json(Value::from(language_ids.to_vec())),
        (Some("MainThreadOutputService"), "$register") => Reply::Json(json!(format!("ember-output-{seq}"))),
        _ => default_reply(proxy, method),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_learned_replies_and_fallback() {
        let langs = vec!["json".to_string(), "plaintext".to_string()];
        assert_eq!(session_reply(Some("MainThreadLanguages"), "$getLanguages", 1, &langs), Reply::Json(json!(["json", "plaintext"])));
        assert_eq!(session_reply(Some("MainThreadOutputService"), "$register", 7, &langs), Reply::Json(json!("ember-output-7")));
        assert_eq!(
            session_reply(Some("MainThreadWindow"), "$getInitialState", 1, &langs),
            Reply::Json(json!({"isFocused": true, "isActive": true}))
        );
        assert_eq!(session_reply(Some("MainThreadStorage"), "$initializeExtensionStorage", 1, &langs), Reply::Empty);
    }
}
