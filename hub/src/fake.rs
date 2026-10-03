//! An in-process fake of the darkpyonix.dev hub: the subset of `hub.openapi.yaml` (v0.3.0)
//! Ember uses, with the same status codes and bodies. One account; a "signed-in browser" is
//! represented by [`FakeHub::session_token`] sent as a bearer token.
//!
//! Served: `GET /health`, `POST /v1/device-links`, `POST /v1/device-links/{link_id}/token`,
//! `GET`/`POST /v1/link-codes/{user_code}`, `GET /v1/me`, `GET /v1/devices`,
//! `GET`/`DELETE /v1/devices/{endpoint_id}`, `GET /v1/devices/{endpoint_id}/addresses`,
//! `PUT`/`GET /pkarr/{key}` (the pkarr relay protocol, with signature check). Not served: GitHub
//! sign-in, the relay, shares, names.

// Every handler here returns an axum Response as its error; that type's size does not matter in a test fake.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine;
use ember_transport::PeerId;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use crate::link::link_message;
use crate::registration::now_secs;
use crate::types::{Device, Role};
use crate::z32;
use crate::HubClient;

const ACCOUNT_ID: &str = "a_00000000000000f1";
const GITHUB_LOGIN: &str = "octocat";
const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
const MAX_DNS_PACKET: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkStatus {
    Pending,
    Approved,
    Denied,
    Claimed,
}

#[derive(Debug, Clone)]
struct Link {
    link_id: String,
    user_code: String,
    endpoint_id: PeerId,
    name: String,
    role: Role,
    challenge: String,
    status: LinkStatus,
    expires_at: i64,
}

#[derive(Debug, Clone)]
struct FakeDevice {
    device: Device,
    token: String,
    removed: bool,
}

#[derive(Debug, Clone)]
struct Record {
    timestamp_us: u64,
    payload: Vec<u8>,
}

#[derive(Debug)]
struct Inner {
    links: Vec<Link>,
    devices: Vec<FakeDevice>,
    records: HashMap<PeerId, Record>,
    session_token: String,
    interval: u64,
    link_ttl: i64,
    counter: u64,
    base_url: String,
}

