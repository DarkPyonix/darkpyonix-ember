//! HTTP routes for Sign in with ChatGPT (FR-U4). Every route answers 403 on a hosted server.
//!
//! - `GET    /api/v1/chatgpt`                          enabled?, redirect URI, how the cap works
//! - `POST   /api/v1/chatgpt/signin`                   `{label?, account_id?}` → authorization URL
//! - `GET    /api/v1/chatgpt/signin/{state}`           `pending` | `signed_in` | `failed`
//! - `POST   /api/v1/chatgpt/signin/complete`          `{callback_url}` pasted by the user
//! - `GET    /auth/callback`                           the loopback redirect target
//! - `GET    /api/v1/chatgpt/accounts`                 accounts with today's tokens and the cap
//! - `GET    /api/v1/chatgpt/accounts/{id}`
//! - `DELETE /api/v1/chatgpt/accounts/{id}`            revoke (best effort) and forget
//! - `POST   /api/v1/chatgpt/accounts/{id}/refresh`    refresh the tokens now
//! - `POST   /api/v1/chatgpt/accounts/{id}/clear-limit`
//! - `GET    /api/v1/chatgpt/accounts/{id}/models`     models the plan may use
//!
//! No response ever contains a token. Usage per day appears in `GET /api/v1/usage` with the
//! agent accounts' rows.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{ChatGpt, ChatGptAccount, ChatGptError, MANAGE_USAGE_URL};
use crate::store::now_ms;

pub fn router(chatgpt: Arc<ChatGpt>) -> Router {
    Router::new()
        .route("/api/v1/chatgpt", get(status))
        .route("/api/v1/chatgpt/signin", post(start))
        .route("/api/v1/chatgpt/signin/complete", post(complete_pasted))
        .route("/api/v1/chatgpt/signin/{state}", get(signin_status))
        .route(super::oauth::CALLBACK_PATH, get(callback))
        .route("/api/v1/chatgpt/accounts", get(list))
        .route(
            "/api/v1/chatgpt/accounts/{id}",
            get(get_one).delete(sign_out),
        )
        .route("/api/v1/chatgpt/accounts/{id}/refresh", post(refresh))
        .route("/api/v1/chatgpt/accounts/{id}/clear-limit", post(clear_limit))
        .route("/api/v1/chatgpt/accounts/{id}/models", get(models))
        .with_state(chatgpt)
}

struct ApiError(ChatGptError);

