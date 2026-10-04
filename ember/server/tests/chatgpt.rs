//! Sign in with ChatGPT end to end (FR-U4) against an in-process fake of OpenAI's auth server and
//! Responses API: PKCE and state, callback validation, encrypted tokens that never reach a
//! response, refresh, request shaping, the usage cap, and the hosted switch.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ember_server::accounts::secrets::SecretBox;
use ember_server::accounts::Accounts;
use ember_server::chatgpt::{self, ChatGpt, ChatGptConfig, ChatGptError};
use ember_server::store::Store;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const ISSUED_CLIENT: &str = "issued-client-42";

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Ok,
    LimitHttp,
    LimitStream,
}

/// The fake's knobs and what it saw.
#[derive(Default)]
struct Fake {
    base: Mutex<String>,
    /// Query of the last authorize request.
    authorize: Mutex<HashMap<String, String>>,
    token_requests: Mutex<Vec<HashMap<String, String>>>,
    /// (Authorization header, body) of each Responses request.
    responses: Mutex<Vec<(String, Value)>>,
    revoked: Mutex<Vec<HashMap<String, String>>>,
    mode: Mutex<Mode>,
    expires_in: Mutex<i64>,
    refresh_fails: Mutex<bool>,
    bad_nonce: Mutex<bool>,
    omit_client_id: Mutex<bool>,
    deny: Mutex<bool>,
    issued: AtomicUsize,
}

type F = Arc<Fake>;

fn jwt(claims: Value) -> String {
    format!(
        "{}.{}.fake-signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

/// Plays the browser's part too: answers the authorize request with the redirect to the app.
async fn authorize(State(f): State<F>, Query(q): Query<HashMap<String, String>>) -> Response {
    *f.authorize.lock().unwrap() = q.clone();
    let mut target = reqwest::Url::parse(&q["redirect_uri"]).unwrap();
    {
        let mut pairs = target.query_pairs_mut();
        if *f.deny.lock().unwrap() {
            pairs.append_pair("error", "access_denied");
            pairs.append_pair("error_description", "The user declined");
        } else {
            pairs.append_pair("code", "CODE-1");
            pairs.append_pair("scope", "chatgpt.tokens.use.direct email offline_access openid profile resource.invoke");
            if !*f.omit_client_id.lock().unwrap() {
                pairs.append_pair("client_id", ISSUED_CLIENT);
            }
        }
        pairs.append_pair("state", &q["state"]);
    }
    Redirect::to(target.as_str()).into_response()
}

fn token_error(code: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": code }))).into_response()
}

async fn token(State(f): State<F>, Form(p): Form<HashMap<String, String>>) -> Response {
    f.token_requests.lock().unwrap().push(p.clone());
    if p.get("resource").map(String::as_str) != Some("https://api.openai.com/v1") {
        return token_error("invalid_target");
    }
    if p.get("client_id").map(String::as_str) != Some(ISSUED_CLIENT) {
        return token_error("invalid_client");
    }
    let n = f.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let expires_in = *f.expires_in.lock().unwrap();
    match p.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let a = f.authorize.lock().unwrap().clone();
            let verifier = p.get("code_verifier").cloned().unwrap_or_default();
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            if p.get("code").map(String::as_str) != Some("CODE-1")
                || a.get("code_challenge") != Some(&challenge)
                || a.get("code_challenge_method").map(String::as_str) != Some("S256")
                || p.get("redirect_uri") != a.get("redirect_uri")
            {
                return token_error("invalid_grant");
            }
            let nonce = if *f.bad_nonce.lock().unwrap() {
                "someone-elses".to_string()
            } else {
                a["nonce"].clone()
            };
            let id_token = jwt(json!({
                "iss": *f.base.lock().unwrap(),
                "aud": ISSUED_CLIENT,
                "sub": "user-123",
                "email": "me@example.com",
                "nonce": nonce,
                "exp": ember_server::store::now_ms() / 1000 + 3600,
                "iat": ember_server::store::now_ms() / 1000,
            }));
            Json(json!({
                "access_token": format!("AT-SECRET-{n}"),
                "refresh_token": format!("RT-SECRET-{n}"),
                "id_token": id_token,
                "token_type": "Bearer",
                "expires_in": expires_in,
                "scope": "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            }))
            .into_response()
        }
        Some("refresh_token") => {
            if *f.refresh_fails.lock().unwrap() {
                return token_error("invalid_grant");
            }
            if !p.get("refresh_token").is_some_and(|t| t.starts_with("RT-SECRET-")) {
                return token_error("invalid_refresh_token");
            }
            Json(json!({
                "access_token": format!("AT-SECRET-{n}"),
                "refresh_token": format!("RT-SECRET-{n}"),
                "token_type": "Bearer",
                "expires_in": 3600,
            }))
            .into_response()
        }
        _ => token_error("unsupported_grant_type"),
    }
}

async fn discovery(State(f): State<F>) -> Json<Value> {
    let base = f.base.lock().unwrap().clone();
    Json(json!({ "issuer": base, "revocation_endpoint": format!("{base}/oauth/revoke") }))
}

async fn revoke(State(f): State<F>, Form(p): Form<HashMap<String, String>>) -> StatusCode {
    f.revoked.lock().unwrap().push(p);
    StatusCode::OK
}

async fn responses(State(f): State<F>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    f.responses.lock().unwrap().push((auth, body));
    let sse = |events: &[Value]| {
        let text: String = events
            .iter()
            .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
            .collect();
        ([("content-type", "text/event-stream")], text).into_response()
    };
    match *f.mode.lock().unwrap() {
        Mode::Ok => sse(&[
            json!({"type": "response.created", "response": {"id": "r1"}}),
            json!({"type": "response.output_text.delta", "delta": "Hel"}),
            json!({"type": "response.output_text.delta", "delta": "lo"}),
            json!({"type": "response.completed",
                   "response": {"id": "r1", "usage": {"input_tokens": 12, "output_tokens": 3}}}),
        ]),
        Mode::LimitHttp => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": {
                "code": "subscription_sharing_usage_limit_exceeded",
                "message": "Ember reached its weekly share of your ChatGPT usage"
            }})),
        )
            .into_response(),
        Mode::LimitStream => sse(&[
            json!({"type": "response.created", "response": {"id": "r2"}}),
            json!({"type": "response.failed", "response": {"id": "r2", "error": {
                "code": "subscription_sharing_usage_limit_exceeded",
                "message": "weekly cap reached"
            }}}),
        ]),
    }
}

