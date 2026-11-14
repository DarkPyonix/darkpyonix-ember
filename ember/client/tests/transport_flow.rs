//! The client against a real ember-server router served over the in-memory fake transport
//! (SPEC FR-N1, FR-N3, PR-1): HTTP calls and the push socket run on transport streams; a
//! revoked device loses its push connection and cannot reconnect.

use std::sync::Arc;
use std::time::Duration;

use ember_client::api::Api;
use ember_client::transcript::TranscriptItem;
use ember_client::wire::{self, ApprovalDecision, NewSession};
use ember_client::{Client, ClientConfig, ConnectionState};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind};
use ember_server::devices::Devices;
use ember_server::events::SessionStatus;
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_transport::mem::MemNetwork;
use ember_transport::Dialer;

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn config() -> ClientConfig {
    // `base_url` is unused with `Client::with_api`.
    let mut c = ClientConfig::new("http://unused.invalid");
    c.lease_interval = Duration::from_millis(50);
    c.backoff_min = Duration::from_millis(20);
    c.backoff_max = Duration::from_millis(200);
    c.resync_settle = Duration::from_millis(50);
    c.cache_debounce = Duration::from_millis(10);
    c
}

fn pending_approval(c: &Client, id: &str) -> Option<String> {
    c.read(|st| {
        st.transcript(id)?.pending_approvals().next().and_then(|i| match i {
            TranscriptItem::Approval { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
    })
}

#[tokio::test]
async fn client_syncs_over_the_transport_and_revocation_cuts_it_off() {
    let net = MemNetwork::new();
    let (server_t, device_t) = (net.transport(), net.transport());

    let store = Arc::new(Store::open_in_memory().unwrap());
    let devices = Devices::open(store.clone()).unwrap();
    devices.add(device_t.peer_id(), "phone").unwrap();
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let s = Sessions::new(store, adapters);
    let serve =
        ember_server::transport::serve(&server_t, ember_server::api::router(s.clone()), devices.gate().clone())
            .unwrap();
    tokio::spawn(serve);

    let api = Api::over_transport(Dialer::new(device_t.clone()), server_t.peer_id());
    assert!(api.base_url().starts_with("peer:"));
    assert!(api.health().await.unwrap().ok);

    let c = Client::with_api(config(), api).await;
    c.start();
    until("connected", || c.read(|st| *st.connection() == ConnectionState::Connected)).await;

    // HTTP over the transport.
    let a = c
        .create_session(&NewSession {
            project: "acme".into(),
            agent: "scripted".into(),
            cwd: std::env::temp_dir().to_string_lossy().into(),
            model: None,
            title: Some("A".into()),
        })
        .await
        .unwrap()
        .id;
    // Push over the transport: a session created on the server side arrives.
    let b = s
        .create(ember_server::session::NewSession {
            project: "acme".into(),
            agent: AgentKind::Scripted,
            cwd: std::env::temp_dir(),
            model: None,
            title: "B".into(),
        })
        .unwrap()
        .id;
    until("B pushed", || c.read(|st| st.session(&b).is_some())).await;

    // A turn with an approval, observed through push.
    c.open_session(&a);
    c.send_message(&a, "hello").await.unwrap();
    until("approval shown", || pending_approval(&c, &a).is_some()).await;
    let approval = pending_approval(&c, &a).unwrap();
    c.answer(&a, &approval, ApprovalDecision::AllowOnce).await.unwrap();
    until("finished on server", || s.store().session(&a).unwrap().unwrap().status == SessionStatus::Finished).await;
    until("finished pushed", || {
        c.read(|st| st.session(&a).map(|v| v.record.status) == Some(wire::SessionStatus::Finished))
    })
    .await;

    // Revoke the device: the push connection drops and reconnects keep failing.
    assert!(devices.remove(&device_t.peer_id()).unwrap() >= 1);
    until("push lost", || {
        c.read(|st| matches!(st.connection(), ConnectionState::Reconnecting { .. } | ConnectionState::Connecting { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!c.read(|st| *st.connection() == ConnectionState::Connected));
    assert!(c.api().health().await.is_err());
    c.stop();
}
