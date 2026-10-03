//! Revocation on the hub (device removed) is detected and surfaced.

use std::time::Duration;

use ember_hub::fake::FakeHub;
use ember_hub::{check_registration, watch_registration, HubError, RegistrationState, Role};
use ember_transport::SecretKey;

#[tokio::test]
async fn a_removed_device_sees_revoked() {
    let hub = FakeHub::start().await;
    let key = SecretKey::generate();
    let token = hub.register(key.peer_id(), "node", Role::Computer);
    let client = hub.client().with_token(&token);
    assert!(matches!(check_registration(&client).await, RegistrationState::Active(_)));

    let (mut rx, task) = watch_registration(client.clone(), Duration::from_millis(50));
    rx.wait_for(|s| matches!(s, RegistrationState::Active(_))).await.unwrap();

    // The owner removes it (in the browser, or the main server with its token).
    let server = SecretKey::generate();
    let main = hub.client().with_token(&hub.register(server.peer_id(), "server", Role::MainServer));
    main.remove_device(&key.peer_id()).await.unwrap();
    assert!(hub.devices().iter().all(|d| d.endpoint_id != key.peer_id()));

    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(RegistrationState::is_revoked)).await.unwrap().unwrap();
    // The watcher stops after a revocation (it is final).
    tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
    assert!(matches!(client.devices().await, Err(HubError::Unauthorized(_))));
    assert_eq!(check_registration(&client).await, RegistrationState::Revoked);

    // An unreachable hub is not a revocation.
    drop(hub);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(matches!(check_registration(&client).await, RegistrationState::Unreachable(_)));
}

#[tokio::test]
async fn no_token_is_not_registered() {
    let hub = FakeHub::start().await;
    assert!(matches!(hub.client().devices().await, Err(HubError::NotRegistered)));
    assert!(matches!(hub.client().with_token("dpd_bogus").me().await, Err(HubError::Unauthorized(_))));
}
