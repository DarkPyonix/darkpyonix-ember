//! The terminal-state model of a persistent session (FR-P2, NFR-P1).
//!
//! Every byte the PTY produces is fed to an `alacritty_terminal` emulator so that a client that
//! attaches later gets the same terminal: a [`Screen::snapshot`] is a byte stream that, written to
//! a freshly reset terminal, reproduces the scrollback, the visible screen (primary and, if a TUI
//! is running, alternate), the cursor, the pen, the input modes (application cursor keys,
//! bracketed paste, mouse reporting, kitty keyboard flags, …) and the title. The live byte stream
//! continues exactly where the snapshot ends.
//!
//! **Why `alacritty_terminal`** (and not `vt100`): its grid exposes the scrollback history
//! (`history_size`, negative line indices) and lets us drop it (`clear_history`). That lets this
//! model *harvest* lines as they scroll off the top and store them compactly as SGR-encoded text
//! (≈ the size of the text itself) instead of as cell grids (24 bytes per cell — 10,000 lines × 80
//! columns would be ≈ 19 MB per session, far above NFR-P1's 2 MB). `vt100` keeps its scrollback
//! as full rows of 30+ byte cells with no way to drain it. alacritty is also the emulator behind
//! the Alacritty terminal and Zed, so its VT coverage is broad and maintained.
//!
//! Memory: the emulator keeps only the visible grids (plus alacritty's row cache, released by
//! [`Screen::compact`] when the session goes idle); scrollback is a ring of encoded lines bounded
//! by [`SCROLLBACK_LINES`] and [`SCROLLBACK_BYTES`].
//!
//! Known gaps of the snapshot: the scroll region (DECSTBM), origin mode, saved cursor (DECSC),
//! charsets, tab stops, hyperlinks (OSC 8) and underline colour are not reproduced; a TUI redraws
//! them itself on its next full repaint (any resize triggers one).

use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Grid, GridCell, Row};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, StdSyncHandler};

/// Logical lines of scrollback kept per session (FR-P2: at least 10,000).
pub const SCROLLBACK_LINES: usize = 10_000;
/// Byte cap on the encoded scrollback, so a session printing very long coloured lines stays
/// bounded. Typical output (≤ 150 bytes per line with colour) keeps all 10,000 lines within it.
pub const SCROLLBACK_BYTES: usize = 3 * 1024 * 1024;
/// History the emulator itself may hold between two harvests (one PTY read); only the newest
/// lines matter if a single read scrolls more than this.
const HARVEST_LIMIT: usize = SCROLLBACK_LINES;
/// A logical line longer than this (e.g. a progress bar without newlines) is cut.
const MAX_LINE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Default)]
struct Shared {
    title: Option<String>,
    /// Answers the emulator produced for terminal queries (DSR, DA, …).
    replies: Vec<u8>,
}

/// Collects the emulator's events. `send_event` takes `&self`, hence the mutex.
#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Shared>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let mut s = self.0.lock().unwrap();
        match event {
            Event::Title(t) => s.title = Some(t),
            Event::ResetTitle => s.title = None,
            Event::PtyWrite(text) => s.replies.extend_from_slice(text.as_bytes()),
            _ => {}
        }
    }
}

