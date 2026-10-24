//! FR-S5 (#78): a fork uses the agent's own fork when it has one, and copies the conversation
//! otherwise. Scripted agents: one declaring native fork, one without.

use std::sync::Arc;
use std::time::Duration;

use ember_server::agents::scripted::{ScriptedAdapter, ScriptedForkAdapter};
use ember_server::agents::{AgentAdapter, AgentKind, NativeFork};
use ember_server::events::{AgentEvent, ApprovalDecision, SessionStatus};
use ember_server::session::{NewSession, Sessions};
use ember_server::store::Store;

fn sessions(adapter: Arc<dyn AgentAdapter>) -> Arc<Sessions> {
    Sessions::new(Arc::new(Store::open_in_memory().unwrap()), vec![adapter])
}

async fn wait_finished(s: &Sessions, id: &str) {
    for _ in 0..400 {
        if s.store().session(id).unwrap().unwrap().status == SessionStatus::Finished
            && s.store().events_after(id, 0).unwrap().iter().any(|e| matches!(e.event, AgentEvent::TurnEnded { .. }))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("session {id} never finished");
}

/// A session with two completed turns; returns its id and the `turn_ended` sequence numbers.
async fn two_turns(s: &Arc<Sessions>) -> (String, Vec<i64>) {
    let id = s
        .create(NewSession {
            project: "acme".into(),
            agent: AgentKind::Scripted,
            cwd: std::env::temp_dir(),
            model: None,
            title: "t".into(),
        })
        .unwrap()
        .id;
    for (i, text) in ["first question", "second question"].iter().enumerate() {
        s.send(&id, text).await.unwrap();
        for _ in 0..400 {
            if s.store().session(&id).unwrap().unwrap().status == SessionStatus::WaitingForApproval {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        s.answer(&id, &format!("approval-{}", i + 1), ApprovalDecision::AllowOnce).await.unwrap();
        wait_finished(s, &id).await;
        while s.store().events_after(&id, 0).unwrap().iter().filter(|e| matches!(e.event, AgentEvent::TurnEnded { .. })).count() < i + 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let events = s.store().events_after(&id, 0).unwrap();
    (id.clone(), ember_server::fork::completed_turns(&events))
}

/// The text the fork's agent was given for a first message: the notices recorded before it.
fn notices(s: &Sessions, id: &str) -> Vec<String> {
    s.store()
        .events_after(id, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AgentEvent::SystemNotice { text } => Some(text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_agent_with_native_fork_gets_its_native_id_forked_and_no_seed() {
    let s = sessions(Arc::new(ScriptedForkAdapter { mode: NativeFork::Latest, fail_fork: false }));
    let (parent, turns) = two_turns(&s).await;
    let parent_native = s.store().session(&parent).unwrap().unwrap().native_id.unwrap();

    let fork = s.fork(&parent, Some(*turns.last().unwrap()), None).unwrap();
    s.send(&fork.id, "and now?").await.unwrap();

    for _ in 0..400 {
        if s.store().session(&fork.id).unwrap().unwrap().native_id.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let rec2 = s.store().session(&fork.id).unwrap().unwrap();
    assert_eq!(rec2.native_id.as_deref(), Some(format!("fork-of-{parent_native}").as_str()));
    assert!(notices(&s, &fork.id).is_empty(), "a native fork needs no copied conversation");
    assert_eq!(s.store().pending_native_fork(&fork.id).unwrap(), None, "settled once the agent forked");
}

#[tokio::test]
async fn a_latest_only_agent_copies_the_conversation_for_an_earlier_turn() {
    let s = sessions(Arc::new(ScriptedForkAdapter { mode: NativeFork::Latest, fail_fork: false }));
    let (parent, turns) = two_turns(&s).await;
    let fork = s.fork(&parent, Some(turns[0]), None).unwrap();
    s.send(&fork.id, "and now?").await.unwrap();
    let seed = notices(&s, &fork.id);
    assert_eq!(seed.len(), 1);
    assert!(seed[0].contains("first question") && !seed[0].contains("second question"), "{seed:?}");
}

#[tokio::test]
async fn an_at_turn_agent_forks_natively_through_the_chosen_turn() {
    let s = sessions(Arc::new(ScriptedForkAdapter { mode: NativeFork::AtTurn, fail_fork: false }));
    let (parent, turns) = two_turns(&s).await;
    let parent_native = s.store().session(&parent).unwrap().unwrap().native_id.unwrap();
    let fork = s.fork(&parent, Some(turns[0]), None).unwrap();
    s.send(&fork.id, "and now?").await.unwrap();
    for _ in 0..400 {
        if s.store().session(&fork.id).unwrap().unwrap().native_id.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let native = s.store().session(&fork.id).unwrap().unwrap().native_id;
    assert_eq!(native.as_deref(), Some(format!("fork-of-{parent_native}@1").as_str()));
    assert!(notices(&s, &fork.id).is_empty());
}

#[tokio::test]
async fn an_agent_without_native_fork_gets_the_seed() {
    let s = sessions(Arc::new(ScriptedAdapter));
    let (parent, turns) = two_turns(&s).await;
    let fork = s.fork(&parent, Some(*turns.last().unwrap()), None).unwrap();
    assert_eq!(s.store().pending_native_fork(&fork.id).unwrap(), None);
    s.send(&fork.id, "and now?").await.unwrap();
    let seed = notices(&s, &fork.id);
    assert_eq!(seed.len(), 1);
    assert!(seed[0].contains("first question") && seed[0].contains("second question"));
}

#[tokio::test]
async fn a_native_fork_that_fails_falls_back_to_the_seed_with_a_notice() {
    let s = sessions(Arc::new(ScriptedForkAdapter { mode: NativeFork::AtTurn, fail_fork: true }));
    let (parent, turns) = two_turns(&s).await;
    let fork = s.fork(&parent, Some(*turns.last().unwrap()), None).unwrap();
    s.send(&fork.id, "and now?").await.unwrap();
    let seed = notices(&s, &fork.id);
    assert_eq!(seed.len(), 1, "the copy takes over: {seed:?}");
    assert!(seed[0].contains("first question"));
    let notes: Vec<_> = s
        .store()
        .events_after(&fork.id, 0)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(&e.event, AgentEvent::Notice { message } if message.contains("could not fork")))
        .collect();
    assert_eq!(notes.len(), 1);
}

#[test]
fn plan_native_fork_follows_the_agent_capability() {
    use ember_server::fork::plan_native_fork as plan;
    assert_eq!(plan(NativeFork::None, Some("n"), 2, 2, false), None);
    assert_eq!(plan(NativeFork::Latest, None, 2, 2, false), None, "no native id");
    assert_eq!(plan(NativeFork::Latest, Some("n"), 1, 2, false), None, "earlier turn");
    assert_eq!(plan(NativeFork::Latest, Some("n"), 2, 2, true), None, "later user message");
    assert_eq!(plan(NativeFork::Latest, Some("n"), 2, 2, false).unwrap().keep_turns, None);
    assert_eq!(plan(NativeFork::AtTurn, Some("n"), 1, 2, true).unwrap().keep_turns, Some(1));
}
