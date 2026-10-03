//! Sign in with ChatGPT and ChatGPT plan usage (SPEC FR-U4, INTENT D7).
//!
//! A self-hosted ember server signs a user in with their ChatGPT account and may then send
//! Responses API requests billed to that user's ChatGPT Plus/Pro plan. OpenAI allows this for
//! open-source, locally hosted apps; a paid or remotely hosted app needs OpenAI's approval, so the
//! feature is **off whenever the server runs as a hosted service** (`EMBER_HOSTED=1`), and it is
//! never offered through `darkpyonix.dev`.
//!
//! # What OpenAI documents (developers.openai.com/siwc/token-sharing-open-source/…, 2026-10-03)
//!
//! | Item | Value | Page |
//! | ---- | ----- | ---- |
//! | Authorization endpoint | `https://auth.openai.com/api/accounts/authorize` | `sign-in` |
//! | Token endpoint (code and refresh) | `https://auth.openai.com/api/accounts/oauth/token`, form-encoded, no client secret | `sign-in`, `profiles-and-sessions` |
//! | Client registration | First sign-in uses `client_id=dynamic_agent_client` plus `agent_name_hint`; OpenAI registers a client bound to the user and workspace and returns its id as `client_id` on the callback. Later sign-ins and refreshes use that id. | `sign-in`, overview |
//! | Host id | `ext_agent_host_id`, stable per host, chosen before the first sign-in (`urn:uuid:` is accepted) | overview |
//! | Scopes | `openid profile email offline_access resource.invoke chatgpt.tokens.use.direct`; plan usage only if `chatgpt.tokens.use.direct` was granted | `sign-in`, `errors-and-recovery` |
//! | Resource | `resource=https://api.openai.com/v1` on authorize, code exchange and refresh | `sign-in`, `profiles-and-sessions` |
//! | PKCE | S256, plus `state` and `nonce` | `sign-in` |
//! | Redirect | `http://127.0.0.1:<port>/auth/callback`; only the port may vary, never `localhost` | `sign-in` |
//! | Lifetimes | access token 1 h; refresh token 30 days, rotated on each refresh; serialise refreshes | `token-reference`, `profiles-and-sessions` |
//! | Revocation | `revocation_endpoint` from `https://auth.openai.com/.well-known/openid-configuration` | `profiles-and-sessions` |
//! | Inference | `POST https://api.openai.com/v1/responses`, `store:false`, `stream:true`, `input` array | `models-and-inference`, `preview-limitations` |
//! | Usage cap | Set by the user per app as a share of their weekly ChatGPT usage, in ChatGPT settings. No API reports it; reaching it returns `subscription_sharing_usage_limit_exceeded` (HTTP 429 or a `response.failed` event) | `errors-and-recovery`, `models-and-inference` |
//!
//! # How Ember does it
//!
//! 1. `POST /api/v1/chatgpt/signin` returns the authorization URL (fresh PKCE verifier, `state`,
//!    `nonce`; kept in memory for [`PENDING_TTL_MS`]).
//! 2. The browser comes back to `GET /auth/callback` on this server (the redirect port is the
//!    server's listen port). When the browser runs on another machine, `127.0.0.1` does not reach
//!    the server; the user pastes the final URL into `POST /api/v1/chatgpt/signin/complete`.
//! 3. The callback is checked (known `state`, no `error`, issued `client_id`), the code is
//!    exchanged with the verifier, the ID token's issuer, audience, expiry and nonce are checked,
//!    and the tokens are stored sealed with the accounts' [`SecretBox`](crate::accounts::secrets).
//! 4. [`ChatGpt::access_token`] refreshes five minutes before expiry, one refresh at a time; a
//!    dead refresh token (`invalid_grant`, …) clears the tokens and marks the account signed out.
//! 5. [`ChatGpt::responses`] shapes the request ([`responses::shape_request`]), streams the reply
//!    and records token usage per day, next to the agent accounts' usage (`GET /api/v1/usage`).
//!
//! **ID token signature.** OpenAI's page says to verify the ID token against the published JWKS.
//! Ember checks the claims only: the token arrives directly from the token endpoint over TLS,
//! which OIDC Core §3.1.3.7 accepts instead of a signature check for the code flow. Adding JWKS
//! verification needs a JOSE dependency; it is an open item.