pub struct Screen {
    term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    listener: Listener,
    /// Complete logical lines that scrolled off the top, SGR-encoded, oldest first.
    scrollback: VecDeque<Box<[u8]>>,
    scrollback_bytes: usize,
    /// The start of a logical line whose rows wrapped and whose remainder is still on screen.
    partial: Vec<u8>,
    /// alacritty's row cache may hold freed rows; [`Screen::compact`] releases them.
    cache_dirty: bool,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = Size { cols: cols.max(2) as usize, rows: rows.max(1) as usize };
        let listener = Listener::default();
        let config = Config { scrolling_history: HARVEST_LIMIT, ..Config::default() };
        let term = Term::new(config, &size, listener.clone());
        Self {
            term,
            parser: Processor::new(),
            listener,
            scrollback: VecDeque::new(),
            scrollback_bytes: 0,
            partial: Vec::new(),
            cache_dirty: false,
        }
    }

    /// Feed PTY output.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        // A synchronized update (DECSET 2026) buffers inside the parser until it ends or times
        // out; nothing else drives the timeout here, so end it once it has expired.
        if self.parser.sync_timeout().sync_timeout().is_some_and(|t| t <= Instant::now()) {
            self.parser.stop_sync(&mut self.term);
        }
        self.harvest();
    }

    pub fn size(&self) -> (u16, u16) {
        (self.term.columns() as u16, self.term.screen_lines() as u16)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let size = Size { cols: cols.max(2) as usize, rows: rows.max(1) as usize };
        self.term.resize(size);
        // Shrinking the screen pushes rows into history.
        self.harvest();
    }

    pub fn title(&self) -> Option<String> {
        self.listener.0.lock().unwrap().title.clone()
    }

    /// Answers to terminal queries produced since the last call. The session writes them to the
    /// PTY only while no client is attached (attached clients answer queries themselves).
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.listener.0.lock().unwrap().replies)
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Approximate heap use of the encoded scrollback.
    pub fn scrollback_bytes(&self) -> usize {
        self.scrollback_bytes + self.partial.len()
    }

    /// Release the emulator's cached rows (call when the session has gone idle).
    pub fn compact(&mut self) {
        if self.cache_dirty && self.term.grid().history_size() == 0 {
            self.term.grid_mut().truncate();
            self.cache_dirty = false;
        }
        self.partial.shrink_to_fit();
    }

    /// Move the emulator's history into the encoded scrollback ring.
    fn harvest(&mut self) {
        let hist = self.term.grid().history_size();
        if hist == 0 {
            return;
        }
        let cols = self.term.columns();
        for i in (1..=hist).rev() {
            let wrapped;
            {
                let row = &self.term.grid()[Line(-(i as i32))];
                wrapped = row_wraps(row, cols);
                encode_row(row, cols, wrapped, &mut self.partial);
            }
            if !wrapped || self.partial.len() > MAX_LINE_BYTES {
                let line = std::mem::take(&mut self.partial);
                self.push_line(line);
            }
        }
        self.term.grid_mut().clear_history();
        self.cache_dirty = true;
    }

    fn push_line(&mut self, line: Vec<u8>) {
        self.scrollback_bytes += line.len();
        self.scrollback.push_back(line.into_boxed_slice());
        while self.scrollback.len() > SCROLLBACK_LINES || self.scrollback_bytes > SCROLLBACK_BYTES {
            match self.scrollback.pop_front() {
                Some(old) => self.scrollback_bytes -= old.len(),
                None => break,
            }
        }
    }

    /// The bytes that redraw this terminal on a reset client of the same size.
    pub fn snapshot(&mut self) -> Vec<u8> {
        if self.parser.sync_timeout().sync_timeout().is_some() {
            self.parser.stop_sync(&mut self.term);
            self.harvest();
        }
        let mut out = Vec::with_capacity(self.scrollback_bytes + self.partial.len() + 16 * 1024);
        // RIS: full reset, so the client starts from a known state.
        out.extend_from_slice(b"\x1bc");
        for line in &self.scrollback {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(&self.partial);

        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            // The primary screen is alacritty's private inactive grid while a TUI runs. Swap it
            // in to read it, then swap back and restore the alternate grid (swapping into the
            // alternate screen clears it, hence the clone; it is one screenful).
            let alt: Grid<Cell> = self.term.grid().clone();
            self.term.swap_alt();
            encode_screen_flow(self.term.grid(), &mut out);
            self.term.swap_alt();
            *self.term.grid_mut() = alt;
            out.extend_from_slice(b"\x1b[?1049h\x1b[H\x1b[2J");
            encode_screen_positioned(self.term.grid(), &mut out);
        } else {
            encode_screen_flow(self.term.grid(), &mut out);
        }
        self.encode_state(&mut out);
        out
    }

    /// Cursor, pen, modes and title.
    fn encode_state(&self, out: &mut Vec<u8>) {
        let mode = *self.term.mode();
        let cursor = &self.term.grid().cursor;
        let _ = write!(out, "\x1b[{};{}H", cursor.point.line.0 + 1, cursor.point.column.0 + 1);
        Pen::of(&cursor.template).write_sgr(out);

        let private = |out: &mut Vec<u8>, n: u16, on: bool| {
            let _ = write!(out, "\x1b[?{n}{}", if on { 'h' } else { 'l' });
        };
        if mode.contains(TermMode::APP_CURSOR) {
            private(out, 1, true);
        }
        if mode.contains(TermMode::APP_KEYPAD) {
            out.extend_from_slice(b"\x1b=");
        }
        if !mode.contains(TermMode::LINE_WRAP) {
            private(out, 7, false);
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            private(out, 25, false);
        }
        if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            private(out, 1000, true);
        }
        if mode.contains(TermMode::MOUSE_DRAG) {
            private(out, 1002, true);
        }
        if mode.contains(TermMode::MOUSE_MOTION) {
            private(out, 1003, true);
        }
        if mode.contains(TermMode::FOCUS_IN_OUT) {
            private(out, 1004, true);
        }
        if mode.contains(TermMode::UTF8_MOUSE) {
            private(out, 1005, true);
        }
        if mode.contains(TermMode::SGR_MOUSE) {
            private(out, 1006, true);
        }
        if mode.contains(TermMode::BRACKETED_PASTE) {
            private(out, 2004, true);
        }
        if mode.contains(TermMode::INSERT) {
            out.extend_from_slice(b"\x1b[4h");
        }
        if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
            out.extend_from_slice(b"\x1b[20h");
        }
        let mut kitty = 0u8;
        for (flag, bit) in [
            (TermMode::DISAMBIGUATE_ESC_CODES, 1),
            (TermMode::REPORT_EVENT_TYPES, 2),
            (TermMode::REPORT_ALTERNATE_KEYS, 4),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
            (TermMode::REPORT_ASSOCIATED_TEXT, 16),
        ] {
            if mode.contains(flag) {
                kitty |= bit;
            }
        }
        if kitty != 0 {
            let _ = write!(out, "\x1b[={kitty};1u");
        }
        let style = self.term.cursor_style();
        if style != Config::default().default_cursor_style {
            let n = match style.shape {
                CursorShape::Block | CursorShape::HollowBlock => 1,
                CursorShape::Underline => 3,
                CursorShape::Beam => 5,
                CursorShape::Hidden => 0,
            };
            if n != 0 {
                let _ = write!(out, "\x1b[{} q", if style.blinking { n } else { n + 1 });
            }
        }
        if let Some(title) = self.title() {
            let clean: String = title.chars().filter(|c| !c.is_control()).collect();
            let _ = write!(out, "\x1b]2;{clean}\x07");
        }
    }

    /// The visible screen as plain text (trailing blanks trimmed), for listings and agents.
    pub fn text(&self) -> String {
        let grid = self.term.grid();
        let cols = grid.columns();
        let mut out = String::new();
        for r in 0..grid.screen_lines() {
            let row = &grid[Line(r as i32)];
            let mut line = String::new();
            for c in 0..cols {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                line.push(if cell.c == '\t' { ' ' } else { cell.c });
                if let Some(zw) = cell.zerowidth() {
                    line.extend(zw.iter());
                }
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    /// Cursor position (row, column), 0-based.
    pub fn cursor(&self) -> (usize, usize) {
        let p = self.term.grid().cursor.point;
        (p.line.0.max(0) as usize, p.column.0)
    }

    pub fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn mode_bits(&self) -> u32 {
        self.term.mode().bits()
    }
}

fn row_wraps(row: &Row<Cell>, cols: usize) -> bool {
    cols > 0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE)
}

/// Rows top to bottom, joined with CRLF except where a row soft-wraps (then the next row
/// continues by autowrap, as it did originally). Written after the scrollback, so on a client of
/// the same height the rows land exactly on screen.
fn encode_screen_flow(grid: &Grid<Cell>, out: &mut Vec<u8>) {
    let cols = grid.columns();
    let rows = grid.screen_lines();
    for r in 0..rows {
        let row = &grid[Line(r as i32)];
        let wrapped = row_wraps(row, cols);
        encode_row(row, cols, wrapped, out);
        if r + 1 < rows && !wrapped {
            out.extend_from_slice(b"\r\n");
        }
    }
}

/// Each row at an absolute position (the alternate screen has no scrollback to push).
fn encode_screen_positioned(grid: &Grid<Cell>, out: &mut Vec<u8>) {
    let cols = grid.columns();
    for r in 0..grid.screen_lines() {
        let row = &grid[Line(r as i32)];
        let _ = write!(out, "\x1b[{};1H", r + 1);
        encode_row(row, cols, false, out);
    }
}

/// One row's cells as text with SGR changes. Trailing blank cells are dropped unless the row
/// soft-wraps (then every column is written so the client wraps at the same place).
fn encode_row(row: &Row<Cell>, cols: usize, keep_trailing: bool, out: &mut Vec<u8>) {
    let mut end = cols;
    if !keep_trailing {
        while end > 0 && row[Column(end - 1)].is_empty() {
            end -= 1;
        }
    }
    let mut pen = Pen::default();
    for c in 0..end {
        let cell = &row[Column(c)];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let want = Pen::of(cell);
        if want != pen {
            want.write_sgr(out);
            pen = want;
        }
        push_char(out, if cell.c == '\t' { ' ' } else { cell.c });
        if let Some(zw) = cell.zerowidth() {
            for ch in zw {
                push_char(out, *ch);
            }
        }
    }
    if pen != Pen::default() {
        out.extend_from_slice(b"\x1b[0m");
    }
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// The graphic rendition of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl Default for Pen {
    fn default() -> Self {
        Pen { fg: Color::Named(NamedColor::Foreground), bg: Color::Named(NamedColor::Background), flags: Flags::empty() }
    }
}

impl Pen {
    fn of(cell: &Cell) -> Self {
        Pen { fg: cell.fg, bg: cell.bg, flags: cell.flags & STYLE_FLAGS }
    }

    /// `CSI 0 ; … m`: always from a reset, so the result does not depend on the previous pen.
    fn write_sgr(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        let f = self.flags;
        for (flag, code) in [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ] {
            if f.contains(flag) {
                out.push(b';');
                out.extend_from_slice(code.as_bytes());
            }
        }
        if f.contains(Flags::DOUBLE_UNDERLINE) {
            out.extend_from_slice(b";21");
        } else if f.contains(Flags::UNDERCURL) {
            out.extend_from_slice(b";4:3");
        } else if f.contains(Flags::DOTTED_UNDERLINE) {
            out.extend_from_slice(b";4:4");
        } else if f.contains(Flags::DASHED_UNDERLINE) {
            out.extend_from_slice(b";4:5");
        } else if f.contains(Flags::UNDERLINE) {
            out.extend_from_slice(b";4");
        }
        write_color(out, self.fg, true);
        write_color(out, self.bg, false);
        out.push(b'm');
    }
}

fn write_color(out: &mut Vec<u8>, color: Color, fg: bool) {
    match color {
        Color::Named(n) => {
            let idx = n as usize;
            let base = match idx {
                0..=7 => Some((if fg { 30 } else { 40 }) + idx),
                8..=15 => Some((if fg { 90 } else { 100 }) + idx - 8),
                _ => {
                    let dim0 = NamedColor::DimBlack as usize;
                    let dim7 = NamedColor::DimWhite as usize;
                    (dim0..=dim7).contains(&idx).then(|| (if fg { 30 } else { 40 }) + idx - dim0)
                }
            };
            // Foreground/Background/Cursor and friends are the defaults: nothing to write.
            if let Some(code) = base {
                let _ = write!(out, ";{code}");
            }
        }
        Color::Indexed(i) => {
            let _ = write!(out, ";{};5;{i}", if fg { 38 } else { 48 });
        }
        Color::Spec(rgb) => {
            let _ = write!(out, ";{};2;{};{};{}", if fg { 38 } else { 48 }, rgb.r, rgb.g, rgb.b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Snapshot `a` and replay it into a fresh screen of the same size.
    fn replay(a: &mut Screen) -> Screen {
        let (cols, rows) = a.size();
        let snap = a.snapshot();
        let mut b = Screen::new(cols, rows);
        b.feed(&snap);
        b
    }

    #[test]
    fn plain_screen_and_cursor_round_trip() {
        let mut a = Screen::new(40, 6);
        a.feed(b"$ echo hi\r\nhi\r\n$ partial");
        let b = replay(&mut a);
        assert_eq!(a.text(), b.text());
        assert_eq!(b.cursor(), (2, 9));
        assert!(b.text().starts_with("$ echo hi\nhi\n$ partial\n"));
    }

    #[test]
    fn scrollback_keeps_the_newest_10000_lines_compactly() {
        let mut a = Screen::new(80, 24);
        for i in 0..12_000 {
            a.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(a.scrollback_len(), SCROLLBACK_LINES);
        // ≈ 10 bytes per line, nowhere near the cell-grid cost.
        assert!(a.scrollback_bytes() < 200 * 1024, "{}", a.scrollback_bytes());
        let snap = String::from_utf8(a.snapshot()).unwrap();
        // 12,000 lines, 23 still on screen (the cursor row is empty): the oldest kept is
        // 12,000 - 23 - 10,000.
        assert!(snap.contains("\r\nline 1977\r\n") || snap.starts_with("\x1bcline 1977\r\n"), "oldest line missing");
        assert!(!snap.contains("line 1976\r\n"));
        assert!(snap.contains("line 11999"));
        a.compact();
    }

    #[test]
    fn soft_wrapped_lines_are_stored_as_one_logical_line() {
        let mut a = Screen::new(10, 3);
        a.feed(b"abcdefghijKLMNOPQRSTuv\r\nx\r\ny\r\nz");
        // The 22-char line wrapped over three rows and has scrolled off entirely.
        assert_eq!(a.scrollback_len(), 1);
        let snap = String::from_utf8(a.snapshot()).unwrap();
        assert!(snap.contains("abcdefghijKLMNOPQRSTuv\r\n"), "{snap:?}");
    }

    #[test]
    fn colours_attributes_and_wide_characters_round_trip() {
        let mut a = Screen::new(30, 4);
        a.feed("\x1b[1;31mred\x1b[0m \x1b[38;5;123mi\x1b[48;2;1;2;3mrgb\x1b[0m 한글 é\u{301}\r\n".as_bytes());
        let b = replay(&mut a);
        assert_eq!(a.text(), b.text());
        let ga = a.term.grid();
        let gb = b.term.grid();
        for c in 0..30 {
            let (x, y) = (&ga[Line(0)][Column(c)], &gb[Line(0)][Column(c)]);
            assert_eq!((x.c, x.fg, x.bg, x.flags & STYLE_FLAGS), (y.c, y.fg, y.bg, y.flags & STYLE_FLAGS), "column {c}");
        }
    }

    #[test]
    fn alternate_screen_and_primary_screen_are_both_restored() {
        let mut a = Screen::new(20, 5);
        a.feed(b"$ htop\r\n");
        a.feed(b"\x1b[?1049h\x1b[H\x1b[2J\x1b[3;4HTUI here\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
        let mut b = replay(&mut a);
        assert!(b.alt_screen());
        assert_eq!(a.text(), b.text());
        assert!(b.text().lines().nth(2).unwrap().contains("TUI here"));
        assert_eq!(a.mode_bits() & mode_mask(), b.mode_bits() & mode_mask());
        // Snapshotting did not disturb the source.
        assert!(a.alt_screen());
        assert!(a.text().contains("TUI here"));
        // Leaving the TUI shows the same primary screen on both.
        a.feed(b"\x1b[?1049l");
        b.feed(b"\x1b[?1049l");
        assert_eq!(a.text(), b.text());
        assert!(b.text().starts_with("$ htop\n"));
    }

    fn mode_mask() -> u32 {
        (TermMode::APP_CURSOR
            | TermMode::BRACKETED_PASTE
            | TermMode::MOUSE_REPORT_CLICK
            | TermMode::SGR_MOUSE
            | TermMode::ALT_SCREEN
            | TermMode::SHOW_CURSOR)
            .bits()
    }

    #[test]
    fn title_and_query_replies_are_captured() {
        let mut a = Screen::new(20, 5);
        a.feed(b"\x1b]2;my title\x07\x1b[6n");
        assert_eq!(a.title().as_deref(), Some("my title"));
        assert_eq!(a.take_replies(), b"\x1b[1;1R");
        assert!(a.take_replies().is_empty());
        let snap = String::from_utf8(a.snapshot()).unwrap();
        assert!(snap.contains("\x1b]2;my title\x07"));
    }

    #[test]
    fn shrinking_the_screen_moves_rows_into_scrollback() {
        let mut a = Screen::new(20, 6);
        a.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
        a.resize(20, 3);
        assert_eq!(a.size(), (20, 3));
        assert_eq!(a.scrollback_len(), 3);
        assert!(a.text().starts_with("4\n5\n6\n"));
    }
}
