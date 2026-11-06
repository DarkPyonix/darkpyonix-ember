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
    /// Session ids of the agents the message mentions (FR-T12); absent on an older server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    /// Display name of the author, resolved by the server at read time (FR-T15). A guest's `from`
    /// is an opaque `guest:<id>`, so show this instead. Empty on an older server.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author_name: String,
    /// `person`, `agent`, `guest` or `system`; empty on an older server.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author_kind: String,
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
    /// Usable guests of the channel (FR-T17); empty on an older server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guests: Vec<ChannelGuest>,
}

/// A guest present in a channel, as `Channel.guests` carries it (FR-T17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelGuest {
    pub id: String,
    pub name: String,
    /// Unix ms the allowance ends; `None` when none is in force now.
    #[serde(default)]
    pub allow_until: Option<i64>,
}

/// Body of `POST /api/channels/{id}/invites`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NewInvite {
    /// Whole hours 1..=168; the server default is 24.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_hours: Option<u32>,
}

/// Path of the guest door status route (owner route, FR-T14).
pub const GUEST_DOOR_PATH: &str = "/guest-door";

/// The `guest_door` object of a create-invite answer; absent on a server without the guest door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestDoorState {
    /// `on` or `off`.
    pub state: String,
    #[serde(default)]
    pub listen: Option<String>,
    #[serde(default)]
    pub public_url: Option<String>,
    /// True when the door is on and no public URL is set: ask the owner to check that the listen
    /// address is reachable from outside. Only the guest-door route sends it (FR-T14).
    #[serde(default)]
    pub link_warning: bool,
}

impl GuestDoorState {
    pub fn is_on(&self) -> bool {
        self.state == "on"
    }
}

/// Answer of `POST /api/channels/{id}/invites`. `token` and `link` exist only here: the server
/// keeps just a hash. `expires_at` is Unix ms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedInvite {
    pub id: String,
    pub token: String,
    /// Absolute when the guest door is on; the relative `/join#<token>` when it is off.
    pub link: String,
    pub expires_at: i64,
    /// True when the link uses the listen address (no public URL): ask the owner to check reach.
    #[serde(default)]
    pub link_warning: bool,
    #[serde(default)]
    pub guest_door: Option<GuestDoorState>,
}

/// One entry of `GET /api/channels/{id}/invites`: no token and no link. Times are Unix ms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub id: String,
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub used_at: Option<i64>,
    #[serde(default)]
    pub revoked_at: Option<i64>,
    /// The name the guest chose; absent while the invite is unused.
    #[serde(default)]
    pub guest_name: Option<String>,
}

/// One entry of `GET /api/channels/{id}/guests`, revoked and expired ones included. Times are
/// Unix ms; `allow_until` is the stored value and may lie in the past.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub revoked_at: Option<i64>,
    #[serde(default)]
    pub allow_until: Option<i64>,
}

/// Body of `PUT /api/channels/{id}/guests/{guest_id}/allow`: whole hours, 1..=168.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowanceBody {
    pub hours: u32,
}

/// Answer of the set-allowance route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allowance {
    pub guest_id: String,
    /// Unix ms.
    pub allow_until: i64,
    /// True when the guest credential's own expiry cut the allowance short.
    #[serde(default)]
    pub capped: bool,
}

/// `/channels/{id}/invites`
pub fn invites_path(channel: &str) -> String {
    format!("/channels/{}/invites", crate::api::seg(channel))
}

/// `/channels/{id}/invites/{invite_id}`
pub fn invite_path(channel: &str, invite: &str) -> String {
    format!("{}/{}", invites_path(channel), crate::api::seg(invite))
}

/// `/channels/{id}/guests`
pub fn guests_path(channel: &str) -> String {
    format!("/channels/{}/guests", crate::api::seg(channel))
}

/// `/channels/{id}/guests/{guest_id}`
pub fn guest_path(channel: &str, guest: &str) -> String {
    format!("{}/{}", guests_path(channel), crate::api::seg(guest))
}

/// `/channels/{id}/guests/{guest_id}/allow`
pub fn guest_allow_path(channel: &str, guest: &str) -> String {
    format!("{}/allow", guest_path(channel, guest))
}

/// Body of the create-invite request: `{}` without a TTL, never an empty body with a content type.
pub fn new_invite_body(ttl_hours: Option<u32>) -> serde_json::Value {
    serde_json::to_value(NewInvite { ttl_hours }).expect("NewInvite serialises")
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

/// Body of `POST /api/channels/{id}/mentions/preview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionPreviewRequest {
    pub text: String,
}

