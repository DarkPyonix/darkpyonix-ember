//! Workspace trust on the extension-host side (FR-L31.18, #450).
//!
//! Code-OSS at [`crate::PINNED_COMMIT`] runs an untrusted folder in restricted mode: an extension
//! whose manifest does not support untrusted workspaces is `DisabledByTrustRequirement`
//! (extensionEnablementService.ts L489, L603), and so is one that depends on such an extension
//! (`DisabledByExtensionDependency`). A disabled extension is left out of the extension host's
//! `myExtensions`, so it never activates. When the user grants trust the workbench adds them with
//! `ExtHostExtensionService.$deltaExtensions(IExtensionDescriptionDelta)` (extHost.protocol.ts
//! L2614, extensionHostProtocol.ts L19-26) and tells the host with
//! `ExtHostWorkspace.$onDidGrantWorkspaceTrust()` (extHost.protocol.ts L2513, sent by
//! mainThreadWorkspace.ts L264 on `onDidChangeTrust`). The host flips `workspace.isTrusted` once,
//! from false to true (extHostWorkspace.ts L964-969); the trust a host starts with is the second
//! argument of `$initializeWorkspace` (L2510, see [`crate::exthost::bootstrap_calls`]).
//!
//! The support rule is `ExtensionManifestPropertiesService.getExtensionUntrustedWorkspaceSupportType`
//! (extensionManifestPropertiesService.ts L193-226): an extension with no `main` is supported; else
//! `capabilities.untrustedWorkspaces.supported` (`true`, `false` or `"limited"`) decides; a manifest
//! that says nothing is not supported. Ember has no `security.workspace.trust.extensionSupport`
//! setting and its product file has no `extensionUntrustedWorkspaceSupport` map, so those two
//! override layers are empty.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::exthost::{activation_events_map, proxy, Call};
use crate::management::extension_id;
use crate::rpc::Arg;

/// Whether an extension may run in an untrusted workspace (support type `true` or `"limited"`).
pub fn supports_untrusted(desc: &Value) -> bool {
    if desc.get("main").is_none() {
        return true;
    }
    match desc.pointer("/capabilities/untrustedWorkspaces/supported") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "limited",
        _ => false,
    }
}

/// The lower-case ids of the extensions restricted mode withholds: those not supporting untrusted
/// workspaces, and every extension that depends on one of them (transitively).
pub fn withheld_ids(extensions: &[Value]) -> BTreeSet<String> {
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut out = BTreeSet::new();
    for desc in extensions {
        let Some(id) = extension_id(desc) else { continue };
        let id = id.to_lowercase();
        let list = desc
            .get("extensionDependencies")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_lowercase).collect())
            .unwrap_or_default();
        deps.insert(id.clone(), list);
        if !supports_untrusted(desc) {
            out.insert(id);
        }
    }
    loop {
        let more: Vec<String> = deps
            .iter()
            .filter(|(id, d)| !out.contains(*id) && d.iter().any(|x| out.contains(x)))
            .map(|(id, _)| id.clone())
            .collect();
        if more.is_empty() {
            return out;
        }
        out.extend(more);
    }
}

fn identifier(id: &str) -> Value {
    json!({ "value": id, "_lower": id.to_lowercase() })
}

/// The extensions the host runs: all of them when `trusted`, else those restricted mode keeps.
pub fn running(extensions: &[Value], trusted: bool) -> Vec<Value> {
    let withheld = if trusted { BTreeSet::new() } else { withheld_ids(extensions) };
    extensions
        .iter()
        .filter(|d| extension_id(d).is_some_and(|id| !withheld.contains(&id.to_lowercase())))
        .cloned()
        .collect()
}

/// Rewrites `extensions.myExtensions` and `extensions.activationEvents` of an
/// `IExtensionHostInitData` (from [`crate::exthost::InitDataParams::to_json`]) so the host runs
/// only [`running`]. `allExtensions` stays whole, as in Code-OSS, so `extensions.all` still lists
/// a withheld extension.
pub fn restrict_init_data(init: &mut Value, extensions: &[Value], trusted: bool) {
    let run = running(extensions, trusted);
    let ids: Vec<Value> = run.iter().filter_map(extension_id).map(identifier).collect();
    if let Some(e) = init.get_mut("extensions").and_then(Value::as_object_mut) {
        e.insert("myExtensions".into(), Value::Array(ids));
        e.insert("activationEvents".into(), activation_events_map(&run));
    }
}

