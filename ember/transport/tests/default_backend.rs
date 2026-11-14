//! SPEC FR-N5, INTENT D36: letify-link is the default backend.

use ember_transport::{BackendKind, SecretKey, Transport, TransportConfig};

#[test]
fn fr_n5_default_backend_is_letify() {
    assert_eq!(BackendKind::default(), BackendKind::Letify);
    let cfg = TransportConfig::new(SecretKey::generate());
    assert_eq!(cfg.backend, BackendKind::Letify);
}

#[tokio::test]
async fn fr_n5_bind_without_a_backend_choice_uses_letify() {
    // No `.backend(..)`: the default config must bind on the letify backend (a TCP listener).
    let cfg = TransportConfig::new(SecretKey::generate()).bind_addr("127.0.0.1:0".parse().unwrap());
    let t = Transport::bind(cfg).await.unwrap();
    let addr = t.local_addr();
    assert!(
        addr.direct.iter().any(|a| a.port() != 0),
        "the letify backend advertises its TCP listener: {addr:?}"
    );
}
