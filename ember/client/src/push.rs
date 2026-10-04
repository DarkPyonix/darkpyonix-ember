//! Push channel decoding and reconnect backoff (PR-1).
//!
//! The push protocol has no version. Unknown fields are ignored, and a message of an unknown
//! `type` is skipped by the caller ([`DecodeError::UnknownType`]).

use std::time::Duration;

use serde::Deserialize;

use crate::wire::{Project, Push, SessionRecord, SessionStatus, StoredEvent, TeamView};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum DecodeError {
    #[error("unknown push type {0:?}")]
    UnknownType(String),
    /// A push type this client knows and deliberately does not use yet (see [`SKIPPED_TYPES`]).
    #[error("push type {0:?} is not used by this client")]
    Skipped(String),
    #[error("malformed push message: {0}")]
    Malformed(String),
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct SessionCreatedBody {
    session: SessionRecord,
}

#[derive(Deserialize)]
struct EventBody {
    status: SessionStatus,
    event: StoredEvent,
}

#[derive(Deserialize)]
struct ProjectBody {
    project: Project,
}

#[derive(Deserialize)]
struct TeamBody {
    team: TeamView,
}

#[derive(Deserialize)]
struct LaggedBody {
    #[serde(default)]
    missed: u64,
}

/// Push types the server sends that this client knows about and ignores for now: the
/// schedule pushes (FR-A8) have no client UI yet. Skipped quietly, unlike an unknown type.
pub const SKIPPED_TYPES: &[&str] =
    &["schedule_created", "schedule_updated", "schedule_deleted", "schedule_run"];

/// Decode one push text frame. 
pub fn decode(text: &str) -> Result<Push, DecodeError> {
    let malformed = |e: serde_json::Error| DecodeError::Malformed(e.to_string());
    let value: serde_json::Value = serde_json::from_str(text).map_err(malformed)?;
    let env: Envelope = serde_json::from_value(value.clone()).map_err(malformed)?;
    match env.kind.as_str() {
        "session_created" => {
            let b: SessionCreatedBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::SessionCreated { session: b.session })
        }
        "event" => {
            let b: EventBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::Event { status: b.status, event: b.event })
        }
        "session_updated" => {
            let b: SessionCreatedBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::SessionUpdated { session: b.session })
        }
        "project_updated" => {
            let b: ProjectBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ProjectUpdated { project: b.project })
        }
        "team_updated" => {
            let b: TeamBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::TeamUpdated { team: b.team })
        }
        "lagged" => {
            let b: LaggedBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::Lagged { missed: b.missed })
        }
        other if SKIPPED_TYPES.contains(&other) => Err(DecodeError::Skipped(other.to_string())),
        other => Err(DecodeError::UnknownType(other.to_string())),
    }
}

