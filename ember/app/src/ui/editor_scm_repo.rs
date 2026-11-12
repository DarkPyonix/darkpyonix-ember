//! Where the editor's folder sits in its repository (FR-L31.9, #748).
//!
//! Code-OSS finds the repository that encloses a workspace folder (`git rev-parse
//! --show-toplevel`) and shows that repository's changes, all of them, by their path from the
//! repository root. Ember does the same: `git/toplevel` gives the root and the folder's path
//! below it (the *prefix*), every `git/*` path stays relative to the repository root (the node
//! runs `diff`, `stage`, `unstage` and `apply` there), and these functions map such a path to
//! the editor's folder and back. A path is `/` separated; a Windows folder (`C:\w\r\sub`) is read
//! with either separator.

use serde::Deserialize;

/// The `git/toplevel` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Toplevel {
    pub toplevel: String,
    #[serde(default)]
    pub prefix: String,
}

/// A path with `/` separators and no trailing `/` (a bare `/` or `C:/` keeps its own).
fn norm(path: &str) -> String {
    let p = path.replace('\\', "/");
    let t = p.trim_end_matches('/');
    if t.is_empty() && p.starts_with('/') {
        "/".to_string()
    } else if t.len() == 2 && t.ends_with(':') {
        format!("{t}/")
    } else {
        t.to_string()
    }
}

/// `child` below `dir` (both normalized), or `None` when it is not below it. The equal path is
/// the empty string. A Windows drive compares without case.
fn below(dir: &str, child: &str) -> Option<String> {
    if dir == child || (is_drive_path(dir) && dir.eq_ignore_ascii_case(child)) {
        return Some(String::new());
    }
    let head = if dir.ends_with('/') { dir.to_string() } else { format!("{dir}/") };
    if child.len() > head.len() && child.is_char_boundary(head.len()) {
        let (a, b) = child.split_at(head.len());
        if a == head || (is_drive_path(dir) && a.eq_ignore_ascii_case(&head)) {
            return Some(b.to_string());
        }
    }
    None
}

