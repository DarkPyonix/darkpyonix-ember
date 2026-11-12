//! Blame of the cursor line in the Ember editor window (FR-L31.8, #747).
//!
//! Code-OSS's git extension (1.139) shows who last changed the cursor line in the status bar,
//! as `${authorName} (${authorDateAgo})` with a hover that names the author, the date, the
//! commit and its summary, and as an end-of-line hint. Ember draws the status bar item; the
//! end-of-line hint needs a decoration kind the `CodeEditor` widget does not have (compose-rust
//! v0.0.6 has four: underline, code lens, hover anchor and ghost text, and ghost text is a
//! suggestion that Tab inserts), so it is not drawn.
//!
//! The answer comes from the project's git relay (`git/blame`, FR-G4): one line per call, the
//! editor's text sent along while the buffer has unsaved edits, so the line numbers are the
//! buffer's. The text goes with `\n` breaks; the node gives it the saved file's CRLF when the
//! file has them. A cursor move waits [`DEBOUNCE_MS`] before asking, answers are kept per file, line
//! and buffer version, and every status read of the window (after a save, a commit, a stage)
//! drops them.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
use serde::Deserialize;
use serde_json::json;

/// How long the cursor rests on a line before its blame is asked for.
pub const DEBOUNCE_MS: u64 = 250;
/// Answers kept before the cache starts over.
pub const CACHE_LIMIT: usize = 512;
/// Code-OSS's "Not Committed Yet".
pub const NOT_COMMITTED: &str = "아직 커밋하지 않음";

/// The commit that last changed a line, as `git/blame` answers it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BlameCommit {
    pub sha: String,
    pub author: String,
    #[serde(default)]
    pub email: String,
    /// Author time, seconds since the epoch.
    pub time: i64,
    #[serde(default)]
    pub summary: String,
}

/// One line's blame; `commit` is `None` when no commit holds the line yet.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BlameLine {
    pub line: u32,
    pub commit: Option<BlameCommit>,
}

/// Code-OSS's `fromNow(date, true, true)` in Korean: under 30 seconds "지금", then seconds,
/// minutes, hours, days, weeks (under 30 days), months (30 days, under 365) and years, "전"
/// for the past and "후" for a time ahead of this clock. The 버전 관리 screen's
/// `vcs::relative_time` has no weeks; this one follows Code-OSS because FR-L31 matches it.
pub fn ago_text(now: i64, then: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = MINUTE * 60;
    const DAY: i64 = HOUR * 24;
    const WEEK: i64 = DAY * 7;
    const MONTH: i64 = DAY * 30;
    const YEAR: i64 = DAY * 365;
    let seconds = now - then;
    if seconds.abs() < 30 {
        return "지금".to_string();
    }
    let (s, suffix) = if seconds < 0 { (-seconds, "후") } else { (seconds, "전") };
    let amount = if s < MINUTE {
        format!("{s}초")
    } else if s < HOUR {
        format!("{}분", s / MINUTE)
    } else if s < DAY {
        format!("{}시간", s / HOUR)
    } else if s < WEEK {
        format!("{}일", s / DAY)
    } else if s < MONTH {
        format!("{}주", s / WEEK)
    } else if s < YEAR {
        format!("{}개월", s / MONTH)
    } else {
        format!("{}년", s / YEAR)
    };
    format!("{amount} {suffix}")
}

/// The status bar text: "작성자 (3일 전)", or [`NOT_COMMITTED`].
pub fn status_text(b: &BlameLine, now: i64) -> String {
    match &b.commit {
        Some(c) => format!("{} ({})", c.author, ago_text(now, c.time)),
        None => NOT_COMMITTED.to_string(),
    }
}

/// The hover of the item: author and mail, when, the summary and the short sha, one per line.
pub fn hover_text(b: &BlameLine, now: i64) -> String {
    match &b.commit {
        Some(c) => {
            let who = if c.email.is_empty() { c.author.clone() } else { format!("{} <{}>", c.author, c.email) };
            let short: String = c.sha.chars().take(8).collect();
            let summary = if c.summary.is_empty() { "(요약 없음)" } else { c.summary.as_str() };
            format!("{who}, {}\n{summary}\n커밋 {short}", ago_text(now, c.time))
        }
        None => format!("{}번째 줄은 아직 커밋하지 않은 변경입니다.", b.line),
    }
}

/// The accessible name of the item.
pub fn aria_text(b: &BlameLine, now: i64) -> String {
    format!("Git blame: {}", status_text(b, now))
}

