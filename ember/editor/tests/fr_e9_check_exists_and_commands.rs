//! FR-E9 (#1043): `MainThreadWorkspace.$checkExists` and `MainThreadCommands.$getCommands` answer as
//! Code-OSS does, through the pure session core (no server).
//!
//! `$checkExists` is Code-OSS's `checkGlobFileExists`: an `exists` file search through the extension
//! host's own file search provider. The test plays that provider over a fake workspace (a list of
//! files): it reads the query the core sends, matches its include and exclude globs against the files
//! below each folder and replies `limitHit` when one matches, as the provider's `exists` search does.

use std::time::Instant;

use ember_editor::engine::Token;
use ember_editor::languages::LanguageRegistry;
use ember_editor::search::{path_included, relative_to};
use ember_editor::{CoreOptions, Effect, Input, SessionCore};
use ember_editor_conn::exthost::Call;
use ember_editor_conn::rpc::{js_error, Arg, IncomingRequest, Reply};
use serde_json::{json, Value};

const AUTH: &str = "127.0.0.1:9";

/// The fake workspace: two folders, a dependency folder Code-OSS excludes by default, and a `.git`.
const FILES: [&str; 7] = [
    "/w/Cargo.toml",
    "/w/src/main.rs",
    "/w/node_modules/left-pad/package.json",
    "/w/.git/config",
    "/w/docs/guide/intro.md",
    "/other/setup.py",
    "/other/README",
];

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
        cancellable: true,
    })
}

fn folder(path: &str) -> Value {
    json!({"$mid": 1, "scheme": "vscode-remote", "authority": AUTH, "path": path})
}

fn check_exists(req: u32, folders: &[&str], includes: &[&str]) -> Input {
    let folders: Vec<Value> = folders.iter().map(|f| folder(f)).collect();
    request(req, "MainThreadWorkspace", "$checkExists", vec![json!(folders), json!(includes)])
}

fn responded(fx: &[Effect], req: u32) -> Option<&Result<Reply, Option<Value>>> {
    fx.iter().find_map(|e| match e {
        Effect::Respond { req: r, result } if *r == req => Some(result),
        _ => None,
    })
}

fn file_search(fx: &[Effect]) -> Option<(Token, &Call)> {
    fx.iter().find_map(|e| match e {
        Effect::Rpc { token: Some(t), call } if call.method == "$provideFileSearchResults" => Some((*t, call)),
        _ => None,
    })
}

fn globs(v: &Value) -> Vec<String> {
    v.as_object().map(|o| o.iter().filter(|(_, on)| on.as_bool() == Some(true)).map(|(g, _)| g.clone()).collect()).unwrap_or_default()
}

/// The provider's `exists` search over [`FILES`]: `limitHit` when a file below a folder of the query
/// is included and not excluded.
fn fake_provider(query: &Value) -> Value {
    let include = globs(&query["includePattern"]);
    let exclude = globs(&query["excludePattern"]);
    let found = query["folderQueries"].as_array().unwrap().iter().any(|fq| {
        let root = fq["folder"]["path"].as_str().unwrap();
        FILES.iter().filter_map(|f| relative_to(root, f)).any(|rel| path_included(rel, &include, &exclude))
    });
    json!({"limitHit": found, "messages": [], "stats": {"type": "searchProcess"}})
}

/// A core whose extension host registered its file search provider as the real one does at its start:
/// handle 2 for the remote scheme (`file` turned into `vscode-remote` by the URI transformer).
fn with_provider(t0: Instant) -> SessionCore {
    let mut c = core(t0);
    let fx = c.handle(t0, request(1, "MainThreadSearch", "$registerFileSearchProvider", vec![json!(2), json!("vscode-remote")]));
    assert_eq!(responded(&fx, 1), Some(&Ok(Reply::Empty)));
    assert!(c.unimplemented_calls().is_empty(), "{:?}", c.unimplemented_calls());
    c
}

/// Ask `$checkExists`, play the provider, and return the core's answer.
fn ask(c: &mut SessionCore, t0: Instant, req: u32, folders: &[&str], includes: &[&str]) -> Result<Reply, Option<Value>> {
    let fx = c.handle(t0, check_exists(req, folders, includes));
    assert!(responded(&fx, req).is_none(), "answered before the search: {fx:?}");
    let (token, call) = file_search(&fx).unwrap_or_else(|| panic!("no file search in {fx:?}"));
    assert_eq!(call.args[0], Arg::Json(json!(2)), "the remote provider's handle");
    let query = call.args[2].as_json().unwrap().clone();
    assert_eq!(query["exists"], true);
    assert_eq!(query["_reason"], "checkExists");
    let fx = c.handle(t0, Input::RpcReply { token, result: Ok(fake_provider(&query)) });
    responded(&fx, req).cloned().unwrap_or_else(|| panic!("no answer for {req}: {fx:?}"))
}