impl Inner {
    fn next(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    fn user_code(&mut self) -> String {
        let mut n = self.next().wrapping_mul(2_654_435_761).wrapping_add(12345);
        let mut s = String::new();
        for i in 0..8 {
            if i == 4 {
                s.push('-');
            }
            s.push(USER_CODE_ALPHABET[(n % 20) as usize] as char);
            n /= 20;
        }
        s
    }

    fn token(&mut self) -> String {
        let n = self.next();
        format!("dpd_fake{n:04}{}", hex::encode(n.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes()))
    }

    fn device_by_token(&self, token: &str) -> Option<&FakeDevice> {
        self.devices.iter().find(|d| d.token == token && !d.removed)
    }

    fn live_device(&self, id: &PeerId) -> Option<&FakeDevice> {
        self.devices.iter().find(|d| d.device.endpoint_id == *id && !d.removed)
    }

    fn insert_device(&mut self, endpoint_id: PeerId, name: &str, role: Role) -> (Device, String) {
        let token = self.token();
        let device = Device { endpoint_id, name: name.into(), role, created_at: now_secs(), last_seen: None, online: false };
        self.devices.push(FakeDevice { device: device.clone(), token: token.clone(), removed: false });
        (device, token)
    }
}

type Shared = Arc<Mutex<Inner>>;

/// A running fake hub on `127.0.0.1`. Stops when dropped.
pub struct FakeHub {
    url: String,
    state: Shared,
    task: JoinHandle<()>,
}

impl FakeHub {
    /// Starts the fake on an ephemeral loopback port.
    pub async fn start() -> FakeHub {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind fake hub");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state: Shared = Arc::new(Mutex::new(Inner {
            links: Vec::new(),
            devices: Vec::new(),
            records: HashMap::new(),
            session_token: "fake-session-signed-in-as-octocat".into(),
            interval: 1,
            link_ttl: 900,
            counter: 0,
            base_url: url.clone(),
        }));
        let app = router(state.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeHub { url, state, task }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// A client for this hub without credentials.
    pub fn client(&self) -> HubClient {
        HubClient::new(&self.url)
    }

    /// The bearer that stands for a person signed in with GitHub (account rights).
    pub fn session_token(&self) -> String {
        self.state.lock().unwrap().session_token.clone()
    }

    /// A client acting as the signed-in person in a browser.
    pub fn browser(&self) -> HubClient {
        self.client().with_token(&self.session_token())
    }

    /// The `interval` handed to new links (seconds).
    pub fn set_interval(&self, secs: u64) {
        self.state.lock().unwrap().interval = secs;
    }

    /// Lifetime of new links (seconds; the hub's is 900).
    pub fn set_link_ttl(&self, secs: i64) {
        self.state.lock().unwrap().link_ttl = secs;
    }

    /// User codes of pending links.
    pub fn pending_codes(&self) -> Vec<String> {
        let now = now_secs();
        let s = self.state.lock().unwrap();
        s.links.iter().filter(|l| l.status == LinkStatus::Pending && l.expires_at >= now).map(|l| l.user_code.clone()).collect()
    }

    /// Decides a pending code as the signed-in person would on `/link`.
    pub fn decide(&self, user_code: &str, approve: bool) -> bool {
        let code = normalize_code(user_code);
        let now = now_secs();
        let mut s = self.state.lock().unwrap();
        match s.links.iter_mut().find(|l| Some(&l.user_code) == code.as_ref() && l.status == LinkStatus::Pending && l.expires_at >= now) {
            Some(l) => {
                l.status = if approve { LinkStatus::Approved } else { LinkStatus::Denied };
                true
            }
            None => false,
        }
    }

    pub fn approve(&self, user_code: &str) -> bool {
        self.decide(user_code, true)
    }

    /// Registers a device directly (skipping the link flow); returns its token.
    pub fn register(&self, endpoint_id: PeerId, name: &str, role: Role) -> String {
        self.state.lock().unwrap().insert_device(endpoint_id, name, role).1
    }

    /// Removes a device as the account owner would (`DELETE /v1/devices/{id}`).
    pub fn remove(&self, endpoint_id: &PeerId) -> bool {
        let mut s = self.state.lock().unwrap();
        let found = match s.devices.iter_mut().find(|d| d.device.endpoint_id == *endpoint_id && !d.removed) {
            Some(d) => {
                d.removed = true;
                true
            }
            None => false,
        };
        s.records.remove(endpoint_id);
        found
    }

    /// Devices not removed.
    pub fn devices(&self) -> Vec<Device> {
        self.state.lock().unwrap().devices.iter().filter(|d| !d.removed).map(|d| d.device.clone()).collect()
    }

    /// The stored pkarr relay payload of a device.
    pub fn record(&self, endpoint_id: &PeerId) -> Option<Vec<u8>> {
        self.state.lock().unwrap().records.get(endpoint_id).map(|r| r.payload.clone())
    }
}

impl Drop for FakeHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl std::fmt::Debug for FakeHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FakeHub({})", self.url)
    }
}

// ---------------------------------------------------------------------------------------------
// HTTP

fn router(state: Shared) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({ "status": "ok", "version": "fake" })) }))
        .route("/v1/device-links", post(create_link))
        .route("/v1/device-links/{link_id}/token", post(claim_link))
        .route("/v1/link-codes/{code}", get(get_link_code).post(decide_link_code))
        .route("/v1/me", get(me))
        .route("/v1/devices", get(list_devices))
        .route("/v1/devices/{id}", get(get_device).delete(remove_device))
        .route("/v1/devices/{id}/addresses", get(device_addresses))
        .route("/pkarr/{key}", put(pkarr_put).get(pkarr_get))
        .with_state(state)
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

type HResult = Result<Response, Response>;

enum Principal {
    Session,
    Device { endpoint_id: PeerId, role: Role },
}

impl Principal {
    fn require_account_rights(&self) -> Result<(), Response> {
        match self {
            Principal::Session | Principal::Device { role: Role::MainServer, .. } => Ok(()),
            Principal::Device { .. } => Err(err(StatusCode::FORBIDDEN, "account rights required")),
        }
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("authorization")?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

fn principal(s: &Inner, headers: &HeaderMap, query_token: Option<&str>) -> Result<Principal, Response> {
    let token = bearer(headers).or_else(|| query_token.map(str::to_string));
    let Some(token) = token else { return Err(err(StatusCode::UNAUTHORIZED, "unauthorized")) };
    if token == s.session_token {
        return Ok(Principal::Session);
    }
    match s.device_by_token(&token) {
        Some(d) => Ok(Principal::Device { endpoint_id: d.device.endpoint_id, role: d.device.role }),
        None => Err(err(StatusCode::UNAUTHORIZED, "unauthorized")),
    }
}

fn normalize_code(raw: &str) -> Option<String> {
    let letters: String = raw.chars().filter(|c| *c != '-' && !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
    if letters.len() != 8 || !letters.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)) {
        return None;
    }
    Some(format!("{}-{}", &letters[..4], &letters[4..]))
}

fn parse_peer(s: &str) -> Option<PeerId> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    s.parse().ok()
}