/// The path `git/blame` takes: relative to the editor's folder (taken to be the repository root,
/// as FR-L31.2 does), or `None` for a file outside it.
pub fn rel_path(folder: &str, abs_path: &str) -> Option<String> {
    let folder = folder.trim_end_matches('/');
    let rel = abs_path.strip_prefix(folder)?.strip_prefix('/')?;
    (!rel.is_empty()).then(|| rel.to_string())
}

/// What one blame is of: the file, the line (from 1) and, while the buffer has unsaved edits, its
/// version, because then the buffer's text is what git blames.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlameKey {
    pub path: String,
    pub line: u32,
    pub version: Option<u32>,
}

/// What [`BlameState::want`] asks of the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum Want {
    /// The answer is known.
    Ready(BlameLine),
    /// The line has no blame to show (the call failed).
    Nothing,
    /// Ask after the debounce; the answer carries `epoch`.
    Fetch { epoch: u64, key: BlameKey },
    /// A call for this key is already waiting.
    Wait,
}

/// The answers and the line the cursor is on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlameState {
    cache: HashMap<BlameKey, Option<BlameLine>>,
    wanted: Option<BlameKey>,
    epoch: u64,
    /// The last answer shown, by path: shown while the next line's answer is on the way, so the
    /// item does not blink on every cursor move.
    shown: Option<(String, BlameLine)>,
}

impl BlameState {
    /// The cursor is on `key`.
    pub fn want(&mut self, key: BlameKey) -> Want {
        if let Some(hit) = self.cache.get(&key) {
            self.wanted = None;
            return match hit {
                Some(b) => {
                    self.shown = Some((key.path.clone(), b.clone()));
                    Want::Ready(b.clone())
                }
                None => {
                    self.shown = None;
                    Want::Nothing
                }
            };
        }
        if self.wanted.as_ref() == Some(&key) {
            return Want::Wait;
        }
        self.wanted = Some(key.clone());
        Want::Fetch { epoch: self.epoch, key }
    }

    /// Whether a call made for `key` at `epoch` is still worth making after the debounce.
    pub fn still_wanted(&self, epoch: u64, key: &BlameKey) -> bool {
        epoch == self.epoch && self.wanted.as_ref() == Some(key)
    }

    /// Takes an answer (`None` for a failed call) unless the answers were dropped since it was
    /// asked. Returns whether the cursor is still on it, so the window should redraw.
    pub fn finish(&mut self, epoch: u64, key: BlameKey, answer: Option<BlameLine>) -> bool {
        if epoch != self.epoch {
            return false;
        }
        if self.cache.len() >= CACHE_LIMIT {
            self.cache.clear();
        }
        let current = self.wanted.as_ref() == Some(&key);
        self.cache.insert(key, answer);
        if current {
            self.wanted = None;
        }
        current
    }

    /// Drops every answer: the repository or the file changed (a save, a commit, a stage).
    pub fn invalidate(&mut self) {
        self.epoch += 1;
        self.cache.clear();
        self.wanted = None;
    }

    /// The last answer shown for `path`, for the time the next one is on the way.
    pub fn shown_for(&self, path: &str) -> Option<BlameLine> {
        self.shown.as_ref().filter(|(p, _)| p == path).map(|(_, b)| b.clone())
    }
}

/// The blame handle the editor window shares through [`crate::ui::editor_scm::Scm`].
#[derive(Clone)]
pub struct Blame {
    state: Arc<Mutex<BlameState>>,
    revision: dioxus_compose::prelude::SyncSignal<u64>,
    project: String,
}

impl Blame {
    pub fn new(project: &str, revision: dioxus_compose::prelude::SyncSignal<u64>) -> Blame {
        Blame { state: Arc::new(Mutex::new(BlameState::default())), revision, project: project.trim().to_string() }
    }