pub mod api;
pub mod oauth;
pub mod responses;
pub mod schema;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::accounts::{Accounts, DEFAULT_COOLDOWN_MS};
use crate::store::{now_ms, Store};
use oauth::{AuthorizeParams, Pkce, TokenResponse, CALLBACK_PATH, DYNAMIC_CLIENT};
use responses::{SseEvent, SseParser};
pub use store::{ChatGptAccount, Lifetimes, Tokens, PLAN_SCOPE};

/// How long a started sign-in waits for its callback.
pub const PENDING_TTL_MS: i64 = 10 * 60 * 1000;
/// Refresh this long before the access token expires.
pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;
/// Where the user sets and reads the per-app weekly cap (no API exposes it).
pub const MANAGE_USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// Endpoints and switches. [`ChatGptConfig::openai`] is the real service; tests point the URLs
/// at an in-process fake.
#[derive(Debug, Clone)]
pub struct ChatGptConfig {
    /// False on a hosted server: every operation then fails with [`ChatGptError::Disabled`].
    pub enabled: bool,
    pub issuer: String,
    pub authorize_url: String,
    pub token_url: String,
    pub discovery_url: String,
    /// `https://api.openai.com/v1`; `/responses` and `/models` are appended.
    pub api_base: String,
    pub resource: String,
    /// The port of `http://127.0.0.1:<port>/auth/callback` (the server's listen port).
    pub redirect_port: u16,
    /// `agent_name_hint` on first registration: the app's name, the same on every install.
    pub agent_name: String,
}

impl ChatGptConfig {
    pub fn openai(redirect_port: u16) -> ChatGptConfig {
        ChatGptConfig {
            enabled: true,
            issuer: "https://auth.openai.com".into(),
            authorize_url: "https://auth.openai.com/api/accounts/authorize".into(),
            token_url: "https://auth.openai.com/api/accounts/oauth/token".into(),
            discovery_url: "https://auth.openai.com/.well-known/openid-configuration".into(),
            api_base: "https://api.openai.com/v1".into(),
            resource: "https://api.openai.com/v1".into(),
            redirect_port,
            agent_name: "DarkPyonix Ember".into(),
        }
    }

    /// `EMBER_HOSTED=1` (or `true`) disables the feature; `EMBER_CHATGPT_REDIRECT_PORT`
    /// overrides the callback port (default: the listen port).
    pub fn from_env(listen_port: u16) -> ChatGptConfig {
        let port = std::env::var("EMBER_CHATGPT_REDIRECT_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(listen_port);
        let mut c = ChatGptConfig::openai(port);
        c.enabled = !hosted_from_env();
        c
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.redirect_port)
    }
}

