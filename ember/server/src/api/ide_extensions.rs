//! Extension management for the Ember editor's OSE (SPEC FR-W4, #459, EDITOR-PARITY task 6; search
//! and uninstall: SPEC FR-L31.14, #446).
//!
//! | Method | Path | Body | Answer |
//! | ------ | ---- | ---- | ------ |
//! | POST | `/api/computers/{id}/ide/extensions` | `{"id":"publisher.name","version":"1.2.3"}` | 200 `{installed,id,output}` |
//! | DELETE | `/api/computers/{id}/ide/extensions/{publisher.name}` | none | 200 `{uninstalled,id,output}` |
//! | GET | `/api/ide/extensions/search?q=<text>&offset=<n>` | none | 200 `{offset,total,extensions}` |
//!
//! Only the local computer has an OSE (same reason as `ide.rs`: it runs on the server machine),
//! so another registered computer answers 400, an unknown one 404. The id and version are
//! checked against a strict pattern (`ose::extension_spec`): no VSIX paths, no URLs, nothing
//! that starts with `-`. The OSE binary runs directly (no shell) with the computer's data dir;
//! at most one install or uninstall per computer runs at a time (a second answers 409); a run is
//! killed after 120 s (child and process group). The OSE child gets a cleared environment
//! (allowlist only). A failed run answers 502 with what it printed (at most 16 KiB).
//!
//! **Search goes through this server, never from the client** (FR-L31.14). The gallery host is
//! the constant [`OPEN_VSX`]; a request supplies only the query text and an offset, so the route
//! cannot be pointed at another host. The upstream request carries no Ember credential, follows no
//! redirect, times out after [`SEARCH_TIMEOUT`] and reads at most [`SEARCH_BODY_CAP`] bytes. The
//! answer is rebuilt into fixed fields with capped text; URL fields are dropped, so a client never
//! fetches a third-party URL; an entry whose id the install route would refuse is dropped.
//!
//! There is no list route: the installed list is the scan the editor session reads on the OSE
//! management connection. The routes sit in the token-protected router and are not exempt; the
//! guest door never serves them.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::computers::{Computers, LOCAL};
use crate::ose::{extension_spec, valid_computer_id, InstallError, OseManager};

/// The gallery the OSE installs from (`ember/ose/README.md`, `extensionsGallery`).
pub const OPEN_VSX: &str = "https://open-vsx.org";
/// Longest wait for one gallery search.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
/// The most bytes read from one gallery answer.
pub const SEARCH_BODY_CAP: usize = 1024 * 1024;
/// Results asked for per page.
pub const SEARCH_PAGE: usize = 30;
/// The longest query text, in characters.
pub const SEARCH_QUERY_MAX: usize = 200;
/// The largest offset a request may ask for.
pub const SEARCH_OFFSET_MAX: u64 = 10_000;
const NAME_MAX: usize = 120;
const DESCRIPTION_MAX: usize = 400;
const VERSION_MAX: usize = 64;

/// One search result as the client gets it. No URL of the gallery is passed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GalleryExtension {
    /// `namespace.name`: what the install route takes.
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub display_name: String,
    pub description: String,
    pub version: String,
    pub downloads: u64,
    pub verified: bool,
    pub deprecated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchPage {
    pub offset: u64,
    /// How many extensions match in all (`totalSize`).
    pub total: u64,
    pub extensions: Vec<GalleryExtension>,
}

/// Text from the gallery: control characters become spaces, the ends are trimmed, and at most
/// `max` characters are kept.
fn clean(v: Option<&Value>, max: usize) -> String {
    let text = v.and_then(Value::as_str).unwrap_or_default();
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    flat.trim().chars().take(max).collect::<String>().trim_end().to_string()
}

