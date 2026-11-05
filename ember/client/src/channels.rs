//! Wire types of the channels API (SPEC FR-T10, FR-T16), copied by shape from the server's
//! `channels` module (`ember/server/src/channels/`). The client does not depend on the server crate.

use serde::{Deserialize, Serialize};

/// The `from` of a message written by the person at this client.
pub const USER: &str = "user";

/// A person or an agent session in a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// `user` or an agent session id.
    pub id: String,
    pub name: String,
    pub is_agent: bool,
    /// The agent harness (`claude-code`, `codex`, ...); empty for a person.
    #[serde(default)]
    pub harness: String,
}

/// One log entry. `at` is Unix seconds; `unread` is 1 when the message is after the `user` read
/// cursor and was not written by `user`, else 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// The log sequence number as text.
    pub id: String,
    pub seq: i64,
    pub from: String,
    pub text: String,
    pub at: i64,
    #[serde(default)]
    pub unread: u32,
}

/// A channel as the list and detail routes return it. `messages` is empty in a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub project: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub last_message: Option<ChannelMessage>,
    #[serde(default)]
    pub messages: Vec<ChannelMessage>,
    #[serde(default)]
    pub unread: u32,
}

/// Body of `POST /api/channels`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewChannel {
    pub project: String,
    pub name: String,
    /// Session ids of the project to include. None other becomes a member.
    #[serde(default)]
    pub agents: Vec<String>,
}

/// Query of `GET /api/channels/{id}/messages`: messages with `seq > after`, oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MessagesQuery {
    pub after: Option<i64>,
}

/// Body of `POST /api/channels/{id}/messages`. `from` defaults to `user` on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewChannelMessage {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Body of `POST /api/channels/{id}/read`; `seq` defaults to the latest message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReadBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
}

/// The member agent a feed item is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedAgent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub harness: String,
}

/// One feed entry (FR-T11). `kind` is `approval` or `report` and stays a string so a new kind
/// does not break decoding. Fields not meant for the kind are absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedItem {
    /// `approval:<approval_id>` or `report:<session id>:<seq>`.
    pub id: String,
    pub kind: String,
    pub agent: FeedAgent,
    pub session_id: String,
    /// The session's route, for the client to open.
    pub link: String,
    /// Unix seconds.
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// `completed`, `failed` or `interrupted` (reports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The answer to a pending approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedDecision {
    AllowOnce,
    AllowAlways,
    Deny,
}

/// Body of `POST /api/channels/{id}/feed/{approval_id}/answer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedAnswer {
    pub decision: FeedDecision,
}

/// Path of `GET /channels/{id}/feed`.
pub(crate) fn feed_path(id: &str) -> String {
    format!("/channels/{}/feed", crate::api::seg(id))
}

/// Path of `POST /channels/{id}/feed/{approval_id}/answer`.
pub(crate) fn feed_answer_path(id: &str, approval_id: &str) -> String {
    format!("/channels/{}/feed/{}/answer", crate::api::seg(id), crate::api::seg(approval_id))
}

/// Path of `GET /channels`, with `?project=` when given.
pub(crate) fn list_path(project: Option<&str>) -> Result<String, serde_urlencoded::ser::Error> {
    match project {
        Some(p) => Ok(format!("/channels?{}", serde_urlencoded::to_string([("project", p)])?)),
        None => Ok("/channels".to_string()),
    }
}

