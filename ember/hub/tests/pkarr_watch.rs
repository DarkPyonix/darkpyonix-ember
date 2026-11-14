//! The hub's batched pkarr long-poll (`GET /pkarr/watch`, core FR-H2 #132) through the
//! transport's HTTP store (SPEC FR-N2, #783), against the fake hub.

use std::time::{Duration, Instant};

use ember_hub::fake::FakeHub;
use ember_hub::{HubConfig, Role};
use ember_transport::{pkarr_signable, HttpPkarrStore, PeerId, PkarrStore, SecretKey, WatchOutcome};

/// A signed pkarr payload of `key` with an empty DNS packet at timestamp `ts`.
fn payload(key: &SecretKey, ts: u64) -> Vec<u8> {
    let dns = [0u8, 0, 0x84, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut out = key.sign(&pkarr_signable(ts, &dns)).to_vec();
    out.extend_from_slice(&ts.to_be_bytes());
    out.extend_from_slice(&dns);
    out
}

async fn store_of(hub: &FakeHub, device_token: &str) -> HttpPkarrStore {
    let cfg = HubConfig::new(hub.url()).discover().await;
    let dir = cfg.directory_with_watch(None, Some(device_token.to_string()));
    let watch = dir.watch.clone();
    assert!(watch.is_some(), "the fake advertises pkarr_watch in /config");
    HttpPkarrStore::new(dir.pkarr_url, None).unwrap().with_watch(watch)
}

#[tokio::test]
async fn fr_n2_watch_returns_changed_packets_and_waits_for_new_ones() {
    let hub = FakeHub::start().await;
    let (ka, kb) = (SecretKey::generate(), SecretKey::generate());
    let ta = hub.register(ka.peer_id(), "server", Role::MainServer);
    hub.register(kb.peer_id(), "node", Role::Computer);
    let store = std::sync::Arc::new(store_of(&hub, &ta).await);
    let limits = store.watch_limits().unwrap();
    assert_eq!((limits.max_keys, limits.max_wait), (32, Duration::from_secs(25)));
    let b = kb.peer_id();

    // A device that never published stays absent: nothing changed.
    assert_eq!(store.watch(&[(b, 0)], Duration::ZERO).await.unwrap(), WatchOutcome::Unchanged);

    // A published packet is returned with the exact bytes of the PUT body.
    let first = payload(&kb, 1_000);
    store.put(b, first.clone()).await.unwrap();
    assert_eq!(store.watch(&[(b, 0)], Duration::ZERO).await.unwrap(), WatchOutcome::Changed(vec![(b, first)]));

    // A held watch returns as soon as a newer packet is published.
    let held = {
        let store = store.clone();
        tokio::spawn(async move { store.watch(&[(b, 1_000)], Duration::from_secs(25)).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    let second = payload(&kb, 2_000);
    store.put(b, second.clone()).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), held).await.expect("the watch was not woken").unwrap();
    assert_eq!(got.unwrap(), WatchOutcome::Changed(vec![(b, second)]));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(hub.pkarr_watch_requests(), 3);
}

#[tokio::test]
async fn fr_n2_watch_names_the_keys_that_are_not_devices_of_the_account() {
    let hub = FakeHub::start().await;
    let (ka, kb) = (SecretKey::generate(), SecretKey::generate());
    let ta = hub.register(ka.peer_id(), "server", Role::MainServer);
    hub.register(kb.peer_id(), "node", Role::Computer);
    let store = store_of(&hub, &ta).await;
    let stranger: PeerId = SecretKey::generate().peer_id();
    let got = store.watch(&[(kb.peer_id(), 0), (stranger, 0)], Duration::ZERO).await.unwrap();
    assert_eq!(got, WatchOutcome::NotFound(vec![stranger]));
}

#[tokio::test]
async fn fr_n2_without_pkarr_watch_in_config_the_directory_polls() {
    let hub = FakeHub::start().await;
    hub.set_pkarr_watch(false);
    let cfg = HubConfig::new(hub.url()).discover().await;
    assert_eq!(cfg.directory_with_watch(None, Some("dpd_x".into())).watch, None);
    // A bad device token is refused, so the node falls back to polling.
    hub.set_pkarr_watch(true);
    let cfg = HubConfig::new(hub.url()).discover().await;
    let dir = cfg.directory_with_watch(None, Some("dpd_not_a_token".into()));
    let store = HttpPkarrStore::new(dir.pkarr_url, None).unwrap().with_watch(dir.watch);
    assert!(store.watch(&[(SecretKey::generate().peer_id(), 0)], Duration::ZERO).await.is_err());
}
