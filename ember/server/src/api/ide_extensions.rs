//! Extension install for the Ember editor's OSE (SPEC FR-W4, #459, EDITOR-PARITY task 6).
//!
//! | Method | Path | Body | Answer |
//! | ------ | ---- | ---- | ------ |
//! | POST | `/api/computers/{id}/ide/extensions` | `{"id":"publisher.name","version":"1.2.3"}` | 200 `{installed,id,output}` |
//!
//! Only the local computer has an OSE (same reason as `ide.rs`: it runs on the server machine),
//! so another registered computer answers 400, an unknown one 404. The id and version are
//! checked against a strict pattern (`ose::extension_spec`): no VSIX paths, no URLs, nothing
//! that starts with `-`. The OSE binary runs directly (no shell) with the computer's data dir;
//! at most one install per computer runs at a time (a second answers 409); a run is killed
//! after 120 s. A failed run answers 502 with what it printed (at most 16 KiB).
//!
//! There is no list route: `--list-extensions` is not documented in this repository
//! (unverified), so it is not called. The route sits in the token-protected router and is not
//! exempt; the guest door never serves it.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::computers::{Computers, LOCAL};
use crate::ose::{valid_computer_id, InstallError, OseManager};

#[derive(Clone)]
struct St {
    computers: Arc<Computers>,
    ose: Arc<OseManager>,
}

pub fn router(computers: Arc<Computers>, ose: Arc<OseManager>) -> Router {
    Router::new()
        .route("/api/computers/{id}/ide/extensions", post(install))
        .with_state(St { computers, ose })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRequest {
    id: String,
    version: Option<String>,
}

fn error(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

async fn install(State(st): State<St>, Path(computer): Path<String>, body: Bytes) -> Response {
    if !valid_computer_id(&computer) {
        return error(StatusCode::BAD_REQUEST, "invalid computer id");
    }
    if computer != LOCAL {
        match st.computers.registry().get(&computer) {
            Ok(Some(_)) => {
                return error(StatusCode::BAD_REQUEST, "OSE server is only available on the local computer")
            }
            Ok(None) => return error(StatusCode::NOT_FOUND, format!("unknown computer: {computer}")),
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        }
    }
    let req: InstallRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("bad request body: {e}")),
    };
    let ose = st.ose.clone();
    let (id, version) = (req.id.clone(), req.version.clone());
    let ran = tokio::task::spawn_blocking(move || ose.install_extension(&computer, &id, version.as_deref())).await;
    match ran {
        Ok(Ok(output)) => {
            Json(json!({ "installed": true, "id": req.id, "output": output })).into_response()
        }
        Ok(Err(InstallError::Bad(m))) => error(StatusCode::BAD_REQUEST, m),
        Ok(Err(InstallError::NotInstalled)) => error(StatusCode::SERVICE_UNAVAILABLE, "OSE server binary is not installed"),
        Ok(Err(InstallError::Busy)) => error(StatusCode::CONFLICT, "an install is already running on this computer"),
        Ok(Err(InstallError::Failed { message, output })) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": message, "installed": false, "id": req.id, "output": output })),
        )
            .into_response(),
        Ok(Err(InstallError::Io(m))) => error(StatusCode::INTERNAL_SERVER_ERROR, m),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("install task failed: {e}")),
    }
}