/// Exponential reconnect backoff with a cap and a little deterministic jitter.
#[derive(Debug, Clone)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(min: Duration, max: Duration) -> Backoff {
        Backoff { min, max: max.max(min), attempt: 0 }
    }

    /// Delay before the next attempt; grows ×2 per failure up to `max`, ±12.5 % jitter.
    pub fn next_delay(&mut self) -> Duration {
        let base = self.min.saturating_mul(1u32 << self.attempt.min(16)).min(self.max);
        self.attempt = self.attempt.saturating_add(1);
        // Spread reconnecting clients without pulling in an RNG.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let spread = base / 4;
        let offset = spread.mul_f64(f64::from(nanos % 1000) / 1000.0);
        (base - spread / 2 + offset).min(self.max)
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::AgentEvent;

    #[test]
    fn decodes_event_and_lagged() {
        let p = decode(
            r#"{"type":"event","status":"running",
                "event":{"session_id":"s","seq":2,"at":5,"event":{"kind":"user_message","text":"hi"}}}"#,
        )
        .unwrap();
        assert_eq!(
            p,
            Push::Event {
                status: SessionStatus::Running,
                event: StoredEvent {
                    session_id: "s".into(),
                    seq: 2,
                    at: 5,
                    event: AgentEvent::UserMessage { text: "hi".into() },
                },
            }
        );
        assert_eq!(decode(r#"{"type":"lagged","missed":3}"#).unwrap(), Push::Lagged { missed: 3 });
    }

    #[test]
    fn decodes_session_and_project_updates() {
        let p = decode(
            r#"{"type":"session_updated","session":{"id":"s","project":"p","agent":"codex","cwd":"/w",
                "status":"finished","title":"Renamed","created_at":1,"updated_at":2,"last_seq":3,
                "account_id":"a1","account_reason":"default","pinned":true,"archived":false}}"#,
        )
        .unwrap();
        let Push::SessionUpdated { session } = p else { panic!("{p:?}") };
        assert_eq!((session.title.as_str(), session.pinned, session.account_id.as_deref()), ("Renamed", true, Some("a1")));
        assert_eq!(
            decode(r#"{"type":"project_updated","project":{"name":"p","created_at":1,"computers":["local"]}}"#).unwrap(),
            Push::ProjectUpdated {
                project: Project { name: "p".into(), created_at: 1, computers: vec!["local".into()] }
            }
        );
        // An older server's record (no metadata fields) still decodes.
        let p = decode(
            r#"{"type":"session_created","session":{"id":"s","project":"p","agent":"codex","cwd":"/w",
                "status":"idle","title":"t","created_at":1,"updated_at":1,"last_seq":0}}"#,
        )
        .unwrap();
        assert!(matches!(p, Push::SessionCreated { session } if !session.pinned && session.account_id.is_none()));
        // A type this client does not know is reported, and the push loop skips it.
        assert_eq!(decode(r#"{"type":"future_thing"}"#), Err(DecodeError::UnknownType("future_thing".into())));
        // The schedule pushes (FR-A8) are known and skipped on purpose.
        assert_eq!(
            decode(r#"{"type":"schedule_run","run":{"id":"r"}}"#),
            Err(DecodeError::Skipped("schedule_run".into()))
        );
    }

    #[test]
    fn decodes_team_updates_and_notices() {
        let p = decode(
            r#"{"type":"team_updated","team":{"id":"team_1","project":"acme","leader":"s1","created_at":1,
                "members":[{"session_id":"s1","name":"lead","role":"leader","joined_at":1,"ended_at":null,
                            "title":"Lead","agent":"codex","status":"idle"},
                           {"session_id":"s2","name":"alice","role":"teammate","joined_at":2,"ended_at":null,
                            "title":"alice","agent":"codex","status":"waiting_for_approval","future":1}],
                "tasks":[{"id":"task_1","team_id":"team_1","number":1,"title":"tests","detail":"","status":"in_progress",
                          "assignee":"s2","assignee_name":"alice","created_by":"s1","created_at":3,"updated_at":4}]}}"#,
        )
        .unwrap();
        let Push::TeamUpdated { team } = p else { panic!("{p:?}") };
        assert_eq!(team.members[1].status, Some(SessionStatus::WaitingForApproval));
        assert_eq!(team.tasks[0].status, crate::wire::TaskStatus::InProgress);
        assert_eq!(team.member("s2").map(|m| m.name.as_str()), Some("alice"));
        // A future task status still decodes.
        let p = decode(
            r#"{"type":"team_updated","team":{"id":"t","leader":"s","tasks":[{"id":"x","number":1,"title":"y","status":"review"}]}}"#,
        )
        .unwrap();
        assert!(matches!(p, Push::TeamUpdated { team } if team.tasks[0].status == crate::wire::TaskStatus::Unknown));
        let p = decode(
            r#"{"type":"event","status":"running",
                "event":{"session_id":"s","seq":3,"at":0,"event":{"kind":"notice","message":"Mention not delivered"}}}"#,
        )
        .unwrap();
        assert!(matches!(p, Push::Event { event: StoredEvent { event: AgentEvent::Notice { .. }, .. }, .. }));
    }

    #[test]
    fn unknown_fields_are_ignored_and_a_missing_body_is_malformed() {
        assert_eq!(
            decode(r#"{"type":"lagged","v":2,"whatever":true,"missed":4}"#),
            Ok(Push::Lagged { missed: 4 })
        );
        assert!(matches!(decode(r#"{"type":"event"}"#), Err(DecodeError::Malformed(_))));
    }

    #[test]
    fn unknown_event_kind_still_decodes() {
        let p = decode(
            r#"{"type":"event","status":"running",
                "event":{"session_id":"s","seq":1,"at":0,"event":{"kind":"file_changed","path":"x"}}}"#,
        )
        .unwrap();
        assert!(matches!(p, Push::Event { event: StoredEvent { event: AgentEvent::Unknown, .. }, .. }));
    }

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::new(Duration::from_millis(100), Duration::from_secs(1));
        let d: Vec<_> = (0..8).map(|_| b.next_delay()).collect();
        assert!(d[0] <= Duration::from_millis(125));
        assert!(d[3] >= Duration::from_millis(700));
        assert!(d.iter().all(|x| *x <= Duration::from_secs(1)));
        b.reset();
        assert!(b.next_delay() <= Duration::from_millis(125));
    }
}
