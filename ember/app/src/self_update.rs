//! The installed app updates itself (SPEC `NFR-R2`, #880).
//!
//! Velopack installs and updates the desktop app. Three parts:
//!
//! - [`startup`]: Velopack's startup hook, the first thing the desktop `main` runs, so the
//!   install, update and uninstall hooks exit before any window (also in the `--editor`
//!   process). A downloaded update is not applied at start on its own: applying is the user's
//!   choice (NFR-R2 (5)).
//! - [`Updater`]: a small state machine over an [`UpdateBackend`]. The real backend is
//!   Velopack's `UpdateManager` (desktop only); the tests use a fake one. Every call on it
//!   blocks, so the app runs it on the runtime's blocking pool ([`request_check`]).
//! - [`state_text`] and the button rules the 설정 section reads (`ui::cr::self_update`).
//!
//! The feed is [`DEFAULT_FEED`] (GitHub Releases, `releases/latest/download`); Velopack's
//! `HttpSource` reads `releases.<channel>.json` and the packages under that base, for the
//! channel the installed package was built with. [`FEED_ENV`] overrides it with another URL
//! or a local directory.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Where the packages and `releases.<channel>.json` live: the newest GitHub release.
pub const DEFAULT_FEED: &str = "https://github.com/DarkPyonix/darkpyonix-ember/releases/latest/download";

/// Overrides [`DEFAULT_FEED`]: an `http(s)://` URL, or a local directory holding a feed.
pub const FEED_ENV: &str = "EMBER_UPDATE_FEED";

/// The version this binary was built as. The release job refuses a tag that differs from it,
/// so it is also the installed package's version.
pub const RUNNING_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where to look for updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    Url(String),
    Dir(PathBuf),
}

/// The feed for an [`FEED_ENV`] value: unset or blank is [`DEFAULT_FEED`], `http://` or
/// `https://` is a URL, anything else a directory.
pub fn feed_from(value: Option<&str>) -> Feed {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Feed::Url(DEFAULT_FEED.to_string()),
        Some(v) if v.starts_with("http://") || v.starts_with("https://") => Feed::Url(v.to_string()),
        Some(v) => Feed::Dir(PathBuf::from(v)),
    }
}

/// [`feed_from`] for the environment.
pub fn feed_from_env() -> Feed {
    feed_from(std::env::var(FEED_ENV).ok().as_deref())
}

/// Which step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Check,
    Download,
    Apply,
}

/// Where the update stands. Versions are the feed's strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    /// A build Velopack did not install (`cargo run`, the Store package, Android, iOS):
    /// nothing is checked.
    NotInstalled,
    /// Installed, not checked yet.
    Idle,
    Checking,
    /// No newer release; the installed version.
    UpToDate(String),
    /// A newer release was found; its download starts at once.
    Available(String),
    Downloading(String),
    /// Downloaded; applied when the user restarts with it.
    Ready(String),
    Failed { stage: Stage, reason: String },
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Nothing newer (or an empty feed); the installed version.
    UpToDate(String),
    /// A newer release to download.
    Available(String),
    /// A newer release already downloaded (an earlier run), waiting to be applied.
    Pending(String),
}

/// The update mechanism. Every call blocks. Velopack's `UpdateManager` on the desktop,
/// a fake in the tests.
pub trait UpdateBackend: Send + Sync + 'static {
    /// Reads the feed. Remembers what it found for [`UpdateBackend::download`] and
    /// [`UpdateBackend::apply_and_restart`].
    fn check(&self) -> Result<Found, String>;
    /// Downloads the release the last [`UpdateBackend::check`] found.
    fn download(&self) -> Result<(), String>;
    /// Applies the downloaded release and restarts the app. Does not return on success.
    fn apply_and_restart(&self) -> Result<(), String>;
}