#[test]
fn fr_e9_check_exists_searches_the_remote_workspace() {
    let t0 = Instant::now();
    let mut c = with_provider(t0);
    let yes = Ok(Reply::Json(json!(true)));
    let no = Ok(Reply::Json(json!(false)));
    assert_eq!(ask(&mut c, t0, 10, &["/w"], &["**/Cargo.toml"]), yes);
    assert_eq!(ask(&mut c, t0, 11, &["/w"], &["**/*.py"]), no, "setup.py is in another folder");
    assert_eq!(ask(&mut c, t0, 12, &["/w", "/other"], &["**/*.py"]), yes, "every folder is searched");
    assert_eq!(ask(&mut c, t0, 13, &["/w"], &["**/*.md", "**/*.py"]), yes, "any include glob");
    assert_eq!(ask(&mut c, t0, 14, &["/w"], &["docs/**/*.md"]), yes, "a glob relative to the folder");
    assert_eq!(ask(&mut c, t0, 15, &["/w"], &["**/package.json"]), no, "node_modules is excluded by default");
    assert_eq!(ask(&mut c, t0, 16, &["/w"], &["**/config"]), no, ".git is excluded by default");
    assert!(c.unimplemented_calls().is_empty(), "{:?}", c.unimplemented_calls());
}

#[test]
fn fr_e9_check_exists_without_a_provider_or_folder_is_false() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let fx = c.handle(t0, check_exists(1, &["/w"], &["**/Cargo.toml"]));
    assert_eq!(responded(&fx, 1), Some(&Ok(Reply::Json(json!(false)))));
    assert!(file_search(&fx).is_none());

    let mut c = with_provider(t0);
    let fx = c.handle(t0, check_exists(2, &[], &["**/Cargo.toml"]));
    assert_eq!(responded(&fx, 2), Some(&Ok(Reply::Json(json!(false)))));
    assert!(file_search(&fx).is_none());

    // An unregistered provider is not asked any more.
    c.handle(t0, request(3, "MainThreadSearch", "$unregisterProvider", vec![json!(2)]));
    let fx = c.handle(t0, check_exists(4, &["/w"], &["**/Cargo.toml"]));
    assert_eq!(responded(&fx, 4), Some(&Ok(Reply::Json(json!(false)))));
}

#[test]
fn fr_e9_check_exists_cancelled_is_false_and_a_failure_rejects() {
    let t0 = Instant::now();
    let mut c = with_provider(t0);
    let fx = c.handle(t0, check_exists(5, &["/w"], &["**/Cargo.toml"]));
    let (token, _) = file_search(&fx).unwrap();
    let fx = c.handle(t0, Input::RpcReply { token, result: Err("Canceled".into()) });
    assert_eq!(responded(&fx, 5), Some(&Ok(Reply::Json(json!(false)))));

    let fx = c.handle(t0, check_exists(6, &["/w"], &["**/Cargo.toml"]));
    let (token, _) = file_search(&fx).unwrap();
    let fx = c.handle(t0, Input::RpcReply { token, result: Err("rg exited with 2".into()) });
    assert_eq!(responded(&fx, 6), Some(&Err(Some(js_error("Error", "rg exited with 2")))));
}

#[test]
fn fr_e9_file_matches_of_the_exists_search_are_acknowledged() {
    let t0 = Instant::now();
    let mut c = with_provider(t0);
    let fx = c.handle(t0, request(7, "MainThreadSearch", "$handleFileMatch", vec![json!(2), json!(1), json!([folder("/w/Cargo.toml")])]));
    assert_eq!(responded(&fx, 7), Some(&Ok(Reply::Empty)));
    assert!(c.unimplemented_calls().is_empty(), "{:?}", c.unimplemented_calls());
}

#[test]
fn fr_e9_get_commands_lists_the_registered_and_workbench_commands() {
    let t0 = Instant::now();
    let mut c = core(t0);
    let get = |c: &mut SessionCore, req: u32| {
        let fx = c.handle(t0, request(req, "MainThreadCommands", "$getCommands", vec![]));
        responded(&fx, req).cloned().unwrap_or_else(|| panic!("no answer: {fx:?}"))
    };
    // Before any registration: the workbench command Ember runs itself.
    assert_eq!(get(&mut c, 1), Ok(Reply::Json(json!(["_setContext"]))));

    for (req, id) in [(2, "rust-analyzer.run"), (3, "eslint.restart"), (4, "rust-analyzer.run")] {
        c.handle(t0, request(req, "MainThreadCommands", "$registerCommand", vec![json!(id)]));
    }
    assert_eq!(get(&mut c, 5), Ok(Reply::Json(json!(["_setContext", "eslint.restart", "rust-analyzer.run"]))));

    c.handle(t0, request(6, "MainThreadCommands", "$unregisterCommand", vec![json!("eslint.restart")]));
    assert_eq!(get(&mut c, 7), Ok(Reply::Json(json!(["_setContext", "rust-analyzer.run"]))));
    assert!(c.unimplemented_calls().is_empty(), "{:?}", c.unimplemented_calls());
}
