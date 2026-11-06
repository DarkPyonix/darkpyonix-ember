//! The loader against the grammars of a real Code-OSS build. Ignored by default and gated on
//! `EMBER_OSE_EXTENSIONS`, the `extensions` folder of an unpacked OSE server:
//!
//! ```sh
//! export EMBER_OSE_EXTENSIONS="$(dirname "$(ember/editor-conn/scripts/fetch-ose-artifact.sh)")/../extensions"
//! cargo test -p ember-editor --test textmate_real_grammars -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use ember_editor::textmate::{read_grammar, GrammarCatalog};
use serde_json::{json, Value};

#[test]
#[ignore = "needs a real OSE extensions folder: EMBER_OSE_EXTENSIONS=<unpacked build>/extensions"]
fn fr_e6_every_grammar_the_real_extensions_contribute_loads_and_parses() {
    let Some(dir) = std::env::var_os("EMBER_OSE_EXTENSIONS").map(PathBuf::from) else {
        eprintln!("[textmate] EMBER_OSE_EXTENSIONS not set; skipping");
        return;
    };
    let mut descriptions: Vec<Value> = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())).flatten() {
        let package = entry.path().join("package.json");
        let Ok(text) = std::fs::read_to_string(&package) else { continue };
        let Ok(pkg) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(contributes) = pkg.get("contributes").cloned() else { continue };
        descriptions.push(json!({
            "identifier": {"value": format!("{}.{}", pkg["publisher"].as_str().unwrap_or("?"), pkg["name"].as_str().unwrap_or("?"))},
            "extensionLocation": {"path": entry.path().to_string_lossy()},
            "contributes": contributes,
        }));
    }
    let catalog = GrammarCatalog::from_extensions(&descriptions);
    assert!(catalog.skipped().is_empty(), "skipped: {:?}", catalog.skipped());
    assert!(catalog.len() > 20, "a real build has dozens of grammars, found {}", catalog.len());
    for scope in ["source.ts", "source.js", "source.rust", "source.python", "source.json", "text.html.markdown"] {
        assert!(catalog.by_scope(scope).is_some(), "no grammar for {scope}");
    }
    let (mut parsed, mut other_format) = (0, Vec::new());
    for scope in descriptions.iter().flat_map(|d| d["contributes"]["grammars"].as_array().cloned().unwrap_or_default()) {
        let scope_name = scope["scopeName"].as_str().unwrap();
        let entry = catalog.by_scope(scope_name).unwrap();
        let ext_dir = std::path::Path::new(&entry.path).ancestors().find(|p| p.parent() == Some(dir.as_path())).unwrap().to_path_buf();
        match read_grammar(&ext_dir, std::path::Path::new(&entry.path)) {
            Ok(g) => {
                assert_eq!(g.scope_name, scope_name, "{}", entry.path);
                parsed += 1;
            }
            Err(ember_editor::textmate::GrammarError::UnsupportedFormat(p)) => other_format.push(p),
            Err(e) => panic!("{}: {e}", entry.path),
        }
    }
    println!("[textmate] {} grammars in the catalog, {parsed} parsed, {} in another format: {other_format:?}", catalog.len(), other_format.len());
    assert!(parsed > 20);
}