/// The state machine. One check (with its download) runs at a time.
pub struct Updater {
    backend: Option<Arc<dyn UpdateBackend>>,
    state: Mutex<UpdateState>,
    busy: AtomicBool,
    on_change: Box<dyn Fn(&UpdateState) + Send + Sync>,
}

impl Updater {
    /// `backend` is `None` for a build Velopack did not install. `on_change` hears every new
    /// state, from whatever thread made it.
    pub fn new(backend: Option<Arc<dyn UpdateBackend>>, on_change: impl Fn(&UpdateState) + Send + Sync + 'static) -> Self {
        let state = if backend.is_some() { UpdateState::Idle } else { UpdateState::NotInstalled };
        Updater { backend, state: Mutex::new(state), busy: AtomicBool::new(false), on_change: Box::new(on_change) }
    }

    /// The current state.
    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, next: UpdateState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = next.clone();
        (self.on_change)(&next);
    }

    /// Claims the one check slot and shows [`UpdateState::Checking`]. False (and nothing
    /// changes) when the build is not installed, a check or download runs, or an update is
    /// already downloaded.
    pub fn begin_check(&self) -> bool {
        if self.backend.is_none() || matches!(self.state(), UpdateState::Ready(_)) {
            return false;
        }
        if self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        self.set(UpdateState::Checking);
        true
    }

    /// The rest of a check [`Updater::begin_check`] started: read the feed, download a newer
    /// release. Blocks. Releases the slot.
    pub fn finish_check(&self) {
        let Some(backend) = self.backend.clone() else { return };
        let next = match backend.check() {
            Err(reason) => UpdateState::Failed { stage: Stage::Check, reason },
            Ok(Found::UpToDate(v)) => UpdateState::UpToDate(v),
            Ok(Found::Pending(v)) => UpdateState::Ready(v),
            Ok(Found::Available(v)) => {
                self.set(UpdateState::Available(v.clone()));
                self.set(UpdateState::Downloading(v.clone()));
                match backend.download() {
                    Ok(()) => UpdateState::Ready(v),
                    Err(reason) => UpdateState::Failed { stage: Stage::Download, reason },
                }
            }
        };
        self.set(next);
        self.busy.store(false, Ordering::Release);
    }

    /// [`Updater::begin_check`] then [`Updater::finish_check`], blocking. False when the
    /// check was not started.
    pub fn check_now(&self) -> bool {
        if !self.begin_check() {
            return false;
        }
        self.finish_check();
        true
    }

    /// Applies a downloaded update and restarts. `before` runs first (the app saves its
    /// preferences and cache). Returns only when nothing was ready or applying failed.
    pub fn apply(&self, before: impl FnOnce()) -> bool {
        let Some(backend) = self.backend.clone() else { return false };
        if !matches!(self.state(), UpdateState::Ready(_)) {
            return false;
        }
        // A second click while the first one saves and applies does nothing.
        if self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        before();
        if let Err(reason) = backend.apply_and_restart() {
            self.set(UpdateState::Failed { stage: Stage::Apply, reason });
        }
        self.busy.store(false, Ordering::Release);
        false
    }
}

// ---- what 설정 shows ----------------------------------------------------------------------

/// The section's title and the line under it.
pub const TITLE: &str = "업데이트";
pub const SUBTITLE: &str = "설치된 Ember는 GitHub 릴리스에서 새 버전을 확인합니다.";
pub const VERSION_LABEL: &str = "현재 버전";
pub const CHECK_LABEL: &str = "업데이트 확인";
pub const RESTART_LABEL: &str = "업데이트 후 다시 시작";

