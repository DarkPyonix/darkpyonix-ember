//! Team member definition (SPEC FR-A9, FR-U1, FR-U6; INTENT D18).
//!
//! A member is defined by a harness plus exactly one account choice: a fixed account id, or the
//! automatic account-switching pipeline (the account router, FR-U3). Optionally a model proxy
//! path (opencodex style) sits on top: the harness is pointed at a proxy endpoint so it runs a
//! model from another source. Only Claude Code supports the proxy path today
//! (`ANTHROPIC_BASE_URL`); the other harnesses are rejected with an error rather than guessed at,
//! because their proxy behaviour is not verified (AGENTS.md rule 4).
//!
//! The proxy server itself is not part of Ember; the member only names its endpoint. The
//! endpoint is stored per session; the key is never stored, it is read from the server
//! environment variable the member names, so it stays out of transcripts and the database
//! (FR-U5).

use rusqlite::{params, OptionalExtension};
use serde::Deserialize;

use crate::agents::AgentKind;
use crate::session::{NewSession, SessionError, Sessions};
use crate::store::{SessionRecord, Store};

/// Store migration (appended to `store::MIGRATIONS`): the proxy endpoint of a session.
pub const MIGRATION: &str = "
CREATE TABLE session_proxy (
    session_id  TEXT PRIMARY KEY,
    endpoint    TEXT NOT NULL,
    key_env     TEXT
);
";

/// Placeholder credential when the proxy needs none, so the harness still starts.
pub const PLACEHOLDER_KEY: &str = "ember-proxy";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MemberError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Unsupported(String),
}

/// A model proxy path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProxySpec {
    /// Base URL of the proxy (`http://` or `https://`).
    pub endpoint: String,
    /// Name of a server environment variable holding the proxy key. The key itself is never
    /// part of the definition.
    #[serde(default)]
    pub key_env: Option<String>,
}

/// How a member picks its account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountChoice {
    Fixed(String),
    Pipeline,
}

/// What a client sends to define a member.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MemberDef {
    /// `claude-code` (`claude code`, `claude`), `codex`, `agy` (`antigravity`), or an ACP agent
    /// name.
    pub harness: String,
    /// A fixed account id.
    #[serde(default)]
    pub account: Option<String>,
    /// Use the automatic account-switching pipeline.
    #[serde(default)]
    pub pipeline: bool,
    #[serde(default)]
    pub proxy: Option<ProxySpec>,
}

/// A validated definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMember {
    pub agent: AgentKind,
    pub account: AccountChoice,
    pub proxy: Option<ProxySpec>,
}

/// Map a harness label to an agent.
pub fn parse_harness(s: &str) -> Option<AgentKind> {
    let k = s.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    match k.as_str() {
        "claude-code" | "claude" | "claudecode" => Some(AgentKind::ClaudeCode),
        "agy" | "antigravity" => Some(AgentKind::Antigravity),
        _ => AgentKind::parse(&k),
    }
}

impl MemberDef {
    pub fn resolve(&self) -> Result<ResolvedMember, MemberError> {
        let agent = parse_harness(&self.harness)
            .ok_or_else(|| MemberError::Invalid(format!("unknown harness {:?}", self.harness)))?;
        let account = self.account.as_deref().map(str::trim).filter(|a| !a.is_empty());
        let account = match (account, self.pipeline) {
            (Some(_), true) => {
                return Err(MemberError::Invalid(
                    "choose either a fixed account or the switching pipeline, not both".into(),
                ))
            }
            (None, false) => {
                return Err(MemberError::Invalid(
                    "choose an account: a fixed account id or the switching pipeline".into(),
                ))
            }
            (Some(id), false) => AccountChoice::Fixed(id.to_string()),
            (None, true) => AccountChoice::Pipeline,
        };
        if let Some(p) = &self.proxy {
            proxy_env(agent, p, |_| None)?;
        }
        Ok(ResolvedMember { agent, account, proxy: self.proxy.clone() })
    }
}

/// The environment that points `agent` at the proxy. `lookup` reads the key variable.
pub fn proxy_env(
    agent: AgentKind,
    proxy: &ProxySpec,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Vec<(String, String)>, MemberError> {
    let endpoint = proxy.endpoint.trim();
    if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
        return Err(MemberError::Invalid(format!(
            "proxy endpoint {endpoint:?} must start with http:// or https://"
        )));
    }
    match agent {
        AgentKind::ClaudeCode => {
            let key = proxy
                .key_env
                .as_deref()
                .and_then(&lookup)
                .unwrap_or_else(|| PLACEHOLDER_KEY.to_string());
            Ok(vec![
                ("ANTHROPIC_BASE_URL".into(), endpoint.to_string()),
                ("ANTHROPIC_AUTH_TOKEN".into(), key),
            ])
        }
        other => Err(MemberError::Unsupported(format!(
            "the model proxy path is supported for claude-code only; {} has no verified proxy \
             setting",
            other.as_str()
        ))),
    }
}

