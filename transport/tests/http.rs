//! An axum router (HTTP + WebSocket) served over transport streams: fake and iroh.
#![cfg(feature = "http")]

mod common;

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::routing::{get, post};
use axum::Router;
use ember_transport::http::{http1_client, HttpListener};
use ember_transport::mem::MemNetwork;
use ember_transport::{PeerId, Transport};
use futures::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;

const SERVICE: &str = "ember/http/0";

fn router() -> Router {
    async fn ws(ws: WebSocketUpgrade) -> axum::response::Response {
        ws.on_upgrade(|mut socket: WebSocket| async move {
            while let Some(Ok(msg)) = socket.recv().await {
                if let Message::Text(t) = msg {
                    let reply = format!("echo:{}", t.as_str());
                    if socket.send(Message::Text(reply.into())).await.is_err() {
                        break;
                    }
                }
            }
        })
    }
    Router::new()
        .route(
            "/whoami",
            get(|ConnectInfo(peer): ConnectInfo<PeerId>| async move { peer.to_string() }),
        )
        .route("/upper", post(|body: String| async move { body.to_uppercase() }))
        .route("/ws", get(ws))
}

fn serve(server: &Transport) {
    let listener = HttpListener::new(server.listen(SERVICE).unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router().into_make_service_with_connect_info::<PeerId>())
            .await
            .unwrap();
    });
}

async fn exercise(client: &Transport, server: &Transport) {
    let conn = client.connect(server.peer_id(), SERVICE).await.unwrap();

    // Plain HTTP; two sequential requests reuse one stream.
    let mut http = http1_client::<Full<Bytes>>(&conn).await.unwrap();
    let req = hyper::Request::get("http://peer/whoami").body(Full::default()).unwrap();
    let resp = http.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), client.peer_id().to_string());

    let req = hyper::Request::post("http://peer/upper")
        .body(Full::new(Bytes::from_static(b"over the transport")))
        .unwrap();
    let resp = http.send_request(req).await.unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"OVER THE TRANSPORT");

    // WebSocket on its own stream.
    let stream = conn.open_bi().await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://peer/ws", stream).await.unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::text("hi")).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply.into_text().unwrap().as_str(), "echo:hi");
    ws.close(None).await.ok();
}

#[tokio::test]
async fn axum_over_fake_transport() {
    let net = MemNetwork::new();
    let (client, server) = (net.transport(), net.transport());
    serve(&server);
    exercise(&client, &server).await;
}

#[cfg(feature = "iroh")]
#[tokio::test]
async fn axum_over_iroh_localhost() {
    let (client, server, _dir) = common::iroh_pair().await;
    serve(&server);
    tokio::time::timeout(Duration::from_secs(20), exercise(&client, &server))
        .await
        .unwrap();
    client.close().await;
    server.close().await;
}
