//! The window size every screen lays out from (FR-L10.3).
//!
//! The toolkit reads 0 x 0, the compact class, until the renderer reports the window's size, and
//! a 1440 x 900 desktop window was captured still at 0 x 0 seconds after its first frame: the
//! shell drew the phone layout, sheets fell back to their unknown-height body and the channel
//! composer hit its floor (`.scratch/ui-audit-r2/REVIEW-r2.md`). A desktop window always has a
//! size, the one it was opened with, so a 0 x 0 reading is replaced here by the last size reported
//! in this run, else by that opened size. Android records no opened size (its window is the
//! screen), and neither do tests that build a Host by hand, so for them the reading is unchanged.
//!
//! [`current_window_size`] reads through `compose_rust::runtime::current_window_size`, so a
//! composable that calls it still runs again when the renderer reports a new size.

use std::cell::Cell;
use std::sync::OnceLock;

use compose_rust::{WindowHeightClass, WindowSize, WindowSizeClass};

/// The size the desktop window was opened with, in dp. Set once by the shell's launch.
static OPENED: OnceLock<(f32, f32)> = OnceLock::new();

thread_local! {
    /// The last size the renderer reported in this run, kept while the guard is active.
    static LAST: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// The toolkit reading last logged, so each change is logged once, not once per read.
    static LOGGED: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// A test's opened size, on the test's own thread, so parallel tests are not affected.
    #[cfg(test)]
    static TEST_OPENED: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
}

/// Records the size the desktop window is opened with. Called by the shell's launch; Android
/// does not call it.
pub fn record_opened(width_dp: f32, height_dp: f32) {
    let _ = OPENED.set((width_dp, height_dp));
}

#[cfg(not(test))]
fn opened() -> Option<(f32, f32)> {
    OPENED.get().copied()
}

#[cfg(test)]
fn opened() -> Option<(f32, f32)> {
    TEST_OPENED.with(Cell::get)
}

/// Whether the toolkit has a size from the renderer. Its default before any report is 0 x 0; no
/// real desktop window measures that.
pub fn is_reported(size: &WindowSize) -> bool {
    size.width_dp > 0.0 || size.height_dp > 0.0
}

/// A window size of `width_dp` x `height_dp` with the toolkit's classes (600 and 840 dp wide,
/// 480 and 900 dp tall) and the safe area of `like`.
fn sized_like(like: &WindowSize, width_dp: f32, height_dp: f32) -> WindowSize {
    WindowSize::new(
        width_dp,
        height_dp,
        WindowSizeClass::from_width_dp(width_dp),
        WindowHeightClass::from_height_dp(height_dp),
    )
    .with_safe_area(like.safe_area)
}

/// The size to lay out at, from the toolkit's reading, the last reported size and the opened
/// size. Pure: a reported size is used as it is; a 0 x 0 reading becomes the last reported size,
/// else the opened size; with neither it stays as read.
pub fn resolve(read: WindowSize, last: Option<(f32, f32)>, opened: Option<(f32, f32)>) -> WindowSize {
    if is_reported(&read) {
        return read;
    }
    match last.or(opened) {
        Some((w, h)) => sized_like(&read, w, h),
        None => read,
    }
}

fn resolved(read: WindowSize) -> WindowSize {
    let Some(opened) = opened() else {
        return read;
    };
    // One line per change of the toolkit's reading, for a live check of the missing report
    // (FR-L10.3): `0x0` lines that never turn into the window's size mean the renderer's report
    // did not arrive; a real size followed by `0x0` means a zero size was reported.
    let raw = (read.width_dp, read.height_dp);
    if LOGGED.with(|logged| logged.replace(Some(raw))) != Some(raw) {
        tracing::info!(width_dp = raw.0, height_dp = raw.1, reported = is_reported(&read), "window size read");
    }
    if is_reported(&read) {
        LAST.with(|last| last.set(Some((read.width_dp, read.height_dp))));
        return read;
    }
    resolve(read, LAST.with(Cell::get), Some(opened))
}

/// The window's size for layout, recorded as read like the toolkit's
/// `compose_rust::runtime::current_window_size`, with a 0 x 0 reading on a desktop window
/// replaced as [`resolve`] says.
pub fn current_window_size() -> WindowSize {
    resolved(compose_rust::runtime::current_window_size())
}

/// The same, as a plain read that subscribes to nothing, for a click handler.
pub fn window_size_untracked() -> WindowSize {
    resolved(compose_rust::window_size())
}