impl Store {
    pub fn set_session_proxy(&self, session_id: &str, p: &ProxySpec) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO session_proxy (session_id, endpoint, key_env) VALUES (?1, ?2, ?3)",
            params![session_id, p.endpoint.trim(), p.key_env],
        )?;
        Ok(())
    }

    pub fn session_proxy(&self, session_id: &str) -> anyhow::Result<Option<ProxySpec>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT endpoint, key_env FROM session_proxy WHERE session_id = ?1",
                params![session_id],
                |r| Ok(ProxySpec { endpoint: r.get(0)?, key_env: r.get(1)? }),
            )
            .optional()?)
    }
}

/// Add the start hook that applies a session's proxy path to its agent process.
pub fn install(sessions: &std::sync::Arc<Sessions>) {
    let weak = std::sync::Arc::downgrade(sessions);
    sessions.add_start_hook(std::sync::Arc::new(move |rec: &SessionRecord| {
        let Some(s) = weak.upgrade() else { return Vec::new() };
        let Ok(Some(p)) = s.store().session_proxy(&rec.id) else { return Vec::new() };
        proxy_env(rec.agent, &p, |k| std::env::var(k).ok()).unwrap_or_default()
    }));
}

#[derive(Debug, thiserror::Error)]
pub enum CreateMemberError {
    #[error(transparent)]
    Member(#[from] MemberError),
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// Create the session of a member from its definition. A fixed account is passed to the
/// router as requested; the pipeline lets the router choose (FR-U3).
pub fn create_member_session(
    sessions: &Sessions,
    def: &MemberDef,
    new: impl FnOnce(AgentKind) -> NewSession,
) -> Result<SessionRecord, CreateMemberError> {
    let r = def.resolve()?;
    let requested = match &r.account {
        AccountChoice::Fixed(id) => Some(id.as_str()),
        AccountChoice::Pipeline => None,
    };
    let rec = sessions.create_with_account(new(r.agent), requested)?;
    if let Some(p) = &r.proxy {
        if let Err(e) = sessions.store().set_session_proxy(&rec.id, p) {
            return Err(SessionError::Other(e).into());
        }
    }
    Ok(rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(account: Option<&str>, pipeline: bool) -> MemberDef {
        MemberDef {
            harness: "claude code".into(),
            account: account.map(String::from),
            pipeline,
            proxy: None,
        }
    }

    #[test]
    fn fr_a9_neither_or_both_account_choices_are_rejected() {
        assert!(def(None, false).resolve().is_err());
        assert!(def(Some("a1"), true).resolve().is_err());
        assert_eq!(def(Some("a1"), false).resolve().unwrap().account, AccountChoice::Fixed("a1".into()));
        assert_eq!(def(None, true).resolve().unwrap().account, AccountChoice::Pipeline);
    }

    #[test]
    fn fr_a9_harness_labels_map_to_agents() {
        assert_eq!(parse_harness("Claude Code"), Some(AgentKind::ClaudeCode));
        assert_eq!(parse_harness("agy"), Some(AgentKind::Antigravity));
        assert_eq!(parse_harness("codex"), Some(AgentKind::Codex));
        assert_eq!(parse_harness("nope"), None);
    }

    #[test]
    fn fr_u6_claude_code_proxy_sets_anthropic_base_url() {
        let p = ProxySpec { endpoint: "http://127.0.0.1:9000".into(), key_env: Some("K".into()) };
        let env = proxy_env(AgentKind::ClaudeCode, &p, |k| (k == "K").then(|| "sekret".into())).unwrap();
        assert!(env.contains(&("ANTHROPIC_BASE_URL".into(), "http://127.0.0.1:9000".into())));
        assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".into(), "sekret".into())));
    }

    #[test]
    fn fr_u6_unsupported_harness_with_proxy_errors() {
        let p = ProxySpec { endpoint: "http://x".into(), key_env: None };
        let e = proxy_env(AgentKind::Codex, &p, |_| None).unwrap_err();
        assert!(matches!(e, MemberError::Unsupported(_)));
        let mut d = def(None, true);
        d.harness = "codex".into();
        d.proxy = Some(p);
        assert!(d.resolve().is_err());
    }
}
