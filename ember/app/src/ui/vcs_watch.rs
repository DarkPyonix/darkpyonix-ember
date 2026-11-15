//! 버전 관리 refreshes on file changes (SPEC `FR-W4d`, `FR-L19`, #460).
//!
//! The view subscribes to its repository folder (recursive) through [`crate::fs_watch`] and
//! reads the snapshot again when the change counter goes up, without the loading line (the
//! old snapshot stays on screen until the new one is there). The repository is the folder the
//! server's git relay uses: the first non-archived session of the project, most recently
//! updated first, else the first session (`ember/server/src/api/git.rs` `relay`), on that
//! session's computer. No data, demo mode or a refused watch: no subscription, and the manual
//! refresh stays as it was.

use std::cell::RefCell;
use std::rc::Rc;

use compose_rust::runtime::{disposable_effect, launched_effect, mutable_state_of, on_dispose, remember_with_key, MutableState};
use ember_client::State;
use ember_client::wire::SessionRecord;

use crate::fs_watch::{Subscription, WatchKey};
use crate::ui::vcs::{fetch_snapshot, load_from, Load};

/// The session whose folder is the repository, from records most recently updated first: the
/// first non-archived one, else the first (the server's rule).
pub fn repo_session<'a>(records: impl IntoIterator<Item = &'a SessionRecord>) -> Option<&'a SessionRecord> {
    let all: Vec<&SessionRecord> = records.into_iter().collect();
    all.iter().find(|r| !r.archived).or(all.first()).copied()
}

/// `(session id, folder)` of `project`'s repository, from the client state.
pub fn repo_folder(state: &State, project: &str) -> Option<(String, String)> {
    let view = state.projects().into_iter().find(|p| p.name == project)?;
    let records: Vec<&SessionRecord> = view.sessions.iter().filter_map(|id| state.session(id).map(|s| s.record)).collect();
    repo_session(records).filter(|r| !r.cwd.is_empty()).map(|r| (r.id.clone(), r.cwd.clone()))
}

/// The tag a 버전 관리 view holds while a state-changing git call of `project` runs.
pub fn tag(project: &str) -> String {
    format!("vcs:{project}")
}

/// A state-changing git call of `project` starts: no watch refresh until [`call_ended`].
pub fn call_started(project: &str) {
    crate::fs_watch::hold(&tag(project));
}

/// The call ended; `reloaded` when it bumped the view's reload counter itself.
pub fn call_ended(project: &str, reloaded: bool) {
    crate::fs_watch::release(&tag(project), reloaded);
}

/// Subscribe the 버전 관리 view of `project` to its repository and write a fresh snapshot to
/// `load` after each change. Call once per composition of the view, in the same place.
pub fn watch_and_refresh(project: Option<&str>, load: &MutableState<Load>) {
    let live = crate::services::try_services().filter(|s| s.demo.is_none());
    let project = project.unwrap_or_default().to_string();
    let folder = match (live, project.is_empty()) {
        (Some(s), false) => s.client.read(|st| repo_folder(st, &project)),
        _ => None,
    };
    let session = folder.as_ref().map(|f| f.0.clone());
    let tick = remember_with_key(project.clone(), || mutable_state_of(0_u64));
    let computer = remember_with_key((project.clone(), session.clone()), || mutable_state_of(None::<String>));
    {
        let (session, computer) = (session.clone(), computer.clone());
        launched_effect(session.clone(), move || async move {
            let Some(sid) = session else { return };
            crate::services::run_shared(
                async move { crate::services::services().server.session_computer(&sid).await },
                move |r| match r {
                    Ok(c) => computer.set(Some(c.computer.id)),
                    Err(e) => tracing::info!("file watch of the repository is off, manual refresh only: {e}"),
                },
            );
        });
    }
    let handle = remember_with_key(project.clone(), || Rc::new(RefCell::new(None::<Subscription>)));
    let computer_id = computer.get();
    {
        let (handle, tick, project, folder) = (handle.clone(), tick.clone(), project.clone(), folder.clone().map(|f| f.1));
        disposable_effect((project.clone(), folder.clone(), computer_id.clone()), move || {
            if let (Some(folder), Some(computer)) = (folder, computer_id) {
                let key = WatchKey { computer, folder: folder.into(), recursive: true };
                let bump = tick.clone();
                *handle.borrow_mut() = crate::fs_watch::subscribe(key, &tag(&project), Box::new(move |n| bump.set(n)));
            }
            let stop = handle.clone();
            on_dispose(move || {
                // Dropping the subscription unsubscribes; the last view closes the socket.
                stop.borrow_mut().take();
            })
        });
    }
    let n = tick.get();
    let load = load.clone();
    launched_effect((project.clone(), n), move || async move {
        if n == 0 || project.is_empty() || crate::services::try_services().is_none() {
            return;
        }
        crate::services::run_shared(async move { load_from(fetch_snapshot(&project).await) }, move |result| load.set(result));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, archived: bool) -> SessionRecord {
        let mut r: SessionRecord = serde_json::from_value(serde_json::json!({
            "id": id, "project": "p", "agent": "claude-code", "cwd": format!("/r/{id}"),
            "status": "idle", "title": "", "created_at": 0, "updated_at": 0, "last_seq": 0
        }))
        .expect("record");
        r.archived = archived;
        r
    }

    #[test]
    fn fr_w4d_vcs_watches_the_folder_the_git_relay_uses() {
        let (a, b, c) = (rec("a", true), rec("b", false), rec("c", false));
        assert_eq!(repo_session([&a, &b, &c]).map(|r| r.id.as_str()), Some("b"));
        assert_eq!(repo_session([&a]).map(|r| r.id.as_str()), Some("a"), "all archived: the first");
        assert!(repo_session(std::iter::empty()).is_none());
    }

    #[test]
    fn fr_w4d_vcs_holds_by_project() {
        assert_eq!(tag("ember"), "vcs:ember");
        assert_ne!(tag("a"), tag("b"));
    }
}