/// Is this ember server configured as a hosted (not self-hosted) service?
pub fn hosted_from_env() -> bool {
    matches!(
        std::env::var("EMBER_HOSTED").as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
}

#[derive(Debug, thiserror::Error)]
pub enum ChatGptError {
    #[error(
        "Sign in with ChatGPT is only available on a self-hosted ember server (EMBER_HOSTED is \
         set); OpenAI requires its approval for remotely hosted apps"
    )]
    Disabled,
    #[error("ChatGPT account {0} not found")]
    NotFound(String),
    #[error("invalid sign-in callback: {0}")]
    BadCallback(String),
    #[error("sign-in was not completed: {error}{}", .description.as_deref().map(|d| format!(" ({d})")).unwrap_or_default())]
    OAuth {
        error: String,
        description: Option<String>,
    },
    #[error("OpenAI's token endpoint answered {status}: {error}{}", .description.as_deref().map(|d| format!(" ({d})")).unwrap_or_default())]
    Token {
        status: u16,
        error: String,
        description: Option<String>,
    },
    #[error("sign in to ChatGPT again: {0}")]
    SignInRequired(String),
    #[error(
        "ChatGPT plan usage was not granted (scope chatgpt.tokens.use.direct missing); sign in \
         again and allow it, or use an API-key provider"
    )]
    PlanUsageNotGranted,
    #[error("this ChatGPT account cannot use its plan in other apps (Plus or Pro required): {0}")]
    NotEligible(String),
    #[error("ChatGPT plan usage limit reached for Ember (until {until_utc}): {message}. Manage it at https://chatgpt.com/settings/usage")]
    UsageLimited {
        until: i64,
        until_utc: String,
        message: String,
    },
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("ChatGPT request failed{}: {message}", .code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Api {
        status: Option<u16>,
        code: Option<String>,
        message: String,
    },
    #[error("network error talking to OpenAI: {0}")]
    Network(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ChatGptError {
    /// The HTTP status the API answers with.
    pub fn http_status(&self) -> u16 {
        match self {
            ChatGptError::Disabled => 403,
            ChatGptError::NotFound(_) => 404,
            ChatGptError::BadCallback(_) | ChatGptError::OAuth { .. } => 400,
            ChatGptError::Token { .. } => 502,
            ChatGptError::SignInRequired(_) => 401,
            ChatGptError::PlanUsageNotGranted | ChatGptError::NotEligible(_) => 403,
            ChatGptError::UsageLimited { .. } => 429,
            ChatGptError::Unsupported(_) | ChatGptError::BadRequest(_) => 400,
            ChatGptError::Api { status, .. } => status.filter(|s| *s >= 400).unwrap_or(502),
            ChatGptError::Network(_) => 502,
            ChatGptError::Conflict(_) => 409,
            ChatGptError::Other(_) => 500,
        }
    }
}

fn net(e: reqwest::Error) -> ChatGptError {
    ChatGptError::Network(e.without_url().to_string())
}

/// What a started sign-in hands the client.
#[derive(Debug, Clone, Serialize)]
pub struct SignInStart {
    /// Open this in a browser.
    pub authorization_url: String,
    /// Poll `GET /api/v1/chatgpt/signin/{state}` for the outcome.
    pub state: String,
    pub redirect_uri: String,
    pub expires_at: i64,
    pub note: &'static str,
}

/// The outcome of a sign-in, by `state`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SignInStatus {
    Pending,
    SignedIn { account_id: String },
    Failed { error: String },
}

struct Pending {
    verifier: Zeroizing<String>,
    nonce: String,
    redirect_uri: String,
    client_id: String,
    reauth: Option<String>,
    label: String,
    created_at: i64,
}

pub struct ChatGpt {
    config: ChatGptConfig,
    accounts: Arc<Accounts>,
    http: reqwest::Client,
    pending: Mutex<HashMap<String, Pending>>,
    outcomes: Mutex<HashMap<String, (i64, SignInStatus)>>,
    /// Refresh tokens rotate: two concurrent refreshes would burn one (`refresh_token_reused`).
    refresh_lock: tokio::sync::Mutex<()>,
}

impl ChatGpt {
    pub fn new(accounts: Arc<Accounts>, config: ChatGptConfig) -> anyhow::Result<Arc<ChatGpt>> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()?;
        Ok(Arc::new(ChatGpt {
            config,
            accounts,
            http,
            pending: Mutex::new(HashMap::new()),
            outcomes: Mutex::new(HashMap::new()),
            refresh_lock: tokio::sync::Mutex::new(()),
        }))
    }

    pub fn config(&self) -> &ChatGptConfig {
        &self.config
    }

    fn store(&self) -> &Store {
        self.accounts.store()
    }

    fn guard(&self) -> Result<(), ChatGptError> {
        if self.config.enabled {
            Ok(())
        } else {
            Err(ChatGptError::Disabled)
        }
    }

    /// This server's `ext_agent_host_id`.
    pub fn host_id(&self) -> Result<String, ChatGptError> {
        self.guard()?;
        Ok(store::host_id(self.store())?)
    }

    pub fn list(&self) -> Result<Vec<ChatGptAccount>, ChatGptError> {
        self.guard()?;
        Ok(store::list(self.store())?)
    }

    pub fn get(&self, id: &str) -> Result<ChatGptAccount, ChatGptError> {
        self.guard()?;
        store::get(self.store(), id)?.ok_or_else(|| ChatGptError::NotFound(id.into()))
    }

    /// Start a sign-in. `reauth` signs an existing account in again with its issued client.
    pub fn start_signin(
        &self,
        label: Option<&str>,
        reauth: Option<&str>,
    ) -> Result<SignInStart, ChatGptError> {
        self.guard()?;
        let now = now_ms();
        self.prune(now);
        let (client_id, login_hint, label) = match reauth {
            Some(id) => {
                let acc = self.get(id)?;
                (acc.client_id, acc.email, acc.label)
            }
            None => (
                DYNAMIC_CLIENT.to_string(),
                None,
                label.filter(|l| !l.trim().is_empty()).unwrap_or("ChatGPT").to_string(),
            ),
        };
        let pkce = Pkce::new();
        let state = oauth::random_token();
        let nonce = oauth::random_token();
        let redirect_uri = self.config.redirect_uri();
        let host_id = store::host_id(self.store())?;
        let url = oauth::authorize_url(
            &self.config.authorize_url,
            &AuthorizeParams {
                client_id: &client_id,
                agent_name_hint: (client_id == DYNAMIC_CLIENT).then_some(self.config.agent_name.as_str()),
                ext_agent_host_id: &host_id,
                redirect_uri: &redirect_uri,
                resource: &self.config.resource,
                state: &state,
                nonce: &nonce,
                code_challenge: &pkce.challenge,
                login_hint: login_hint.as_deref(),
            },
        )?;
        self.pending.lock().unwrap().insert(
            state.clone(),
            Pending {
                verifier: pkce.verifier,
                nonce,
                redirect_uri: redirect_uri.clone(),
                client_id,
                reauth: reauth.map(str::to_string),
                label,
                created_at: now,
            },
        );
        self.outcomes
            .lock()
            .unwrap()
            .insert(state.clone(), (now, SignInStatus::Pending));
        Ok(SignInStart {
            authorization_url: url,
            state,
            redirect_uri,
            expires_at: now + PENDING_TTL_MS,
            note: "Open the URL in a browser on the machine running ember server. If the browser \
                   is elsewhere, the final 127.0.0.1 page will not load: copy its full URL into \
                   POST /api/v1/chatgpt/signin/complete.",
        })
    }

    /// The outcome of the sign-in started with `state`, while it is remembered.
    pub fn signin_status(&self, state: &str) -> Result<Option<SignInStatus>, ChatGptError> {
        self.guard()?;
        self.prune(now_ms());
        Ok(self
            .outcomes
            .lock()
            .unwrap()
            .get(state)
            .map(|(_, s)| s.clone()))
    }

    fn prune(&self, now: i64) {
        self.pending
            .lock()
            .unwrap()
            .retain(|_, p| now - p.created_at < PENDING_TTL_MS);
        self.outcomes
            .lock()
            .unwrap()
            .retain(|_, (t, _)| now - *t < 2 * PENDING_TTL_MS);
    }

    /// Finish a sign-in from the callback's query parameters.
    pub async fn complete_signin(
        &self,
        params: &HashMap<String, String>,
    ) -> Result<ChatGptAccount, ChatGptError> {
        self.guard()?;
        let state = params
            .get("state")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ChatGptError::BadCallback("missing state".into()))?;
        // An unknown state consumes nothing: a forged callback cannot cancel a real sign-in.
        let pending = self.pending.lock().unwrap().remove(state);
        let Some(pending) = pending else {
            return Err(ChatGptError::BadCallback(
                "unknown or expired sign-in state; start the sign-in again".into(),
            ));
        };
        let result = if now_ms() - pending.created_at >= PENDING_TTL_MS {
            Err(ChatGptError::BadCallback(
                "the sign-in expired; start it again".into(),
            ))
        } else {
            self.finish(pending, params).await
        };
        let outcome = match &result {
            Ok(acc) => SignInStatus::SignedIn {
                account_id: acc.id.clone(),
            },
            Err(e) => SignInStatus::Failed {
                error: e.to_string(),
            },
        };
        self.outcomes
            .lock()
            .unwrap()
            .insert(state.clone(), (now_ms(), outcome));
        result
    }

    async fn finish(
        &self,
        pending: Pending,
        params: &HashMap<String, String>,
    ) -> Result<ChatGptAccount, ChatGptError> {
        if let Some(error) = params.get("error") {
            return Err(ChatGptError::OAuth {
                error: error.clone(),
                description: params.get("error_description").cloned(),
            });
        }
        let code = params
            .get("code")
            .filter(|c| !c.is_empty())
            .ok_or_else(|| ChatGptError::BadCallback("missing code".into()))?;
        let returned = params.get("client_id").filter(|c| !c.is_empty());
        let client_id = if pending.client_id == DYNAMIC_CLIENT {
            returned
                .ok_or_else(|| {
                    ChatGptError::BadCallback(
                        "no client_id was issued on the callback of a first sign-in".into(),
                    )
                })?
                .clone()
        } else {
            if returned.is_some_and(|c| *c != pending.client_id) {
                return Err(ChatGptError::BadCallback(
                    "the callback's client_id is not this account's client".into(),
                ));
            }
            pending.client_id.clone()
        };

        let resp = self
            .http
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", pending.verifier.as_str()),
                ("redirect_uri", pending.redirect_uri.as_str()),
                ("resource", self.config.resource.as_str()),
            ])
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        let body = Zeroizing::new(resp.bytes().await.map_err(net)?.to_vec());
        if !status.is_success() {
            let (error, description) = oauth::oauth_error(&body);
            return Err(ChatGptError::Token {
                status: status.as_u16(),
                error,
                description,
            });
        }
        let tr: TokenResponse = serde_json::from_slice(&body).map_err(|_| ChatGptError::Token {
            status: status.as_u16(),
            error: "malformed token response".into(),
            description: None,
        })?;
        let now = now_ms();
        let id_token = tr.id_token.as_deref().ok_or_else(|| {
            ChatGptError::BadCallback("the token response has no ID token".into())
        })?;
        let claims = oauth::check_id_token(
            id_token,
            &self.config.issuer,
            &client_id,
            &pending.nonce,
            now,
        )
        .map_err(ChatGptError::BadCallback)?;
        let scopes = tr
            .scope
            .clone()
            .or_else(|| params.get("scope").cloned())
            .unwrap_or_default();
        let lifetimes = tr.lifetimes(now);

        let existing = match &pending.reauth {
            Some(id) => {
                let sub = store::subject_of(self.store(), id)?
                    .ok_or_else(|| ChatGptError::NotFound(id.clone()))?;
                if sub != claims.sub {
                    return Err(ChatGptError::Conflict(
                        "signed in as a different ChatGPT user than this account; add it as a \
                         new account instead"
                            .into(),
                    ));
                }
                Some(id.clone())
            }
            None => store::find(self.store(), &client_id, &claims.sub)?.map(|a| a.id),
        };
        let tokens = tr.into_tokens(None, None);
        let acc = store::save_sign_in(
            self.store(),
            self.accounts.secrets(),
            existing.as_deref(),
            store::SignedIn {
                label: &pending.label,
                client_id: &client_id,
                subject: &claims.sub,
                email: claims.email.as_deref(),
                scopes: &scopes,
                tokens: &tokens,
                lifetimes,
            },
        )?;
        tracing::info!(account = %acc.id, plan_usage = acc.plan_usage, "ChatGPT sign-in completed");
        Ok(acc)
    }

    fn needs_refresh(l: &Lifetimes, now: i64) -> bool {
        let Some(exp) = l.access_expires_at else {
            return true;
        };
        if exp - now > REFRESH_MARGIN_MS {
            return false;
        }
        // Not before the server's earliest refresh time, unless the token is about to lapse.
        match l.earliest_refresh_at {
            Some(e) if now < e && exp - now > 30_000 => false,
            _ => true,
        }
    }

    /// A valid access token for `id`, refreshed first when it is close to expiry.
    pub async fn access_token(&self, id: &str) -> Result<Zeroizing<String>, ChatGptError> {
        let acc = self.get(id)?;
        if acc.status != "signed_in" {
            return Err(ChatGptError::SignInRequired(format!(
                "account {} is {}",
                acc.label, acc.status
            )));
        }
        let (tokens, lifetimes) = store::tokens(self.store(), self.accounts.secrets(), id)?
            .ok_or_else(|| ChatGptError::SignInRequired("no stored tokens".into()))?;
        if !Self::needs_refresh(&lifetimes, now_ms()) {
            return Ok(tokens.access_token);
        }
        drop(tokens);
        self.refresh(id, false).await?;
        let (tokens, _) = store::tokens(self.store(), self.accounts.secrets(), id)?
            .ok_or_else(|| ChatGptError::SignInRequired("no stored tokens".into()))?;
        Ok(tokens.access_token)
    }

    /// Refresh `id`'s tokens (`force`: even if the access token is still fresh).
    pub async fn refresh(&self, id: &str, force: bool) -> Result<(), ChatGptError> {
        let acc = self.get(id)?;
        let _one_at_a_time = self.refresh_lock.lock().await;
        let (tokens, lifetimes) = store::tokens(self.store(), self.accounts.secrets(), id)?
            .ok_or_else(|| ChatGptError::SignInRequired("no stored tokens".into()))?;
        // Someone else refreshed while this call waited for the lock.
        if !force && !Self::needs_refresh(&lifetimes, now_ms()) {
            return Ok(());
        }
        let Some(refresh_token) = tokens.refresh_token.clone() else {
            store::clear_tokens(self.store(), id, "signed_out")?;
            return Err(ChatGptError::SignInRequired(
                "no refresh token (offline_access was not granted)".into(),
            ));
        };
        let resp = self
            .http
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", acc.client_id.as_str()),
                ("refresh_token", refresh_token.as_str()),
                ("resource", self.config.resource.as_str()),
            ])
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        let body = Zeroizing::new(resp.bytes().await.map_err(net)?.to_vec());
        if !status.is_success() {
            let (error, description) = oauth::oauth_error(&body);
            if oauth::refresh_error_is_terminal(&error) || status.as_u16() == 401 {
                store::clear_tokens(self.store(), id, "signed_out")?;
                return Err(ChatGptError::SignInRequired(format!(
                    "the refresh token was rejected ({error})"
                )));
            }
            return Err(ChatGptError::Token {
                status: status.as_u16(),
                error,
                description,
            });
        }
        let tr: TokenResponse = serde_json::from_slice(&body).map_err(|_| ChatGptError::Token {
            status: status.as_u16(),
            error: "malformed token response".into(),
            description: None,
        })?;
        let lifetimes = tr.lifetimes(now_ms());
        let scope = tr.scope.clone();
        let new = tr.into_tokens(Some(refresh_token), tokens.id_token.clone());
        store::save_refresh(
            self.store(),
            self.accounts.secrets(),
            id,
            &new,
            lifetimes,
            scope.as_deref(),
        )?;
        Ok(())
    }

    /// The models this account may use (`GET /v1/models`, `visibility: "list"` only).
    pub async fn models(&self, id: &str) -> Result<Vec<Value>, ChatGptError> {
        let token = self.access_token(id).await?;
        let resp = self
            .http
            .get(format!("{}/models", self.config.api_base))
            .bearer_auth(token.as_str())
            .send()
            .await
            .map_err(net)?;
        let status = resp.status().as_u16();
        let v: Value = serde_json::from_slice(&resp.bytes().await.map_err(net)?).unwrap_or_default();
        if !(200..300).contains(&status) {
            let (code, message) = responses::error_of(&v);
            return Err(classify(self.store(), id, Some(status), code, message, None));
        }
        Ok(responses::listed_models(&v))
    }

    /// Send a Responses API request billed to the account's ChatGPT plan.
    ///
    /// `body` is a Responses request (`model`, `input`, optional `tools`, `instructions`, …). It
    /// is refused before any network call if it uses something plan usage does not support (see
    /// [`responses`]); `store` and `stream` are set by Ember. The reply is read with
    /// [`ResponseStream::next`].
    pub async fn responses(&self, id: &str, body: Value) -> Result<ResponseStream, ChatGptError> {
        self.guard()?;
        let body = responses::shape_request(body).map_err(ChatGptError::Unsupported)?;
        let acc = self.get(id)?;
        if !acc.plan_usage {
            return Err(ChatGptError::PlanUsageNotGranted);
        }
        if acc.status == "not_eligible" {
            return Err(ChatGptError::NotEligible(
                "OpenAI reported the account as not eligible".into(),
            ));
        }
        let now = now_ms();
        if acc.limited_at(now) {
            let until = acc.limited_until.unwrap_or(now);
            return Err(ChatGptError::UsageLimited {
                until,
                until_utc: crate::accounts::format_utc(until),
                message: acc.limit_reason.unwrap_or_default(),
            });
        }
        let token = self.access_token(id).await?;
        let resp = self
            .http
            .post(format!("{}/responses", self.config.api_base))
            .bearer_auth(token.as_str())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<i64>().ok());
            let bytes = resp.bytes().await.unwrap_or_default();
            let v: Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let (code, message) = responses::error_of(&v);
            return Err(classify(self.store(), id, Some(status), code, message, retry_after));
        }
        Ok(ResponseStream {
            resp,
            parser: SseParser::default(),
            accounts: self.accounts.clone(),
            account_id: id.to_string(),
            done: false,
            ended: false,
        })
    }

    pub fn clear_limit(&self, id: &str) -> Result<ChatGptAccount, ChatGptError> {
        self.get(id)?;
        store::clear_limit(self.store(), id)?;
        self.get(id)
    }

    /// Sign out: revoke the refresh token at OpenAI (best effort) and delete the account, its
    /// tokens and its usage rows. Returns whether OpenAI confirmed the revocation.
    pub async fn sign_out(&self, id: &str) -> Result<bool, ChatGptError> {
        let acc = self.get(id)?;
        let revoked = match store::tokens(self.store(), self.accounts.secrets(), id) {
            Ok(Some((t, _))) => match t.refresh_token {
                Some(rt) => match self.revoke(&acc.client_id, &rt).await {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(account = %id, "ChatGPT token revocation failed: {e:#}");
                        false
                    }
                },
                None => false,
            },
            _ => false,
        };
        store::delete(self.store(), id)?;
        Ok(revoked)
    }

    async fn revoke(&self, client_id: &str, refresh_token: &str) -> anyhow::Result<()> {
        let discovery: Value = self
            .http
            .get(&self.config.discovery_url)
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let endpoint = discovery["revocation_endpoint"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no revocation_endpoint in the discovery document"))?;
        self.http
            .post(endpoint)
            .timeout(Duration::from_secs(10))
            .form(&[
                ("token", refresh_token),
                ("token_type_hint", "refresh_token"),
                ("client_id", client_id),
            ])
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

/// Turn an OpenAI error into a [`ChatGptError`] and record what it says about the account
/// (errors-and-recovery: limit → pause; not eligible → stop; invalid user → sign in again).
fn classify(
    db: &Store,
    id: &str,
    status: Option<u16>,
    code: Option<String>,
    message: String,
    retry_after_secs: Option<i64>,
) -> ChatGptError {
    let record = |r: anyhow::Result<()>| {
        if let Err(e) = r {
            tracing::warn!(account = %id, "recording a ChatGPT error failed: {e:#}");
        }
    };
    // The plan's cap, or any other 429: pause the account (Retry-After when given).
    if code.as_deref() == Some("subscription_sharing_usage_limit_exceeded") || status == Some(429) {
        let until = now_ms() + retry_after_secs.map_or(DEFAULT_COOLDOWN_MS, |s| s * 1000);
        record(store::set_limit(db, id, until, &message));
        return ChatGptError::UsageLimited {
            until,
            until_utc: crate::accounts::format_utc(until),
            message,
        };
    }
    let known = code.clone().unwrap_or_default();
    match known.as_str() {
        "subscription_sharing_user_not_eligible" => {
            record(store::set_status(db, id, "not_eligible"));
            ChatGptError::NotEligible(message)
        }
        "subscription_sharing_invalid_user" => {
            record(store::clear_tokens(db, id, "signed_out"));
            ChatGptError::SignInRequired(message)
        }
        "subscription_sharing_unsupported_capability" => ChatGptError::Unsupported(format!(
            "OpenAI refused a capability ChatGPT plan usage does not support: {message}"
        )),
        _ if status == Some(401) => ChatGptError::SignInRequired(message),
        _ => ChatGptError::Api {
            status,
            code,
            message,
        },
    }
}

/// A streamed Responses API reply. Usage of a completed response is recorded for the account;
/// a usage-limit failure marks the account limited.
pub struct ResponseStream {
    resp: reqwest::Response,
    parser: SseParser,
    accounts: Arc<Accounts>,
    account_id: String,
    done: bool,
    ended: bool,
}

impl std::fmt::Debug for ResponseStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseStream")
            .field("account_id", &self.account_id)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl ResponseStream {
    /// The next event; `None` after a terminal event (`response.completed`, `.failed`,
    /// `.incomplete`). A stream that ends without one yields an error first.
    pub async fn next(&mut self) -> Option<Result<SseEvent, ChatGptError>> {
        if self.done {
            return None;
        }
        loop {
            if let Some(ev) = self.parser.next_event() {
                return Some(self.handle(ev));
            }
            if self.ended {
                let tail = self.parser.finish();
                return match tail {
                    Some(ev) => Some(self.handle(ev)),
                    None => {
                        self.done = true;
                        Some(Err(ChatGptError::Api {
                            status: None,
                            code: None,
                            message: "the stream ended before response.completed".into(),
                        }))
                    }
                };
            }
            match self.resp.chunk().await {
                Ok(Some(bytes)) => self.parser.push(&bytes),
                Ok(None) => self.ended = true,
                Err(e) => {
                    self.done = true;
                    return Some(Err(net(e)));
                }
            }
        }
    }

    fn handle(&mut self, ev: SseEvent) -> Result<SseEvent, ChatGptError> {
        let db = self.accounts.store();
        match ev.kind.as_str() {
            "response.completed" => {
                self.done = true;
                if let Some((input, output)) = responses::completed_usage(&ev.data) {
                    if let Err(e) = store::record_usage(db, &self.account_id, input, output) {
                        tracing::warn!(account = %self.account_id, "recording ChatGPT usage failed: {e:#}");
                    }
                }
                Ok(ev)
            }
            "response.failed" | "error" => {
                self.done = true;
                let (code, message) = responses::error_of(&ev.data);
                Err(classify(db, &self.account_id, None, code, message, None))
            }
            "response.incomplete" => {
                self.done = true;
                Ok(ev)
            }
            _ => Ok(ev),
        }
    }
}
