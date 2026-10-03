//! The hub additions of darkpyonix-core PR #34 against the fake hub: `GET /v1/config` discovery
//! (FR-H8) and the fallback when a hub answers `404`; the device list's weak `ETag`,
//! `If-None-Match` and `?wait=0..25` long-poll (FR-H9); `401 device_removed` as revocation,
//! distinct from `401 invalid_credentials`; resolve tokens (NFR-H2); self-removal, rename and
//! the app record (FR-H1, FR-H10).

use std::time::{Duration, Instant};

use ember_hub::fake::{FakeHub, FAKE_API_VERSION};
use ember_hub::{
    check_registration, ensure_resolve_token, watch_registration_with, DeviceApp, DeviceWatcher, DevicesPoll,
    HubConfig, HubError, HubInfo, Registration, RegistrationState, RelaySource, Role,
};
use ember_transport::{RelayConfig, SecretKey};

#[tokio::test]
async fn config_is_discovered_from_the_hub() {
    let hub = FakeHub::start().await;
    hub.set_config_relays(vec!["https://relay-a.example.net/".into(), "https://relay-b.example.net".into()]);

    let info = hub.client().config().await.unwrap().expect("the fake serves /v1/config");
    assert_eq!(info.api_version, Some(FAKE_API_VERSION));
    assert!(info.supports_devices_wait(), "api_version 1 has the long-poll");
    assert_eq!(info.link_url, Some(format!("{}/link", hub.url())));

    // Without discovery an IP-address hub has no relay; with it, the hub's list is used.
    assert_eq!(HubConfig::new(hub.url()).relay_url, None);
    let cfg = HubConfig::new(hub.url()).discover().await;
    assert_eq!(cfg.relay_source, RelaySource::Hub);
    assert_eq!(cfg.relay_urls(), vec!["https://relay-a.example.net", "https://relay-b.example.net"]);
    assert_eq!(cfg.pkarr_url(), format!("{}/pkarr", hub.url()));
    assert!(cfg.supports_devices_wait());
    if std::env::var_os(ember_transport::RELAY_URL_ENV).is_none() {
        assert_eq!(
            cfg.relay_config(),
            RelayConfig::Custom(vec!["https://relay-a.example.net".into(), "https://relay-b.example.net".into()])
        );
    }

    // An explicit relay (EMBER_HUB_RELAY_URL) is kept.
    let explicit = HubConfig::parse(hub.url(), Some("https://mine.example.org")).unwrap().discover().await;
    assert_eq!(explicit.relay_source, RelaySource::Explicit);
    assert_eq!(explicit.relay_urls(), vec!["https://mine.example.org"]);
}

#[tokio::test]
async fn config_falls_back_to_derivation_on_404_and_when_unreachable() {
    let hub = FakeHub::start().await;
    hub.set_config_enabled(false);
    assert_eq!(hub.client().config().await.unwrap(), None, "404 is 'no such endpoint', not an error");

    let cfg = HubConfig::new(hub.url()).discover().await;
    assert_eq!(cfg, HubConfig::new(hub.url()), "nothing changes without /v1/config");
    assert_eq!(cfg.relay_source, RelaySource::Derived);
    assert_eq!(cfg.pkarr_url(), format!("{}/pkarr", hub.url()));
    assert!(!cfg.supports_devices_wait(), "no config: poll the device list");

    // Unreachable: the derived values stay.
    let url = hub.url().replace("127.0.0.1", "localhost");
    drop(hub);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(HubConfig::new(&url).discover().await, HubConfig::new(&url));
    assert_eq!(HubConfig::new("https://hub.example.net").relay_url.as_deref(), Some("https://relay.example.net"));
}

#[tokio::test]
async fn etag_answers_304_until_the_list_changes() {
    let hub = FakeHub::start().await;
    let key = SecretKey::generate();
    let client = hub.client().with_token(&hub.register(key.peer_id(), "server", Role::MainServer));

    let DevicesPoll::Changed { devices, etag } = client.devices_since(None, None).await.unwrap() else {
        panic!("first request returns the list");
    };
    assert_eq!(devices.len(), 1);
    let etag = etag.expect("the hub sends an ETag");
    assert!(etag.starts_with("W/\""), "weak ETag: {etag}");

    assert_eq!(client.devices_since(Some(&etag), None).await.unwrap(), DevicesPoll::NotModified);
    let t0 = Instant::now();
    assert_eq!(client.devices_since(Some(&etag), Some(Duration::from_secs(1))).await.unwrap(), DevicesPoll::NotModified);
    assert!(t0.elapsed() >= Duration::from_millis(900), "the request was held for `wait`");

    // `last_seen` alone does not change the version; online does.
    let v = hub.list_version();
    hub.set_online(&key.peer_id(), true);
    assert_eq!(hub.list_version(), v + 1);
    let DevicesPoll::Changed { devices, etag: new } = client.devices_since(Some(&etag), None).await.unwrap() else {
        panic!("changed list");
    };
    assert!(devices[0].online);
    assert_ne!(new.as_deref(), Some(etag.as_str()));
}

