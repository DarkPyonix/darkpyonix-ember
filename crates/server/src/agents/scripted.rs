//! A fake agent for tests: deterministic, no process, no network.
//!
//! On each message it emits a native-session event (first turn only), one tool call that needs
//! approval, and (once approved) a tool result, an assistant reply echoing the message, and
//! `TurnEnded`. A message containing "slow" waits until interrupted.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex, Notify};

use super::{AgentAdapter, AgentKind, AgentRun, Detected, StartRequest};
use crate::events::{AgentEvent, ApprovalDecision, TurnOutcome};

#[derive(Default)]
pub struct ScriptedAdapter;

#[async_trait]
impl AgentAdapter for ScriptedAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Scripted
    }

    async fn detect(&self) -> Detected {
        Detected { kind: AgentKind::Scripted, installed: true, version: Some("test".into()) }
    }

    async fn start(
        &self,
        req: StartRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Box<dyn AgentRun>> {
        let native_id = req.resume_native_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        events.send(AgentEvent::NativeSession { native_id }).await?;
        Ok(Box::new(ScriptedRun {
            events,
            pending: Arc::new(Mutex::new(None)),
            interrupt: Arc::new(Notify::new()),
            turn: 0,
        }))
    }
}

struct ScriptedRun {
    events: mpsc::Sender<AgentEvent>,
    /// The message waiting on approval, if any.
    pending: Arc<Mutex<Option<(String, String)>>>,
    interrupt: Arc<Notify>,
    turn: u32,
}

#[async_trait]
impl AgentRun for ScriptedRun {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        self.turn += 1;
        if text.contains("slow") {
            let events = self.events.clone();
            let interrupt = self.interrupt.clone();
            tokio::spawn(async move {
                interrupt.notified().await;
                let _ = events
                    .send(AgentEvent::TurnEnded { outcome: TurnOutcome::Interrupted })
                    .await;
            });
            return Ok(());
        }
        let approval_id = format!("approval-{}", self.turn);
        *self.pending.lock().await = Some((approval_id.clone(), text.to_string()));
        self.events
            .send(AgentEvent::ToolCall {
                call_id: format!("call-{}", self.turn),
                name: "Bash".into(),
                input: serde_json::json!({ "command": "echo hi" }),
            })
            .await?;
        self.events
            .send(AgentEvent::ApprovalRequested {
                approval_id,
                tool: "Bash".into(),
                input: serde_json::json!({ "command": "echo hi" }),
            })
            .await?;
        Ok(())
    }

    async fn answer(
        &mut self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> anyhow::Result<()> {
        let Some((id, text)) = self.pending.lock().await.take() else {
            anyhow::bail!("no pending approval");
        };
        anyhow::ensure!(id == approval_id, "unknown approval {approval_id}");
        let call_id = format!("call-{}", self.turn);
        let (output, is_error) = match decision {
            ApprovalDecision::Deny => ("denied".to_string(), true),
            _ => ("hi".to_string(), false),
        };
        self.events.send(AgentEvent::ToolResult { call_id, output, is_error }).await?;
        self.events.send(AgentEvent::AssistantMessage { text: format!("echo: {text}") }).await?;
        self.events.send(AgentEvent::Usage { input_tokens: 10, output_tokens: 5 }).await?;
        self.events.send(AgentEvent::TurnEnded { outcome: TurnOutcome::Completed }).await?;
        Ok(())
    }

    async fn interrupt(&mut self) -> anyhow::Result<()> {
        self.interrupt.notify_one();
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
