//! Reconnect policy for the editor terminal (FR-L31.22, #336).
//!
//! When the attach stream ends without the process exiting and without the window letting go of
//! the terminal, the editor reattaches to the same PTY: the create request carries the session
//! key, so the server re-adopts the running session the node kept alive (FR-P6), and the hello
//! asks for a snapshot, so the screen comes back. This module is the pure part: why a stream
//! ended, what to do next, how long to wait, and the fixed Korean sentences the panel shows.
//! Raw transport text never reaches the panel; it goes to the log only.

use std::time::Duration;

/// Attempts before the panel stops retrying and offers 다시 연결 instead.
pub const MAX_ATTEMPTS: u32 = 6;

/// The longest wait between two attempts.
pub const MAX_DELAY: Duration = Duration::from_secs(30);

/// Shown for any attach or relay failure, whatever the transport said.
pub const FAILED_TEXT: &str = "터미널 연결에 문제가 생겼습니다.";

/// Shown once a frame from the relay could not be read. The stream keeps going.
pub const FRAME_ERROR_TEXT: &str = "서버가 보낸 터미널 데이터 일부를 읽지 못했습니다.";

/// Shown after the last automatic attempt failed.
pub const GAVE_UP_TEXT: &str = "터미널에 다시 연결하지 못했습니다. 다시 연결을 눌러 주십시오.";

/// Shown while a reattach is in flight after a drop.
pub const RECONNECTING_TEXT: &str = "터미널에 다시 연결하는 중입니다.";

/// Why one attach stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEnd {
    /// The window dropped the terminal (or detached): stop, nothing to reconnect.
    Released,
    /// The process exited: the session is over, so reattaching would start a new shell.
    Exited,
    /// The socket closed, failed, or the create request failed.
    Dropped,
}

/// What the transport task does after a stream ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Stop,
    /// Wait this long, then reattach. `attempt` counts from 1.
    Retry { attempt: u32, delay: Duration },
    /// Out of attempts: the panel offers the manual button.
    GiveUp,
}

/// Exponential backoff: 1, 2, 4, 8, 16 seconds, then [`MAX_DELAY`].
pub fn delay_for(attempt: u32) -> Duration {
    let secs = 1u64.checked_shl(attempt.saturating_sub(1)).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_DELAY)
}

/// Retry bookkeeping for one terminal. A stream that got as far as attaching resets the count,
/// so a long-lived terminal that drops once a day always starts again from one second.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconnect {
    attempts: u32,
}

impl Reconnect {
    /// The stream reached `attached`: the next drop starts the backoff over.
    pub fn attached(&mut self) {
        self.attempts = 0;
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Decide what follows a stream that ended for `end`.
    pub fn on_end(&mut self, end: StreamEnd) -> Next {
        match end {
            StreamEnd::Released | StreamEnd::Exited => Next::Stop,
            StreamEnd::Dropped if self.attempts >= MAX_ATTEMPTS => Next::GiveUp,
            StreamEnd::Dropped => {
                self.attempts += 1;
                Next::Retry { attempt: self.attempts, delay: delay_for(self.attempts) }
            }
        }
    }
}

/// The state line while waiting for the next attempt.
pub fn waiting_text(attempt: u32, delay: Duration) -> String {
    format!(
        "터미널 연결이 끊어졌습니다. {}초 후 다시 연결합니다. ({attempt}/{MAX_ATTEMPTS})",
        delay.as_secs().max(1)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_22_backoff_doubles_from_one_second_and_caps() {
        let secs: Vec<u64> = (1..=8).map(|a| delay_for(a).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(delay_for(200), MAX_DELAY);
    }

    #[test]
    fn fr_l31_22_a_drop_retries_until_the_attempts_run_out() {
        let mut r = Reconnect::default();
        for attempt in 1..=MAX_ATTEMPTS {
            assert_eq!(r.on_end(StreamEnd::Dropped), Next::Retry { attempt, delay: delay_for(attempt) });
        }
        assert_eq!(r.on_end(StreamEnd::Dropped), Next::GiveUp);
    }

    #[test]
    fn fr_l31_22_attaching_resets_the_backoff() {
        let mut r = Reconnect::default();
        r.on_end(StreamEnd::Dropped);
        r.on_end(StreamEnd::Dropped);
        assert_eq!(r.attempts(), 2);
        r.attached();
        assert_eq!(r.on_end(StreamEnd::Dropped), Next::Retry { attempt: 1, delay: Duration::from_secs(1) });
    }

    #[test]
    fn fr_l31_22_exit_and_release_never_reconnect() {
        let mut r = Reconnect::default();
        assert_eq!(r.on_end(StreamEnd::Exited), Next::Stop);
        assert_eq!(r.on_end(StreamEnd::Released), Next::Stop);
        assert_eq!(r.attempts(), 0);
    }

    #[test]
    fn fr_l31_22_waiting_line_is_korean_with_seconds_and_count() {
        assert_eq!(
            waiting_text(2, Duration::from_secs(2)),
            "터미널 연결이 끊어졌습니다. 2초 후 다시 연결합니다. (2/6)"
        );
    }
}
