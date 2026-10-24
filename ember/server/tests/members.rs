//! Team member definition (FR-A9, FR-U1, FR-U6): harness + fixed account or switching pipeline,
//! optional model proxy path. Uses a recording fake in place of the real Claude Code CLI.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ember_server::accounts::secrets::SecretBox;
use ember_server::accounts::Accounts;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use ember_server::events::{AgentEvent, SessionStatus};
use ember_server::members::{create_member_session, MemberDef, ProxySpec};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;
use tokio::sync::mpsc;

type EnvLog = Arc<Mutex<Vec<Vec<(String, String)>>>>;

/// The scripted agent posing as Claude Code, recording each start's environment.
#[derive(Default)]
struct FakeClaude {
    starts: EnvLog,
}

#[async_trait]
impl AgentAdapter for FakeClaude {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }
    async fn detect(&self) -> Detected {
        ScriptedAdapter.detect().await
    }
    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        self.starts.lock().unwrap().push(req.env.clone());
        ScriptedAdapter.start(req, events).await
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    sessions: Arc<Sessions>,
    accounts: Arc<Accounts>,
    starts: EnvLog,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let fake = FakeClaude::default();
    let starts = fake.starts.clone();
    let sessions = Sessions::new(store.clone(), vec![Arc::new(fake) as Arc<dyn AgentAdapter>]);
    let accounts =
        Accounts::with_secrets(store, &dir.path().join("accounts"), SecretBox::ephemeral()).unwrap();
    accounts.install(&sessions);
    ember_server::members::install(&sessions);
    Fx { _dir: dir, sessions, accounts, starts }
}

fn new(agent: AgentKind) -> NewSession {
    NewSession {
        project: "acme".into(),
        agent,
        cwd: std::env::temp_dir(),
        model: None,
        title: "m".into(),
    }
}

fn def(account: Option<&str>, pipeline: bool, proxy: Option<ProxySpec>) -> MemberDef {
    MemberDef { harness: "claude code".into(), account: account.map(String::from), pipeline, proxy }
}

async fn start(f: &Fx, id: &str) {
    f.sessions.send(id, "go").await.unwrap();
    for _ in 0..200 {
        if f.sessions.store().session(id).unwrap().unwrap().status == SessionStatus::WaitingForApproval {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never started");
}

#[tokio::test]
async fn fr_a9_member_with_a_fixed_account_uses_that_account() {
    let f = fx();
    let a = f.accounts.create(AgentKind::ClaudeCode, "a").unwrap();
    let b = f.accounts.create(AgentKind::ClaudeCode, "b").unwrap();
    f.accounts.set_default(&a.id).ok();
    let rec = create_member_session(&f.sessions, &def(Some(&b.id), false, None), new).unwrap();
    assert_eq!(rec.account_id.as_deref(), Some(b.id.as_str()));
    assert!(rec.account_reason.unwrap().starts_with("chosen by the user"));
    start(&f, &rec.id).await;
    let env = f.starts.lock().unwrap()[0].clone();
    assert!(env.iter().any(|(_, v)| *v == b.config_dir), "{env:?}");
}

#[tokio::test]
async fn fr_a9_member_with_the_pipeline_lets_the_router_choose() {
    let f = fx();
    let a = f.accounts.create(AgentKind::ClaudeCode, "a").unwrap();
    let rec = create_member_session(&f.sessions, &def(None, true, None), new).unwrap();
    assert_eq!(rec.account_id.as_deref(), Some(a.id.as_str()));
    assert!(!rec.account_reason.unwrap().starts_with("chosen by the user"));
}

#[tokio::test]
async fn fr_a9_neither_or_both_account_choices_are_rejected() {
    let f = fx();
    let a = f.accounts.create(AgentKind::ClaudeCode, "a").unwrap();
    assert!(create_member_session(&f.sessions, &def(None, false, None), new).is_err());
    assert!(create_member_session(&f.sessions, &def(Some(&a.id), true, None), new).is_err());
}

#[tokio::test]
async fn fr_u6_claude_code_proxy_sets_anthropic_base_url_in_the_spawn_env() {
    let f = fx();
    f.accounts.create(AgentKind::ClaudeCode, "a").unwrap();
    let proxy = ProxySpec { endpoint: "http://127.0.0.1:7777".into(), key_env: None };
    let rec = create_member_session(&f.sessions, &def(None, true, Some(proxy)), new).unwrap();
    start(&f, &rec.id).await;
    let env = f.starts.lock().unwrap()[0].clone();
    assert!(env.contains(&("ANTHROPIC_BASE_URL".into(), "http://127.0.0.1:7777".into())), "{env:?}");
    // The proxy key is never stored with the session.
    let stored = f.sessions.store().session_proxy(&rec.id).unwrap().unwrap();
    assert_eq!(stored.key_env, None);
}

#[tokio::test]
async fn fr_u6_a_member_without_a_proxy_gets_no_proxy_env() {
    let f = fx();
    f.accounts.create(AgentKind::ClaudeCode, "a").unwrap();
    let rec = create_member_session(&f.sessions, &def(None, true, None), new).unwrap();
    start(&f, &rec.id).await;
    let env = f.starts.lock().unwrap()[0].clone();
    assert!(!env.iter().any(|(k, _)| k == "ANTHROPIC_BASE_URL"));
}

#[tokio::test]
async fn fr_u6_unsupported_harness_with_a_proxy_is_an_error() {
    let f = fx();
    let proxy = ProxySpec { endpoint: "http://127.0.0.1:7777".into(), key_env: None };
    let mut d = def(None, true, Some(proxy));
    d.harness = "codex".into();
    let e = create_member_session(&f.sessions, &d, new).unwrap_err().to_string();
    assert!(e.contains("claude-code only"), "{e}");
}
