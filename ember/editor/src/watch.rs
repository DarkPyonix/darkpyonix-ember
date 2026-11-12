//! FR-L31.24: how the session watches the workspace folder.
//!
//! Code-OSS's workbench watches every workspace folder recursively (`workspaceWatcher.ts`,
//! `fileService.watch(folder, { recursive: true, excludes })`). The OSE serves a recursive request
//! with the parcel watcher (FSEvents on macOS, inotify on Linux), whose events cover every file
//! below the folder.
//!
//! A request for one file is different: the server's file channel hands it to a separate
//! `fs.watch()` watcher (`common/diskFileSystemProvider.ts` `watchNonRecursive`), and at 1.139.1
//! the OSE refuses that on macOS for any path below `/Volumes/`, whatever the filesystem is
//! (`nodejsWatcherLib.ts` L211, "possibly being a network share"). A project on a local external
//! disk then never reports an external change (#789). The OSE is not patched (E4), so the session
//! asks for the watch the workbench asks for, and asks for no per-file watch below the folder
//! ([`covered_by_workspace_watch`]).

use ember_editor_conn::remote_fs::WatchOptions;

/// The default of Code-OSS's `files.watcherExclude` at 1.139.1 (`files.contribution.ts`):
/// version-control object stores, which change in bulk and hold nothing an editor shows.
pub const DEFAULT_WATCHER_EXCLUDES: [&str; 6] = [
    ".git/objects/**",
    ".git/subtree-cache/**",
    ".hg/store/**",
    "*/.git/objects/**",
    "*/.git/subtree-cache/**",
    "*/.hg/store/**",
];

/// The options of the one recursive watch on the workspace folder.
pub fn workspace_watch_options() -> WatchOptions {
    WatchOptions {
        recursive: true,
        excludes: DEFAULT_WATCHER_EXCLUDES.iter().map(|s| s.to_string()).collect(),
        includes: None,
        correlation_id: None,
    }
}

/// Whether the recursive watch on `workspace` reports changes of `path`: the path is below the
/// folder and not inside one of [`DEFAULT_WATCHER_EXCLUDES`]. Such a file needs no request of its
/// own. A file outside the folder, or inside an excluded store, is still watched by itself.
pub fn covered_by_workspace_watch(workspace: &str, path: &str) -> bool {
    let workspace = workspace.trim_end_matches('/');
    let Some(rest) = path.strip_prefix(workspace).and_then(|r| r.strip_prefix('/')) else { return false };
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    // `<store>/**` at the root and `*/<store>/**` one folder down, as the exclude globs say.
    let excluded = |at: usize| {
        parts.len() > at + 2
            && matches!((parts[at], parts[at + 1]), (".git", "objects") | (".git", "subtree-cache") | (".hg", "store"))
    };
    !(excluded(0) || excluded(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_24_workspace_watch_is_recursive_with_the_code_oss_excludes() {
        let json = serde_json::to_value(workspace_watch_options()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "recursive": true,
                "excludes": [
                    ".git/objects/**", ".git/subtree-cache/**", ".hg/store/**",
                    "*/.git/objects/**", "*/.git/subtree-cache/**", "*/.hg/store/**"
                ]
            })
        );
    }

    #[test]
    fn fr_l31_24_files_below_the_workspace_need_no_watch_of_their_own() {
        let ws = "/Volumes/macMini/proj";
        assert!(covered_by_workspace_watch(ws, "/Volumes/macMini/proj/src/main.rs"));
        assert!(covered_by_workspace_watch(ws, "/Volumes/macMini/proj/Cargo.toml"));
        assert!(covered_by_workspace_watch("/Volumes/macMini/proj/", "/Volumes/macMini/proj/a.txt"));
        assert!(covered_by_workspace_watch(ws, "/Volumes/macMini/proj/.git/HEAD"));
    }

    #[test]
    fn fr_l31_24_files_outside_or_in_an_excluded_store_keep_their_own_watch() {
        let ws = "/Volumes/macMini/proj";
        assert!(!covered_by_workspace_watch(ws, "/Volumes/macMini/other/a.rs"));
        assert!(!covered_by_workspace_watch(ws, "/Volumes/macMini/proj2/a.rs"));
        assert!(!covered_by_workspace_watch(ws, "/Volumes/macMini/proj"));
        assert!(!covered_by_workspace_watch(ws, "/Volumes/macMini/proj/.git/objects/ab/cdef"));
        assert!(!covered_by_workspace_watch(ws, "/Volumes/macMini/proj/sub/.hg/store/data"));
    }

    /// An uncorrelated request is what makes the recursive watcher's events reach the session's
    /// one `fileChange` listener.
    #[test]
    fn fr_l31_24_workspace_watch_is_uncorrelated() {
        assert_eq!(workspace_watch_options().correlation_id, None);
    }
}
