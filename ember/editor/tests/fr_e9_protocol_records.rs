//! FR-E9: the session records calls to `todo` methods of the coverage ledger that return a value,
//! with the call count and, when an argument names it, the extension; `acked` and `out` methods are
//! decided answers and are neither counted nor recorded.

use std::time::Instant;

use ember_editor::languages::LanguageRegistry;
use ember_editor::{CoreOptions, Effect, Input, SessionCore, SessionUpdate};
use ember_editor_conn::rpc::{Arg, IncomingRequest, Reply};
use serde_json::{json, Value};

fn core(t0: Instant) -> SessionCore {
    SessionCore::new(LanguageRegistry::from_extensions(&[]), CoreOptions::default(), t0)
}

fn request(req: u32, proxy: &'static str, method: &str, args: Vec<Value>) -> Input {
    Input::ExtHostRequest(IncomingRequest {
        req,
        rpc_id: ember_editor_conn::rpc_ids::id_of(proxy).unwrap(),
        proxy: Some(proxy),
        method: method.into(),
        args: args.into_iter().map(Arg::Json).collect(),
        cancellable: false,
    })
}

fn gaps(fx: &[Effect]) -> Vec<(String, String, Option<String>, u32)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Ui(SessionUpdate::ProtocolGap { proxy, method, extension, count }) => {
                Some((proxy.clone(), method.clone(), extension.clone(), *count))
            }
            _ => None,
        })
        .collect()
}

fn no_provider() -> Value {
    ember_editor_conn::rpc::js_error("Error", "No authentication provider is currently registered.")
}

fn key(proxy: &str, method: &str) -> (String, String) {
    (proxy.to_string(), method.to_string())
}

#[test]
fn fr_e9_todo_call_with_a_value_is_recorded_with_count_and_named_extension() {
    let t0 = Instant::now();
    let mut c = core(t0);
    // $getSession(providerId, scopes, extensionId, extensionName, options): Promise<Dto | undefined>, todo in the ledger.
    let fx = c.handle(t0, request(1, "MainThreadAuthentication", "$getSession", vec![json!("github"), json!(["repo"]), json!("pub.one"), json!("Pub"), json!({})]));
    assert_eq!(gaps(&fx), vec![("MainThreadAuthentication".into(), "$getSession".into(), Some("pub.one".into()), 1)]);
    // The ledger's `error` for $getSession (Q-E8 (c)): rejected as Code-OSS rejects an unregistered provider.
    assert!(fx.contains(&Effect::Respond { req: 1, result: Err(Some(no_provider())) }), "{fx:?}");
    let fx = c.handle(t0, request(2, "MainThreadAuthentication", "$getSession", vec![json!("github"), json!(["repo"]), json!("pub.two"), json!("Pub"), json!({})]));
    assert_eq!(gaps(&fx), vec![("MainThreadAuthentication".into(), "$getSession".into(), Some("pub.two".into()), 2)]);

    // $isOnBatteryPower(): Promise<boolean>, no argument names an extension.
    let fx = c.handle(t0, request(3, "MainThreadPower", "$isOnBatteryPower", vec![]));
    assert_eq!(gaps(&fx), vec![("MainThreadPower".into(), "$isOnBatteryPower".into(), None, 1)]);
    assert_eq!(c.unimplemented_calls().get(&key("MainThreadPower", "$isOnBatteryPower")), Some(&1));
}

#[test]
fn fr_e9_void_todo_call_is_counted_but_not_recorded() {
    let t0 = Instant::now();
    let mut c = core(t0);
    // $disposeEntry(id): void.
    let fx = c.handle(t0, request(1, "MainThreadStatusBar", "$disposeEntry", vec![json!("e")]));
    assert!(gaps(&fx).is_empty(), "{fx:?}");
    assert_eq!(fx, vec![Effect::Respond { req: 1, result: Ok(Reply::Empty) }]);
    assert_eq!(c.unimplemented_calls().get(&key("MainThreadStatusBar", "$disposeEntry")), Some(&1));
}

#[test]
fn fr_e9_acked_and_out_calls_are_answered_and_neither_counted_nor_recorded() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let cases: Vec<(&'static str, &str, Vec<Value>)> = vec![
        // acked (leader, Q2)
        ("MainThreadTelemetry", "$publicLog2", vec![json!("event"), json!({})]),
        ("MainThreadErrors", "$onUnexpectedError", vec![json!({"name": "Error", "message": "boom", "stack": "at x"})]),
        // out (owner, Q-E7, INTENT D54); $selectChatModels returns a value.
        ("MainThreadLanguageModels", "$selectChatModels", vec![json!({})]),
        ("MainThreadChatStatus", "$setEntry", vec![json!("id"), json!({})]),
    ];
    for (i, (proxy, method, args)) in cases.into_iter().enumerate() {
        let req = i as u32 + 1;
        let fx = c.handle(t0, request(req, proxy, method, args));
        assert!(gaps(&fx).is_empty(), "{proxy}.{method}: {fx:?}");
        assert!(fx.iter().any(|e| matches!(e, Effect::Respond { req: r, result: Ok(_) } if *r == req)), "{proxy}.{method}: {fx:?}");
    }
    assert!(c.unimplemented_calls().is_empty(), "{:?}", c.unimplemented_calls());
}

// Q-E8 (c): the session's fallback reply is the ledger's answer for that method, not one blanket
// rule: a JSON value, a rejection, or by the declared result (`[]` for an array, else `undefined`).
#[test]
fn fr_e9_fallback_reply_is_chosen_per_method_from_the_ledger() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let cases: Vec<(&'static str, &str, Vec<Value>, Result<Reply, Option<Value>>)> = vec![
        // `answer`: Code-OSS's BrowserPowerService constants.
        ("MainThreadPower", "$isOnBatteryPower", vec![], Ok(Reply::Json(json!(false)))),
        ("MainThreadPower", "$getCurrentThermalState", vec![], Ok(Reply::Json(json!("unknown")))),
        ("MainThreadPower", "$getSystemIdleTime", vec![], Ok(Reply::Json(json!(0)))),
        // `error`: Code-OSS rejects for a provider that is not registered.
        ("MainThreadAuthentication", "$getAccounts", vec![json!("github")], Err(Some(no_provider()))),
        // No ledger answer: an array result answers [], anything else undefined.
        ("MainThreadSecretState", "$getKeys", vec![json!("pub.ext")], Ok(Reply::Json(json!([])))),
        ("MainThreadWorkspace", "$startFileSearch", vec![json!(null), json!({}), json!(null)], Ok(Reply::Empty)),
        ("MainThreadStatusBar", "$disposeEntry", vec![json!("e")], Ok(Reply::Empty)),
    ];
    for (i, (proxy, method, args, want)) in cases.into_iter().enumerate() {
        let req = i as u32 + 1;
        let fx = c.handle(t0, request(req, proxy, method, args));
        assert!(fx.contains(&Effect::Respond { req, result: want.clone() }), "{proxy}.{method}: want {want:?}, got {fx:?}");
    }
}