/// The line for a state.
pub fn state_text(state: &UpdateState) -> String {
    match state {
        UpdateState::NotInstalled => {
            "설치 프로그램으로 설치한 앱이 아니므로 업데이트를 확인하지 않습니다.".to_string()
        }
        UpdateState::Idle => "아직 업데이트를 확인하지 않았습니다.".to_string(),
        UpdateState::Checking => "업데이트를 확인하는 중입니다.".to_string(),
        UpdateState::UpToDate(v) => format!("최신 버전을 사용하고 있습니다 ({v})."),
        UpdateState::Available(v) => format!("새 버전 {v}을(를) 찾았습니다."),
        UpdateState::Downloading(v) => format!("새 버전 {v}을(를) 내려받는 중입니다."),
        UpdateState::Ready(v) => format!("새 버전 {v}을(를) 내려받았습니다. 다시 시작하면 적용됩니다."),
        UpdateState::Failed { stage: Stage::Check, reason } => format!("업데이트를 확인하지 못했습니다: {reason}"),
        UpdateState::Failed { stage: Stage::Download, reason } => format!("새 버전을 내려받지 못했습니다: {reason}"),
        UpdateState::Failed { stage: Stage::Apply, reason } => {
            format!("업데이트를 적용하지 못했습니다: {reason}. 앱을 닫았다가 다시 여십시오.")
        }
    }
}

/// 업데이트 확인 is offered unless the build is not installed, a check or download runs, or
/// an update is ready.
pub fn can_check(state: &UpdateState) -> bool {
    !matches!(
        state,
        UpdateState::NotInstalled
            | UpdateState::Checking
            | UpdateState::Available(_)
            | UpdateState::Downloading(_)
            | UpdateState::Ready(_)
    )
}

/// 업데이트 후 다시 시작 shows only with a downloaded update.
pub fn can_restart(state: &UpdateState) -> bool {
    matches!(state, UpdateState::Ready(_))
}

// ---- the app's one updater -------------------------------------------------------------------

static UPDATER: OnceLock<Arc<Updater>> = OnceLock::new();

/// The installed updater, once [`start`] ran (the main shell process only).
pub fn current() -> Option<Arc<Updater>> {
    UPDATER.get().cloned()
}

/// Velopack's startup hook (NFR-R2 (1)): the first thing the desktop `main` runs. On an
/// install, update or uninstall hook it exits the process; otherwise it returns at once, also
/// for a build Velopack did not install.
pub fn startup() {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    velopack::VelopackApp::build().set_auto_apply_on_startup(false).run();
}

/// The real backend for `feed`, or `None` for a build Velopack did not install (and on
/// Android and iOS). Another error opening it is logged and also reads as `None`.
pub fn open_backend(feed: &Feed) -> Option<Arc<dyn UpdateBackend>> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        match velopack_backend::VelopackBackend::open(feed) {
            Ok(b) => Some(Arc::new(b)),
            Err(e) => {
                tracing::info!("self-update off: {e}");
                None
            }
        }
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = feed;
        None
    }
}

/// Installs the app's updater with `backend` and checks once on `rt`'s blocking pool
/// (NFR-R2 (3)); `on_change` hears every state. A second call does nothing.
pub fn start(
    rt: &tokio::runtime::Handle,
    backend: Option<Arc<dyn UpdateBackend>>,
    on_change: impl Fn(&UpdateState) + Send + Sync + 'static,
) {
    let updater = Arc::new(Updater::new(backend, on_change));
    (updater.on_change)(&updater.state());
    if UPDATER.set(updater).is_err() {
        return;
    }
    request_check(rt);
}

/// Starts a check on `rt`'s blocking pool, never on the UI thread. Ignored while one runs or
/// before [`start`].
pub fn request_check(rt: &tokio::runtime::Handle) -> bool {
    let Some(updater) = current() else { return false };
    if !updater.begin_check() {
        return false;
    }
    rt.spawn_blocking(move || updater.finish_check());
    true
}

