//! Explorer edits (D38, #438): the paths a create, rename or delete may name. The checks keep
//! mistakes inside the project folder; they are a safeguard, not a security boundary (the OSE
//! channel writes with the server user's rights).

use ember_editor::paths::{child_path, normalize, paste_destination, renamed_path, target_path, PathError};

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
    let long = "x".repeat(256);
    for name in ["", " ", ".", "..", "a/b", "a\0b", "../x", "a\\b", "a.", " a", "a ", "a\nb", long.as_str()] {
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

#[test]
fn renamed_path_refuses_the_same_name() {
    assert_eq!(renamed_path(FOLDER, "/w/p/a.rs", "a.rs"), Err(PathError::SameName));
}

#[test]
fn path_error_dirty_display_korean_reason() {
    assert_eq!(
        format!("{}", PathError::Dirty),
        "저장하지 않은 변경이 있어 이름을 바꿀 수 없습니다. 먼저 저장하세요"
    );
}

#[test]
fn child_path_allows_ordinary_names_with_dots_and_spaces_inside() {
    for name in [".gitignore", "a b.txt", "한글.md", "x.tar.gz", "a-b_c"] {
        assert!(child_path(FOLDER, "/w/p", name).is_ok(), "name {name:?}");
    }
}

#[test]
fn paste_destination_copies_into_another_directory_without_name_collision() {
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/a.rs", "/w/p/target", &[], false),
        Ok("/w/p/target/a.rs".to_string())
    );
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/a.rs", "/w/p/target", &["other.rs"], false),
        Ok("/w/p/target/a.rs".to_string())
    );
}

#[test]
fn paste_destination_copies_with_copy_suffix_when_name_collides() {
    // "main.rs" -> "main copy.rs", then "main copy 2.rs"
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/main.rs", "/w/p/src", &["main.rs"], false),
        Ok("/w/p/src/main copy.rs".to_string())
    );
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/main.rs", "/w/p/src", &["main.rs", "main copy.rs"], false),
        Ok("/w/p/src/main copy 2.rs".to_string())
    );
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/main.rs", "/w/p/src", &["main.rs", "main copy.rs", "main copy 2.rs"], false),
        Ok("/w/p/src/main copy 3.rs".to_string())
    );

    // "README" without extension
    assert_eq!(
        paste_destination(FOLDER, "/w/p/README", "/w/p", &["README"], false),
        Ok("/w/p/README copy".to_string())
    );
    assert_eq!(
        paste_destination(FOLDER, "/w/p/README", "/w/p", &["README", "README copy"], false),
        Ok("/w/p/README copy 2".to_string())
    );

    // ".gitignore" hidden file without base stem
    assert_eq!(
        paste_destination(FOLDER, "/w/p/.gitignore", "/w/p", &[".gitignore"], false),
        Ok("/w/p/.gitignore copy".to_string())
    );
}

#[test]
fn paste_destination_cuts_into_another_directory_and_refuses_same_destination() {
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/a.rs", "/w/p/other", &[], true),
        Ok("/w/p/other/a.rs".to_string())
    );
    // Cutting into the same directory returns SameName error
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/a.rs", "/w/p/src", &[], true),
        Err(PathError::SameName)
    );
}

#[test]
fn paste_destination_refuses_moving_or_copying_directory_into_itself() {
    // Exactly itself
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src", "/w/p/src", &[], true),
        Err(PathError::IntoSelf)
    );
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src", "/w/p/src", &[], false),
        Err(PathError::IntoSelf)
    );

    // Subdirectory below itself
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src", "/w/p/src/sub", &[], true),
        Err(PathError::IntoSelf)
    );
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src", "/w/p/src/sub/nested", &[], false),
        Err(PathError::IntoSelf)
    );

    // Sibling directory that starts with same prefix is allowed
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src", "/w/p/src2", &[], true),
        Ok("/w/p/src2/src".to_string())
    );
}

#[test]
fn paste_destination_refuses_outside_workspace() {
    // Source outside
    assert_eq!(
        paste_destination(FOLDER, "/etc/passwd", "/w/p/src", &[], true),
        Err(PathError::OutsideFolder)
    );
    // Source is folder root
    assert_eq!(
        paste_destination(FOLDER, "/w/p", "/w/p/src", &[], true),
        Err(PathError::FolderRoot)
    );
    // Destination outside
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/a.rs", "/etc", &[], true),
        Err(PathError::OutsideFolder)
    );
    assert_eq!(
        paste_destination(FOLDER, "/w/p/src/a.rs", "/w/other", &[], false),
        Err(PathError::OutsideFolder)
    );
}