async fn models() -> Json<Value> {
    Json(json!({"models": [
        {"slug": "gpt-x", "display_name": "GPT X", "visibility": "list"},
        {"slug": "internal", "visibility": "hide"}
    ]}))
}

async fn serve(router: Router) -> SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    addr
}

struct Env {
    _dir: tempfile::TempDir,
    fake: F,
    chatgpt: Arc<ChatGpt>,
    /// The ember server (ChatGPT and accounts routes) on a real loopback port.
    ember: String,
    http: reqwest::Client,
}

async fn env_with(enabled: bool) -> Env {
    let fake: F = Arc::default();
    *fake.expires_in.lock().unwrap() = 3600;
    let fake_router = Router::new()
        .route("/api/accounts/authorize", get(authorize))
        .route("/api/accounts/oauth/token", post(token))
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/oauth/revoke", post(revoke))
        .route("/v1/responses", post(responses))
        .route("/v1/models", get(models))
        .with_state(fake.clone());
    let fake_addr = serve(fake_router).await;
    let base = format!("http://{fake_addr}");
    *fake.base.lock().unwrap() = base.clone();

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let accounts =
        Accounts::with_secrets(store, &dir.path().join("accounts"), SecretBox::ephemeral())
            .unwrap();

    // Bind ember first: its port is the redirect port.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ember_addr = l.local_addr().unwrap();
    let config = ChatGptConfig {
        enabled,
        issuer: base.clone(),
        authorize_url: format!("{base}/api/accounts/authorize"),
        token_url: format!("{base}/api/accounts/oauth/token"),
        discovery_url: format!("{base}/.well-known/openid-configuration"),
        api_base: format!("{base}/v1"),
        resource: "https://api.openai.com/v1".into(),
        redirect_port: ember_addr.port(),
        agent_name: "DarkPyonix Ember".into(),
    };
    let chatgpt = ChatGpt::new(accounts.clone(), config).unwrap();
    let app = chatgpt::api::router(chatgpt.clone())
        .merge(ember_server::accounts::api::router(accounts));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    Env {
        _dir: dir,
        fake,
        chatgpt,
        ember: format!("http://{ember_addr}"),
        http: reqwest::Client::new(),
    }
}

