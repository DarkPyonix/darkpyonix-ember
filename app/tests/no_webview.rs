//! NFR-L2 (E1 made testable): the launcher and conversation UI never allocate a webview.
//!
//! Fails if any webview crate is in this app's dependency graph (every target platform), or
//! if a webview API is named in the app's or the client core's sources. The same check runs
//! without building in `scripts/check-no-webview.sh` (CI).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Crates that embed a browser engine (or exist to drive one).
const BANNED_CRATES: &[&str] = &[
    "wry",
    "tauri",
    "webview2",
    "webview2-com",
    "webview2-com-sys",
    "webkit2gtk",
    "webkit2gtk-sys",
    "javascriptcore-rs",
    "objc2-web-kit",
    "web-view",
    "webview-sys",
    "dioxus-desktop",
    "dioxus-mobile",
    "cef",
];

/// Webview APIs, as source text.
const BANNED_APIS: &[&str] = &[
    "WKWebView",
    "WebView2",
    "ICoreWebView2",
    "android.webkit.WebView",
    "android/webkit/WebView",
    "webkit2gtk",
    "WebKitGTK",
    "wry::",
    "tauri::",
    "dioxus_desktop",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn banned_crate(name: &str) -> bool {
    BANNED_CRATES.iter().any(|b| name == *b || name.starts_with(&format!("{b}-")) || name.starts_with(&format!("tauri-")))
}

/// Package names in the resolved dependency graph (all platforms, all edge kinds).
fn resolved_packages() -> Vec<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--offline", "--manifest-path"])
        .arg(manifest_dir().join("Cargo.toml"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("cargo metadata JSON");
    v["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .filter_map(|p| p["name"].as_str().map(str::to_string))
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") || p.file_name().is_some_and(|n| n == "Cargo.toml") {
            out.push(p);
        }
    }
}

#[test]
fn nfr_l2_no_webview_crate_in_dependency_graph() {
    let packages = resolved_packages();
    assert!(packages.iter().any(|p| p == "dioxus-compose"), "dioxus-compose should be in the graph: {packages:?}");
    let hits: Vec<&String> = packages.iter().filter(|p| banned_crate(p)).collect();
    assert!(hits.is_empty(), "webview crates in ember-app's dependency graph (NFR-L2): {hits:?}");
}

#[test]
fn nfr_l2_no_webview_api_in_sources() {
    let root = manifest_dir();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.push(root.join("Cargo.toml"));
    // The client core is linked into the launcher process too.
    rust_files(&root.join("../client/src"), &mut files);
    files.push(root.join("../client/Cargo.toml"));
    assert!(files.len() > 10, "expected to scan the app and client sources, found {files:?}");
    let mut hits = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            for api in BANNED_APIS {
                if line.contains(api) {
                    hits.push(format!("{}:{}: {api}", f.display(), n + 1));
                }
            }
        }
    }
    assert!(hits.is_empty(), "webview APIs referenced from launcher-process code (NFR-L2):\n{}", hits.join("\n"));
}

#[test]
fn banned_list_matches_what_it_should() {
    assert!(banned_crate("wry"));
    assert!(banned_crate("webview2-com-macros"));
    assert!(banned_crate("tauri-runtime-wry"));
    assert!(banned_crate("dioxus-desktop"));
    assert!(!banned_crate("dioxus-compose"));
    assert!(!banned_crate("reqwest"));
}