/// Rebuilds an Open VSX `/api/-/search` answer (`{offset,totalSize,extensions:[{namespace,name,
/// version,displayName,description,downloadCount,verified,deprecated,...}]}`) into a
/// [`SearchPage`]. An entry without a namespace or a name, or whose `namespace.name` is not an id
/// the install route accepts, is dropped. Anything that is not that JSON object is an error.
pub fn parse_search(body: &[u8]) -> Result<SearchPage, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("the gallery answer is not JSON: {e}"))?;
    let list = v
        .get("extensions")
        .and_then(Value::as_array)
        .ok_or_else(|| "the gallery answer has no extensions list".to_string())?;
    let extensions: Vec<GalleryExtension> = list
        .iter()
        .take(SEARCH_PAGE)
        .filter_map(|e| {
            let namespace = clean(e.get("namespace"), NAME_MAX);
            let name = clean(e.get("name"), NAME_MAX);
            let id = format!("{namespace}.{name}");
            extension_spec(&id, None).ok()?;
            let display = clean(e.get("displayName"), NAME_MAX);
            Some(GalleryExtension {
                display_name: if display.is_empty() { name.clone() } else { display },
                description: clean(e.get("description"), DESCRIPTION_MAX),
                version: clean(e.get("version"), VERSION_MAX),
                downloads: e.get("downloadCount").and_then(Value::as_u64).unwrap_or(0),
                verified: e.get("verified").and_then(Value::as_bool).unwrap_or(false),
                deprecated: e.get("deprecated").and_then(Value::as_bool).unwrap_or(false),
                id,
                name,
                namespace,
            })
        })
        .collect();
    Ok(SearchPage {
        offset: v.get("offset").and_then(Value::as_u64).unwrap_or(0),
        total: v.get("totalSize").and_then(Value::as_u64).unwrap_or(extensions.len() as u64),
        extensions,
    })
}

/// The query text a search may send: trimmed, 1 to [`SEARCH_QUERY_MAX`] characters, no control
/// characters.
pub fn search_query(q: &str) -> Result<String, String> {
    let q = q.trim();
    if q.is_empty() {
        return Err("q must not be empty".into());
    }
    if q.chars().count() > SEARCH_QUERY_MAX {
        return Err(format!("q must be at most {SEARCH_QUERY_MAX} characters"));
    }
    if q.chars().any(char::is_control) {
        return Err("q must not contain control characters".into());
    }
    Ok(q.to_string())
}

/// The extension gallery the search asks. Production uses [`Gallery::open_vsx`]; a test names a
/// loopback fake with [`Gallery::with_base`]. There is no environment override.
#[derive(Clone)]
pub struct Gallery {
    base: String,
    http: reqwest::Client,
}

impl Gallery {
    pub fn open_vsx() -> Gallery {
        Gallery::with_base(OPEN_VSX)
    }

    /// A gallery at `base` (scheme and host, no trailing path). Its HTTP client is separate from
    /// every other client of the server: no default headers, so no credential can ride along; no
    /// redirects, so the fixed host cannot send the server elsewhere.
    pub fn with_base(base: &str) -> Gallery {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(SEARCH_TIMEOUT)
            .connect_timeout(SEARCH_TIMEOUT)
            .user_agent("ember-server")
            .build()
            .expect("the gallery HTTP client builds from constants");
        Gallery { base: base.trim_end_matches('/').to_string(), http }
    }

    /// One page of search results. Errors are messages for a 502.
    pub async fn search(&self, q: &str, offset: u64) -> Result<SearchPage, String> {
        let size = SEARCH_PAGE.to_string();
        let offset = offset.to_string();
        let mut res = self
            .http
            .get(format!("{}/api/-/search", self.base))
            .query(&[
                ("query", q),
                ("size", size.as_str()),
                ("offset", offset.as_str()),
                ("sortBy", "relevance"),
                ("sortOrder", "desc"),
            ])
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("the extension gallery cannot be reached: {}", e.without_url()))?;
        let status = res.status();
        if !status.is_success() {
            return Err(format!("the extension gallery answered {status}"));
        }
        let mut body: Vec<u8> = Vec::new();
        loop {
            match res.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > SEARCH_BODY_CAP {
                        return Err("the extension gallery answer is too large".into());
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => return Err(format!("the extension gallery answer was cut off: {}", e.without_url())),
            }
        }
        parse_search(&body)
    }
}

#[derive(Clone)]
struct St {
    computers: Arc<Computers>,
    ose: Arc<OseManager>,
    gallery: Gallery,
}