impl From<ChatGptError> for ApiError {
    fn from(e: ChatGptError) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = StatusCode::from_u16(self.0.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut body = json!({ "error": self.0.to_string() });
        if let ChatGptError::UsageLimited { until, .. } = &self.0 {
            body["limited_until"] = json!(until);
            body["manage_url"] = json!(MANAGE_USAGE_URL);
        }
        (code, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// The weekly cap as far as Ember can know it.
#[derive(Serialize)]
struct WeeklyCap {
    /// OpenAI exposes no API for the cap or what remains of it.
    known: bool,
    manage_url: &'static str,
    note: &'static str,
}

const CAP: WeeklyCap = WeeklyCap {
    known: false,
    manage_url: MANAGE_USAGE_URL,
    note: "The user sets Ember's share of their weekly ChatGPT usage in ChatGPT settings. OpenAI \
           does not report it to apps; when it is reached, requests fail with \
           subscription_sharing_usage_limit_exceeded and the account shows as limited.",
};

#[derive(Serialize)]
struct AccountView {
    #[serde(flatten)]
    account: ChatGptAccount,
    limited: bool,
    tokens_today: u64,
    weekly_cap: WeeklyCap,
}

fn view(c: &ChatGpt, account: ChatGptAccount) -> ApiResult<AccountView> {
    const DAY: i64 = 86_400_000;
    let today = now_ms() - now_ms().rem_euclid(DAY);
    let tokens_today = super::store::usage_since(c.accounts.store(), today)
        .map_err(ChatGptError::from)?
        .into_iter()
        .filter(|u| u.account_id.as_deref() == Some(account.id.as_str()))
        .map(|u| u.input_tokens + u.output_tokens)
        .sum();
    Ok(AccountView {
        limited: account.limited_at(now_ms()),
        tokens_today,
        weekly_cap: CAP,
        account,
    })
}

async fn status(State(c): State<Arc<ChatGpt>>) -> impl IntoResponse {
    let cfg = c.config();
    Json(json!({
        "enabled": cfg.enabled,
        "reason": (!cfg.enabled).then(|| ChatGptError::Disabled.to_string()),
        "redirect_uri": cfg.enabled.then(|| cfg.redirect_uri()),
        "weekly_cap": CAP,
    }))
}

#[derive(Deserialize, Default)]
struct StartBody {
    label: Option<String>,
    /// Sign an existing account in again.
    account_id: Option<String>,
}

/// The body is optional, so it is read as bytes: empty means defaults.
async fn start(
    State(c): State<Arc<ChatGpt>>,
    body: axum::body::Bytes,
) -> ApiResult<impl IntoResponse> {
    let b: StartBody = if body.iter().all(u8::is_ascii_whitespace) {
        StartBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ChatGptError::BadRequest(format!("invalid request body: {e}")))?
    };
    let s = c.start_signin(b.label.as_deref(), b.account_id.as_deref())?;
    Ok((StatusCode::CREATED, Json(s)))
}

async fn signin_status(
    State(c): State<Arc<ChatGpt>>,
    Path(state): Path<String>,
) -> ApiResult<Response> {
    Ok(match c.signin_status(&state)? {
        Some(s) => Json(s).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown or expired sign-in" })),
        )
            .into_response(),
    })
}

/// The browser lands here. Answers a small page; never echoes the code or any token.
async fn callback(
    State(c): State<Arc<ChatGpt>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    match c.complete_signin(&params).await {
        Ok(acc) => Html(page(
            "Signed in to ChatGPT",
            &format!(
                "Ember is signed in as {}.{} You can close this tab.",
                acc.email.as_deref().unwrap_or(&acc.label),
                if acc.plan_usage {
                    ""
                } else {
                    " ChatGPT plan usage was not allowed, so Ember cannot use your plan."
                }
            ),
        ))
        .into_response(),
        Err(e) => (
            StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::BAD_REQUEST),
            Html(page("ChatGPT sign-in failed", &e.to_string())),
        )
            .into_response(),
    }
}

fn page(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>{t}</title>\
         <body style=\"font-family:system-ui;margin:3em\"><h1>{t}</h1><p>{x}</p></body>",
        t = escape(title),
        x = escape(text)
    )
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Deserialize)]
struct CompleteBody {
    callback_url: String,
}

async fn complete_pasted(
    State(c): State<Arc<ChatGpt>>,
    Json(b): Json<CompleteBody>,
) -> ApiResult<impl IntoResponse> {
    let params = super::oauth::callback_params(&b.callback_url)
        .map_err(|e| ChatGptError::BadCallback(format!("{e:#}")))?;
    let acc = c.complete_signin(&params).await?;
    Ok(Json(view(&c, acc)?))
}

async fn list(State(c): State<Arc<ChatGpt>>) -> ApiResult<impl IntoResponse> {
    let views = c
        .list()?
        .into_iter()
        .map(|a| view(&c, a))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(views))
}

async fn get_one(
    State(c): State<Arc<ChatGpt>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let acc = c.get(&id)?;
    Ok(Json(view(&c, acc)?))
}

async fn sign_out(
    State(c): State<Arc<ChatGpt>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let revoked = c.sign_out(&id).await?;
    Ok(Json(json!({ "deleted": true, "revoked": revoked })))
}

async fn refresh(
    State(c): State<Arc<ChatGpt>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    c.refresh(&id, true).await?;
    let acc = c.get(&id)?;
    Ok(Json(view(&c, acc)?))
}

async fn clear_limit(
    State(c): State<Arc<ChatGpt>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let acc = c.clear_limit(&id)?;
    Ok(Json(view(&c, acc)?))
}

async fn models(
    State(c): State<Arc<ChatGpt>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(c.models(&id).await?))
}
