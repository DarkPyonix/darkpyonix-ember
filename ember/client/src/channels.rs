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
}