/// Path of `GET /channels/{id}/messages`, with `?after=` when given.
pub(crate) fn messages_path(id: &str, q: &MessagesQuery) -> String {
    let base = format!("/channels/{}/messages", crate::api::seg(id));
    match q.after {
        Some(n) => format!("{base}?after={n}"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fr_t10_client_message_roundtrip() {
        let j = json!({"id":"1","seq":1,"from":"user","text":"hello","at":1700000000,"unread":0});
        let m: ChannelMessage = serde_json::from_value(j.clone()).unwrap();
        assert_eq!((m.seq, m.from.as_str(), m.text.as_str()), (1, "user", "hello"));
        assert_eq!(serde_json::to_value(&m).unwrap(), j);
    }

    #[test]
    fn fr_t10_client_channel_roundtrip() {
        let j = json!({
            "id":"c1","name":"review","project":"core","default":false,
            "members":[
                {"id":"user","name":"You","is_agent":false,"harness":""},
                {"id":"s1","name":"agent","is_agent":true,"harness":"codex"}
            ],
            "last_message":{"id":"2","seq":2,"from":"s1","text":"done","at":5,"unread":1},
            "messages":[{"id":"2","seq":2,"from":"s1","text":"done","at":5,"unread":1}],
            "unread":1
        });
        let c: Channel = serde_json::from_value(j.clone()).unwrap();
        assert_eq!(c.members.len(), 2);
        assert!(c.members[1].is_agent);
        assert_eq!(c.last_message.as_ref().unwrap().seq, 2);
        assert_eq!(serde_json::to_value(&c).unwrap(), j);
        // A list entry has no messages and may have no last message.
        let l: Channel =
            serde_json::from_value(json!({"id":"c","name":"n","project":"p","default":true,"members":[],"last_message":null,"messages":[],"unread":0}))
                .unwrap();
        assert!(l.default && l.last_message.is_none() && l.messages.is_empty());
    }

    #[test]
    fn fr_t10_client_new_channel_matches_server_body() {
        let n = NewChannel { project: "core".into(), name: "review".into(), agents: vec!["a".into()] };
        assert_eq!(serde_json::to_value(&n).unwrap(), json!({"project":"core","name":"review","agents":["a"]}));
        let no_agents: NewChannel = serde_json::from_value(json!({"project":"core","name":"x"})).unwrap();
        assert!(no_agents.agents.is_empty());
    }

    #[test]
    fn fr_t10_client_new_message_omits_default_from() {
        let n = NewChannelMessage { text: "hello".into(), from: None };
        assert_eq!(serde_json::to_value(&n).unwrap(), json!({"text":"hello"}));
        let n = NewChannelMessage { text: "done".into(), from: Some("s1".into()) };
        assert_eq!(serde_json::to_value(&n).unwrap(), json!({"text":"done","from":"s1"}));
    }

    #[test]
    fn fr_t10_client_read_body() {
        assert_eq!(serde_json::to_value(ReadBody { seq: Some(3) }).unwrap(), json!({"seq":3}));
        assert_eq!(serde_json::to_value(ReadBody::default()).unwrap(), json!({}));
    }

    #[test]
    fn fr_t10_client_paths() {
        assert_eq!(list_path(None).unwrap(), "/channels");
        assert_eq!(list_path(Some("core")).unwrap(), "/channels?project=core");
        assert_eq!(list_path(Some("a b&c")).unwrap(), "/channels?project=a+b%26c");
        assert_eq!(messages_path("c1", &MessagesQuery::default()), "/channels/c1/messages");
        assert_eq!(messages_path("c1", &MessagesQuery { after: Some(4) }), "/channels/c1/messages?after=4");
        assert_eq!(messages_path("a/b", &MessagesQuery::default()), "/channels/a%2Fb/messages");
    }

    #[test]
    fn fr_t10_client_feed_item_roundtrip() {
        let approval = json!({
            "id":"approval:approval-1","kind":"approval",
            "agent":{"id":"s1","name":"alpha","harness":"codex"},
            "session_id":"s1","link":"/sessions/s1","at":1700000000,
            "approval_id":"approval-1","tool":"Bash","input":{"command":"ls"}
        });
        let i: FeedItem = serde_json::from_value(approval.clone()).unwrap();
        assert_eq!((i.kind.as_str(), i.approval_id.as_deref(), i.tool.as_deref()), ("approval", Some("approval-1"), Some("Bash")));
        assert_eq!(i.agent.name, "alpha");
        assert_eq!(serde_json::to_value(&i).unwrap(), approval);
        let report = json!({
            "id":"report:s1:4","kind":"report",
            "agent":{"id":"s1","name":"alpha","harness":"codex"},
            "session_id":"s1","link":"/sessions/s1","at":5,
            "outcome":"failed","summary":"partial","error":"boom"
        });
        let r: FeedItem = serde_json::from_value(report.clone()).unwrap();
        assert_eq!((r.outcome.as_deref(), r.error.as_deref()), (Some("failed"), Some("boom")));
        assert!(r.approval_id.is_none() && r.input.is_none());
        assert_eq!(serde_json::to_value(&r).unwrap(), report);
        // Unknown fields and kinds from a newer server do not break decoding.
        let n: FeedItem = serde_json::from_value(json!({
            "id":"q:1","kind":"question","agent":{"id":"s","name":"n","harness":"h","extra":1},
            "session_id":"s","link":"/sessions/s","at":1,"new_field":true
        }))
        .unwrap();
        assert_eq!(n.kind, "question");
    }

    #[test]
    fn fr_t10_client_feed_decision_wire_names() {
        assert_eq!(serde_json::to_value(FeedDecision::AllowOnce).unwrap(), json!("allow_once"));
        assert_eq!(serde_json::to_value(FeedDecision::AllowAlways).unwrap(), json!("allow_always"));
        assert_eq!(serde_json::to_value(FeedDecision::Deny).unwrap(), json!("deny"));
        assert_eq!(
            serde_json::to_value(FeedAnswer { decision: FeedDecision::Deny }).unwrap(),
            json!({"decision":"deny"})
        );
        let a: FeedAnswer = serde_json::from_value(json!({"decision":"allow_always"})).unwrap();
        assert_eq!(a.decision, FeedDecision::AllowAlways);
    }

    #[test]
    fn fr_t10_client_feed_paths() {
        assert_eq!(feed_path("c1"), "/channels/c1/feed");
        assert_eq!(feed_path("a/b"), "/channels/a%2Fb/feed");
        assert_eq!(feed_answer_path("c1", "approval-1"), "/channels/c1/feed/approval-1/answer");
        assert_eq!(feed_answer_path("c/1", "a b"), "/channels/c%2F1/feed/a%20b/answer");
    }
}