fn role_of(v: &Value) -> Option<Role> {
    match v.as_str()? {
        "main_server" => Some(Role::MainServer),
        "computer" => Some(Role::Computer),
        _ => None,
    }
}

async fn create_link(State(st): State<Shared>, body: Bytes) -> HResult {
    let v: Value = serde_json::from_slice(&body).map_err(|_| err(StatusCode::BAD_REQUEST, "malformed body"))?;
    let endpoint_id = v["endpoint_id"]
        .as_str()
        .and_then(parse_peer)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "endpoint_id must be 64 lowercase hex characters"))?;
    let name = v["name"].as_str().filter(|n| !n.is_empty() && n.chars().count() <= 64).ok_or_else(|| err(StatusCode::BAD_REQUEST, "name must be 1 to 64 characters"))?;
    let role = role_of(&v["role"]).ok_or_else(|| err(StatusCode::BAD_REQUEST, "role must be main_server or computer"))?;
    let mut s = st.lock().unwrap();
    if s.devices.iter().any(|d| d.device.endpoint_id == endpoint_id) {
        return Err(err(StatusCode::CONFLICT, "endpoint id already registered (removed keys are not reused)"));
    }
    let n = s.next();
    let link_id = format!("l_{n:032x}");
    let challenge = hex::encode([n.to_be_bytes(), (!n).to_be_bytes(), n.rotate_left(17).to_be_bytes(), n.wrapping_mul(7).to_be_bytes()].concat());
    let user_code = s.user_code();
    let expires_at = now_secs() + s.link_ttl;
    s.links.push(Link {
        link_id: link_id.clone(),
        user_code: user_code.clone(),
        endpoint_id,
        name: name.into(),
        role,
        challenge: challenge.clone(),
        status: LinkStatus::Pending,
        expires_at,
    });
    let verification = format!("{}/link", s.base_url);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "link_id": link_id,
            "user_code": user_code,
            "verification_uri": verification,
            "verification_uri_complete": format!("{verification}?code={user_code}"),
            "challenge": challenge,
            "interval": s.interval,
            "expires_at": expires_at,
        })),
    )
        .into_response())
}

async fn claim_link(State(st): State<Shared>, Path(link_id): Path<String>, body: Bytes) -> HResult {
    let v: Value = serde_json::from_slice(&body).map_err(|_| err(StatusCode::BAD_REQUEST, "malformed body"))?;
    let mut s = st.lock().unwrap();
    let now = now_secs();
    let Some(idx) = s.links.iter().position(|l| l.link_id == link_id) else {
        return Err(err(StatusCode::NOT_FOUND, "unknown, expired or already claimed link"));
    };
    let link = s.links[idx].clone();
    if link.expires_at < now || link.status == LinkStatus::Claimed {
        return Err(err(StatusCode::NOT_FOUND, "unknown, expired or already claimed link"));
    }
    let sig: [u8; 64] = v["signature"]
        .as_str()
        .filter(|h| h.len() == 128)
        .and_then(|h| hex::decode(h).ok())
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "signature must be 128 lowercase hex characters"))?;
    if !link.endpoint_id.verify(&link_message(&link.link_id, &link.challenge), &sig) {
        return Err(err(StatusCode::BAD_REQUEST, "signature does not verify"));
    }
    match link.status {
        LinkStatus::Pending => Ok((StatusCode::ACCEPTED, Json(json!({ "status": "pending" }))).into_response()),
        LinkStatus::Denied => Err(err(StatusCode::FORBIDDEN, "the link was denied")),
        LinkStatus::Claimed => unreachable!(),
        LinkStatus::Approved => {
            s.links[idx].status = LinkStatus::Claimed;
            if s.devices.iter().any(|d| d.device.endpoint_id == link.endpoint_id) {
                return Err(err(StatusCode::CONFLICT, "endpoint id already registered"));
            }
            let (device, token) = s.insert_device(link.endpoint_id, &link.name, link.role);
            Ok((StatusCode::CREATED, Json(json!({ "device": device, "device_token": token }))).into_response())
        }
    }
}

fn pending_by_code(s: &Inner, raw: &str) -> Result<usize, Response> {
    let code = normalize_code(raw).ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown or expired code"))?;
    let now = now_secs();
    s.links
        .iter()
        .position(|l| l.user_code == code && l.status == LinkStatus::Pending && l.expires_at >= now)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown or expired code"))
}