/// Sets this test thread's opened size (`None` turns the guard off) and forgets the last size.
#[cfg(test)]
pub fn set_opened_for_test(size: Option<(f32, f32)>) {
    TEST_OPENED.with(|opened| opened.set(size));
    LAST.with(|last| last.set(None));
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::runtime::Composable;
    use compose_rust::ui as cu;

    fn zero() -> WindowSize {
        WindowSize::default()
    }

    #[test]
    fn fr_l10_3_a_reported_size_is_used_as_it_is() {
        let read = WindowSize::new(390.0, 844.0, WindowSizeClass::Compact, WindowHeightClass::Medium);
        assert_eq!(resolve(read, Some((1440.0, 900.0)), Some((1440.0, 900.0))), read);
    }

    #[test]
    fn fr_l10_3_an_unreported_size_becomes_the_opened_size_with_its_classes() {
        let got = resolve(zero(), None, Some((1440.0, 900.0)));
        assert_eq!((got.width_dp, got.height_dp), (1440.0, 900.0));
        assert_eq!(got.class, WindowSizeClass::Expanded);
        assert_eq!(got.height_class, WindowHeightClass::Expanded);
        let got = resolve(zero(), None, Some((768.0, 1024.0)));
        assert_eq!(got.class, WindowSizeClass::Medium);
        let got = resolve(zero(), None, Some((390.0, 844.0)));
        assert_eq!(got.class, WindowSizeClass::Compact);
        assert_eq!(got.height_class, WindowHeightClass::Medium);
    }

    #[test]
    fn fr_l10_3_the_last_reported_size_wins_over_the_opened_size() {
        let got = resolve(zero(), Some((1024.0, 700.0)), Some((1440.0, 900.0)));
        assert_eq!((got.width_dp, got.height_dp), (1024.0, 700.0));
        assert_eq!(got.class, WindowSizeClass::Expanded);
    }

    #[test]
    fn fr_l10_3_without_an_opened_size_the_reading_is_unchanged() {
        assert_eq!(resolve(zero(), None, None), zero());
        assert!(!is_reported(&zero()));
    }

    #[Composable]
    fn size_probe() {
        let w = current_window_size();
        let class = match w.class {
            WindowSizeClass::Compact => "Compact",
            WindowSizeClass::Medium => "Medium",
            WindowSizeClass::Expanded => "Expanded",
        };
        cu::Text(format!("{}x{} {class}", w.width_dp, w.height_dp));
    }

    fn texts(batch: &[compose_rust::protocol::Mutation<'_>]) -> String {
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    #[test]
    fn fr_l10_3_a_composable_reads_the_opened_size_then_follows_reports() {
        use compose_rust::protocol::{decode_batch, HostEvent};
        use compose_rust::{EventPayload, Host, Recomposer};
        compose_rust::window::reset_window_size();
        set_opened_for_test(Some((1440.0, 900.0)));
        let mut host = Host::new(|| Box::new(Recomposer::new(size_probe)));
        let first = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        assert!(first.contains("1440x900 Expanded"), "nothing reported yet: {first}");
        let mut report = |width: f32, height: f32| {
            let event = HostEvent {
                node_id: 0,
                handler_id: 0,
                payload: EventPayload::WindowSizeChanged {
                    width_dp: width,
                    height_dp: height,
                    class: WindowSizeClass::from_width_dp(width),
                    height_class: WindowHeightClass::from_height_dp(height),
                    safe_area_top: 0.0,
                    safe_area_leading: 0.0,
                    safe_area_trailing: 0.0,
                },
            };
            let (bytes, _) = host.dispatch(event).expect("dispatch");
            texts(&decode_batch(bytes).expect("decode"))
        };
        assert!(report(390.0, 844.0).contains("390x844 Compact"));
        assert!(report(1024.0, 700.0).contains("1024x700 Expanded"));
        // A 0 x 0 report after a real one keeps the last real size.
        let after_zero = report(0.0, 0.0);
        assert!(!after_zero.contains("0x0"), "a 0 x 0 report must not reach the layout: {after_zero}");
        set_opened_for_test(None);
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l10_3_without_an_opened_size_a_composable_reads_zero() {
        use compose_rust::protocol::decode_batch;
        use compose_rust::{Host, Recomposer};
        compose_rust::window::reset_window_size();
        set_opened_for_test(None);
        let mut host = Host::new(|| Box::new(Recomposer::new(size_probe)));
        let first = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        assert!(first.contains("0x0 Compact"), "tests and Android keep the toolkit reading: {first}");
        compose_rust::window::reset_window_size();
    }
}
