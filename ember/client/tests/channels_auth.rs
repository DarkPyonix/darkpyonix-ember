//! FR-S11: the channel calls go through the common Bearer path (`Api` default headers).

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use ember_client::api::Api;

type Seen = Arc<Mutex<Vec<Option<String>>>>;

async fn recorder() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let app = axum::Router::new().fallback(move |headers: HeaderMap| {
        let log = log.clone();
        async move {
            let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
            log.lock().unwrap().push(auth);
            StatusCode::NOT_FOUND
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

async fn call_all(api: &Api) {
    // The recorder answers 404, so the results are errors; only the request headers matter.
    let _ = api.channels(None).await;
    let _ = api.channel("c1").await;
    let _ = api.channel_messages("c1", &ember_client::channels::MessagesQuery::default()).await;
    let _ = api.post_channel_message("c1", &ember_client::channels::NewChannelMessage { text: "hi".into(), from: None }).await;
}

#[tokio::test]
async fn fr_s11_channel_calls_carry_the_bearer_token() {
    let (url, seen) = recorder().await;
    let api = Api::with_token(&url, Some("tok123".into())).unwrap();
    call_all(&api).await;
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(seen.iter().all(|a| a.as_deref() == Some("Bearer tok123")), "{seen:?}");
    }

    seen.lock().unwrap().clear();
    let bare = Api::with_token(&url, None).unwrap();
    call_all(&bare).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    assert!(seen.iter().all(|a| a.is_none()), "{seen:?}");
}
