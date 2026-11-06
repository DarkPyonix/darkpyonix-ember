//! Explorer edits (D38, #438): the paths a create, rename or delete may name. The checks keep
//! mistakes inside the project folder; they are a safeguard, not a security boundary (the OSE
//! channel writes with the server user's rights).

use ember_editor::paths::{child_path, normalize, renamed_path, target_path, PathError};

const FOLDER: &str = "/w/p";

#[test]
fn normalize_resolves_dots_double_slashes_and_the_trailing_slash() {
    assert_eq!(normalize("/w/p/./src//a/../lib.rs"), Some("/w/p/src/lib.rs".to_string()));
    assert_eq!(normalize("/w/p/"), Some("/w/p".to_string()));
    assert_eq!(normalize("/"), Some("/".to_string()));
    assert_eq!(normalize("/../x"), None, "a path that climbs above the root has no meaning");
    assert_eq!(normalize("relative/x"), None);
}

#[test]
fn child_path_joins_a_valid_name_below_a_directory_inside_the_folder() {
    assert_eq!(child_path(FOLDER, "/w/p/src", "main.rs"), Ok("/w/p/src/main.rs".to_string()));
    assert_eq!(child_path(FOLDER, "/w/p", "new dir"), Ok("/w/p/new dir".to_string()));
    assert_eq!(child_path(FOLDER, "/w/p/src/", "a.rs"), Ok("/w/p/src/a.rs".to_string()));
}

#[test]
fn child_path_rejects_bad_names() {
    for name in ["", " ", ".", "..", "a/b", "a\0b", "../x"] {
        assert_eq!(child_path(FOLDER, "/w/p", name), Err(PathError::BadName), "name {name:?}");
    }
}

#[test]
fn child_path_rejects_a_parent_outside_the_folder() {
    assert_eq!(child_path(FOLDER, "/w", "x"), Err(PathError::OutsideFolder));
    assert_eq!(child_path(FOLDER, "/w/p/../q", "x"), Err(PathError::OutsideFolder));
    assert_eq!(child_path(FOLDER, "/w/pp", "x"), Err(PathError::OutsideFolder), "a sibling that shares a prefix");
    assert_eq!(child_path(FOLDER, "/etc", "x"), Err(PathError::OutsideFolder));
}

#[test]
fn target_path_is_a_path_strictly_inside_the_folder() {
    assert_eq!(target_path(FOLDER, "/w/p/src/./a.rs"), Ok("/w/p/src/a.rs".to_string()));
    assert_eq!(target_path(FOLDER, "/w/p"), Err(PathError::FolderRoot));
    assert_eq!(target_path(FOLDER, "/w/p/"), Err(PathError::FolderRoot));
    assert_eq!(target_path(FOLDER, "/w"), Err(PathError::OutsideFolder));
    assert_eq!(target_path(FOLDER, "/w/p/../x"), Err(PathError::OutsideFolder));
}

#[test]
fn renamed_path_keeps_the_directory_and_changes_the_name() {
    assert_eq!(renamed_path(FOLDER, "/w/p/src/a.rs", "b.rs"), Ok("/w/p/src/b.rs".to_string()));
    assert_eq!(renamed_path(FOLDER, "/w/p/src", "lib"), Ok("/w/p/lib".to_string()));
    assert_eq!(renamed_path(FOLDER, "/w/p", "x"), Err(PathError::FolderRoot));
    assert_eq!(renamed_path(FOLDER, "/w/p/a.rs", "../b.rs"), Err(PathError::BadName));
}