pub fn router(computers: Arc<Computers>, ose: Arc<OseManager>) -> Router {
    router_with_gallery(computers, ose, Gallery::open_vsx())
}

/// Like [`router`] with the gallery given (tests name a loopback fake).
pub fn router_with_gallery(computers: Arc<Computers>, ose: Arc<OseManager>, gallery: Gallery) -> Router {
    Router::new()
        .route("/api/computers/{id}/ide/extensions", post(install))
        .route("/api/computers/{id}/ide/extensions/{extension}", delete(uninstall))
        .route("/api/ide/extensions/search", get(search))
        .with_state(St { computers, ose, gallery })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRequest {
    id: String,
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchParams {
    q: String,
    #[serde(default)]
    offset: u64,
}

fn error(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

/// The answer for a computer that has no OSE here: `None` for the local computer.
fn not_local(st: &St, computer: &str) -> Option<Response> {
    if !valid_computer_id(computer) {
        return Some(error(StatusCode::BAD_REQUEST, "invalid computer id"));
    }
    if computer == LOCAL {
        return None;
    }
    Some(match st.computers.registry().get(computer) {
        Ok(Some(_)) => error(StatusCode::BAD_REQUEST, "OSE server is only available on the local computer"),
        Ok(None) => error(StatusCode::NOT_FOUND, format!("unknown computer: {computer}")),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    })
}

/// The answer for a run that did not succeed. `done` is the body flag of the run (`installed` or
/// `uninstalled`), false in a 502.
fn failure(err: InstallError, id: &str, done: &str) -> Response {
    match err {
        InstallError::Bad(m) => error(StatusCode::BAD_REQUEST, m),
        InstallError::NotInstalled => error(StatusCode::SERVICE_UNAVAILABLE, "OSE server binary is not installed"),
        InstallError::Busy => error(StatusCode::CONFLICT, "an install is already running on this computer"),
        InstallError::Failed { message, output } => {
            let mut body = json!({ "error": message, "id": id, "output": output });
            body[done] = Value::Bool(false);
            (StatusCode::BAD_GATEWAY, Json(body)).into_response()
        }
        InstallError::Io(m) => error(StatusCode::INTERNAL_SERVER_ERROR, m),
    }
}

async fn install(State(st): State<St>, Path(computer): Path<String>, body: Bytes) -> Response {
    if let Some(answer) = not_local(&st, &computer) {
        return answer;
    }
    let req: InstallRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("bad request body: {e}")),
    };
    let ose = st.ose.clone();
    let (id, version) = (req.id.clone(), req.version.clone());
    let ran = tokio::task::spawn_blocking(move || ose.install_extension(&computer, &id, version.as_deref())).await;
    match ran {
        Ok(Ok(output)) => Json(json!({ "installed": true, "id": req.id, "output": output })).into_response(),
        Ok(Err(e)) => failure(e, &req.id, "installed"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("install task failed: {e}")),
    }
}

async fn uninstall(State(st): State<St>, Path((computer, extension)): Path<(String, String)>) -> Response {
    if let Some(answer) = not_local(&st, &computer) {
        return answer;
    }
    let ose = st.ose.clone();
    let id = extension.clone();
    let ran = tokio::task::spawn_blocking(move || ose.uninstall_extension(&computer, &id)).await;
    match ran {
        Ok(Ok(output)) => Json(json!({ "uninstalled": true, "id": extension, "output": output })).into_response(),
        Ok(Err(e)) => failure(e, &extension, "uninstalled"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("uninstall task failed: {e}")),
    }
}

async fn search(State(st): State<St>, params: Result<Query<SearchParams>, QueryRejection>) -> Response {
    let Query(params) = match params {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("bad query: {e}")),
    };
    let q = match search_query(&params.q) {
        Ok(q) => q,
        Err(m) => return error(StatusCode::BAD_REQUEST, m),
    };
    if params.offset > SEARCH_OFFSET_MAX {
        return error(StatusCode::BAD_REQUEST, format!("offset must be at most {SEARCH_OFFSET_MAX}"));
    }
    match st.gallery.search(&q, params.offset).await {
        Ok(page) => Json(page).into_response(),
        Err(m) => error(StatusCode::BAD_GATEWAY, m),
    }
}