    fn with<R>(&self, f: impl FnOnce(&mut BlameState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn invalidate(&self) {
        self.with(BlameState::invalidate);
    }

    /// The blame of `line` (from 1) of the open file, or the last one shown while it is asked
    /// for. `unsaved` is the buffer's version and text while it has unsaved edits. It does not
    /// redraw before the answer, so the status bar's render may call it.
    pub fn item(&self, folder: &str, abs_path: &str, line: u32, unsaved: Option<(u32, &str)>) -> Option<BlameLine> {
        if self.project.is_empty() || line == 0 {
            return None;
        }
        let path = rel_path(folder, abs_path)?;
        let key = BlameKey { path: path.clone(), line, version: unsaved.map(|(v, _)| v) };
        match self.with(|s| s.want(key)) {
            Want::Ready(b) => Some(b),
            Want::Nothing => None,
            Want::Wait => self.with(|s| s.shown_for(&path)),
            Want::Fetch { epoch, key } => {
                let mut body = json!({ "path": key.path, "line": key.line });
                if let Some((_, text)) = unsaved {
                    body["contents"] = json!(text);
                }
                let (state, revision, project) = (self.state.clone(), self.revision, self.project.clone());
                let asked = key.clone();
                crate::services::run_shared(
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS)).await;
                        let wanted = state.lock().unwrap_or_else(|e| e.into_inner()).still_wanted(epoch, &asked);
                        if !wanted {
                            return (state, None);
                        }
                        let answer = crate::ui::vcs::git_call::<BlameLine>(&project, "blame", body).await;
                        (state, Some(answer.ok()))
                    },
                    move |(state, answer)| {
                        let Some(answer) = answer else { return };
                        let redraw = state.lock().unwrap_or_else(|e| e.into_inner()).finish(epoch, key, answer);
                        if redraw {
                            let mut revision = revision;
                            *revision.write() += 1;
                        }
                    },
                );
                self.with(|s| s.shown_for(&path))
            }
        }
    }
}

