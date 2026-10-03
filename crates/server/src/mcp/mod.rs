//! Central MCP server registry (SPEC FR-A7).
//!
//! The user adds an MCP server once (`/api/v1/mcp`, [`api`]); every agent session that supports
//! MCP gets it at process start, through [`StartRequest::mcp_servers`] — the same path as the
//! project browser (FR-R3, `crate::browser::agent`), so Claude Code receives it in
//! `--mcp-config`, Codex as `-c mcp_servers.<name>.…` overrides and Antigravity in its session
//! root's `mcp_config.json`. A server applies to every project (`scope: "all"`) or to one
//! (`"project:<name>"`), and can be switched off without deleting it.
//!
//! # Secrets
//!
//! A server's environment is usually where its credentials go (`GITHUB_TOKEN`, …), so every
//! value is treated as a secret:
//! - sealed at rest with the server key (`accounts::secrets::SecretBox`), bound to the row;
//! - never returned: [`McpEntry`] carries the variable names only (`env_keys`);
//! - never on an agent's command line: Claude Code reads it from a private `0600` config file,
//!   Codex from its process environment through `env_vars`, Antigravity from a private `0600`
//!   `mcp_config.json` in its `0700` session root (see the adapters);
//! - never logged (`McpServer`'s `Debug` prints names only) and never an event, so it cannot
//!   reach a transcript or the push channel.

pub mod api;
pub mod schema;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::accounts::secrets::SecretBox;
use crate::agents::{McpServer, StartRequest};
use crate::session::{StartConfigHook, Sessions};
use crate::store::{now_ms, SessionRecord, Store};

const AAD_PREFIX: &str = "ember/mcp-env/v1:";

/// Names the registry may not use: the project browser's server (FR-R3).
pub const RESERVED_NAMES: &[&str] = &[crate::browser::agent::MCP_NAME];

/// Which sessions a registry server applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpScope {
    All,
    Project(String),
}

impl McpScope {
    pub fn parse(s: &str) -> Result<McpScope, McpError> {
        let s = s.trim();
        if s == "all" {
            return Ok(McpScope::All);
        }
        match s.strip_prefix("project:") {
            Some(p) if !p.trim().is_empty() => Ok(McpScope::Project(p.to_string())),
            _ => Err(McpError::BadRequest(format!(
                "scope must be \"all\" or \"project:<name>\" (got {s:?})"
            ))),
        }
    }

    pub fn as_string(&self) -> String {
        match self {
            McpScope::All => "all".into(),
            McpScope::Project(p) => format!("project:{p}"),
        }
    }

    pub fn applies_to(&self, project: &str) -> bool {
        match self {
            McpScope::All => true,
            McpScope::Project(p) => p == project,
        }
    }
}

/// A registry entry as clients see it: no environment values, only their names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpEntry {
    pub id: String,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Names of the environment variables set for the server (values are never returned).
    pub env_keys: Vec<String>,
    pub enabled: bool,
    /// `all` or `project:<name>`.
    pub scope: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// `POST /api/v1/mcp`. No `Debug`: `env` holds secrets.
#[derive(Deserialize)]
pub struct NewMcp {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "all_scope")]
    pub scope: String,
}

fn yes() -> bool {
    true
}

fn all_scope() -> String {
    "all".into()
}

/// `PATCH /api/v1/mcp/{id}`: `None` leaves a field alone. `env` is merged: a string sets that
/// variable, `null` removes it, unnamed variables stay. No `Debug`: `env` holds secrets.
#[derive(Default, Deserialize)]
pub struct McpPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, Option<String>>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("MCP server {0} not found")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("an MCP server named {0:?} already exists")]
    Conflict(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for McpError {
    fn from(e: rusqlite::Error) -> Self {
        McpError::Other(e.into())
    }
}

/// A name the agents can use as an MCP server name and Codex as a TOML bare key.
pub fn validate_name(name: &str) -> Result<(), McpError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        return Err(McpError::BadRequest(format!(
            "MCP server name {name:?} must be 1-64 letters, digits, '-' or '_'"
        )));
    }
    if RESERVED_NAMES.contains(&name) {
        return Err(McpError::BadRequest(format!("MCP server name {name:?} is reserved")));
    }
    Ok(())
}

/// A portable environment variable name. The error never contains a value.
pub fn validate_env_key(key: &str) -> Result<(), McpError> {
    let mut chars = key.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(McpError::BadRequest(format!("environment variable name {key:?} is not valid")))
    }
}

