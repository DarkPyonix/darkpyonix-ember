//! HTTP routes for accounts, usage and API-key providers (FR-U1–U3, FR-U5).
//!
//! - `GET    /api/accounts[?agent=codex]`   accounts with today's usage, limit and login help
//! - `POST   /api/accounts`                 `{agent, label}` → creates the isolated directory
//! - `GET    /api/accounts/{id}`
//! - `DELETE /api/accounts/{id}`            409 while any session uses it
//! - `POST   /api/accounts/{id}/default`    make it the agent's default
//! - `POST   /api/accounts/{id}/check`      run the agent's own login status command
//! - `POST   /api/accounts/{id}/clear-limit`
//! - `GET    /api/accounts/route?agent=…`   what the router would choose now, and why
//! - `GET    /api/usage[?days=7]`           tokens per account per UTC day (ChatGPT accounts
//!   from `crate::chatgpt` included, by their account id)
//! - `GET    /api/providers`, `POST /api/providers` `{label, kind, base_url?, api_key}`,
//!   `DELETE /api/providers/{id}`. Responses never include the key.
//!
//! A session's account and the routing reason are on the session record (`account_id`,
//! `account_reason`).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::secrets::{self, ApiProvider};
use super::{Account, AccountError, Accounts, LoginInstructions};
use crate::agents::AgentKind;
use crate::store::now_ms;

pub fn router(accounts: Arc<Accounts>) -> Router {
    Router::new()
        .route("/api/accounts", get(list).post(create))
        .route("/api/accounts/route", get(preview_route))
        .route("/api/accounts/{id}", get(get_one).delete(delete))
        .route("/api/accounts/{id}/default", post(set_default))
        .route("/api/accounts/{id}/check", post(check))
        .route("/api/accounts/{id}/clear-limit", post(clear_limit))
        .route("/api/usage", get(usage))
        .route(
            "/api/providers",
            get(list_providers).post(create_provider),
        )
        .route(
            "/api/providers/{id}",
            axum::routing::delete(delete_provider),
        )
        .with_state(accounts)
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl From<AccountError> for ApiError {
    fn from(e: AccountError) -> Self {
        let code = match &e {
            AccountError::NotFound(_) => StatusCode::NOT_FOUND,
            AccountError::Conflict(_) => StatusCode::CONFLICT,
            AccountError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(code, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn parse_agent(s: &str) -> ApiResult<AgentKind> {
    AgentKind::parse(s)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("unknown agent {s}")))
}

#[derive(Serialize)]
struct AccountView {
    #[serde(flatten)]
    account: Account,
    limited: bool,
    tokens_today: u64,
    login: LoginInstructions,
}

fn view(a: &Accounts, account: Account) -> ApiResult<AccountView> {
    let tokens_today = a.tokens_today()?.get(&account.id).copied().unwrap_or(0);
    Ok(AccountView {
        limited: account.limited_at(now_ms()),
        tokens_today,
        login: a.login_instructions(&account),
        account,
    })
}

#[derive(Deserialize)]
struct AgentQuery {
    agent: Option<String>,
}

async fn list(
    State(a): State<Arc<Accounts>>,
    Query(q): Query<AgentQuery>,
) -> ApiResult<impl IntoResponse> {
    let agent = q.agent.as_deref().map(parse_agent).transpose()?;
    let views = a
        .list(agent)?
        .into_iter()
        .map(|acc| view(&a, acc))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(views))
}

#[derive(Deserialize)]
struct CreateBody {
    agent: String,
    label: String,
}

async fn create(
    State(a): State<Arc<Accounts>>,
    Json(b): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let agent = parse_agent(&b.agent)?;
    let acc = a.create(agent, &b.label)?;
    Ok((StatusCode::CREATED, Json(view(&a, acc)?)))
}

async fn get_one(
    State(a): State<Arc<Accounts>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let acc = a.get(&id)?.ok_or(AccountError::NotFound(id))?;
    Ok(Json(view(&a, acc)?))
}

async fn delete(
    State(a): State<Arc<Accounts>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    a.delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_default(
    State(a): State<Arc<Accounts>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let acc = a.set_default(&id)?;
    Ok(Json(view(&a, acc)?))
}

async fn check(
    State(a): State<Arc<Accounts>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let acc = a.check_login(&id).await?;
    Ok(Json(view(&a, acc)?))
}

async fn clear_limit(
    State(a): State<Arc<Accounts>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    a.get(&id)?
        .ok_or_else(|| AccountError::NotFound(id.clone()))?;
    a.clear_limit(&id)?;
    let acc = a.get(&id)?.ok_or(AccountError::NotFound(id))?;
    Ok(Json(view(&a, acc)?))
}

async fn preview_route(
    State(a): State<Arc<Accounts>>,
    Query(q): Query<AgentQuery>,
) -> ApiResult<impl IntoResponse> {
    let agent = parse_agent(q.agent.as_deref().unwrap_or_default())?;
    Ok(Json(match a.route(agent, None) {
        Ok(Some((id, reason))) => json!({ "account_id": id, "reason": reason }),
        Ok(None) => json!({ "account_id": null, "reason": "no accounts; the server's own login" }),
        Err(reason) => json!({ "account_id": null, "unavailable": true, "reason": reason }),
    }))
}

#[derive(Deserialize)]
struct UsageQuery {
    days: Option<i64>,
}

async fn usage(
    State(a): State<Arc<Accounts>>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<impl IntoResponse> {
    const DAY: i64 = 86_400_000;
    let days = q.days.unwrap_or(7).clamp(1, 366);
    let today = now_ms() - now_ms().rem_euclid(DAY);
    Ok(Json(a.usage_since(today - (days - 1) * DAY)?))
}

async fn list_providers(State(a): State<Arc<Accounts>>) -> ApiResult<Json<Vec<ApiProvider>>> {
    Ok(Json(secrets::list_providers(a.store())?))
}

/// Deliberately not `Debug`: it holds a plaintext key.
#[derive(Deserialize)]
struct CreateProviderBody {
    label: String,
    kind: String,
    base_url: Option<String>,
    api_key: String,
}

async fn create_provider(
    State(a): State<Arc<Accounts>>,
    Json(b): Json<CreateProviderBody>,
) -> ApiResult<impl IntoResponse> {
    let key = zeroize::Zeroizing::new(b.api_key);
    if key.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "api_key is empty".into()));
    }
    let p = secrets::create_provider(
        a.store(),
        a.secrets(),
        &b.label,
        &b.kind,
        b.base_url.as_deref(),
        &key,
    )?;
    Ok((StatusCode::CREATED, Json(p)))
}

async fn delete_provider(
    State(a): State<Arc<Accounts>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    if secrets::delete_provider(a.store(), &id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("provider {id} not found"),
        ))
    }
}