async fn env() -> Env {
    env_with(true).await
}

impl Env {
    async fn start(&self, body: Value) -> Value {
        let r = self
            .http
            .post(format!("{}/api/chatgpt/signin", self.ember))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 201);
        r.json().await.unwrap()
    }

    /// The browser: open the authorization URL and follow the redirect to the callback.
    async fn browse(&self, authorization_url: &str) -> (u16, String) {
        let r = self.http.get(authorization_url).send().await.unwrap();
        (r.status().as_u16(), r.text().await.unwrap())
    }

    async fn get_json(&self, path: &str) -> (u16, String) {
        let r = self.http.get(format!("{}{path}", self.ember)).send().await.unwrap();
        (r.status().as_u16(), r.text().await.unwrap())
    }

    async fn sign_in(&self) -> String {
        let s = self.start(json!({"label": "me"})).await;
        let (code, page) = self.browse(s["authorization_url"].as_str().unwrap()).await;
        assert_eq!(code, 200, "{page}");
        let accounts = self.chatgpt.list().unwrap();
        accounts.last().unwrap().id.clone()
    }
}

fn assert_no_secrets(text: &str) {
    for needle in ["AT-SECRET", "RT-SECRET", "fake-signature", "CODE-1"] {
        assert!(!text.contains(needle), "{needle} leaked: {text}");
    }
}

#[tokio::test]
async fn sign_in_uses_pkce_and_state_and_tokens_never_leave_the_server() {
    let e = env().await;
    let s = e.start(json!({"label": "me"})).await;
    let url = reqwest::Url::parse(s["authorization_url"].as_str().unwrap()).unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(q["client_id"], "dynamic_agent_client");
    assert_eq!(q["agent_name_hint"], "DarkPyonix Ember");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["state"], s["state"].as_str().unwrap());
    assert!(q["ext_agent_host_id"].starts_with("urn:uuid:"));
    assert_eq!(q["resource"], "https://api.openai.com/v1");
    assert!(q["scope"].split(' ').any(|x| x == "chatgpt.tokens.use.direct"));
    let port = e.ember.rsplit(':').next().unwrap();
    assert_eq!(q["redirect_uri"], format!("http://127.0.0.1:{port}/auth/callback"));
    // The verifier itself never leaves the server.
    assert_eq!(q["code_challenge"].len(), 43);
    assert!(!q.contains_key("code_verifier"));

    let (code, page) = e.browse(url.as_str()).await;
    assert_eq!(code, 200, "{page}");
    assert!(page.contains("me@example.com"), "{page}");
    assert_no_secrets(&page);
    // The fake accepted the verifier for the challenge (it answers invalid_grant otherwise).
    let tr = e.fake.token_requests.lock().unwrap().clone();
    assert_eq!(tr.len(), 1);
    assert_eq!(tr[0]["grant_type"], "authorization_code");
    assert_eq!(tr[0]["client_id"], ISSUED_CLIENT);

    let (code, st) = e
        .get_json(&format!("/api/chatgpt/signin/{}", s["state"].as_str().unwrap()))
        .await;
    assert_eq!(code, 200);
    let st: Value = serde_json::from_str(&st).unwrap();
    assert_eq!(st["status"], "signed_in");

    let (code, list) = e.get_json("/api/chatgpt/accounts").await;
    assert_eq!(code, 200);
    assert_no_secrets(&list);
    let list: Value = serde_json::from_str(&list).unwrap();
    let acc = &list[0];
    assert_eq!(acc["kind"], "chatgpt");
    assert_eq!(acc["client_id"], ISSUED_CLIENT);
    assert_eq!(acc["email"], "me@example.com");
    assert_eq!(acc["plan_usage"], true);
    assert_eq!(acc["status"], "signed_in");
    assert_eq!(acc["weekly_cap"]["known"], false);
    let id = acc["id"].as_str().unwrap().to_string();

    // Inside the server the token is usable.
    assert_eq!(e.chatgpt.access_token(&id).await.unwrap().as_str(), "AT-SECRET-1");

    // The same callback again: its state is spent.
    let (code, page) = e.browse(url.as_str()).await;
    assert_eq!(code, 400, "{page}");
    assert!(page.contains("unknown or expired"), "{page}");

    // Signing in again as the same user updates the account instead of adding one.
    e.sign_in().await;
    assert_eq!(e.chatgpt.list().unwrap().len(), 1);
}

