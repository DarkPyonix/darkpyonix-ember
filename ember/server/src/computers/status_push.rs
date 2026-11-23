//! Reachability checks for clients attached to the push socket.

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::{self, MissedTickBehavior};

use super::{strip_urls_in_text, ComputerStatus, Computers, LOCAL};
use crate::session::{Push, Sessions};

const PERIOD: Duration = Duration::from_secs(30);

/// Probe immediately on the first connection, then once per interval while clients are connected.
pub async fn run(computers: &Computers, sessions: &Sessions) {
    let mut clients = sessions.subscribe_push_clients();
    while clients.wait_for(|count| *count > 0).await.is_ok() {
        let mut previous = HashMap::new();
        let mut ticks = time::interval(PERIOD);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = clients.wait_for(|count| *count == 0) => break,
                _ = ticks.tick() => {}
            }
            tokio::select! {
                biased;
                _ = clients.wait_for(|count| *count == 0) => break,
                result = computers.list(true) => match result {
                    Ok(snapshot) => publish_changes(sessions, &mut previous, snapshot),
                    Err(error) => tracing::warn!(%error, "could not probe registered computers"),
                }
            }
        }
    }
}

fn publish_changes(
    sessions: &Sessions,
    previous: &mut HashMap<String, Option<bool>>,
    snapshot: Vec<ComputerStatus>,
) {
    let mut current = HashMap::new();
    for status in snapshot {
        let id = status.computer.id;
        if id == LOCAL {
            continue;
        }
        current.insert(id.clone(), status.reachable);
        if previous.get(&id) != Some(&status.reachable) {
            sessions.publish(Push::ComputerStatus {
                computer_id: id,
                reachable: status.reachable,
                error: status.error.map(|error| strip_urls_in_text(&error)),
            });
        }
    }
    *previous = current;
}
