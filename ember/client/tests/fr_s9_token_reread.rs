//! FR-S9, #369: after the server rotates `server-token`, a client re-reads the file on a 401 and
//! retries once, so it keeps working without a restart. A token given by the caller is not re-read.

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use ember_client::api::Api;

/// A server that accepts only `Bearer <current>` and records every header it saw.
async fn server(current: Arc<Mutex<String>>) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let log = seen.clone();
    let app = axum::Router::new().fallback(move |headers: HeaderMap| {
        let (log, current) = (log.clone(), current.clone());
        async move {
            let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
            log.lock().unwrap().push(auth.clone());
            if auth.as_deref() == Some(&format!("Bearer {}", current.lock().unwrap())) {
                StatusCode::ACCEPTED
            } else {
                StatusCode::UNAUTHORIZED
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn fr_s9_client_rereads_a_rotated_token_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("server-token");
    std::fs::write(&file, "old-token\n").unwrap();
    let current = Arc::new(Mutex::new("old-token".to_string()));
    let (url, seen) = server(current.clone()).await;
    let api = Api::with_token_file(&url, &file).unwrap();
    let clone = api.clone();

    api.send_message("s1", "hi").await.expect("old token passes before the rotation");

    // The server rotates: new token in memory and in the file.
    *current.lock().unwrap() = "new-token".into();
    std::fs::write(&file, "new-token\n").unwrap();

    api.send_message("s1", "hi").await.expect("the client re-reads the file and retries");
    {
        let seen = seen.lock().unwrap();
        let tail: Vec<_> = seen.iter().rev().take(2).rev().cloned().collect();
        assert_eq!(tail, vec![Some("Bearer old-token".into()), Some("Bearer new-token".into())]);
    }
    // A clone shares the re-read token: one request, no 401 first.
    let before = seen.lock().unwrap().len();
    clone.send_message("s1", "hi").await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), before + 1);
    assert_eq!(seen.lock().unwrap().last().unwrap().as_deref(), Some("Bearer new-token"));
}

#[tokio::test]
async fn fr_s9_a_wrong_token_file_is_retried_once_only() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("server-token");
    std::fs::write(&file, "stale\n").unwrap();
    let current = Arc::new(Mutex::new("other".to_string()));
    let (url, seen) = server(current).await;
    let api = Api::with_token_file(&url, &file).unwrap();
    // The file is unchanged, so there is nothing new to retry with: one request, a 401.
    assert!(api.send_message("s1", "hi").await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn fr_s9_a_given_token_is_not_reread() {
    let dir = tempfile::tempdir().unwrap();
    let current = Arc::new(Mutex::new("new-token".to_string()));
    let (url, seen) = server(current).await;
    let _ = dir;
    let api = Api::with_token(&url, Some("old-token".into())).unwrap();
    assert!(api.send_message("s1", "hi").await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 1);
}
