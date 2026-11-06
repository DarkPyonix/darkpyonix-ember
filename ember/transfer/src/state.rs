//! The transfer state machine (SPEC `FR-F3`, `FR-F4`).
//!
//! A state allows only its own transitions; anything else is a [`TransitionError`]. Expiry is
//! driven by an injected [`Clock`], so tests never sleep.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A monotonic clock. `now` is the time since an arbitrary start.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// The real clock.
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// A clock that moves only when told to (for tests).
#[derive(Default)]
pub struct ManualClock {
    millis: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn advance(&self, by: Duration) {
        self.millis.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.millis.load(Ordering::SeqCst))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Offered,
    Accepted,
    Receiving { bytes: u64 },
    Done,
    Declined,
    Expired,
    Cancelled,
    Failed(String),
}

impl State {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            State::Done | State::Declined | State::Expired | State::Cancelled | State::Failed(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Accept,
    Decline,
    /// The first byte is about to move (not used before the next unit).
    Start,
    /// Total bytes received so far.
    Progress(u64),
    Finish,
    Expire,
    /// From either side.
    Cancel,
    Fail(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    #[error("event {event:?} is not allowed in state {state:?}")]
    Illegal { state: State, event: Event },
    #[error("the offer expired before an answer")]
    Expired,
    #[error("progress went back from {from} to {to} bytes")]
    ProgressWentBack { from: u64, to: u64 },
}

/// The transition table. Pure.
pub fn next(state: &State, event: &Event) -> Result<State, TransitionError> {
    use Event as E;
    use State as S;
    let illegal = || TransitionError::Illegal { state: state.clone(), event: event.clone() };
    Ok(match (state, event) {
        (S::Offered, E::Accept) => S::Accepted,
        (S::Offered, E::Decline) => S::Declined,
        (S::Offered, E::Expire) => S::Expired,
        (S::Accepted, E::Start) => S::Receiving { bytes: 0 },
        (S::Receiving { bytes }, E::Progress(n)) => {
            if n < bytes {
                return Err(TransitionError::ProgressWentBack { from: *bytes, to: *n });
            }
            S::Receiving { bytes: *n }
        }
        (S::Receiving { .. }, E::Finish) => S::Done,
        (S::Offered | S::Accepted | S::Receiving { .. }, E::Cancel) => S::Cancelled,
        (S::Offered | S::Accepted | S::Receiving { .. }, E::Fail(r)) => S::Failed(r.clone()),
        _ => return Err(illegal()),
    })
}

/// One transfer: a state and, while it is only offered, a deadline.
#[derive(Debug, Clone)]
pub struct Transfer {
    state: State,
    deadline: Duration,
}

impl Transfer {
    /// A new offer made at `now` that expires after `ttl` without an answer.
    pub fn new(now: Duration, ttl: Duration) -> Self {
        Self { state: State::Offered, deadline: now.saturating_add(ttl) }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Applies `event` at `now`. An offer past its deadline becomes `Expired` and any event other
    /// than `Expire` and `Cancel` returns [`TransitionError::Expired`].
    pub fn apply(&mut self, event: Event, now: Duration) -> Result<&State, TransitionError> {
        let due = self.state == State::Offered && now >= self.deadline;
        if due && !matches!(event, Event::Expire | Event::Cancel) {
            self.state = State::Expired;
            return Err(TransitionError::Expired);
        }
        self.state = next(&self.state, &event)?;
        Ok(&self.state)
    }

    /// Expires an unanswered offer that is past its deadline. True if it did.
    pub fn expire_if_due(&mut self, now: Duration) -> bool {
        if self.state == State::Offered && now >= self.deadline {
            self.state = State::Expired;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Duration = Duration::ZERO;
    const TTL: Duration = Duration::from_secs(60);

    #[test]
    fn fr_f3_legal_path_to_done() {
        let mut t = Transfer::new(T0, TTL);
        assert_eq!(t.apply(Event::Accept, T0).unwrap(), &State::Accepted);
        assert_eq!(t.apply(Event::Start, T0).unwrap(), &State::Receiving { bytes: 0 });
        assert_eq!(t.apply(Event::Progress(10), T0).unwrap(), &State::Receiving { bytes: 10 });
        assert_eq!(t.apply(Event::Finish, T0).unwrap(), &State::Done);
    }

    #[test]
    fn fr_f3_decline_ends_the_offer() {
        let mut t = Transfer::new(T0, TTL);
        assert_eq!(t.apply(Event::Decline, T0).unwrap(), &State::Declined);
        assert!(t.apply(Event::Accept, T0).is_err());
    }

    #[test]
    fn fr_f3_a_state_allows_only_its_own_transitions() {
        let mut t = Transfer::new(T0, TTL);
        assert!(matches!(t.apply(Event::Start, T0), Err(TransitionError::Illegal { .. })));
        assert!(matches!(t.apply(Event::Finish, T0), Err(TransitionError::Illegal { .. })));
        t.apply(Event::Accept, T0).unwrap();
        assert!(matches!(t.apply(Event::Accept, T0), Err(TransitionError::Illegal { .. })));
        assert!(matches!(t.apply(Event::Decline, T0), Err(TransitionError::Illegal { .. })));
        assert!(matches!(t.apply(Event::Expire, T0), Err(TransitionError::Illegal { .. })));
        assert_eq!(t.state(), &State::Accepted);
    }

    #[test]
    fn fr_f3_terminal_states_allow_nothing() {
        for ev in [Event::Decline, Event::Expire, Event::Cancel, Event::Fail("x".into())] {
            let mut t = Transfer::new(T0, TTL);
            t.apply(ev, T0).unwrap();
            assert!(t.state().is_terminal());
            for again in [Event::Accept, Event::Cancel, Event::Finish, Event::Fail("y".into())] {
                assert!(t.apply(again, T0).is_err());
            }
        }
    }

    #[test]
    fn fr_f4_cancel_from_every_live_state() {
        let mut offered = Transfer::new(T0, TTL);
        assert_eq!(offered.apply(Event::Cancel, T0).unwrap(), &State::Cancelled);
        let mut accepted = Transfer::new(T0, TTL);
        accepted.apply(Event::Accept, T0).unwrap();
        assert_eq!(accepted.apply(Event::Cancel, T0).unwrap(), &State::Cancelled);
        let mut receiving = Transfer::new(T0, TTL);
        receiving.apply(Event::Accept, T0).unwrap();
        receiving.apply(Event::Start, T0).unwrap();
        assert_eq!(receiving.apply(Event::Cancel, T0).unwrap(), &State::Cancelled);
    }

    #[test]
    fn fr_f4_progress_never_goes_back() {
        let mut t = Transfer::new(T0, TTL);
        t.apply(Event::Accept, T0).unwrap();
        t.apply(Event::Start, T0).unwrap();
        t.apply(Event::Progress(10), T0).unwrap();
        assert_eq!(
            t.apply(Event::Progress(5), T0),
            Err(TransitionError::ProgressWentBack { from: 10, to: 5 })
        );
    }

    #[test]
    fn fr_f3_failure_carries_the_reason() {
        let mut t = Transfer::new(T0, TTL);
        t.apply(Event::Accept, T0).unwrap();
        assert_eq!(t.apply(Event::Fail("link lost".into()), T0).unwrap(), &State::Failed("link lost".into()));
    }

    #[test]
    fn fr_f3_unanswered_offer_expires_with_the_injected_clock() {
        let clock = ManualClock::new();
        let mut t = Transfer::new(clock.now(), TTL);
        assert!(!t.expire_if_due(clock.now()));
        clock.advance(Duration::from_secs(59));
        assert!(!t.expire_if_due(clock.now()));
        clock.advance(Duration::from_secs(1));
        assert!(t.expire_if_due(clock.now()));
        assert_eq!(t.state(), &State::Expired);
    }

    #[test]
    fn fr_f3_an_answer_after_the_deadline_is_refused() {
        let clock = ManualClock::new();
        let mut t = Transfer::new(clock.now(), TTL);
        clock.advance(TTL);
        assert_eq!(t.apply(Event::Accept, clock.now()), Err(TransitionError::Expired));
        assert_eq!(t.state(), &State::Expired);
    }

    #[test]
    fn fr_f3_accepted_transfers_do_not_expire() {
        let clock = ManualClock::new();
        let mut t = Transfer::new(clock.now(), TTL);
        t.apply(Event::Accept, clock.now()).unwrap();
        clock.advance(TTL * 10);
        assert!(!t.expire_if_due(clock.now()));
        assert_eq!(t.state(), &State::Accepted);
    }
}