/// Seconds since the epoch on this computer's clock.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed(line: u32) -> BlameLine {
        BlameLine {
            line,
            commit: Some(BlameCommit {
                sha: "1234567890abcdef1234567890abcdef12345678".into(),
                author: "김민지".into(),
                email: "minji@example.com".into(),
                time: 1_000_000,
                summary: "Fix: 저장 오류를 고칩니다".into(),
            }),
        }
    }

    fn key(line: u32) -> BlameKey {
        BlameKey { path: "src/a.rs".into(), line, version: None }
    }

    #[test]
    fn fr_l31_8_ago_text_follows_code_oss_from_now() {
        let t = 10_000_000_000;
        assert_eq!(ago_text(t, t), "지금");
        assert_eq!(ago_text(t, t - 29), "지금");
        assert_eq!(ago_text(t, t - 45), "45초 전");
        assert_eq!(ago_text(t, t - 60), "1분 전");
        assert_eq!(ago_text(t, t - 59 * 60), "59분 전");
        assert_eq!(ago_text(t, t - 3 * 3600), "3시간 전");
        assert_eq!(ago_text(t, t - 86_400), "1일 전");
        assert_eq!(ago_text(t, t - 6 * 86_400), "6일 전");
        assert_eq!(ago_text(t, t - 7 * 86_400), "1주 전");
        assert_eq!(ago_text(t, t - 29 * 86_400), "4주 전");
        assert_eq!(ago_text(t, t - 30 * 86_400), "1개월 전");
        assert_eq!(ago_text(t, t - 364 * 86_400), "12개월 전");
        assert_eq!(ago_text(t, t - 365 * 86_400), "1년 전");
        assert_eq!(ago_text(t, t - 3 * 365 * 86_400), "3년 전");
        assert_eq!(ago_text(t, t + 2 * 3600), "2시간 후");
    }

    #[test]
    fn fr_l31_8_status_and_hover_text() {
        let b = committed(3);
        let now = 1_000_000 + 2 * 86_400;
        assert_eq!(status_text(&b, now), "김민지 (2일 전)");
        assert_eq!(hover_text(&b, now), "김민지 <minji@example.com>, 2일 전\nFix: 저장 오류를 고칩니다\n커밋 12345678");
        assert_eq!(aria_text(&b, now), "Git blame: 김민지 (2일 전)");
        let none = BlameLine { line: 7, commit: None };
        assert_eq!(status_text(&none, now), "아직 커밋하지 않음");
        assert_eq!(hover_text(&none, now), "7번째 줄은 아직 커밋하지 않은 변경입니다.");
    }

    #[test]
    fn fr_l31_8_answer_decodes_from_the_node() {
        let b: BlameLine = serde_json::from_value(json!({
            "line": 3,
            "commit": {"sha": "1234567890abcdef1234567890abcdef12345678", "author": "김민지", "email": "minji@example.com", "time": 1_000_000, "summary": "Fix: 저장 오류를 고칩니다"}
        }))
        .unwrap();
        assert_eq!(b, committed(3));
        let none: BlameLine = serde_json::from_value(json!({"line": 1, "commit": null})).unwrap();
        assert_eq!(none.commit, None);
    }

    #[test]
    fn fr_l31_8_rel_path_is_inside_the_folder() {
        assert_eq!(rel_path("/w/p", "/w/p/src/a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(rel_path("/w/p/", "/w/p/a.rs").as_deref(), Some("a.rs"));
        assert_eq!(rel_path("/w/p", "/w/pp/a.rs"), None);
        assert_eq!(rel_path("/w/p", "/w/p"), None);
        assert_eq!(rel_path("/w/p", "/x/a.rs"), None);
    }

    #[test]
    fn fr_l31_8_a_line_is_asked_once_and_then_cached() {
        let mut s = BlameState::default();
        let Want::Fetch { epoch, key: k } = s.want(key(3)) else { panic!("first want fetches") };
        assert_eq!(s.want(key(3)), Want::Wait);
        assert!(s.still_wanted(epoch, &k));
        assert!(s.finish(epoch, k, Some(committed(3))));
        assert_eq!(s.want(key(3)), Want::Ready(committed(3)));
    }

    #[test]
    fn fr_l31_8_moving_on_supersedes_the_waiting_line() {
        let mut s = BlameState::default();
        let Want::Fetch { epoch, key: first } = s.want(key(3)) else { panic!() };
        let Want::Fetch { key: second, .. } = s.want(key(4)) else { panic!() };
        // After the debounce the first line is no longer asked for.
        assert!(!s.still_wanted(epoch, &first));
        assert!(s.still_wanted(epoch, &second));
        // A late answer for the first line is kept but does not redraw.
        assert!(!s.finish(epoch, first, Some(committed(3))));
        assert_eq!(s.want(key(3)), Want::Ready(committed(3)));
    }

    #[test]
    fn fr_l31_8_save_or_commit_drops_answers_and_stale_ones() {
        let mut s = BlameState::default();
        let Want::Fetch { epoch, key: k } = s.want(key(3)) else { panic!() };
        s.invalidate();
        assert!(!s.still_wanted(epoch, &k));
        assert!(!s.finish(epoch, k.clone(), Some(committed(3))));
        assert!(matches!(s.want(key(3)), Want::Fetch { .. }));
        let Want::Fetch { epoch, key: k } = s.want(key(5)) else { panic!() };
        assert!(s.finish(epoch, k, Some(committed(5))));
        s.invalidate();
        assert!(matches!(s.want(key(5)), Want::Fetch { .. }));
    }

    #[test]
    fn fr_l31_8_unsaved_buffer_versions_are_separate_keys() {
        let mut s = BlameState::default();
        let v1 = BlameKey { version: Some(1), ..key(2) };
        let Want::Fetch { epoch, key: k } = s.want(v1.clone()) else { panic!() };
        assert!(s.finish(epoch, k, Some(BlameLine { line: 2, commit: None })));
        assert!(matches!(s.want(BlameKey { version: Some(2), ..key(2) }), Want::Fetch { .. }));
        assert_eq!(s.want(v1), Want::Ready(BlameLine { line: 2, commit: None }));
    }

    #[test]
    fn fr_l31_8_failed_call_shows_nothing_and_the_last_answer_bridges_the_wait() {
        let mut s = BlameState::default();
        let Want::Fetch { epoch, key: k } = s.want(key(1)) else { panic!() };
        s.finish(epoch, k, Some(committed(1)));
        assert_eq!(s.want(key(1)), Want::Ready(committed(1)));
        assert!(matches!(s.want(key(2)), Want::Fetch { .. }));
        assert_eq!(s.shown_for("src/a.rs"), Some(committed(1)));
        assert_eq!(s.shown_for("src/b.rs"), None);
        let Want::Fetch { epoch, key: k } = s.want(key(9)) else { panic!() };
        s.finish(epoch, k, None);
        assert_eq!(s.want(key(9)), Want::Nothing);
        assert_eq!(s.shown_for("src/a.rs"), None);
    }

    #[test]
    fn fr_l31_8_cache_is_bounded() {
        let mut s = BlameState::default();
        for line in 1..=(CACHE_LIMIT as u32 + 1) {
            if let Want::Fetch { epoch, key: k } = s.want(key(line)) {
                s.finish(epoch, k, Some(BlameLine { line, commit: None }));
            }
        }
        assert!(s.cache.len() <= CACHE_LIMIT);
    }
}