/// Applies a downloaded update on its own thread (not a runtime thread, because `before`
/// may block on the runtime) and restarts the app.
pub fn request_apply(before: impl FnOnce() + Send + 'static) {
    let Some(updater) = current() else { return };
    std::thread::spawn(move || {
        updater.apply(before);
    });
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod velopack_backend {
    //! [`UpdateBackend`] over Velopack's `UpdateManager` (velopack 1.2.161). Its calls are
    //! blocking (ureq); the app runs them off the UI thread.

    use std::sync::Mutex;

    use velopack::{sources, UpdateCheck, UpdateInfo, UpdateManager, VelopackAsset};

    use super::{Feed, Found, UpdateBackend};

    pub struct VelopackBackend {
        um: UpdateManager,
        /// What the last check found, for the download.
        found: Mutex<Option<UpdateInfo>>,
        /// The downloaded (or already pending) release, for the apply.
        ready: Mutex<Option<VelopackAsset>>,
    }

    impl VelopackBackend {
        /// `Err` with Velopack's words when the app is not installed by Velopack
        /// (`Error::NotInstalled`) or the manager cannot start.
        pub fn open(feed: &Feed) -> Result<Self, String> {
            let um = match feed {
                Feed::Url(url) => UpdateManager::new(sources::HttpSource::new(url), None, None),
                Feed::Dir(dir) => UpdateManager::new(sources::FileSource::new(dir), None, None),
            }
            .map_err(|e| e.to_string())?;
            Ok(VelopackBackend { um, found: Mutex::new(None), ready: Mutex::new(None) })
        }
    }

    impl UpdateBackend for VelopackBackend {
        fn check(&self) -> Result<Found, String> {
            if let Some(asset) = self.um.get_update_pending_restart() {
                let v = asset.Version.clone();
                *self.ready.lock().unwrap_or_else(|e| e.into_inner()) = Some(asset);
                return Ok(Found::Pending(v));
            }
            match self.um.check_for_updates().map_err(|e| e.to_string())? {
                UpdateCheck::RemoteIsEmpty | UpdateCheck::NoUpdateAvailable => {
                    Ok(Found::UpToDate(self.um.get_current_version_as_string()))
                }
                UpdateCheck::UpdateAvailable(info) => {
                    let v = info.TargetFullRelease.Version.clone();
                    *self.found.lock().unwrap_or_else(|e| e.into_inner()) = Some(*info);
                    Ok(Found::Available(v))
                }
            }
        }

        fn download(&self) -> Result<(), String> {
            let info = self
                .found
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .ok_or_else(|| "내려받을 새 버전이 없습니다".to_string())?;
            self.um.download_updates(&info, None).map_err(|e| e.to_string())?;
            *self.ready.lock().unwrap_or_else(|e| e.into_inner()) = Some(info.TargetFullRelease);
            Ok(())
        }

        fn apply_and_restart(&self) -> Result<(), String> {
            let asset = self
                .ready
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| "적용할 새 버전이 없습니다".to_string())?;
            self.um.apply_updates_and_restart(&asset).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A backend that answers from a script. `gate`, when set, holds `check` until the test
    /// sends on it.
    struct Fake {
        check: Result<Found, String>,
        download: Result<(), String>,
        apply: Result<(), String>,
        gate: Mutex<Option<mpsc::Receiver<()>>>,
        entered: Mutex<Option<mpsc::Sender<()>>>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl Fake {
        fn new(check: Result<Found, String>, download: Result<(), String>) -> Self {
            Fake {
                check,
                download,
                apply: Err("테스트".into()),
                gate: Mutex::new(None),
                entered: Mutex::new(None),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl UpdateBackend for Fake {
        fn check(&self) -> Result<Found, String> {
            self.calls.lock().unwrap().push("check");
            if let Some(tx) = self.entered.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = self.gate.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            self.check.clone()
        }
        fn download(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("download");
            self.download.clone()
        }
        fn apply_and_restart(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("apply");
            self.apply.clone()
        }
    }

    /// An updater over `fake` that records every state it reports.
    fn updater(fake: Option<Arc<Fake>>) -> (Updater, Arc<Mutex<Vec<UpdateState>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let backend = fake.map(|f| f as Arc<dyn UpdateBackend>);
        (Updater::new(backend, move |s| log.lock().unwrap().push(s.clone())), seen)
    }

    #[test]
    fn nfr_r2_a_build_not_installed_checks_nothing() {
        let (u, seen) = updater(None);
        assert_eq!(u.state(), UpdateState::NotInstalled);
        assert!(!u.check_now());
        assert!(!u.apply(|| panic!("nothing to apply")));
        assert_eq!(u.state(), UpdateState::NotInstalled);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn nfr_r2_up_to_date() {
        let fake = Arc::new(Fake::new(Ok(Found::UpToDate("0.1.0".into())), Ok(())));
        let (u, seen) = updater(Some(fake.clone()));
        assert_eq!(u.state(), UpdateState::Idle);
        assert!(u.check_now());
        assert_eq!(u.state(), UpdateState::UpToDate("0.1.0".into()));
        assert_eq!(*seen.lock().unwrap(), vec![UpdateState::Checking, UpdateState::UpToDate("0.1.0".into())]);
        assert_eq!(fake.calls(), vec!["check"]);
        assert!(u.check_now(), "a later check runs again");
    }

    #[test]
    fn nfr_r2_an_update_is_found_then_downloaded() {
        let fake = Arc::new(Fake::new(Ok(Found::Available("0.2.0".into())), Ok(())));
        let (u, seen) = updater(Some(fake.clone()));
        assert!(u.check_now());
        let v = || "0.2.0".to_string();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![UpdateState::Checking, UpdateState::Available(v()), UpdateState::Downloading(v()), UpdateState::Ready(v())]
        );
        assert_eq!(fake.calls(), vec!["check", "download"]);
        assert!(!u.check_now(), "no check while an update waits to be applied");
    }

    #[test]
    fn nfr_r2_an_update_downloaded_earlier_is_ready_without_a_download() {
        let fake = Arc::new(Fake::new(Ok(Found::Pending("0.2.0".into())), Ok(())));
        let (u, _) = updater(Some(fake.clone()));
        assert!(u.check_now());
        assert_eq!(u.state(), UpdateState::Ready("0.2.0".into()));
        assert_eq!(fake.calls(), vec!["check"]);
    }

    #[test]
    fn nfr_r2_a_failed_check_is_shown_and_can_be_retried() {
        let fake = Arc::new(Fake::new(Err("Network error".into()), Ok(())));
        let (u, _) = updater(Some(fake.clone()));
        assert!(u.check_now());
        assert_eq!(u.state(), UpdateState::Failed { stage: Stage::Check, reason: "Network error".into() });
        assert!(can_check(&u.state()));
        assert!(u.check_now());
        assert_eq!(fake.calls(), vec!["check", "check"]);
    }

    #[test]
    fn nfr_r2_a_failed_download_is_shown() {
        let fake = Arc::new(Fake::new(Ok(Found::Available("0.2.0".into())), Err("Checksum did not match".into())));
        let (u, _) = updater(Some(fake.clone()));
        assert!(u.check_now());
        assert_eq!(u.state(), UpdateState::Failed { stage: Stage::Download, reason: "Checksum did not match".into() });
        assert!(!u.apply(|| panic!("nothing downloaded")));
        assert_eq!(fake.calls(), vec!["check", "download"]);
    }

    #[test]
    fn nfr_r2_a_second_check_while_one_runs_is_ignored() {
        let fake = Arc::new(Fake::new(Ok(Found::UpToDate("0.1.0".into())), Ok(())));
        let (release, gate) = mpsc::channel();
        let (entered_tx, entered) = mpsc::channel();
        *fake.gate.lock().unwrap() = Some(gate);
        *fake.entered.lock().unwrap() = Some(entered_tx);
        let (u, _) = updater(Some(fake.clone()));
        let u = Arc::new(u);
        let first = {
            let u = u.clone();
            std::thread::spawn(move || u.check_now())
        };
        entered.recv().unwrap();
        assert_eq!(u.state(), UpdateState::Checking);
        assert!(!u.check_now(), "a check asked for during a check is ignored");
        assert!(!u.begin_check());
        release.send(()).unwrap();
        assert!(first.join().unwrap());
        assert_eq!(fake.calls(), vec!["check"]);
        assert_eq!(u.state(), UpdateState::UpToDate("0.1.0".into()));
    }

    #[test]
    fn nfr_r2_apply_saves_first_and_reports_a_failure() {
        let fake = Arc::new(Fake::new(Ok(Found::Available("0.2.0".into())), Ok(())));
        let (u, _) = updater(Some(fake.clone()));
        let saved = Arc::new(AtomicBool::new(false));
        let s = saved.clone();
        assert!(!u.apply(|| panic!("nothing ready before a check")));
        assert!(u.check_now());
        assert!(!u.apply(move || s.store(true, Ordering::SeqCst)));
        assert!(saved.load(Ordering::SeqCst), "prefs and cache are saved before applying");
        assert_eq!(fake.calls(), vec!["check", "download", "apply"]);
        assert_eq!(u.state(), UpdateState::Failed { stage: Stage::Apply, reason: "테스트".into() });
    }

    #[test]
    fn nfr_r2_the_feed_defaults_to_github_and_the_env_value_overrides_it() {
        assert_eq!(feed_from(None), Feed::Url(DEFAULT_FEED.to_string()));
        assert_eq!(feed_from(Some("  ")), Feed::Url(DEFAULT_FEED.to_string()));
        assert_eq!(feed_from(Some("https://example.test/feed")), Feed::Url("https://example.test/feed".into()));
        assert_eq!(feed_from(Some("http://127.0.0.1:8000/")), Feed::Url("http://127.0.0.1:8000/".into()));
        assert_eq!(feed_from(Some(" /srv/ember-feed ")), Feed::Dir(PathBuf::from("/srv/ember-feed")));
        assert_eq!(DEFAULT_FEED, "https://github.com/DarkPyonix/darkpyonix-ember/releases/latest/download");
        assert_eq!(FEED_ENV, "EMBER_UPDATE_FEED");
    }

    #[test]
    fn nfr_r2_settings_text_and_buttons_for_every_state() {
        let v = || "0.2.0".to_string();
        let rows = [
            (UpdateState::NotInstalled, "설치 프로그램으로 설치한 앱이 아니므로 업데이트를 확인하지 않습니다.", false, false),
            (UpdateState::Idle, "아직 업데이트를 확인하지 않았습니다.", true, false),
            (UpdateState::Checking, "업데이트를 확인하는 중입니다.", false, false),
            (UpdateState::UpToDate("0.1.0".into()), "최신 버전을 사용하고 있습니다 (0.1.0).", true, false),
            (UpdateState::Available(v()), "새 버전 0.2.0을(를) 찾았습니다.", false, false),
            (UpdateState::Downloading(v()), "새 버전 0.2.0을(를) 내려받는 중입니다.", false, false),
            (UpdateState::Ready(v()), "새 버전 0.2.0을(를) 내려받았습니다. 다시 시작하면 적용됩니다.", false, true),
            (UpdateState::Failed { stage: Stage::Check, reason: "x".into() }, "업데이트를 확인하지 못했습니다: x", true, false),
            (UpdateState::Failed { stage: Stage::Download, reason: "x".into() }, "새 버전을 내려받지 못했습니다: x", true, false),
            (
                UpdateState::Failed { stage: Stage::Apply, reason: "x".into() },
                "업데이트를 적용하지 못했습니다: x. 앱을 닫았다가 다시 여십시오.",
                true,
                false,
            ),
        ];
        for (state, text, check, restart) in rows {
            assert_eq!(state_text(&state), text, "{state:?}");
            assert_eq!(can_check(&state), check, "{state:?}");
            assert_eq!(can_restart(&state), restart, "{state:?}");
        }
        assert_eq!(RUNNING_VERSION, env!("CARGO_PKG_VERSION"));
    }
}