#[tokio::test]
async fn callback_is_validated() {
    let e = env().await;
    let s = e.start(json!({})).await;
    let state = s["state"].as_str().unwrap();

    // Missing and forged state: refused, and the real sign-in stays pending.
    let (code, _) = e.get_json("/auth/callback?code=CODE-1").await;
    assert_eq!(code, 400);
    let (code, page) = e
        .get_json("/auth/callback?code=CODE-1&state=forged&client_id=issued-client-42")
        .await;
    assert_eq!(code, 400);
    assert!(page.contains("unknown or expired"), "{page}");
    let (_, st) = e.get_json(&format!("/api/chatgpt/signin/{state}")).await;
    assert!(st.contains("pending"), "{st}");
    assert!(e.fake.token_requests.lock().unwrap().is_empty());

    // An error from OpenAI: reported, nothing exchanged, the attempt is over.
    let (code, page) = e
        .get_json(&format!(
            "/auth/callback?error=access_denied&error_description=The+user+declined&state={state}"
        ))
        .await;
    assert_eq!(code, 400);
    assert!(page.contains("access_denied"), "{page}");
    let (_, st) = e.get_json(&format!("/api/chatgpt/signin/{state}")).await;
    assert!(st.contains("failed") && st.contains("access_denied"), "{st}");
    assert!(e.fake.token_requests.lock().unwrap().is_empty());

    // A first sign-in whose callback carries no issued client_id.
    *e.fake.omit_client_id.lock().unwrap() = true;
    let s = e.start(json!({})).await;
    let (code, page) = e.browse(s["authorization_url"].as_str().unwrap()).await;
    assert_eq!(code, 400);
    assert!(page.contains("client_id"), "{page}");
    *e.fake.omit_client_id.lock().unwrap() = false;

    // An ID token minted for another sign-in (nonce mismatch).
    *e.fake.bad_nonce.lock().unwrap() = true;
    let s = e.start(json!({})).await;
    let (code, page) = e.browse(s["authorization_url"].as_str().unwrap()).await;
    assert_eq!(code, 400);
    assert!(page.contains("nonce"), "{page}");
    assert_no_secrets(&page);
    assert!(e.chatgpt.list().unwrap().is_empty());
    *e.fake.bad_nonce.lock().unwrap() = false;

    // Denied by the user through the full redirect.
    *e.fake.deny.lock().unwrap() = true;
    let s = e.start(json!({})).await;
    let (code, page) = e.browse(s["authorization_url"].as_str().unwrap()).await;
    assert_eq!(code, 400);
    assert!(page.contains("access_denied"), "{page}");
    *e.fake.deny.lock().unwrap() = false;

    // The pasted-URL path for a browser on another machine.
    let s = e.start(json!({})).await;
    let r = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(s["authorization_url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    let location = r.headers()["location"].to_str().unwrap().to_string();
    let r = e
        .http
        .post(format!("{}/api/chatgpt/signin/complete", e.ember))
        .json(&json!({ "callback_url": location }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body = r.text().await.unwrap();
    assert_no_secrets(&body);
    assert_eq!(e.chatgpt.list().unwrap().len(), 1);
}

#[tokio::test]
async fn hosted_server_never_offers_chatgpt_sign_in() {
    let e = env_with(false).await;
    let (code, body) = e.get_json("/api/chatgpt").await;
    assert_eq!(code, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["enabled"], false);
    assert!(v["reason"].as_str().unwrap().contains("self-hosted"));

    let r = e
        .http
        .post(format!("{}/api/chatgpt/signin", e.ember))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let (code, _) = e.get_json("/auth/callback?code=c&state=s").await;
    assert_eq!(code, 403);
    let (code, _) = e.get_json("/api/chatgpt/accounts").await;
    assert_eq!(code, 403);
    assert!(matches!(
        e.chatgpt.responses("x", json!({"model": "m", "input": []})).await,
        Err(ChatGptError::Disabled)
    ));
    assert!(e.fake.authorize.lock().unwrap().is_empty());
}

#[tokio::test]
async fn responses_are_shaped_streamed_and_counted() {
    let e = env().await;
    let id = e.sign_in().await;

    let mut stream = e
        .chatgpt
        .responses(
            &id,
            json!({
                "model": "gpt-x",
                "instructions": "be brief",
                "input": [{"role": "user", "content": "hi"}],
                "store": true,
                "stream": false,
                "tools": [{"type": "function", "name": "f", "parameters": {"type": "object"}}]
            }),
        )
        .await
        .unwrap();
    let mut text = String::new();
    let mut kinds = Vec::new();
    while let Some(ev) = stream.next().await {
        let ev = ev.unwrap();
        if ev.kind == "response.output_text.delta" {
            text += ev.data["delta"].as_str().unwrap();
        }
        kinds.push(ev.kind);
    }
    assert_eq!(text, "Hello");
    assert_eq!(kinds.last().unwrap(), "response.completed");

    let sent = e.fake.responses.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "Bearer AT-SECRET-1");
    assert_eq!(sent[0].1["store"], false);
    assert_eq!(sent[0].1["stream"], true);
    assert_eq!(sent[0].1["instructions"], "be brief");

    // Usage shows up with the other accounts' usage.
    let (_, usage) = e.get_json("/api/usage?days=1").await;
    let usage: Value = serde_json::from_str(&usage).unwrap();
    let row = usage
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account_id"] == id.as_str())
        .unwrap();
    assert_eq!((row["input_tokens"].as_u64(), row["output_tokens"].as_u64()), (Some(12), Some(3)));
    let (_, list) = e.get_json("/api/chatgpt/accounts").await;
    assert!(list.contains("\"tokens_today\":15"), "{list}");

    // Refused before any request leaves the server.
    for bad in [
        json!({"model": "gpt-x", "input": [], "tools": [{"type": "image_generation"}]}),
        json!({"model": "gpt-x", "input": [], "tools": [{"type": "file_search"}]}),
        json!({"model": "gpt-x", "input": [], "tools": [{"type": "code_interpreter"}]}),
        json!({"model": "gpt-x", "input": [], "tools": [{"type": "mcp", "server_url": "x"}]}),
        json!({"model": "gpt-x", "input": [], "previous_response_id": "r1"}),
        json!({"model": "gpt-x", "input": [], "temperature": 0.2}),
        json!({"model": "gpt-x", "input": "hi"}),
    ] {
        let err = e.chatgpt.responses(&id, bad.clone()).await.unwrap_err();
        assert!(matches!(err, ChatGptError::Unsupported(_)), "{bad}: {err}");
        assert_eq!(err.http_status(), 400);
    }
    assert_eq!(e.fake.responses.lock().unwrap().len(), 1);

    let models = e.chatgpt.models(&id).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["slug"], "gpt-x");
}

#[tokio::test]
async fn the_weekly_cap_pauses_the_account() {
    let e = env().await;
    let id = e.sign_in().await;
    let req = json!({"model": "gpt-x", "input": [{"role": "user", "content": "hi"}]});

    *e.fake.mode.lock().unwrap() = Mode::LimitHttp;
    let err = e.chatgpt.responses(&id, req.clone()).await.unwrap_err();
    assert!(matches!(err, ChatGptError::UsageLimited { .. }), "{err}");
    assert_eq!(err.http_status(), 429);
    let (_, list) = e.get_json("/api/chatgpt/accounts").await;
    assert!(list.contains("\"limited\":true"), "{list}");

    // Paused: the next request does not reach OpenAI.
    let before = e.fake.responses.lock().unwrap().len();
    assert!(matches!(
        e.chatgpt.responses(&id, req.clone()).await,
        Err(ChatGptError::UsageLimited { .. })
    ));
    assert_eq!(e.fake.responses.lock().unwrap().len(), before);

    // Cleared by hand; a cap hit reported mid-stream marks it again.
    e.chatgpt.clear_limit(&id).unwrap();
    *e.fake.mode.lock().unwrap() = Mode::LimitStream;
    let mut stream = e.chatgpt.responses(&id, req).await.unwrap();
    let mut last = None;
    while let Some(ev) = stream.next().await {
        last = Some(ev);
    }
    assert!(matches!(last, Some(Err(ChatGptError::UsageLimited { .. }))));
    assert!(e.chatgpt.get(&id).unwrap().limited_until.is_some());
}

#[tokio::test]
async fn tokens_refresh_before_expiry_and_a_dead_refresh_token_signs_out() {
    let e = env().await;
    // Access tokens that expire inside the refresh margin.
    *e.fake.expires_in.lock().unwrap() = 60;
    let id = e.sign_in().await;

    let t = e.chatgpt.access_token(&id).await.unwrap();
    assert_eq!(t.as_str(), "AT-SECRET-2");
    let reqs = e.fake.token_requests.lock().unwrap().clone();
    let refresh = reqs.last().unwrap();
    assert_eq!(refresh["grant_type"], "refresh_token");
    assert_eq!(refresh["client_id"], ISSUED_CLIENT);
    assert_eq!(refresh["refresh_token"], "RT-SECRET-1");
    assert_eq!(refresh["resource"], "https://api.openai.com/v1");
    // Fresh now (the refresh answered expires_in 3600): no second refresh.
    assert_eq!(e.chatgpt.access_token(&id).await.unwrap().as_str(), "AT-SECRET-2");
    assert_eq!(e.fake.token_requests.lock().unwrap().len(), reqs.len());

    // The refresh endpoint (forced) through the API; the rotated token is used next time.
    let r = e
        .http
        .post(format!("{}/api/chatgpt/accounts/{id}/refresh", e.ember))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_no_secrets(&r.text().await.unwrap());
    let last = e.fake.token_requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["refresh_token"], "RT-SECRET-2");

    *e.fake.refresh_fails.lock().unwrap() = true;
    let err = e.chatgpt.refresh(&id, true).await.unwrap_err();
    assert!(matches!(err, ChatGptError::SignInRequired(_)), "{err}");
    let acc = e.chatgpt.get(&id).unwrap();
    assert_eq!(acc.status, "signed_out");
    assert!(matches!(
        e.chatgpt.access_token(&id).await,
        Err(ChatGptError::SignInRequired(_))
    ));
}

#[tokio::test]
async fn sign_out_revokes_and_forgets() {
    let e = env().await;
    let id = e.sign_in().await;
    let r = e
        .http
        .delete(format!("{}/api/chatgpt/accounts/{id}", e.ember))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["revoked"], true);
    let revoked = e.fake.revoked.lock().unwrap().clone();
    assert_eq!(revoked[0]["token"], "RT-SECRET-1");
    assert_eq!(revoked[0]["token_type_hint"], "refresh_token");
    assert_eq!(revoked[0]["client_id"], ISSUED_CLIENT);
    assert!(e.chatgpt.list().unwrap().is_empty());
}
