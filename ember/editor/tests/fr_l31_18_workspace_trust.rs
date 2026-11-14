//! FR-L31.18: workspace trust through the pure session core. An untrusted folder holds the
//! extensions' trust requests until the user answers; a grant adds the withheld extensions and
//! tells the extension host.

use std::time::Instant;

use ember_editor::languages::LanguageRegistry;
use ember_editor::trust::WorkspaceTrust;
use ember_editor::{CoreOptions, Effect, Input, SessionCore, SessionUpdate};
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use serde_json::{json, Value};

fn extensions() -> Vec<Value> {
    vec![
        json!({ "identifier": { "value": "a.safe" }, "main": "./m.js", "capabilities": { "untrustedWorkspaces": { "supported": true } } }),
        json!({ "identifier": { "value": "a.needs" }, "main": "./m.js", "activationEvents": ["onLanguage:x"] }),
    ]
}

fn core(trusted: bool) -> SessionCore {
    let mut c = SessionCore::new(LanguageRegistry::from_extensions(&[]), CoreOptions::default(), Instant::now());
    c.set_workspace_trust(WorkspaceTrust::new(trusted, extensions()));
    c
}

fn request(req: u32, method: &str) -> Input {
    Input::ExtHostRequest(IncomingRequest {
        req,
        rpc_id: ember_editor_conn::rpc_ids::id_of("MainThreadWorkspace").unwrap(),
        proxy: Some("MainThreadWorkspace"),
        method: method.into(),
        args: vec![Arg::Json(json!({}))],
        cancellable: false,
    })
}

fn responses(effects: &[Effect]) -> Vec<(u32, Reply)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Respond { req, result: Ok(r) } => Some((*req, r.clone())),
            _ => None,
        })
        .collect()
}

fn methods(effects: &[Effect]) -> Vec<&'static str> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Rpc { call, .. } => Some(call.method),
            _ => None,
        })
        .collect()
}

#[test]
fn fr_l31_18_trusted_folder_answers_trust_requests_true() {
    let mut c = core(true);
    let fx = c.handle(Instant::now(), request(1, "$requestWorkspaceTrust"));
    assert_eq!(responses(&fx), vec![(1, Reply::Json(json!(true)))]);
    let fx = c.handle(Instant::now(), request(2, "$isResourceTrusted"));
    assert_eq!(responses(&fx), vec![(2, Reply::Json(json!(true)))]);
}

#[test]
fn fr_l31_18_restricted_request_prompts_then_grant_adds_extensions() {
    let mut c = core(false);
    let fx = c.handle(Instant::now(), request(3, "$isResourceTrusted"));
    assert_eq!(responses(&fx), vec![(3, Reply::Json(json!(false)))]);

    let fx = c.handle(Instant::now(), request(4, "$requestWorkspaceTrust"));
    assert!(responses(&fx).is_empty(), "the request waits for the user");
    assert!(fx.iter().any(|e| matches!(e, Effect::Ui(SessionUpdate::WorkspaceTrustRequested))));

    let fx = c.handle(Instant::now(), Input::DecideWorkspaceTrust { trusted: true });
    assert_eq!(methods(&fx), vec!["$deltaExtensions", "$onDidGrantWorkspaceTrust"]);
    let delta = fx
        .iter()
        .find_map(|e| match e {
            Effect::Rpc { call, .. } if call.method == "$deltaExtensions" => call.args[0].as_json().cloned(),
            _ => None,
        })
        .unwrap();
    assert_eq!(delta["myToAdd"], json!([{ "value": "a.needs", "_lower": "a.needs" }]));
    assert_eq!(responses(&fx), vec![(4, Reply::Json(json!(true)))]);
    assert!(c.workspace_trusted());
}

#[test]
fn fr_l31_18_restricted_mode_answer_is_undefined_and_sends_nothing() {
    let mut c = core(false);
    c.handle(Instant::now(), request(5, "$requestResourceTrust"));
    let fx = c.handle(Instant::now(), Input::DecideWorkspaceTrust { trusted: false });
    assert!(methods(&fx).is_empty());
    assert_eq!(responses(&fx), vec![(5, Reply::Empty)]);
    assert!(!c.workspace_trusted());
}
