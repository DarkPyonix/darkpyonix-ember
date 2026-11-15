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
    if reloaded {
        reloads().bump(&tag(project));
    }
    crate::fs_watch::release(&tag(project), reloaded);
}

/// Run a state-changing git call of `project` under the watch guard: the hold starts now (before
/// the call is spawned) and ends when the call does. `reloads` says whether the caller reloads
/// the view after this answer; then the pending watch events are dropped, else they fire.
pub fn guarded<T, F>(project: &str, call: F, reloads: impl FnOnce(&T) -> bool + Send + 'static) -> impl std::future::Future<Output = T> + Send + 'static
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let project = project.to_string();
    guard_with(call, reloads, move || call_started(&project), {
        let project = project.clone();
        move |r| call_ended(&project, r)
    })
}

/// [`guarded`] for a call whose success reloads the view (the usual case).
pub fn guarded_ok<T, E, F>(project: &str, call: F) -> impl std::future::Future<Output = Result<T, E>> + Send + 'static
where
    T: Send + 'static,
    E: Send + 'static,
    F: std::future::Future<Output = Result<T, E>> + Send + 'static,
{
    guarded(project, call, |r: &Result<T, E>| r.is_ok())
}

/// The guard with its hold and release passed in, so tests can drive a [`crate::fs_watch`] core.
fn guard_with<T, F>(call: F, reloads: impl FnOnce(&T) -> bool, hold: impl FnOnce(), release: impl FnOnce(bool)) -> impl std::future::Future<Output = T>
where
    F: std::future::Future<Output = T>,
{
    hold();
    async move {
        let out = call.await;
        release(reloads(&out));
        out
    }
}

/// How many guarded calls of each tag ended with a reload. A watch refresh records the count
/// when it starts and drops its snapshot when the count moved meanwhile: the call's own reload
/// started later, so its snapshot is newer and must not be replaced by the older one.
#[derive(Default)]
pub struct Reloads(std::sync::Mutex<std::collections::HashMap<String, u64>>);

impl Reloads {
    pub fn bump(&self, tag: &str) {
        if let Ok(mut m) = self.0.lock() {
            *m.entry(tag.to_string()).or_default() += 1;
        }
    }

    pub fn current(&self, tag: &str) -> u64 {
        self.0.lock().map(|m| m.get(tag).copied().unwrap_or(0)).unwrap_or(0)
    }

    /// The refresh that started at `seen` may still write its snapshot.
    pub fn fresh(&self, tag: &str, seen: u64) -> bool {
        self.current(tag) == seen
    }
}

fn reloads() -> &'static Reloads {
    static R: std::sync::OnceLock<Reloads> = std::sync::OnceLock::new();
    R.get_or_init(Reloads::default)
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
        let seen = reloads().current(&tag(&project));
        let held = tag(&project);
        crate::services::run_shared(async move { load_from(fetch_snapshot(&project).await) }, move |result| {
            // A guarded call that reloaded since this refresh began has a newer snapshot coming.
            if reloads().fresh(&held, seen) {
                load.set(result);
            }
        });
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

    use crate::fs_watch::Core;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    fn core_with_view(now: Instant) -> Arc<Mutex<Core>> {
        Arc::new(Mutex::new(crate::fs_watch::tests_live(now, "p")))
    }

    fn guard(core: &Arc<Mutex<Core>>, now: Instant, ok: bool) -> impl std::future::Future<Output = Result<(), ()>> {
        let (h, r) = (core.clone(), core.clone());
        guard_with(async move { if ok { Ok(()) } else { Err(()) } }, |x: &Result<(), ()>| x.is_ok(), move || h.lock().unwrap().hold("p"), move |re| r.lock().unwrap().release("p", re, now))
    }

    #[tokio::test]
    async fn fr_w4d_vcs_the_guard_holds_for_the_call_and_releases_either_way() {
        let t0 = Instant::now();
        let core = core_with_view(t0);
        for ok in [true, false] {
            let call = guard(&core, t0, ok);
            assert!(core.lock().unwrap().is_held("p"), "held before the call runs");
            assert_eq!(call.await.is_ok(), ok);
            assert!(!core.lock().unwrap().is_held("p"), "released after ok={ok}");
        }
    }

    #[tokio::test]
    async fn fr_w4d_vcs_a_refresh_during_the_hold_fires_after_a_failure_only() {
        let t0 = Instant::now();
        let d = crate::fs_watch::DEBOUNCE;
        let core = core_with_view(t0);
        let call = guard(&core, t0, true);
        crate::fs_watch::tests_change(&mut core.lock().unwrap(), t0);
        assert!(core.lock().unwrap().poll(t0 + d * 3).0.is_empty(), "held");
        call.await.unwrap();
        assert!(core.lock().unwrap().poll(t0 + d * 10).0.is_empty(), "the success reloaded: dropped");
        let t1 = t0 + d * 11;
        let call = guard(&core, t1, false);
        crate::fs_watch::tests_change(&mut core.lock().unwrap(), t1);
        assert!(core.lock().unwrap().poll(t1 + d * 3).0.is_empty(), "held");
        assert!(call.await.is_err());
        assert_eq!(core.lock().unwrap().poll(t1 + d * 4).0.len(), 1, "the failure did not reload: it fires");
    }

    #[tokio::test]
    async fn fr_w4d_vcs_overlapping_calls_release_when_the_last_ends() {
        let t0 = Instant::now();
        let core = core_with_view(t0);
        let first = guard(&core, t0, false);
        let second = guard(&core, t0, false);
        first.await.unwrap_err();
        assert!(core.lock().unwrap().is_held("p"), "the second call still runs");
        second.await.unwrap_err();
        assert!(!core.lock().unwrap().is_held("p"));
    }

    #[test]
    fn fr_w4d_vcs_a_refresh_older_than_a_reload_is_dropped() {
        let r = Reloads::default();
        let seen = r.current("vcs:p");
        assert!(r.fresh("vcs:p", seen));
        r.bump("vcs:p");
        assert!(!r.fresh("vcs:p", seen), "a guarded call reloaded after the refresh began");
        assert!(r.fresh("vcs:p", r.current("vcs:p")));
        assert!(r.fresh("vcs:q", 0), "other projects are untouched");
    }
}
