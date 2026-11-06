//! The accept loop of a door: the connection cap, the header-read timeout, the peer address in
//! every request and the graceful shutdown. Moved without change from `guest_door.rs`
//! (`GuestDoor::serve_capped`, which now calls this); the tests in
//! `ember/server/tests/fr_t14_guest_limits.rs` and `fr_t14_guest_door_characterization.rs` pin it.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::connect_info::ConnectInfo;
use axum::Router;

/// `label` names the door in the three log lines (`"guest door"` for the guest door).
pub async fn serve_capped(
    label: &'static str,
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    header_read_timeout: Duration,
    max_connections: usize,
) -> std::io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use hyper_util::server::graceful::GracefulShutdown;
    use tower::ServiceExt;

    let graceful = GracefulShutdown::new();
    let conns = Arc::new(tokio::sync::Semaphore::new(max_connections));
    let mut dropped: u64 = 0;
    let mut shutdown = Box::pin(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(e) => {
                    // Out of descriptors and the like: back off, never spin, never stop the door.
                    tracing::warn!("{label} accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = &mut shutdown => break,
        };
        let Ok(permit) = conns.clone().try_acquire_owned() else {
            dropped += 1;
            tracing::debug!("{label} at its connection cap, dropped {peer} ({dropped} dropped so far)");
            drop(stream);
            continue;
        };
        let app = app.clone();
        let svc = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
            req.extensions_mut().insert(ConnectInfo(peer));
            app.clone().oneshot(req)
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder.timer(TokioTimer::new());
        builder.header_read_timeout(header_read_timeout);
        let conn = graceful.watch(builder.serve_connection(TokioIo::new(stream), svc));
        tokio::spawn(async move {
            let _permit = permit; // held for the life of the connection
            if let Err(e) = conn.await {
                tracing::debug!("{label} connection closed: {e}");
            }
        });
    }
    drop(listener);
    graceful.shutdown().await;
    Ok(())
}
