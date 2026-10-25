//! Workaround for compose-rust #321 (material: flips window decoration); remove once Ember is on v0.0.3 and #321 is settled. Tracked in #166.
//! The renderer flips the live window to undecorated when any `material:` modifier appears in the
//! tree, and AWT then throws IllegalComponentStateException (#166). The app must not use one.

use std::fs;
use std::path::Path;

fn scan(dir: &Path, hits: &mut Vec<String>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            scan(&p, hits);
        } else if p.extension().is_some_and(|x| x == "rs") {
            for (i, l) in fs::read_to_string(&p).unwrap().lines().enumerate() {
                let t = l.trim_start();
                if !t.starts_with("//") && t.starts_with("material:") {
                    hits.push(format!("{}:{}", p.display(), i + 1));
                }
            }
        }
    }
}

#[test]
fn no_material_modifier_in_app_sources() {
    let mut hits = vec![];
    scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut hits);
    assert!(hits.is_empty(), "material modifier crashes the window (#166): {hits:?}");
}
