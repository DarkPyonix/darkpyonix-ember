//! Fitting the editor's terminal PTY to its panel (FR-L31.20, #334).
//!
//! Code-OSS sizes the PTY from the xterm element: its fit addon divides the element's inner size by
//! the renderer's cell size, keeps at least 2 columns and 1 row, and the terminal instance debounces
//! resizes before it tells the pty host. This module does the same arithmetic and the debounce as
//! plain functions, so the panel only measures and calls [`TerminalHandle::resize`].
//!
//! [`TerminalHandle::resize`]: crate::terminal_stream::TerminalHandle::resize

use std::time::{Duration, Instant};

/// The width and height in pixels of one terminal cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellSize {
    pub width: f32,
    pub height: f32,
}

/// The `.ed-term` cell: `--code-fs` 13 px in `--mono` (JetBrains Mono and the system monospace
/// fallbacks advance 0.6 em) on `--code-lh` 22 px lines (`assets/editor/editor.css`).
// TODO: measure the advance from the font the renderer actually uses once dioxus-compose reports
// text metrics; a fixed 0.6 em is a stand-in (dioxus-compose#196 can also drop the font family).
pub const PANEL_CELL: CellSize = CellSize { width: 13.0 * 0.6, height: 22.0 };

/// The `.ed-term` padding, `var(--sp3) var(--sp4)`: (left plus right, top plus bottom).
pub const PANEL_PADDING: (f32, f32) = (16.0 * 2.0, 12.0 * 2.0);

/// xterm's fit addon never goes below these.
pub const MIN_COLS: u16 = 2;
pub const MIN_ROWS: u16 = 1;

/// How long the panel size must stay the same before the PTY is told (Code-OSS waits 100 ms too).
pub const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);


/// Columns and rows for a panel of `width` by `height` pixels (border box, padding included).
/// `None` while the panel has no size (hidden, not laid out yet) or the cell is degenerate, so a
/// hidden panel never shrinks the PTY.
pub fn fit(width: f32, height: f32, cell: CellSize, padding: (f32, f32)) -> Option<(u16, u16)> {
    let ok = |v: f32| v.is_finite() && v > 0.0;
    if !ok(width) || !ok(height) || !ok(cell.width) || !ok(cell.height) {
        return None;
    }
    let inner_w = (width - padding.0).max(0.0);
    let inner_h = (height - padding.1).max(0.0);
    let cols = ((inner_w / cell.width).floor() as u32).clamp(MIN_COLS as u32, u16::MAX as u32) as u16;
    let rows = ((inner_h / cell.height).floor() as u32).clamp(MIN_ROWS as u32, u16::MAX as u32) as u16;
    Some((cols, rows))
}

/// Holds back panel sizes until they settle. The first size goes out at once (the terminal opened);
/// after that a size goes out only when it has not changed for [`RESIZE_DEBOUNCE`].
#[derive(Debug, Default)]
pub struct ResizeDebouncer {
    sent: Option<(u16, u16)>,
    pending: Option<((u16, u16), Instant)>,
}

impl ResizeDebouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The size the panel has now. Returns the size to send when it goes out at once.
    pub fn observe(&mut self, size: (u16, u16), now: Instant) -> Option<(u16, u16)> {
        let Some(sent) = self.sent else {
            self.sent = Some(size);
            self.pending = None;
            return Some(size);
        };
        if size == sent {
            self.pending = None;
            return None;
        }
        match self.pending {
            Some((p, _)) if p == size => {}
            _ => self.pending = Some((size, now)),
        }
        None
    }

    /// The pending size, once it has held for [`RESIZE_DEBOUNCE`].
    pub fn poll(&mut self, now: Instant) -> Option<(u16, u16)> {
        let (size, since) = self.pending?;
        if now.duration_since(since) < RESIZE_DEBOUNCE {
            return None;
        }
        self.pending = None;
        self.sent = Some(size);
        Some(size)
    }

    /// The last size let through, which the PTY should have.
    pub fn settled(&self) -> Option<(u16, u16)> {
        self.sent
    }
}

/// What one render of the panel does with its measured size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// Columns and rows to give the PTY now.
    pub send: Option<(u16, u16)>,
    /// Render again after this long, so a held-back size goes out without other activity.
    pub wake_after: Option<Duration>,
}

/// The panel's fit state across renders: measure, debounce, and say when to look again.
#[derive(Debug, Default)]
pub struct PanelFit {
    debounce: ResizeDebouncer,
}

