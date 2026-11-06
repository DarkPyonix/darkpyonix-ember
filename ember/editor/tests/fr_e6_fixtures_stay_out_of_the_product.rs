//! The grammar copies under `tests/fixtures/textmate/grammars` are test data with their own licences. They
//! must not reach the product: the crate is not published, the manifest excludes the folder, and no source
//! file embeds anything from it.

use std::path::Path;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn fr_e6_the_crate_is_not_published_and_excludes_the_fixture_folder() {
    let manifest = std::fs::read_to_string(manifest_dir().join("Cargo.toml")).unwrap();
    assert!(manifest.lines().any(|l| l.trim() == "publish = false"), "ember-editor must stay unpublished");
    let exclude = manifest.lines().find(|l| l.trim_start().starts_with("exclude")).expect("an exclude list");
    assert!(exclude.contains("tests/fixtures/**"), "{exclude}");
}

#[test]
fn fr_e6_no_source_file_embeds_a_fixture() {
    fn walk(dir: &Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                if text.contains("fixtures/textmate") && (text.contains("include_str!") || text.contains("include_bytes!")) {
                    hits.push(path.display().to_string());
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(&manifest_dir().join("src"), &mut hits);
    assert!(hits.is_empty(), "embedded fixtures: {hits:?}");
}

#[test]
fn fr_e6_every_vendored_grammar_is_in_the_notice_with_its_source_commit() {
    let dir = manifest_dir().join("tests/fixtures/textmate/grammars");
    let notice = std::fs::read_to_string(dir.join("NOTICE.md")).unwrap();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            assert!(notice.contains(&name), "{name} is not in NOTICE.md");
            let grammar: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            // A grammar without an upstream `version` must be one of the four NOTICE.md lists by name, each with its
            // origin: the two Code-OSS JSDoc injections with the Code-OSS commit, the two test grammars as written here.
            match grammar["version"].as_str() {
                Some(version) => {
                    let commit = version.rsplit('/').next().unwrap().to_owned();
                    assert!(notice.contains(&commit), "{name}: commit {commit} is not in NOTICE.md");
                }
                None => {
                    let row = notice.lines().find(|l| l.contains(&name)).unwrap_or_else(|| panic!("{name} has no row in NOTICE.md"));
                    match name.as_str() {
                        "jsdoc.ts.injection.tmLanguage.json" | "jsdoc.js.injection.tmLanguage.json" => {
                            assert!(notice.contains("04c0d99f4fb0d8afe6ce4f0c58e31e183ac3e4b1"), "the Code-OSS commit is named");
                            assert!(row.contains("MIT") || row.contains("the same"), "{row}");
                        }
                        "injection-host.tmLanguage.json" | "injection-guest.tmLanguage.json" => {
                            assert!(row.contains("written for these tests"), "{row}");
                        }
                        other => panic!("{other} has no upstream version and is not one of the four NOTICE.md names"),
                    }
                }
            }
        }
    }
}
