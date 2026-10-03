//! The device-link flow end to end against the fake hub (FR-N2: a device joins the user's
//! GitHub account without entering an address).

use std::time::Duration;

use ember_hub::fake::FakeHub;
use ember_hub::{check_registration, DeviceLink, HubError, LinkError, LinkPoll, RegistrationState, Role};
use ember_transport::SecretKey;

const FAST: Duration = Duration::from_millis(50);

#[tokio::test]
async fn approved_in_the_browser_then_claimed_once() {
    let hub = FakeHub::start().await;
    let key = SecretKey::generate();
    let link = DeviceLink::start(&hub.client(), &key, "mac mini", Role::Computer).await.unwrap().with_poll_interval(FAST);
    let p = link.pending().clone();
    assert_eq!(p.verification_uri, format!("{}/link", hub.url()));
    assert!(p.verification_uri_complete.ends_with(&p.user_code));
    assert!(matches!(link.poll().await.unwrap(), LinkPoll::Pending));

    // The signed-in person looks the code up (case and dash do not matter) and approves it.
    let info = hub.browser().link_code(&p.user_code.to_lowercase().replace('-', "")).await.unwrap();
    assert_eq!((info.endpoint_id, info.name.as_str(), info.role), (key.peer_id(), "mac mini", Role::Computer));
    hub.browser().decide_link_code(&p.user_code, true).await.unwrap();

    let reg = link.wait().await.unwrap();
    assert_eq!(reg.endpoint_id(), key.peer_id());
    assert_eq!(reg.device.role, Role::Computer);
    assert_eq!(reg.hub_url, hub.url());
    assert!(reg.device_token.starts_with("dpd_"));
    assert!(!format!("{reg:?}").contains(&reg.device_token));

    // The token works and says who we are.
    match check_registration(&reg.client()).await {
        RegistrationState::Active(me) => {
            assert_eq!(me.via, "device");
            assert_eq!(me.endpoint_id, Some(key.peer_id()));
        }
        other => panic!("expected active, got {other:?}"),
    }
    let devices = reg.client().devices().await.unwrap();
    assert_eq!(devices.len(), 1);

    // Claimed exactly once; the key cannot join again.
    assert!(matches!(link.poll().await, Err(LinkError::Expired(_))));
    assert!(matches!(
        DeviceLink::start(&hub.client(), &key, "again", Role::Computer).await,
        Err(LinkError::AlreadyRegistered(_))
    ));
}

#[tokio::test]
async fn the_main_server_approves_a_computer_and_a_computer_cannot() {
    let hub = FakeHub::start().await;
    let server = SecretKey::generate();
    let server_token = hub.register(server.peer_id(), "home server", Role::MainServer);
    let other = SecretKey::generate();
    let other_token = hub.register(other.peer_id(), "laptop", Role::Computer);

    let key = SecretKey::generate();
    let link = DeviceLink::start(&hub.client(), &key, "pi", Role::Computer).await.unwrap().with_poll_interval(FAST);
    let code = link.user_code().to_string();

    // A computer's token has no account rights.
    let computer = hub.client().with_token(&other_token);
    assert!(matches!(computer.link_code(&code).await, Err(HubError::Forbidden(_))));
    assert!(matches!(computer.decide_link_code(&code, true).await, Err(HubError::Forbidden(_))));

    let main = hub.client().with_token(&server_token);
    assert_eq!(main.link_code(&code).await.unwrap().endpoint_id, key.peer_id());
    main.decide_link_code(&code, true).await.unwrap();
    let reg = link.wait().await.unwrap();
    assert_eq!(reg.device.name, "pi");
    // A decided code is gone.
    assert!(matches!(main.link_code(&code).await, Err(HubError::NotFound(_))));
}

#[tokio::test]
async fn denied_expired_and_wrong_key() {
    let hub = FakeHub::start().await;

    let key = SecretKey::generate();
    let link = DeviceLink::start(&hub.client(), &key, "x", Role::Computer).await.unwrap().with_poll_interval(FAST);
    assert!(hub.decide(link.user_code(), false));
    assert!(matches!(link.wait().await, Err(LinkError::Denied)));

    // Proof of possession: a link polled with another key is refused.
    let key2 = SecretKey::generate();
    let link2 = DeviceLink::start(&hub.client(), &key2, "y", Role::Computer).await.unwrap();
    let thief = DeviceLink::resume(&hub.client(), &SecretKey::generate(), link2.pending().clone());
    assert!(matches!(thief.poll().await, Err(LinkError::Hub(HubError::BadRequest(_)))));
    assert!(matches!(link2.poll().await.unwrap(), LinkPoll::Pending));

    // Expiry.
    hub.set_link_ttl(-1);
    let key3 = SecretKey::generate();
    let link3 = DeviceLink::start(&hub.client(), &key3, "z", Role::Computer).await.unwrap().with_poll_interval(FAST);
    assert!(matches!(link3.wait().await, Err(LinkError::Expired(_))));
    assert!(hub.pending_codes().iter().all(|c| c != link3.user_code()));

    // Malformed requests are 400s.
    assert!(matches!(
        DeviceLink::start(&hub.client(), &SecretKey::generate(), "", Role::Computer).await,
        Err(LinkError::Hub(HubError::BadRequest(_)))
    ));
}