#[tokio::test]
async fn long_poll_returns_as_soon_as_the_list_changes() {
    let hub = FakeHub::start().await;
    let main = hub.client().with_token(&hub.register(SecretKey::generate().peer_id(), "server", Role::MainServer));
    let info = main.config().await.unwrap();
    // Period one minute: only the long-poll can be this quick.
    let mut w = DeviceWatcher::new(main.clone(), info.as_ref(), Duration::from_secs(60));
    assert!(w.is_long_poll());
    assert_eq!(w.next().await.unwrap().len(), 1, "the first call returns the current list");

    let node = SecretKey::generate().peer_id();
    let fake = &hub;
    let adder = async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        fake.register(node, "node", Role::Computer);
    };
    let t0 = Instant::now();
    let (list, ()) = tokio::join!(async { tokio::time::timeout(Duration::from_secs(3), w.next()).await }, adder);
    let list = list.expect("woken by the change").unwrap();
    assert!(list.iter().any(|d| d.endpoint_id == node));
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn long_poll_wakes_the_revocation_watcher_within_a_second() {
    let hub = FakeHub::start().await;
    let key = SecretKey::generate();
    let client = hub.client().with_token(&hub.register(key.peer_id(), "node", Role::Computer));
    let info = HubConfig::new(hub.url()).discover().await.info;
    assert!(info.as_ref().is_some_and(HubInfo::supports_devices_wait));

    // A one-hour period: a poll could not see the removal in time.
    let (mut rx, task) = watch_registration_with(client.clone(), info.as_ref(), Duration::from_secs(3600));
    rx.wait_for(|s| matches!(s, RegistrationState::Active(_))).await.unwrap();
    let held = Instant::now() + Duration::from_secs(5);
    while hub.device_list_requests() < 2 {
        assert!(Instant::now() < held, "the watcher never long-polled");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let t0 = Instant::now();
    assert!(hub.remove(&key.peer_id()));
    tokio::time::timeout(Duration::from_millis(1500), rx.wait_for(RegistrationState::is_revoked))
        .await
        .expect("revocation seen within ~1 s (the real hub checks about every 2 s)")
        .unwrap();
    assert!(t0.elapsed() < Duration::from_millis(1500));
    tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
}

#[tokio::test]
async fn without_config_the_watcher_polls_and_a_hub_ignoring_wait_is_polled() {
    let hub = FakeHub::start().await;
    hub.set_config_enabled(false);
    let client = hub.client().with_token(&hub.register(SecretKey::generate().peer_id(), "s", Role::MainServer));
    let info = HubConfig::new(hub.url()).discover().await.info;
    assert_eq!(info, None);
    let mut w = DeviceWatcher::new(client.clone(), info.as_ref(), Duration::from_millis(100));
    assert!(!w.is_long_poll());
    w.next().await.unwrap();
    // No change: conditional polls every period, all 304, nothing returned.
    let before = hub.device_list_requests();
    assert!(tokio::time::timeout(Duration::from_millis(450), w.next()).await.is_err());
    let polls = hub.device_list_requests() - before;
    assert!((2..=6).contains(&polls), "{polls} polls in 450 ms at a 100 ms period");

    // A hub that says api_version 1 but answers `wait` at once is detected and polled.
    hub.set_honour_wait(false);
    let v1 = HubInfo { api_version: Some(1), ..Default::default() };
    let mut w = DeviceWatcher::new(client, Some(&v1), Duration::from_millis(100));
    assert!(w.is_long_poll());
    w.next().await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(300), w.next()).await;
    assert!(!w.is_long_poll(), "fell back after immediate answers");
}

