//! Workspace trust inside the session (FR-L31.18, #450).
//!
//! The window decides whether the folder is trusted (its per-device memory and the prompt); the
//! session starts the extension host with that state ([`ember_editor_conn::trust`]) and answers
//! the host's trust requests the way `MainThreadWorkspace` does at the pinned commit
//! (mainThreadWorkspace.ts L243-262 over workspaceTrust.ts L856-):
//!
//! * `$isResourceTrusted(uri)`: the workspace's trust. Ember opens one folder per window and keeps
//!   no list of other trusted folders on the server, so a resource outside it is answered the same.
//! * `$requestWorkspaceTrust(options)` and `$requestResourceTrust(options)`: `true` at once when
//!   trusted; otherwise the request waits for the user. The window shows the prompt
//!   ([`crate::engine::SessionUpdate::WorkspaceTrustRequested`]); 신뢰 answers `true`, 제한 모드
//!   answers `undefined` (`cancelWorkspaceTrustRequest`, workspaceTrust.ts L834-841).
//!
//! Trust is granted at most once per session, as in Code-OSS: going back to restricted mode needs
//! the window to open again (the extension host cannot unload an activated extension).

use ember_editor_conn::exthost::Call;
use ember_editor_conn::rpc::Reply;
use serde_json::Value;

/// What to do with one trust request from the extension host.
#[derive(Debug, Clone, PartialEq)]
pub enum TrustAnswer {
    /// Answer now.
    Now(Reply),
    /// Keep the request until the user decides; show the prompt.
    Ask,
}

#[derive(Debug, Clone)]
pub struct WorkspaceTrust {
    trusted: bool,
    extensions: Vec<Value>,
    waiting: Vec<u32>,
    /// The next `IExtensionDescriptionDelta.versionId`; the init data used 1.
    next_version: u32,
}

impl Default for WorkspaceTrust {
    /// Trusted with no extensions: what a core built without a trust decision does (the earlier
    /// behaviour, and the unit tests that do not care).
    fn default() -> Self {
        Self::new(true, Vec::new())
    }
}

impl WorkspaceTrust {
    /// `extensions` is the `scanExtensions` result the host was started with.
    pub fn new(trusted: bool, extensions: Vec<Value>) -> Self {
        Self { trusted, extensions, waiting: Vec::new(), next_version: 2 }
    }

    pub fn is_trusted(&self) -> bool {
        self.trusted
    }

    /// The answer to `proxy.method` request `req`, or `None` when it is not a trust request.
    pub fn on_request(&mut self, proxy: Option<&str>, method: &str, req: u32) -> Option<TrustAnswer> {
        if proxy != Some("MainThreadWorkspace") {
            return None;
        }
        match method {
            "$isResourceTrusted" => Some(TrustAnswer::Now(Reply::Json(Value::Bool(self.trusted)))),
            "$requestWorkspaceTrust" | "$requestResourceTrust" if self.trusted => Some(TrustAnswer::Now(Reply::Json(Value::Bool(true)))),
            "$requestWorkspaceTrust" | "$requestResourceTrust" => {
                self.waiting.push(req);
                Some(TrustAnswer::Ask)
            }
            _ => None,
        }
    }

    /// The user's answer. Returns the calls to send to the host (in order) and the waiting
    /// requests with their replies. A second grant, or a grant when already trusted, sends nothing.
    pub fn decide(&mut self, trusted: bool) -> (Vec<Call>, Vec<(u32, Reply)>) {
        let calls = if trusted && !self.trusted {
            self.trusted = true;
            let calls = ember_editor_conn::trust::grant_calls(&self.extensions, self.next_version);
            self.next_version += 1;
            calls
        } else {
            Vec::new()
        };
        let reply = if self.trusted { Reply::Json(Value::Bool(true)) } else { Reply::Empty };
        let answers = self.waiting.drain(..).map(|r| (r, reply.clone())).collect();
        (calls, answers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn exts() -> Vec<Value> {
        vec![json!({ "identifier": { "value": "a.no" }, "main": "./m.js", "activationEvents": ["*"] })]
    }

    #[test]
    fn fr_l31_18_trusted_answers_at_once() {
        let mut t = WorkspaceTrust::new(true, exts());
        assert_eq!(t.on_request(Some("MainThreadWorkspace"), "$isResourceTrusted", 1), Some(TrustAnswer::Now(Reply::Json(json!(true)))));
        assert_eq!(t.on_request(Some("MainThreadWorkspace"), "$requestWorkspaceTrust", 2), Some(TrustAnswer::Now(Reply::Json(json!(true)))));
        assert_eq!(t.on_request(Some("MainThreadWorkspace"), "$checkExists", 3), None);
        assert_eq!(t.on_request(Some("MainThreadWindow"), "$isResourceTrusted", 4), None);
        assert!(t.decide(true).0.is_empty(), "already trusted: no grant");
    }

    #[test]
    fn fr_l31_18_untrusted_request_waits_for_the_user() {
        let mut t = WorkspaceTrust::new(false, exts());
        assert_eq!(t.on_request(Some("MainThreadWorkspace"), "$isResourceTrusted", 1), Some(TrustAnswer::Now(Reply::Json(json!(false)))));
        assert_eq!(t.on_request(Some("MainThreadWorkspace"), "$requestWorkspaceTrust", 7), Some(TrustAnswer::Ask));
        let (calls, answers) = t.decide(false);
        assert!(calls.is_empty());
        assert_eq!(answers, vec![(7, Reply::Empty)], "restricted mode answers undefined");
        assert_eq!(t.on_request(Some("MainThreadWorkspace"), "$requestResourceTrust", 8), Some(TrustAnswer::Ask));
        let (calls, answers) = t.decide(true);
        assert_eq!(calls.iter().map(|c| c.method).collect::<Vec<_>>(), vec!["$deltaExtensions", "$onDidGrantWorkspaceTrust"]);
        assert_eq!(answers, vec![(8, Reply::Json(json!(true)))]);
        assert!(t.is_trusted());
        assert!(t.decide(true).0.is_empty(), "trust is granted once");
    }
}