fn validate_command(command: &str) -> Result<(), McpError> {
    if command.trim().is_empty() {
        return Err(McpError::BadRequest("command must not be empty".into()));
    }
    Ok(())
}

fn aad(id: &str) -> Vec<u8> {
    format!("{AAD_PREFIX}{id}").into_bytes()
}

/// The registry: the `mcp_servers` table plus the key that seals environments.
pub struct McpRegistry {
    store: Arc<Store>,
    secrets: SecretBox,
}

const COLUMNS: &str = "id, name, command, args, env_keys, enabled, scope, created_at, updated_at";

fn row_to_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<McpEntry> {
    let args: String = r.get(3)?;
    let keys: String = r.get(4)?;
    Ok(McpEntry {
        id: r.get(0)?,
        name: r.get(1)?,
        command: r.get(2)?,
        args: serde_json::from_str(&args).unwrap_or_default(),
        env_keys: serde_json::from_str(&keys).unwrap_or_default(),
        enabled: r.get(5)?,
        scope: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

impl McpRegistry {
    /// The registry in `store`, sealing with `<data dir>/secret.key` (created if missing; the same
    /// key as API-provider keys, FR-U5).
    pub fn open(store: Arc<Store>, data_dir: &Path) -> anyhow::Result<Arc<McpRegistry>> {
        let secrets = SecretBox::open_or_create(&data_dir.join("secret.key"))?;
        Ok(Self::with_secrets(store, secrets))
    }

    pub fn with_secrets(store: Arc<Store>, secrets: SecretBox) -> Arc<McpRegistry> {
        Arc::new(McpRegistry { store, secrets })
    }

    /// Give every agent process this registry's servers (a start-config hook).
    pub fn install(self: &Arc<Self>, sessions: &Sessions) {
        sessions.add_start_config_hook(self.start_config_hook());
    }

    pub fn list(&self) -> anyhow::Result<Vec<McpEntry>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM mcp_servers ORDER BY name"))?;
        let rows = stmt.query_map([], row_to_entry)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get(&self, id: &str) -> Result<McpEntry, McpError> {
        self.store
            .conn()
            .query_row(&format!("SELECT {COLUMNS} FROM mcp_servers WHERE id = ?1"), params![id], row_to_entry)
            .optional()?
            .ok_or_else(|| McpError::NotFound(id.to_string()))
    }

    fn name_taken(&self, name: &str, except: Option<&str>) -> anyhow::Result<bool> {
        Ok(self
            .store
            .conn()
            .query_row(
                "SELECT 1 FROM mcp_servers WHERE name = ?1 AND (?2 IS NULL OR id != ?2)",
                params![name, except],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Seal `env` for row `id`: `(keys JSON, nonce, ciphertext)`, nonce/ciphertext `None` when
    /// empty.
    fn seal_env(
        &self,
        id: &str,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<(String, Option<Vec<u8>>, Option<Vec<u8>>)> {
        let keys: Vec<&String> = env.keys().collect();
        let keys_json = serde_json::to_string(&keys)?;
        if env.is_empty() {
            return Ok((keys_json, None, None));
        }
        let plain = Zeroizing::new(serde_json::to_vec(env)?);
        let (nonce, ct) = self.secrets.seal(&plain, &aad(id));
        Ok((keys_json, Some(nonce), Some(ct)))
    }

    /// The decrypted environment of server `id` (empty when it has none). For the start hook
    /// only; never send it to a client.
    fn env_of(&self, id: &str) -> anyhow::Result<BTreeMap<String, String>> {
        let row: Option<(Option<Vec<u8>>, Option<Vec<u8>>)> = self
            .store
            .conn()
            .query_row(
                "SELECT env_nonce, env_ciphertext FROM mcp_servers WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((Some(nonce), Some(ct))) = row else {
            return Ok(BTreeMap::new());
        };
        let plain = self
            .secrets
            .open(&nonce, &ct, &aad(id))
            .map_err(|_| anyhow::anyhow!("MCP server environment failed to decrypt (wrong secret.key or tampered row)"))?;
        serde_json::from_slice(&plain).context("stored MCP environment is not a JSON object")
    }

    pub fn add(&self, new: NewMcp) -> Result<McpEntry, McpError> {
        let name = new.name.trim().to_string();
        validate_name(&name)?;
        validate_command(&new.command)?;
        for key in new.env.keys() {
            validate_env_key(key)?;
        }
        let scope = McpScope::parse(&new.scope)?;
        if self.name_taken(&name, None)? {
            return Err(McpError::Conflict(name));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (keys, nonce, ct) = self.seal_env(&id, &new.env)?;
        let now = now_ms();
        self.store.conn().execute(
            "INSERT INTO mcp_servers (id, name, command, args, env_keys, env_nonce, env_ciphertext, enabled, scope, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                id,
                name,
                new.command,
                serde_json::to_string(&new.args).map_err(anyhow::Error::from)?,
                keys,
                nonce,
                ct,
                new.enabled,
                scope.as_string(),
                now
            ],
        )?;
        self.get(&id)
    }

    pub fn update(&self, id: &str, patch: McpPatch) -> Result<McpEntry, McpError> {
        let current = self.get(id)?;
        let name = match &patch.name {
            Some(n) => {
                let n = n.trim().to_string();
                validate_name(&n)?;
                if self.name_taken(&n, Some(id))? {
                    return Err(McpError::Conflict(n));
                }
                n
            }
            None => current.name.clone(),
        };
        if let Some(c) = &patch.command {
            validate_command(c)?;
        }
        let scope = match &patch.scope {
            Some(s) => McpScope::parse(s)?.as_string(),
            None => current.scope.clone(),
        };
        let now = now_ms();
        let conn_args = patch.args.as_ref().map(serde_json::to_string).transpose().map_err(anyhow::Error::from)?;
        if let Some(changes) = &patch.env {
            for key in changes.keys() {
                validate_env_key(key)?;
            }
            let mut env = self.env_of(id)?;
            for (k, v) in changes {
                match v {
                    Some(v) => {
                        env.insert(k.clone(), v.clone());
                    }
                    None => {
                        env.remove(k);
                    }
                }
            }
            let (keys, nonce, ct) = self.seal_env(id, &env)?;
            self.store.conn().execute(
                "UPDATE mcp_servers SET env_keys = ?2, env_nonce = ?3, env_ciphertext = ?4 WHERE id = ?1",
                params![id, keys, nonce, ct],
            )?;
        }
        self.store.conn().execute(
            "UPDATE mcp_servers SET name = ?2, command = COALESCE(?3, command), args = COALESCE(?4, args),
                 enabled = COALESCE(?5, enabled), scope = ?6, updated_at = ?7
             WHERE id = ?1",
            params![id, name, patch.command, conn_args, patch.enabled, scope, now],
        )?;
        self.get(id)
    }

    pub fn delete(&self, id: &str) -> Result<(), McpError> {
        let n = self.store.conn().execute("DELETE FROM mcp_servers WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(McpError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// The enabled servers that apply to `project`, with their environment, as the adapters
    /// take them. A server whose environment cannot be decrypted is left out (logged by name).
    pub fn servers_for(&self, project: &str) -> anyhow::Result<Vec<McpServer>> {
        let mut out = Vec::new();
        for e in self.list()? {
            if !e.enabled {
                continue;
            }
            let applies = McpScope::parse(&e.scope).map(|s| s.applies_to(project)).unwrap_or(false);
            if !applies {
                continue;
            }
            let env = match self.env_of(&e.id) {
                Ok(env) => env,
                Err(err) => {
                    tracing::warn!(mcp = %e.name, "MCP server left out: {err:#}");
                    continue;
                }
            };
            out.push(McpServer {
                name: e.name,
                command: e.command,
                args: e.args,
                startup_timeout_secs: None,
                env: env.into_iter().collect(),
            });
        }
        Ok(out)
    }

    /// Add the registry's servers for the session's project to `req`, after whatever earlier
    /// hooks added (the project browser). A name already present is not added twice.
    pub fn configure_start(&self, rec: &SessionRecord, req: &mut StartRequest) -> anyhow::Result<()> {
        for server in self.servers_for(&rec.project)? {
            if req.mcp_servers.iter().any(|s| s.name == server.name) {
                tracing::warn!(mcp = %server.name, session = %rec.id, "MCP server name already in use for this session; registry entry skipped");
                continue;
            }
            req.mcp_servers.push(server);
        }
        Ok(())
    }

    pub fn start_config_hook(self: &Arc<Self>) -> StartConfigHook {
        let weak = Arc::downgrade(self);
        Arc::new(move |rec: &SessionRecord, req: &mut StartRequest| match weak.upgrade() {
            Some(reg) => {
                // A broken registry must not stop the agent: log and start without it.
                if let Err(e) = reg.configure_start(rec, req) {
                    tracing::warn!(session = %rec.id, "MCP registry unavailable for this start: {e:#}");
                }
                Ok(())
            }
            None => Ok(()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;

    fn registry() -> (Arc<Store>, Arc<McpRegistry>) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let reg = McpRegistry::with_secrets(store.clone(), SecretBox::ephemeral());
        (store, reg)
    }

    fn new(name: &str, scope: &str, env: &[(&str, &str)]) -> NewMcp {
        NewMcp {
            name: name.into(),
            command: "/usr/local/bin/mcp".into(),
            args: vec!["--stdio".into()],
            env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            enabled: true,
            scope: scope.into(),
        }
    }

    #[test]
    fn env_is_sealed_listed_by_name_and_merged_on_update() {
        let (store, reg) = registry();
        let e = reg.add(new("github", "all", &[("GITHUB_TOKEN", "ghp_SECRET_1"), ("LOG", "debug")])).unwrap();
        assert_eq!(e.env_keys, ["GITHUB_TOKEN", "LOG"]);
        let json = serde_json::to_string(&reg.list().unwrap()).unwrap();
        assert!(!json.contains("SECRET"), "{json}");
        assert!(!json.contains("debug"), "{json}");

        // Not in the database in the clear.
        let ct: Vec<u8> = store
            .conn()
            .query_row("SELECT env_ciphertext FROM mcp_servers WHERE id = ?1", [&e.id], |r| r.get(0))
            .unwrap();
        assert!(!ct.windows(6).any(|w| w == b"SECRET"));

        let mut changes = BTreeMap::new();
        changes.insert("LOG".to_string(), None);
        changes.insert("EXTRA".to_string(), Some("x".to_string()));
        let e2 = reg.update(&e.id, McpPatch { env: Some(changes), enabled: Some(false), ..Default::default() }).unwrap();
        assert_eq!(e2.env_keys, ["EXTRA", "GITHUB_TOKEN"]);
        assert!(!e2.enabled);
        let env = reg.env_of(&e.id).unwrap();
        assert_eq!(env.get("GITHUB_TOKEN").map(String::as_str), Some("ghp_SECRET_1"));
        assert!(!env.contains_key("LOG"));
    }

    #[test]
    fn validation_and_conflicts() {
        let (_, reg) = registry();
        assert!(matches!(reg.add(new("bad name", "all", &[])), Err(McpError::BadRequest(_))));
        assert!(matches!(reg.add(new("ember-browser", "all", &[])), Err(McpError::BadRequest(_))));
        assert!(matches!(reg.add(new("x", "everyone", &[])), Err(McpError::BadRequest(_))));
        assert!(matches!(reg.add(new("x", "project:", &[])), Err(McpError::BadRequest(_))));
        let err = reg.add(new("x", "all", &[("1BAD", "sekrit-value")])).err().unwrap();
        assert!(!err.to_string().contains("sekrit"), "errors never echo values");
        reg.add(new("x", "all", &[])).unwrap();
        assert!(matches!(reg.add(new("x", "all", &[])), Err(McpError::Conflict(_))));
        assert!(matches!(reg.delete("nope"), Err(McpError::NotFound(_))));
    }

    #[test]
    fn start_requests_get_enabled_servers_in_scope_after_the_browser() {
        let (store, reg) = registry();
        reg.add(new("everywhere", "all", &[("TOKEN", "t-1")])).unwrap();
        reg.add(new("acme-only", "project:acme", &[])).unwrap();
        reg.add(new("other-only", "project:other", &[])).unwrap();
        let off = reg.add(new("off", "all", &[])).unwrap();
        reg.update(&off.id, McpPatch { enabled: Some(false), ..Default::default() }).unwrap();

        let rec = store.create_session("acme", AgentKind::ClaudeCode, "/tmp", None, "t").unwrap();
        let mut req = StartRequest::default();
        req.mcp_servers.push(McpServer { name: "ember-browser".into(), command: "npx".into(), ..Default::default() });
        reg.start_config_hook()(&rec, &mut req).unwrap();
        let names: Vec<&str> = req.mcp_servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["ember-browser", "acme-only", "everywhere"]);
        let every = req.mcp_servers.iter().find(|s| s.name == "everywhere").unwrap();
        assert_eq!(every.env, [("TOKEN".to_string(), "t-1".to_string())]);
        assert!(!format!("{req:?}").contains("t-1"), "Debug never prints values");
    }
}
