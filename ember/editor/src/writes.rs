//! File writes the session has started and not finished.
//!
//! A save is a detached task: the session's driver hands the bytes to it and goes on. When the
//! window quits, the driver ends and the process exits, and a write still on the wire ends with
//! it, half sent. Each write holds a [`WriteGuard`] until it is done, and the quit path waits for
//! the count to reach zero with [`wait_settled`], for a bounded time.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Held by a file write from the moment it is started until it has finished (or failed).
#[derive(Debug)]
pub struct WriteGuard(());

impl WriteGuard {
    pub fn new() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        WriteGuard(())
    }
}

impl Default for WriteGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How many writes are on the way.
pub fn in_flight() -> usize {
    IN_FLIGHT.load(Ordering::SeqCst)
}

/// Waits until no write is on the way, or `limit` has passed. True when every write finished.
pub async fn wait_settled(limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while in_flight() > 0 {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fr_l31_6_b8_shutdown_waits_for_a_write_in_flight_and_gives_up_after_the_limit() {
        // One test body, and no exact counts: the count is process-wide, so other tests' writes show in it.
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (guard, flag) = (WriteGuard::new(), done.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            flag.store(true, Ordering::SeqCst);
            drop(guard);
        });
        assert!(in_flight() >= 1);
        assert!(wait_settled(Duration::from_secs(5)).await, "the write finished within the limit");
        assert!(done.load(Ordering::SeqCst), "shutdown returned only after the write completed");

        // A write that never ends does not hold the quit for ever.
        let stuck = WriteGuard::new();
        let started = tokio::time::Instant::now();
        assert!(!wait_settled(Duration::from_millis(60)).await);
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(stuck);
    }
}