impl PanelFit {
    /// `rect` is the panel's border box from the last layout (`None` or zero while hidden).
    /// `send` is the settled size whenever there is one: [`TerminalHandle::resize`] drops a size
    /// it already sent, and a handle started again after 다시 연결 gets the size too.
    ///
    /// [`TerminalHandle::resize`]: crate::terminal_stream::TerminalHandle::resize
    pub fn step(&mut self, rect: Option<(f32, f32)>, now: Instant) -> Step {
        if let Some(size) = rect.and_then(|(w, h)| fit(w, h, PANEL_CELL, PANEL_PADDING)) {
            self.debounce.observe(size, now);
        }
        self.debounce.poll(now);
        let wake_after = self.debounce.pending.map(|(_, since)| {
            RESIZE_DEBOUNCE.saturating_sub(now.duration_since(since)) + Duration::from_millis(5)
        });
        Step { send: self.debounce.settled(), wake_after }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_20_cols_and_rows_from_panel_and_cell_size() {
        let cell = CellSize { width: 8.0, height: 20.0 };
        assert_eq!(fit(800.0, 400.0, cell, (0.0, 0.0)), Some((100, 20)));
        // Partial cells are dropped, padding comes off first.
        assert_eq!(fit(839.0, 419.0, cell, (32.0, 24.0)), Some((100, 19)));
        // The panel's own cell: 7.8 x 22 inside 16/12 px padding.
        assert_eq!(fit(1000.0, 300.0, PANEL_CELL, PANEL_PADDING), Some((124, 12)));
    }

    #[test]
    fn fr_l31_20_fit_keeps_xterm_minimum_and_skips_a_hidden_panel() {
        let cell = CellSize { width: 8.0, height: 20.0 };
        assert_eq!(fit(10.0, 5.0, cell, (32.0, 24.0)), Some((MIN_COLS, MIN_ROWS)));
        assert_eq!(fit(0.0, 400.0, cell, (0.0, 0.0)), None);
        assert_eq!(fit(800.0, 0.0, cell, (0.0, 0.0)), None);
        assert_eq!(fit(800.0, f32::NAN, cell, (0.0, 0.0)), None);
        assert_eq!(fit(800.0, 400.0, CellSize { width: 0.0, height: 20.0 }, (0.0, 0.0)), None);
    }

    #[test]
    fn fr_l31_20_first_size_goes_out_at_once() {
        let t = Instant::now();
        let mut d = ResizeDebouncer::new();
        assert_eq!(d.observe((80, 24), t), Some((80, 24)));
        assert_eq!(d.settled(), Some((80, 24)));
        assert_eq!(d.observe((80, 24), t), None);
        assert_eq!(d.poll(t + RESIZE_DEBOUNCE), None);
    }

    #[test]
    fn fr_l31_20_resize_waits_until_the_size_settles() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut d = ResizeDebouncer::new();
        d.observe((80, 24), t);
        // A drag: every new size restarts the wait.
        assert_eq!(d.observe((90, 24), t + ms(10)), None);
        assert_eq!(d.observe((100, 24), t + ms(60)), None);
        assert_eq!(d.poll(t + ms(150)), None);
        assert_eq!(d.observe((100, 24), t + ms(150)), None);
        assert_eq!(d.poll(t + ms(160)), Some((100, 24)));
        assert_eq!(d.settled(), Some((100, 24)));
        assert_eq!(d.poll(t + ms(500)), None);
    }

    #[test]
    fn fr_l31_20_returning_to_the_sent_size_cancels_the_pending_one() {
        let t = Instant::now();
        let mut d = ResizeDebouncer::new();
        d.observe((80, 24), t);
        d.observe((120, 30), t);
        assert_eq!(d.observe((80, 24), t + Duration::from_millis(20)), None);
        assert_eq!(d.poll(t + Duration::from_secs(1)), None);
        assert_eq!(d.settled(), Some((80, 24)));
    }

    #[test]
    fn fr_l31_20_panel_step_sends_on_open_and_wakes_for_a_held_size() {
        let t = Instant::now();
        let mut f = PanelFit::default();
        // Not laid out yet: nothing to send, nothing to wait for.
        assert_eq!(f.step(None, t), Step { send: None, wake_after: None });
        let open = f.step(Some((1000.0, 300.0)), t);
        assert_eq!(open, Step { send: Some((124, 12)), wake_after: None });
        // The panel grows: the old size stays until the new one settles, and a wake is asked for.
        let grow = f.step(Some((1000.0, 410.0)), t + Duration::from_millis(10));
        assert_eq!(grow.send, Some((124, 12)));
        assert_eq!(grow.wake_after, Some(RESIZE_DEBOUNCE + Duration::from_millis(5)));
        let hidden = f.step(Some((0.0, 0.0)), t + Duration::from_millis(120));
        assert_eq!(hidden, Step { send: Some((124, 17)), wake_after: None });
    }

    #[test]
    fn fr_l31_20_fitted_size_goes_out_as_the_node_resize_message() {
        let (cols, rows) = fit(1000.0, 300.0, PANEL_CELL, PANEL_PADDING).unwrap();
        let frame: serde_json::Value = serde_json::from_str(&crate::terminal_stream::resize_frame(cols, rows)).unwrap();
        // `TermInput::Resize { size: PtySize }` in ember/node/src/proto.rs, tagged by `type`.
        assert_eq!(frame, serde_json::json!({ "type": "resize", "size": { "rows": 12, "cols": 124 } }));
    }
}