/// The calls that grant trust to a host started in restricted mode, in order: the withheld
/// extensions join `myExtensions` (`$deltaExtensions`, `toAdd` empty because `allExtensions`
/// already has them), then `$onDidGrantWorkspaceTrust`. `version_id` must be above the init
/// data's `versionId` (1).
pub fn grant_calls(extensions: &[Value], version_id: u32) -> Vec<Call> {
    let withheld = withheld_ids(extensions);
    let added: Vec<Value> = extensions
        .iter()
        .filter(|d| extension_id(d).is_some_and(|id| withheld.contains(&id.to_lowercase())))
        .cloned()
        .collect();
    let my_to_add: Vec<Value> = added.iter().filter_map(extension_id).map(identifier).collect();
    let mut delta = Map::new();
    delta.insert("versionId".into(), json!(version_id));
    delta.insert("toRemove".into(), json!([]));
    delta.insert("toAdd".into(), json!([]));
    delta.insert("addActivationEvents".into(), activation_events_map(&added));
    delta.insert("myToRemove".into(), json!([]));
    delta.insert("myToAdd".into(), Value::Array(my_to_add));
    vec![
        Call {
            proxy: proxy::EXT_HOST_EXTENSION_SERVICE,
            method: "$deltaExtensions",
            args: vec![Arg::Json(Value::Object(delta))],
            cancellable: false,
        },
        Call { proxy: proxy::EXT_HOST_WORKSPACE, method: "$onDidGrantWorkspaceTrust", args: vec![], cancellable: false },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(id: &str, main: bool, supported: Option<Value>, deps: &[&str]) -> Value {
        let (publisher, name) = id.split_once('.').unwrap();
        let mut v = json!({
            "identifier": { "value": id, "_lower": id.to_lowercase() },
            "publisher": publisher,
            "name": name,
            "extensionDependencies": deps,
            "activationEvents": ["onLanguage:x"],
        });
        if main {
            v["main"] = json!("./out/main.js");
        }
        if let Some(s) = supported {
            v["capabilities"] = json!({ "untrustedWorkspaces": { "supported": s } });
        }
        v
    }

    #[test]
    fn fr_l31_18_support_rule_follows_the_manifest() {
        assert!(supports_untrusted(&ext("a.theme", false, None, &[])), "no main: nothing runs, supported");
        assert!(!supports_untrusted(&ext("a.silent", true, None, &[])), "a manifest that says nothing is not supported");
        assert!(supports_untrusted(&ext("a.yes", true, Some(json!(true)), &[])));
        assert!(supports_untrusted(&ext("a.limited", true, Some(json!("limited")), &[])));
        assert!(!supports_untrusted(&ext("a.no", true, Some(json!(false)), &[])));
    }

    #[test]
    fn fr_l31_18_dependants_of_a_withheld_extension_are_withheld() {
        let all = vec![
            ext("a.base", true, Some(json!(false)), &[]),
            ext("a.mid", true, Some(json!(true)), &["A.Base"]),
            ext("a.top", true, Some(json!(true)), &["a.mid"]),
            ext("a.free", true, Some(json!(true)), &[]),
        ];
        let w = withheld_ids(&all);
        assert_eq!(w.into_iter().collect::<Vec<_>>(), vec!["a.base", "a.mid", "a.top"]);
    }

    #[test]
    fn fr_l31_18_init_data_runs_only_supported_extensions_when_untrusted() {
        let all = vec![ext("a.yes", true, Some(json!(true)), &[]), ext("a.no", true, None, &[])];
        let mut init = json!({ "extensions": { "versionId": 1, "allExtensions": all.clone(), "myExtensions": [], "activationEvents": {} } });
        restrict_init_data(&mut init, &all, false);
        assert_eq!(init["extensions"]["myExtensions"], json!([{ "value": "a.yes", "_lower": "a.yes" }]));
        assert!(init["extensions"]["activationEvents"].get("a.no").is_none());
        assert_eq!(init["extensions"]["allExtensions"].as_array().unwrap().len(), 2);
        restrict_init_data(&mut init, &all, true);
        assert_eq!(init["extensions"]["myExtensions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn fr_l31_18_grant_adds_the_withheld_then_signals_the_host() {
        let all = vec![ext("a.yes", true, Some(json!(true)), &[]), ext("a.no", true, None, &[])];
        let calls = grant_calls(&all, 2);
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[0].proxy, calls[0].method), ("ExtHostExtensionService", "$deltaExtensions"));
        let Arg::Json(delta) = &calls[0].args[0] else { panic!("json arg") };
        assert_eq!(delta["versionId"], json!(2));
        assert_eq!(delta["toAdd"], json!([]));
        assert_eq!(delta["myToAdd"], json!([{ "value": "a.no", "_lower": "a.no" }]));
        assert!(delta["addActivationEvents"].get("a.no").is_some());
        assert_eq!((calls[1].proxy, calls[1].method), ("ExtHostWorkspace", "$onDidGrantWorkspaceTrust"));
        assert!(calls[1].args.is_empty());
    }
}
