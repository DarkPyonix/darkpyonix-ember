//! Push channel decoding and reconnect backoff (PR-1).
//!
//! The push protocol has no version. Unknown fields are ignored, and a message of an unknown
//! `type` is skipped by the caller ([`DecodeError::UnknownType`]).

use std::time::Duration;

use serde::Deserialize;

use crate::wire::{Project, Push, Schedule, ScheduleRun, SessionRecord, SessionStatus, StoredEvent, TeamView};

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
struct ApproveAllBody {
    session_id: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct ChannelMessageBody {
    channel_id: String,
    message: crate::channels::ChannelMessage,
}

#[derive(Deserialize)]
struct ProjectDeletedBody {
    name: String,
}

#[derive(Deserialize)]
struct ScheduleBody {
    schedule: Schedule,
}

#[derive(Deserialize)]
struct ScheduleDeletedBody {
    id: String,
    #[serde(default)]
    project: String,
}

#[derive(Deserialize)]
struct ScheduleRunBody {
    run: ScheduleRun,
}

#[derive(Deserialize)]
struct ComputerStatusBody {
    computer_id: String,
    #[serde(default)]
    reachable: Option<bool>,
}

#[derive(Deserialize)]
struct TransferRef {
    id: String,
}

#[derive(Deserialize)]
struct TransferUpdatedBody {
    transfer: TransferRef,
}

#[derive(Deserialize)]
struct LaggedBody {
    #[serde(default)]
    missed: u64,
}

/// Push types the server sends that this client knows about and ignores for now. Skipped
/// quietly, unlike an unknown type. Empty since the schedule pushes are decoded (PR-1, #1049).
pub const SKIPPED_TYPES: &[&str] = &[];

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
        "project_deleted" => {
            let b: ProjectDeletedBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ProjectDeleted { name: b.name })
        }
        "team_updated" => {
            let b: TeamBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::TeamUpdated { team: b.team })
        }
        "approve_all" => {
            let b: ApproveAllBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ApproveAll { session_id: b.session_id, enabled: b.enabled })
        }
        "channel_message" => {
            let b: ChannelMessageBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ChannelMessage { channel_id: b.channel_id, message: b.message })
        }
        "schedule_created" => {
            let b: ScheduleBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ScheduleCreated { schedule: b.schedule })
        }
        "schedule_updated" => {
            let b: ScheduleBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ScheduleUpdated { schedule: b.schedule })
        }
        "schedule_deleted" => {
            let b: ScheduleDeletedBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ScheduleDeleted { id: b.id, project: b.project })
        }
        "schedule_run" => {
            let b: ScheduleRunBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ScheduleRun { run: b.run })
        }
        "computer_status" => {
            let b: ComputerStatusBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::ComputerStatus { computer_id: b.computer_id, reachable: b.reachable })
        }
        "transfer_updated" => {
            let b: TransferUpdatedBody = serde_json::from_value(value).map_err(malformed)?;
            Ok(Push::TransferUpdated { id: b.transfer.id })
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
                project: Project { name: "p".into(), created_at: 1, computers: vec!["local".into()], ..Default::default() }
            }
        );
        assert_eq!(
            decode(r#"{"type":"project_deleted","name":"p"}"#).unwrap(),
            Push::ProjectDeleted { name: "p".into() },
            "FR-L26: a delete carries only the name"
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
        // A schedule push without its body is malformed, not skipped (PR-1, #1049).
        assert!(matches!(decode(r#"{"type":"schedule_run","run":{"id":"r"}}"#), Err(DecodeError::Malformed(_))));
    }

    /// The reachability and transfer pushes (FR-L3, FR-L27.8) decode to the computer's id and
    /// reachability and the transfer's id; anything else in them is ignored (PR-1, #1049).
    #[test]
    fn pr_1_client_decodes_reachability_and_transfer_pushes() {
        assert_eq!(
            decode(r#"{"type":"computer_status","computer_id":"studio","reachable":false,"error":"timed out","name":"x"}"#),
            Ok(Push::ComputerStatus { computer_id: "studio".into(), reachable: Some(false) })
        );
        assert_eq!(
            decode(r#"{"type":"computer_status","computer_id":"studio"}"#),
            Ok(Push::ComputerStatus { computer_id: "studio".into(), reachable: None })
        );
        assert_eq!(
            decode(r#"{"type":"transfer_updated","transfer":{"id":"t1","state":"receiving","received":5,"size":9}}"#),
            Ok(Push::TransferUpdated { id: "t1".into() })
        );
        assert!(matches!(decode(r#"{"type":"transfer_updated","id":"t1"}"#), Err(DecodeError::Malformed(_))));
        assert!(matches!(decode(r#"{"type":"computer_status","reachable":true}"#), Err(DecodeError::Malformed(_))));
    }

    /// The schedule pushes (FR-A8) are decoded, not skipped (PR-1, #1049).
    #[test]
    fn pr_1_client_decodes_schedule_pushes() {
        let schedule = r#"{"id":"sch1","project":"acme","agent":"codex","prompt":"p","kind":{"type":"cron","expr":"0 9 * * *","tz":"UTC"},
            "target":{"type":"new"},"created_at":1,"updated_at":1}"#;
        for kind in ["schedule_created", "schedule_updated"] {
            let p = decode(&format!(r#"{{"type":"{kind}","schedule":{schedule}}}"#)).unwrap_or_else(|e| panic!("{kind}: {e}"));
            match p {
                Push::ScheduleCreated { schedule } | Push::ScheduleUpdated { schedule } => assert_eq!(schedule.id, "sch1"),
                other => panic!("{kind}: {other:?}"),
            }
        }
        assert_eq!(
            decode(r#"{"type":"schedule_deleted","id":"sch1","project":"acme"}"#),
            Ok(Push::ScheduleDeleted { id: "sch1".into(), project: "acme".into() })
        );
        let p = decode(
            r#"{"type":"schedule_run","run":{"id":"r1","schedule_id":"sch1","scheduled_at":5,"started_at":5,"status":"running"}}"#,
        );
        assert!(matches!(p, Ok(Push::ScheduleRun { ref run }) if run.id == "r1" && run.schedule_id == "sch1"), "{p:?}");
        assert!(SKIPPED_TYPES.is_empty());
    }

    #[test]
    fn fr_t10_client_decodes_channel_message_push() {
        let p = decode(
            r#"{"type":"channel_message","channel_id":"c1","message":{"id":"2","seq":2,"from":"s1","text":"hi","at":5,"unread":1}}"#,
        )
        .unwrap();
        assert!(matches!(p, Push::ChannelMessage { channel_id, message } if channel_id == "c1" && message.seq == 2));
        assert!(matches!(decode(r#"{"type":"channel_message","channel_id":"c"}"#), Err(DecodeError::Malformed(_))));
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
    fn fr_a5_decodes_approve_all() {
        assert_eq!(
            decode(r#"{"type":"approve_all","session_id":"a","enabled":true}"#),
            Ok(Push::ApproveAll { session_id: "a".into(), enabled: true })
        );
        assert!(matches!(decode(r#"{"type":"approve_all"}"#), Err(DecodeError::Malformed(_))));
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