fn is_drive_path(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// The folder's path below the repository root: from the two absolute paths when the folder is
/// below `toplevel` as written, else git's own `prefix` (a symlinked or differently spelled
/// folder). Empty when the folder is the root.
pub fn folder_prefix(folder: &str, top: &Toplevel) -> String {
    if !top.toplevel.is_empty() {
        if let Some(rel) = below(&norm(&top.toplevel), &norm(folder)) {
            return rel;
        }
    }
    norm(&top.prefix).trim_start_matches('/').to_string()
}

/// The repository root as the editor's folder spells it: the folder without the last
/// components the prefix names. With a prefix the folder does not end in, the folder itself.
pub fn repo_root(folder: &str, prefix: &str) -> String {
    let folder = norm(folder);
    let prefix = norm(prefix);
    if prefix.is_empty() {
        return folder;
    }
    match folder.strip_suffix(&prefix) {
        Some(rest) if rest.ends_with('/') => norm(rest),
        _ => folder,
    }
}

/// A repository path as a path below the folder, or `None` for a file outside it.
pub fn to_folder(prefix: &str, repo_path: &str) -> Option<String> {
    let prefix = norm(prefix);
    let path = norm(repo_path);
    if prefix.is_empty() {
        return Some(path);
    }
    below(&prefix, &path).filter(|r| !r.is_empty())
}

/// A path below the folder as a repository path.
pub fn to_repo(prefix: &str, folder_path: &str) -> String {
    let prefix = norm(prefix);
    let path = norm(folder_path);
    let path = path.trim_start_matches('/');
    if prefix.is_empty() {
        path.to_string()
    } else if path.is_empty() {
        prefix
    } else {
        format!("{prefix}/{path}")
    }
}

/// The absolute path of a repository path, inside the folder or not (Code-OSS opens a change
/// outside the workspace folder by its absolute path).
pub fn abs_path(folder: &str, prefix: &str, repo_path: &str) -> String {
    let root = repo_root(folder, prefix);
    let path = norm(repo_path);
    if root == "/" || root.ends_with('/') {
        format!("{root}{path}")
    } else if root.is_empty() {
        path
    } else {
        format!("{root}/{path}")
    }
}

/// The path the editor window uses for a repository path (tabs, the explorer's highlight, a
/// discard): a file inside the folder is joined to the folder as the explorer joins it, so it is
/// the same tab; one outside is [`abs_path`].
pub fn editor_path(folder: &str, prefix: &str, repo_path: &str) -> String {
    match to_folder(prefix, repo_path) {
        Some(rel) => crate::ui::editor_workbench::join_path(folder, &rel),
        None => abs_path(folder, prefix, repo_path),
    }
}

/// The repository path of an open file, for the diff gutter's lookup in `git/status`: the file's
/// absolute path below the repository root. `None` for a file outside the repository.
pub fn gutter_key(folder: &str, prefix: &str, abs: &str) -> Option<String> {
    let root = repo_root(folder, prefix);
    below(&root, &norm(abs)).filter(|r| !r.is_empty())
}

/// Whether a repository path is outside the editor's folder (the changes list still shows it,
/// by its repository path, as Code-OSS does).
pub fn is_outside(prefix: &str, repo_path: &str) -> bool {
    to_folder(prefix, repo_path).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(toplevel: &str, prefix: &str) -> Toplevel {
        Toplevel { toplevel: toplevel.into(), prefix: prefix.into() }
    }

    #[test]
    fn fr_l31_9_the_prefix_comes_from_the_paths_or_from_git() {
        assert_eq!(folder_prefix("/w/r/sub", &top("/w/r", "sub")), "sub");
        assert_eq!(folder_prefix("/w/r/sub/deep/", &top("/w/r", "sub/deep")), "sub/deep");
        // Root == folder.
        assert_eq!(folder_prefix("/w/r", &top("/w/r", "")), "");
        assert_eq!(folder_prefix("/w/r/", &top("/w/r", "")), "");
        // A spelling git does not share (a symlink, /private on macOS): git's prefix.
        assert_eq!(folder_prefix("/tmp/r/sub", &top("/private/tmp/r", "sub/")), "sub");
        // A sibling with the same start is not below the root.
        assert_eq!(folder_prefix("/w/repo2/x", &top("/w/repo", "")), "");
        // Windows: backslashes in the folder, git's forward slashes, drive letter case.
        assert_eq!(folder_prefix("C:\\w\\r\\sub", &top("C:/w/r", "sub")), "sub");
        assert_eq!(folder_prefix("c:\\w\\r\\sub\\deep", &top("C:/w/r", "sub/deep")), "sub/deep");
        assert_eq!(folder_prefix("C:\\w\\r", &top("C:/w/r", "")), "");
        // No answer yet (demo, an old node): the folder is the root.
        assert_eq!(folder_prefix("/w/r", &Toplevel::default()), "");
    }

    #[test]
    fn fr_l31_9_repository_paths_map_to_the_folder_and_back() {
        // Inside.
        assert_eq!(to_folder("sub", "sub/f.txt").as_deref(), Some("f.txt"));
        assert_eq!(to_folder("sub/deep", "sub/deep/a/b.rs").as_deref(), Some("a/b.rs"));
        assert_eq!(to_repo("sub", "f.txt"), "sub/f.txt");
        assert_eq!(to_repo("sub/deep", "a/b.rs"), "sub/deep/a/b.rs");
        // Outside, including a sibling that only shares the start of the name.
        assert_eq!(to_folder("sub", "other/g.txt"), None);
        assert_eq!(to_folder("sub", "subway/x.txt"), None);
        assert_eq!(to_folder("sub", "README.md"), None);
        assert!(is_outside("sub", "other/g.txt"));
        assert!(!is_outside("sub", "sub/f.txt"));
        // Root == folder: unchanged both ways.
        assert_eq!(to_folder("", "src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(to_repo("", "src/main.rs"), "src/main.rs");
        assert!(!is_outside("", "README.md"));
        // Windows separators on the folder side.
        assert_eq!(to_repo("sub", "a\\b.rs"), "sub/a/b.rs");
        assert_eq!(to_folder("sub\\deep", "sub/deep/a.rs").as_deref(), Some("a.rs"));
        // Round trip.
        for p in ["sub/f.txt", "sub/a/b/c.rs"] {
            assert_eq!(to_repo("sub", &to_folder("sub", p).unwrap()), p);
        }
    }

    #[test]
    fn fr_l31_9_absolute_paths_of_changes_inside_and_outside() {
        assert_eq!(repo_root("/w/r/sub", "sub"), "/w/r");
        assert_eq!(repo_root("/w/r/sub/deep", "sub/deep"), "/w/r");
        assert_eq!(repo_root("/w/r", ""), "/w/r");
        assert_eq!(repo_root("/sub", "sub"), "/");
        // A prefix the folder does not end in leaves the folder.
        assert_eq!(repo_root("/w/r/x", "sub"), "/w/r/x");
        assert_eq!(repo_root("/w/r/xsub", "sub"), "/w/r/xsub");
        assert_eq!(abs_path("/w/r/sub", "sub", "sub/f.txt"), "/w/r/sub/f.txt");
        assert_eq!(abs_path("/w/r/sub", "sub", "other/g.txt"), "/w/r/other/g.txt");
        assert_eq!(abs_path("/w/r", "", "src/a.rs"), "/w/r/src/a.rs");
        assert_eq!(abs_path("/sub", "sub", "a.txt"), "/a.txt");
        // Windows.
        assert_eq!(repo_root("C:\\w\\r\\sub", "sub"), "C:/w/r");
        assert_eq!(abs_path("C:\\w\\r\\sub", "sub", "other/g.txt"), "C:/w/r/other/g.txt");
        assert_eq!(abs_path("C:\\sub", "sub", "a.txt"), "C:/a.txt");
    }

    #[test]
    fn fr_l31_9_the_gutter_looks_an_open_file_up_by_its_repository_path() {
        assert_eq!(gutter_key("/w/r/sub", "sub", "/w/r/sub/f.txt").as_deref(), Some("sub/f.txt"));
        // A file outside the folder, opened from the changes list by its absolute path.
        assert_eq!(gutter_key("/w/r/sub", "sub", "/w/r/other/g.txt").as_deref(), Some("other/g.txt"));
        assert_eq!(gutter_key("/w/r", "", "/w/r/src/a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(gutter_key("/w/r/", "", "/w/r/src/a.rs").as_deref(), Some("src/a.rs"));
        // Outside the repository, or the root itself: no lookup.
        assert_eq!(gutter_key("/w/r/sub", "sub", "/elsewhere/a.rs"), None);
        assert_eq!(gutter_key("/w/r/sub", "sub", "/w/r2/a.rs"), None);
        assert_eq!(gutter_key("/w/r", "", "/w/r"), None);
        // Windows separators and drive case.
        assert_eq!(gutter_key("C:\\w\\r\\sub", "sub", "C:\\w\\r\\sub\\f.txt").as_deref(), Some("sub/f.txt"));
        assert_eq!(gutter_key("C:\\w\\r\\sub", "sub", "c:/w/r/other/g.txt").as_deref(), Some("other/g.txt"));
    }

    #[test]
    fn fr_l31_9_editor_paths_match_the_explorer_inside_and_are_absolute_outside() {
        assert_eq!(editor_path("/w/r/sub", "sub", "sub/f.txt"), "/w/r/sub/f.txt");
        assert_eq!(editor_path("/w/r/sub", "sub", "other/g.txt"), "/w/r/other/g.txt");
        assert_eq!(editor_path("/w/r", "", "src/a.rs"), "/w/r/src/a.rs");
        // Inside, the folder keeps its own spelling, as the explorer's join does.
        assert_eq!(editor_path("C:\\w\\r\\sub", "sub", "sub/f.txt"), "C:\\w\\r\\sub/f.txt");
        assert_eq!(editor_path("C:\\w\\r\\sub", "sub", "other/g.txt"), "C:/w/r/other/g.txt");
    }

    /// The node applies a patch at the repository root (FR-L31.9), so a staging patch built from
    /// the diff of a file outside the folder keeps the repository path git gave it: no rewrite.
    #[test]
    fn fr_l31_9_a_staging_patch_keeps_the_repository_path() {
        use crate::ui::vcs::{build_patch, parse_unified, FileStaging};
        let diff = "diff --git a/other/g.txt b/other/g.txt\nindex b77b4eb..7061c57 100644\n--- a/other/g.txt\n+++ b/other/g.txt\n@@ -1,2 +1,2 @@\n x\n-y\n+Y\n";
        let fd = parse_unified(diff).into_iter().next().unwrap();
        assert_eq!(fd.path, "other/g.txt");
        assert!(is_outside("sub", &fd.path));
        let patch = build_patch(&fd, &FileStaging::all(&fd)).unwrap();
        assert!(patch.starts_with("diff --git a/other/g.txt b/other/g.txt\n"), "{patch}");
        assert!(patch.contains("--- a/other/g.txt\n+++ b/other/g.txt\n"), "{patch}");
        assert!(!patch.contains("sub/"), "{patch}");
    }

    #[test]
    fn fr_l31_9_the_toplevel_answer_decodes() {
        let t: Toplevel = serde_json::from_str(r#"{"toplevel":"/w/r","prefix":"sub"}"#).unwrap();
        assert_eq!(t, top("/w/r", "sub"));
        let t: Toplevel = serde_json::from_str(r#"{"toplevel":"/w/r"}"#).unwrap();
        assert_eq!(t.prefix, "");
    }
}
