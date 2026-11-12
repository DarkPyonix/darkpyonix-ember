//! Schema for projects, computer assignment, session metadata and message search (store
//! migrations 8 and 9).

/// Migration from `user_version` 7 to 8: projects as a first-class table (FR-L1) and which
/// computers are assigned to each (FR-L4, many-to-many).
///
/// Every project an existing session names becomes a row, created when its first session was.
/// `computer_id` is a registered computer's id or `local` (the server itself); it has no foreign
/// key so `local` needs no row, and removing a computer deletes its assignments explicitly
/// (`computers::Registry::remove`).
pub const MIGRATION: &str = "
CREATE TABLE projects (
    name        TEXT PRIMARY KEY,
    created_at  INTEGER NOT NULL
);
INSERT INTO projects (name, created_at)
    SELECT project, MIN(created_at) FROM sessions GROUP BY project;

CREATE TABLE project_computers (
    project     TEXT NOT NULL REFERENCES projects(name),
    computer_id TEXT NOT NULL,
    assigned_at INTEGER NOT NULL,
    PRIMARY KEY (project, computer_id)
);
CREATE INDEX project_computers_by_computer ON project_computers(computer_id);
";

/// Migration from `user_version` 8 to 9: session metadata (FR-L9) and full-text search over
/// every session's messages (FR-S4).
///
/// `messages_fts` is an FTS5 index of user and assistant messages (`text`), with the session id,
/// sequence number and event kind stored alongside but not indexed. It is filled from the
/// existing events here and kept current by a trigger on `events`, so `Store::append` needs no
/// change and the index can never miss an event written in the same transaction.
///
/// Tokenizer: `unicode61`. Hangul, CJK and other letters are token characters, so Korean words
/// are indexed whole (`오류가`) and found by prefix (`오류*`); queries add the `*` (see
/// `Store::search`). `remove_diacritics 2` folds Latin accents only.
pub const META_FTS_MIGRATION: &str = "
ALTER TABLE sessions ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;

CREATE VIRTUAL TABLE messages_fts USING fts5(
    text,
    session_id UNINDEXED,
    seq UNINDEXED,
    kind UNINDEXED,
    tokenize = 'unicode61 remove_diacritics 2'
);
INSERT INTO messages_fts (text, session_id, seq, kind)
    SELECT json_extract(event, '$.text'), session_id, seq, json_extract(event, '$.kind')
    FROM events
    WHERE json_extract(event, '$.kind') IN ('user_message', 'assistant_message');

CREATE TRIGGER events_messages_fts AFTER INSERT ON events
WHEN json_extract(NEW.event, '$.kind') IN ('user_message', 'assistant_message')
BEGIN
    INSERT INTO messages_fts (text, session_id, seq, kind)
    VALUES (json_extract(NEW.event, '$.text'), NEW.session_id, NEW.seq, json_extract(NEW.event, '$.kind'));
END;
";

/// Migration 23: project settings (FR-L26, #384). Repository, default branch, new-member
/// defaults (a JSON object `{agent?, model?, mode?}`), the team character style (FR-L22) and an
/// `archived` flag, all on `projects`. The leader and profile override columns follow in a later
/// migration. Existing rows get no repository, no branch, `{}` defaults, no style, not archived.
pub const SETTINGS_MIGRATION: &str = "
ALTER TABLE projects ADD COLUMN repository TEXT;
ALTER TABLE projects ADD COLUMN default_branch TEXT;
ALTER TABLE projects ADD COLUMN member_defaults TEXT NOT NULL DEFAULT '{}';
ALTER TABLE projects ADD COLUMN character_style TEXT;
ALTER TABLE projects ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
";

/// Migration 25: the per-project profile override (FR-L26, FR-L20, #384 PR 3). `profile_overrides`
/// is a nullable TEXT holding a JSON object that maps a harness name to an account id
/// (`{"claude-code":"<account id>"}`); `NULL` means none. Existing rows get `NULL`.
pub const PROFILE_OVERRIDES_MIGRATION: &str = "
ALTER TABLE projects ADD COLUMN profile_overrides TEXT;
";

/// Migration 29: per-member character overrides on a project (FR-L22, FR-L26, #489).
/// `character_overrides` is a nullable TEXT holding a JSON object that maps a member name to a
/// [`crate::projects::CharacterOverride`]; `NULL` means none. Existing rows get `NULL`.
pub const CHARACTER_OVERRIDES_MIGRATION: &str = "
ALTER TABLE projects ADD COLUMN character_overrides TEXT;
";