async fn get_link_code(State(st): State<Shared>, headers: HeaderMap, Path(code): Path<String>) -> HResult {
    let s = st.lock().unwrap();
    principal(&s, &headers, None)?.require_account_rights()?;
    let l = &s.links[pending_by_code(&s, &code)?];
    Ok(Json(json!({
        "user_code": l.user_code,
        "endpoint_id": l.endpoint_id,
        "name": l.name,
        "role": l.role,
        "expires_at": l.expires_at,
    }))
    .into_response())
}

async fn decide_link_code(State(st): State<Shared>, headers: HeaderMap, Path(code): Path<String>, body: Bytes) -> HResult {
    let mut s = st.lock().unwrap();
    principal(&s, &headers, None)?.require_account_rights()?;
    let v: Value = serde_json::from_slice(&body).map_err(|_| err(StatusCode::BAD_REQUEST, "malformed body"))?;
    let approve = v["approve"].as_bool().ok_or_else(|| err(StatusCode::BAD_REQUEST, "approve must be a boolean"))?;
    let idx = pending_by_code(&s, &code)?;
    s.links[idx].status = if approve { LinkStatus::Approved } else { LinkStatus::Denied };
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn me(State(st): State<Shared>, headers: HeaderMap) -> HResult {
    let s = st.lock().unwrap();
    let (via, endpoint) = match principal(&s, &headers, None)? {
        Principal::Session => ("session", None),
        Principal::Device { endpoint_id, .. } => ("device", Some(endpoint_id)),
    };
    Ok(Json(json!({ "account_id": ACCOUNT_ID, "github_login": GITHUB_LOGIN, "via": via, "endpoint_id": endpoint })).into_response())
}

async fn list_devices(State(st): State<Shared>, headers: HeaderMap) -> HResult {
    let s = st.lock().unwrap();
    principal(&s, &headers, None)?;
    let devices: Vec<&Device> = s.devices.iter().filter(|d| !d.removed).map(|d| &d.device).collect();
    Ok(Json(json!({ "devices": devices })).into_response())
}

async fn get_device(State(st): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> HResult {
    let s = st.lock().unwrap();
    principal(&s, &headers, None)?;
    let id = parse_peer(&id).ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;
    let d = s.live_device(&id).ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;
    Ok(Json(json!(d.device)).into_response())
}

async fn remove_device(State(st): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> HResult {
    let mut s = st.lock().unwrap();
    principal(&s, &headers, None)?.require_account_rights()?;
    let id = parse_peer(&id).ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;
    let d = s
        .devices
        .iter_mut()
        .find(|d| d.device.endpoint_id == id && !d.removed)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;
    d.removed = true;
    s.records.remove(&id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn device_addresses(State(st): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> HResult {
    let s = st.lock().unwrap();
    principal(&s, &headers, None)?;
    let id = parse_peer(&id).ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;
    s.live_device(&id).ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;
    let rec = s.records.get(&id).ok_or_else(|| err(StatusCode::NOT_FOUND, "nothing published"))?;
    let dns = &rec.payload[72..];
    let answers = parse_txt_answers(dns).ok_or_else(|| err(StatusCode::NOT_FOUND, "nothing published"))?;
    let own = format!("_iroh.{}", z32::encode(id.as_bytes()));
    let mut relays = Vec::new();
    let mut direct = Vec::new();
    for (name, txt) in answers {
        if name != own && name != "_iroh" {
            continue;
        }
        let mut parts = txt.split('=');
        let (Some(k), Some(v)) = (parts.next(), parts.next()) else { continue };
        match k {
            "relay" => relays.push(v.to_string()),
            "addr" => direct.push(v.to_string()),
            _ => {}
        }
    }
    Ok(Json(json!({
        "endpoint_id": id,
        "relay_urls": relays,
        "direct_addresses": direct,
        "published_at_us": rec.timestamp_us,
        "signed_packet": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&rec.payload),
    }))
    .into_response())
}

async fn pkarr_put(State(st): State<Shared>, Path(key): Path<String>, body: Bytes) -> HResult {
    let key = z32::decode_key(&key).ok_or_else(|| err(StatusCode::BAD_REQUEST, "malformed key"))?;
    let peer = PeerId::from_bytes(key).map_err(|_| err(StatusCode::BAD_REQUEST, "malformed key"))?;
    if body.len() < 72 || body.len() > 72 + MAX_DNS_PACKET {
        return Err(err(StatusCode::BAD_REQUEST, "malformed payload or bad signature"));
    }
    let sig: [u8; 64] = body[..64].try_into().unwrap();
    let timestamp_us = u64::from_be_bytes(body[64..72].try_into().unwrap());
    let dns = &body[72..];
    let mut signable = format!("3:seqi{timestamp_us}e1:v{}:", dns.len()).into_bytes();
    signable.extend_from_slice(dns);
    if !peer.verify(&signable, &sig) || parse_txt_answers(dns).is_none() {
        return Err(err(StatusCode::BAD_REQUEST, "malformed payload or bad signature"));
    }
    let mut s = st.lock().unwrap();
    if s.live_device(&peer).is_none() {
        return Err(err(StatusCode::FORBIDDEN, "not a registered device"));
    }
    if s.records.get(&peer).is_some_and(|r| r.timestamp_us >= timestamp_us) {
        return Err(err(StatusCode::CONFLICT, "not newer than the stored packet"));
    }
    s.records.insert(peer, Record { timestamp_us, payload: body.to_vec() });
    let now = now_secs();
    if let Some(d) = s.devices.iter_mut().find(|d| d.device.endpoint_id == peer && !d.removed) {
        d.device.last_seen = Some(now);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

async fn pkarr_get(State(st): State<Shared>, headers: HeaderMap, Path(key): Path<String>, Query(q): Query<TokenQuery>) -> HResult {
    let key = z32::decode_key(&key).ok_or_else(|| err(StatusCode::BAD_REQUEST, "malformed key"))?;
    let peer = PeerId::from_bytes(key).map_err(|_| err(StatusCode::BAD_REQUEST, "malformed key"))?;
    let s = st.lock().unwrap();
    principal(&s, &headers, q.token.as_deref())?;
    s.live_device(&peer).ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;
    let rec = s.records.get(&peer).ok_or_else(|| err(StatusCode::NOT_FOUND, "nothing published"))?;
    Ok(([("content-type", "application/octet-stream")], rec.payload.clone()).into_response())
}

// ---------------------------------------------------------------------------------------------
// DNS: just enough to read TXT answers of a pkarr packet.

fn read_name(b: &[u8], start: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut off = start;
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *b.get(off)? as usize;
        if len & 0xc0 == 0xc0 {
            let lo = *b.get(off + 1)? as usize;
            jumps += 1;
            if jumps > 32 {
                return None;
            }
            end.get_or_insert(off + 2);
            off = ((len & 0x3f) << 8) | lo;
            continue;
        }
        if len & 0xc0 != 0 {
            return None;
        }
        if len == 0 {
            end.get_or_insert(off + 1);
            break;
        }
        let label = b.get(off + 1..off + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        off += 1 + len;
    }
    Some((labels.join("."), end?))
}

/// `(name, text)` of each TXT answer; `None` if malformed.
fn parse_txt_answers(b: &[u8]) -> Option<Vec<(String, String)>> {
    let u16_at = |o: usize| -> Option<u16> { Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?])) };
    let qd = u16_at(4)?;
    let an = u16_at(6)?;
    let mut off = 12;
    for _ in 0..qd {
        off = read_name(b, off)?.1 + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        let (name, end) = read_name(b, off)?;
        let ty = u16_at(end)?;
        let rdlen = u16_at(end + 8)? as usize;
        let rdata = b.get(end + 10..end + 10 + rdlen)?;
        off = end + 10 + rdlen;
        if ty == 16 {
            let mut txt = String::new();
            let mut o = 0;
            while o < rdata.len() {
                let n = rdata[o] as usize;
                txt.push_str(std::str::from_utf8(rdata.get(o + 1..o + 1 + n)?).ok()?);
                o += 1 + n;
            }
            out.push((name, txt));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_dns() {
        assert_eq!(normalize_code("bcdf-ghjk").as_deref(), Some("BCDF-GHJK"));
        assert_eq!(normalize_code("BCDFGHJK").as_deref(), Some("BCDF-GHJK"));
        assert_eq!(normalize_code("AEIO-UUUU"), None);

        // One TXT answer `_iroh.x` = "relay=https://r/" (no compression).
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        p.extend_from_slice(&[5, b'_', b'i', b'r', b'o', b'h', 1, b'x', 0]);
        p.extend_from_slice(&[0, 16, 0, 1, 0, 0, 0, 30]);
        let txt = b"relay=https://r/";
        p.extend_from_slice(&((txt.len() + 1) as u16).to_be_bytes());
        p.push(txt.len() as u8);
        p.extend_from_slice(txt);
        assert_eq!(parse_txt_answers(&p), Some(vec![("_iroh.x".to_string(), "relay=https://r/".to_string())]));
        assert_eq!(parse_txt_answers(&p[..p.len() - 1]), None);
    }
}