/// A member agent a message would reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionTarget {
    pub id: String,
    pub name: String,
}

/// Answer of the mention preview (FR-T12, FR-T13): the agents the text reaches and the
/// `@names` that are not members of the channel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionPreview {
    #[serde(default)]
    pub reaches: Vec<MentionTarget>,
    #[serde(default)]
    pub unknown: Vec<String>,
}

/// Path of `POST /channels/{id}/mentions/preview`.
pub(crate) fn mentions_preview_path(id: &str) -> String {
    format!("/channels/{}/mentions/preview", crate::api::seg(id))
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

    #[test]
    fn fr_t10_client_mentions_default_when_absent() {
        let m: ChannelMessage = serde_json::from_value(json!({"id":"1","seq":1,"from":"user","text":"hi","at":1})).unwrap();
        assert!(m.mentions.is_empty());
        let m: ChannelMessage =
            serde_json::from_value(json!({"id":"2","seq":2,"from":"user","text":"@a hi","at":1,"mentions":["s1"],"extra":1})).unwrap();
        assert_eq!(m.mentions, vec!["s1".to_string()]);
        assert_eq!(serde_json::to_value(&m).unwrap()["mentions"], json!(["s1"]));
    }

    #[test]
    fn fr_t10_client_mention_preview_roundtrip() {
        let j = json!({"reaches":[{"id":"s1","name":"alpha"}],"unknown":["ghost"]});
        let p: MentionPreview = serde_json::from_value(j.clone()).unwrap();
        assert_eq!(p.reaches, vec![MentionTarget { id: "s1".into(), name: "alpha".into() }]);
        assert_eq!(p.unknown, vec!["ghost".to_string()]);
        assert_eq!(serde_json::to_value(&p).unwrap(), j);
        let empty: MentionPreview = serde_json::from_value(json!({})).unwrap();
        assert!(empty.reaches.is_empty() && empty.unknown.is_empty());
        assert_eq!(serde_json::to_value(MentionPreviewRequest { text: "@alpha go".into() }).unwrap(), json!({"text":"@alpha go"}));
    }

    #[test]
    fn fr_t10_client_mentions_paths() {
        assert_eq!(mentions_preview_path("c1"), "/channels/c1/mentions/preview");
        assert_eq!(mentions_preview_path("a/b"), "/channels/a%2Fb/mentions/preview");
    }

    #[test]
    fn fr_t15_client_message_decodes_author_fields() {
        let j = json!({"id":"3","seq":3,"from":"guest:g1","text":"hi","at":9,"unread":0,"author_name":"Mina","author_kind":"guest"});
        let m: ChannelMessage = serde_json::from_value(j.clone()).unwrap();
        assert_eq!((m.author_name.as_str(), m.author_kind.as_str()), ("Mina", "guest"));
        assert_eq!(serde_json::to_value(&m).unwrap(), j);
        let old: ChannelMessage = serde_json::from_value(json!({"id":"1","seq":1,"from":"user","text":"x","at":1})).unwrap();
        assert!(old.author_name.is_empty() && old.author_kind.is_empty());
    }

    #[test]
    fn fr_t17_client_channel_guests_decode_and_default() {
        let c: Channel = serde_json::from_value(json!({
            "id":"c","name":"n","project":"p",
            "guests":[{"id":"g1","name":"Mina","allow_until":1700000000000i64},{"id":"g2","name":"Jun","allow_until":null},{"id":"g3","name":"Sol"}]
        }))
        .unwrap();
        assert_eq!(c.guests.len(), 3);
        assert_eq!(c.guests[0].allow_until, Some(1_700_000_000_000));
        assert!(c.guests[1].allow_until.is_none() && c.guests[2].allow_until.is_none());
        let old: Channel = serde_json::from_value(json!({"id":"c","name":"n","project":"p"})).unwrap();
        assert!(old.guests.is_empty());
    }

    #[test]
    fn fr_t14_client_new_invite_body() {
        assert_eq!(new_invite_body(None), json!({}));
        assert_eq!(new_invite_body(Some(48)), json!({"ttl_hours":48}));
    }

    #[test]
    fn fr_t14_client_created_invite_decodes_with_and_without_door() {
        let old: CreatedInvite =
            serde_json::from_value(json!({"id":"i1","token":"dpi_abc","link":"/join#dpi_abc","expires_at":1700000000000i64})).unwrap();
        assert!(!old.link_warning && old.guest_door.is_none());
        let on: CreatedInvite = serde_json::from_value(json!({
            "id":"i2","token":"dpi_x","link":"http://127.0.0.1:7071/join#dpi_x","expires_at":5,"link_warning":true,
            "guest_door":{"state":"on","listen":"127.0.0.1:7071","public_url":null}
        }))
        .unwrap();
        assert!(on.link_warning);
        let d = on.guest_door.unwrap();
        assert!(d.is_on() && d.listen.as_deref() == Some("127.0.0.1:7071") && d.public_url.is_none());
        let off: CreatedInvite = serde_json::from_value(json!({
            "id":"i3","token":"t","link":"/join#t","expires_at":5,"link_warning":false,
            "guest_door":{"state":"off","listen":null,"public_url":null}
        }))
        .unwrap();
        assert!(!off.guest_door.unwrap().is_on());
    }

    #[test]
    fn fr_t14_client_invite_list_decodes() {
        let v: Vec<Invite> = serde_json::from_value(json!([
            {"id":"i2","channel_id":"c","created_by":"user","created_at":20,"expires_at":90,"used_at":30,"revoked_at":null,"guest_name":"Mina"},
            {"id":"i1","channel_id":"c","created_by":"user","created_at":10,"expires_at":80,"used_at":null,"revoked_at":null}
        ]))
        .unwrap();
        assert_eq!(v[0].guest_name.as_deref(), Some("Mina"));
        assert_eq!(v[0].used_at, Some(30));
        assert!(v[1].guest_name.is_none() && v[1].used_at.is_none() && v[1].revoked_at.is_none());
    }

    #[test]
    fn fr_t14_client_invite_paths() {
        assert_eq!(invites_path("c1"), "/channels/c1/invites");
        assert_eq!(invites_path("a/b"), "/channels/a%2Fb/invites");
        assert_eq!(invite_path("c1", "i 1"), "/channels/c1/invites/i%201");
    }

    #[test]
    fn fr_t17_client_guest_list_decodes() {
        let v: Vec<Guest> = serde_json::from_value(json!([
            {"id":"g1","name":"Mina","created_at":10,"expires_at":99,"revoked_at":null,"allow_until":50},
            {"id":"g2","name":"Jun","created_at":5,"expires_at":99,"revoked_at":60,"allow_until":null}
        ]))
        .unwrap();
        assert_eq!(v[0].allow_until, Some(50));
        assert_eq!(v[1].revoked_at, Some(60));
        assert!(v[1].allow_until.is_none());
    }

    #[test]
    fn fr_t17_client_allowance_body_and_answer() {
        assert_eq!(serde_json::to_value(AllowanceBody { hours: 4 }).unwrap(), json!({"hours":4}));
        let a: Allowance = serde_json::from_value(json!({"guest_id":"g1","allow_until":1700000000000i64,"capped":true})).unwrap();
        assert!(a.capped && a.guest_id == "g1");
        let b: Allowance = serde_json::from_value(json!({"guest_id":"g1","allow_until":7,"capped":false})).unwrap();
        assert!(!b.capped);
    }

    #[test]
    fn fr_t17_client_guest_paths() {
        assert_eq!(guests_path("c1"), "/channels/c1/guests");
        assert_eq!(guest_path("c1", "g/1"), "/channels/c1/guests/g%2F1");
        assert_eq!(guest_allow_path("c1", "g1"), "/channels/c1/guests/g1/allow");
    }

    #[test]
    fn fr_t14_client_guest_door_decodes() {
        let off: GuestDoorState = serde_json::from_str(r#"{"state":"off","listen":null,"public_url":null,"link_warning":false}"#).unwrap();
        assert!(!off.is_on() && !off.link_warning && off.listen.is_none());
        let on: GuestDoorState = serde_json::from_str(r#"{"state":"on","listen":"0.0.0.0:8741","public_url":null,"link_warning":true}"#).unwrap();
        assert!(on.is_on() && on.link_warning);
        assert_eq!(on.listen.as_deref(), Some("0.0.0.0:8741"));
        // The invite answer's guest_door has no link_warning: it defaults to false.
        let embedded: GuestDoorState = serde_json::from_str(r#"{"state":"on","listen":"127.0.0.1:8741","public_url":"https://x.example"}"#).unwrap();
        assert!(embedded.is_on() && !embedded.link_warning);
        assert_eq!(GUEST_DOOR_PATH, "/guest-door");
    }
}