#[tokio::test]
async fn wait_out_of_range_is_400() {
    let hub = FakeHub::start().await;
    let token = hub.register(SecretKey::generate().peer_id(), "s", Role::MainServer);
    let http = reqwest::Client::new();
    let r = http.get(format!("{}/v1/devices?wait=26", hub.url())).bearer_auth(&token).send().await.unwrap();
    assert_eq!(r.status(), 400);
    let r = http.get(format!("{}/v1/devices?wait=0", hub.url())).bearer_auth(&token).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn removed_device_error_is_revocation_and_bad_token_is_not() {
    let hub = FakeHub::start().await;
    let key = SecretKey::generate();
    let client = hub.client().with_token(&hub.register(key.peer_id(), "node", Role::Computer));
    assert!(hub.remove(&key.peer_id()));

    let e = client.me().await.unwrap_err();
    assert!(matches!(e, HubError::DeviceRemoved(_)), "{e:?}");
    assert!(e.is_revocation());
    assert_eq!(check_registration(&client).await, RegistrationState::Revoked);
    // The long-poll path reports it the same way.
    let mut w = DeviceWatcher::new(client.clone(), hub.client().config().await.unwrap().as_ref(), Duration::from_secs(60));
    assert!(w.next().await.unwrap_err().is_device_removed());

    // A token the hub does not know is "rejected", not "removed".
    let bogus = hub.client().with_token("dpd_bogus");
    let e = bogus.me().await.unwrap_err();
    assert!(matches!(e, HubError::InvalidCredentials(_)), "{e:?}");
    assert!(!e.is_revocation());
    assert!(matches!(check_registration(&bogus).await, RegistrationState::Rejected(_)));
}

#[tokio::test]
async fn a_watcher_expecting_itself_treats_its_absence_as_removal() {
    let hub = FakeHub::start().await;
    let me = SecretKey::generate().peer_id();
    hub.register(me, "node", Role::Computer);
    let browser = hub.browser();
    let info = browser.config().await.unwrap();
    let mut w = DeviceWatcher::new(browser, info.as_ref(), Duration::from_secs(60)).expecting(me);
    w.next().await.unwrap();
    hub.remove(&me);
    let e = tokio::time::timeout(Duration::from_secs(2), w.next()).await.unwrap().unwrap_err();
    assert!(e.is_device_removed(), "{e:?}");
}

#[tokio::test]
async fn resolve_tokens_only_in_urls_and_issued_for_old_registrations() {
    let hub = FakeHub::start().await;
    let key = SecretKey::generate();
    let peer = key.peer_id();
    let device_token = hub.register(peer, "node", Role::Computer);
    let pkarr = format!("{}/pkarr/{}", hub.url(), ember_hub::z32::encode(peer.as_bytes()));
    let http = reqwest::Client::new();

    // A device token in the query is refused (401 invalid_credentials); a resolve token passes
    // authentication (404: nothing published yet).
    let r = http.get(format!("{pkarr}?token={device_token}")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["code"], "invalid_credentials");
    let resolve = hub.resolve_token(&peer).unwrap();
    assert!(resolve.starts_with("dpr_"));
    assert_eq!(http.get(format!("{pkarr}?token={resolve}")).send().await.unwrap().status(), 404);

    // A registration from before resolve tokens gets one with its device token; the old one
    // stops working.
    let mut reg = Registration {
        hub_url: hub.url().to_string(),
        device: hub.devices().into_iter().find(|d| d.endpoint_id == peer).unwrap(),
        device_token,
        resolve_token: None,
        registered_at: ember_hub::now_secs(),
        revoked_at: None,
    };
    assert!(ensure_resolve_token(&mut reg).await.unwrap());
    let fresh = reg.directory_token().unwrap();
    assert_ne!(fresh, resolve);
    assert!(!ensure_resolve_token(&mut reg).await.unwrap(), "kept when present");
    assert_eq!(http.get(format!("{pkarr}?token={resolve}")).send().await.unwrap().status(), 401);
    assert_eq!(http.get(format!("{pkarr}?token={fresh}")).send().await.unwrap().status(), 404);
}

#[tokio::test]
async fn devices_rename_report_their_app_and_leave_by_themselves() {
    let hub = FakeHub::start().await;
    let key = SecretKey::generate();
    let me = key.peer_id();
    let own = hub.client().with_token(&hub.register(me, "node", Role::Computer));
    let other = hub.client().with_token(&hub.register(SecretKey::generate().peer_id(), "laptop", Role::Client));

    let app = DeviceApp::new("ember-node", "0.1.0", &["ember-node/1"]);
    let d = own.set_app(&me, Some(&app)).await.unwrap();
    assert_eq!(d.app.as_ref(), Some(&app));
    assert_eq!(own.devices().await.unwrap().iter().find(|d| d.endpoint_id == me).unwrap().app.as_ref(), Some(&app));
    // Only the device itself sets its app, even with account rights.
    assert!(matches!(hub.browser().set_app(&me, Some(&app)).await, Err(HubError::Forbidden(_))));
    assert!(matches!(other.set_app(&me, None).await, Err(HubError::Forbidden(_))));

    assert_eq!(own.rename_device(&me, "gpu box").await.unwrap().name, "gpu box");
    assert!(matches!(other.rename_device(&me, "x").await, Err(HubError::Forbidden(_))));

    // A client has no account rights.
    assert!(matches!(other.remove_device(&me).await, Err(HubError::Forbidden(_))));
    // The device leaves by itself; its token then reports the removal.
    own.remove_device(&me).await.unwrap();
    assert!(hub.devices().iter().all(|d| d.endpoint_id != me));
    assert!(own.me().await.unwrap_err().is_device_removed());
}
