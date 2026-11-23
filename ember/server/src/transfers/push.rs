use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::session::{Push, Sessions};

use super::TransferUpdate;

const UPDATE_INTERVAL: Duration = Duration::from_millis(250);

/// The first update is immediate. Each later window sends its latest record, including
/// a completion or decline. An idle entry drops out after a window with no new update.
pub(super) struct TransferPush {
    sessions: Arc<Sessions>,
    pending: Mutex<HashMap<String, Option<TransferUpdate>>>,
}

impl TransferPush {
    pub(super) fn new(sessions: Arc<Sessions>) -> Arc<Self> {
        Arc::new(Self { sessions, pending: Mutex::new(HashMap::new()) })
    }

    pub(super) fn changed(self: &Arc<Self>, transfer: TransferUpdate) {
        if self.sessions.push_receiver_count() == 0 {
            return;
        }
        let id = transfer.id().to_owned();
        let mut pending = self.pending.lock().unwrap();
        if let Some(slot) = pending.get_mut(&id) {
            *slot = Some(transfer);
            return;
        }
        pending.insert(id.clone(), None);
        self.sessions.publish(Push::TransferUpdated { transfer });
        let sender = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(UPDATE_INTERVAL).await;
                let mut pending = sender.pending.lock().unwrap();
                if let Some(transfer) = pending.get_mut(&id).and_then(Option::take) {
                    sender.sessions.publish(Push::TransferUpdated { transfer });
                } else {
                    pending.remove(&id);
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use ember_transfer::store::{HistoryRecord, TransferDirection, TransferResult};

    fn update(id: &str, timestamp_ms: i64) -> TransferUpdate {
        TransferUpdate::Ended(HistoryRecord {
            id: id.to_owned(),
            name: "file".to_owned(),
            device: "sender".to_owned(),
            direction: TransferDirection::Receive,
            size: 1,
            result: TransferResult::Cancelled,
            timestamp_ms,
        })
    }

    #[tokio::test]
    async fn pr_1_transfer_push_keeps_latest_record_per_transfer_at_four_per_second() {
        let sessions = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), Vec::new());
        let mut received = sessions.subscribe();
        let sender = TransferPush::new(sessions);
        let started = tokio::time::Instant::now();
        sender.changed(update("one", 1));
        sender.changed(update("one", 2));
        sender.changed(update("one", 3));
        sender.changed(update("two", 4));
        let Push::TransferUpdated { transfer: TransferUpdate::Ended(first) } = received.try_recv().unwrap()
            else { panic!("expected initial transfer record") };
        let Push::TransferUpdated { transfer: TransferUpdate::Ended(other) } = received.try_recv().unwrap()
            else { panic!("expected independent transfer record") };
        assert_eq!((first.id.as_str(), first.timestamp_ms), ("one", 1));
        assert_eq!((other.id.as_str(), other.timestamp_ms), ("two", 4));
        assert!(received.try_recv().is_err());

        let trailing = tokio::time::timeout(Duration::from_secs(3), received.recv()).await.unwrap().unwrap();
        let Push::TransferUpdated { transfer: TransferUpdate::Ended(trailing) } = trailing
            else { panic!("expected coalesced transfer record") };
        assert_eq!((trailing.id.as_str(), trailing.timestamp_ms), ("one", 3));
        assert!(started.elapsed() >= UPDATE_INTERVAL);
    }
}
