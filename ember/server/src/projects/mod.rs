//! Projects and the computers assigned to them (SPEC FR-L1, FR-L3, FR-L4).
//!
//! A project is a name sessions belong to (FR-S1). It used to exist only as the `project`
//! column of `sessions`; it is now a row of its own so it can carry computer assignments and
//! exist before its first session. Creating a session creates its project when missing.
//!
//! Assignment is many-to-many: a project has any number of computers, a computer any number of
//! projects. Every change is pushed as [`crate::session::Push::ProjectUpdated`] so a second
//! client sees it without reloading (FR-L4, PR-1). Tables: [`schema::MIGRATION`].

pub mod schema;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::computers::LOCAL;
use crate::store::{now_ms, Store};

/// The project everyone starts in (FR-L13, D28). It cannot be deleted, renamed or given a
/// leader. The client defines the same string in `ember/app/src/ui/project.rs`
/// (`DEFAULT_PROJECT_NAME`) and `ember/app/src/ui/home.rs` (`DEFAULT_PROJECT`).
pub const DEFAULT_PROJECT_NAME: &str = "기본 프로젝트";

/// Longest project name in characters (the client's `NAME_MAX_CHARS`).
pub const NAME_MAX_CHARS: usize = 64;

/// The team character styles (FR-L22), as the client stores them (`CharStyle`, lowercase).
pub const CHARACTER_STYLES: [&str; 6] = ["mongle", "malang", "crew", "pocket", "cream", "keyring"];

/// The permission modes a new member may default to (`docs/DESIGN.md` 4.4).
pub const PERMISSION_MODES: [&str; 4] = ["ask", "edits", "plan", "bypass"];

/// New-member defaults (FR-L26). They apply to members created after the change only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberDefaults {
    /// A harness name (`claude-code`, `codex`, ...), see `AgentKind::parse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// One of [`PERMISSION_MODES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// A project with the computers assigned to it and its settings (FR-L26).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    pub name: String,
    pub created_at: i64,
    /// Assigned computer ids (`local` = this server), sorted.
    pub computers: Vec<String>,
    /// A URL or path, as the user typed it; `null` when none.
    pub repository: Option<String>,
    pub default_branch: Option<String>,
    pub member_defaults: MemberDefaults,
    /// One of [`CHARACTER_STYLES`]; `null` means the client default.
    pub character_style: Option<String>,
    pub archived: bool,
}

/// `PATCH /api/projects/{name}`. Every field is optional; `Some(None)` clears a nullable field.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub repository: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub default_branch: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub member_defaults: Option<Option<MemberDefaults>>,
    #[serde(default, deserialize_with = "nullable")]
    pub character_style: Option<Option<String>>,
}

/// Tell an absent field (`None`) from an explicit `null` (`Some(None)`).
fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(d)?))
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("project {0:?} not found")]
    NotFound(String),
    #[error("computer {0} not found")]
    ComputerNotFound(String),
    #[error("{0}")]
    BadRequest(String),
    /// 기본 프로젝트 may not be renamed, archived, deleted or given a leader.
    #[error("{0:?} is protected")]
    Protected(String),
    /// A rename onto the name of another project.
    #[error("a project named {0:?} already exists")]
    NameTaken(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for ProjectError {
    fn from(e: rusqlite::Error) -> Self {
        ProjectError::Other(e.into())
    }
}

/// A project name must be non-empty after trimming; it is stored as given (sessions already
/// use it verbatim).
/// Tables whose `project` column holds the project name and moves on a rename.
const RENAMED_TABLES: &[&str] =
    &["project_computers", "channels", "sessions", "schedules", "teams", "browser_egress"];

const PROJECT_COLUMNS: &str =
    "name, created_at, repository, default_branch, member_defaults, character_style, archived";

fn project_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let defaults: String = r.get(4)?;
    Ok(Project {
        name: r.get(0)?,
        created_at: r.get(1)?,
        computers: Vec::new(),
        repository: r.get(2)?,
        default_branch: r.get(3)?,
        member_defaults: serde_json::from_str(&defaults).unwrap_or_default(),
        character_style: r.get(5)?,
        archived: r.get::<_, i64>(6)? != 0,
    })
}

fn bad(msg: impl Into<String>) -> ProjectError {
    ProjectError::BadRequest(msg.into())
}

/// Same rules as the client's `validate_settings`: trimmed, 1..=64 characters, no `/`, no control
/// characters.
fn check_new_name(name: &str) -> Result<String, ProjectError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(bad("project name must not be empty"));
    }
    if name.chars().count() > NAME_MAX_CHARS {
        return Err(bad(format!("project name is longer than {NAME_MAX_CHARS} characters")));
    }
    if name.contains('/') || name.chars().any(char::is_control) {
        return Err(bad("project name must not contain '/' or control characters"));
    }
    Ok(name.to_string())
}

/// No whitespace, at most 512 characters; empty clears it.
fn check_repository(v: Option<&str>) -> Result<Option<String>, ProjectError> {
    let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    if v.chars().any(char::is_whitespace) {
        return Err(bad("repository must not contain whitespace"));
    }
    if v.chars().count() > 512 {
        return Err(bad("repository is longer than 512 characters"));
    }
    Ok(Some(v.to_string()))
}

/// The client's branch rules; empty clears it.
fn check_branch(v: Option<&str>) -> Result<Option<String>, ProjectError> {
    let Some(b) = v.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    if b.chars().any(char::is_whitespace) {
        return Err(bad("default_branch must not contain whitespace"));
    }
    if b.starts_with('-') || b.starts_with('/') || b.ends_with('/') || b.ends_with(".lock") || b.contains("..") {
        return Err(bad("default_branch is not a valid git ref name"));
    }
    if b.chars().any(char::is_control) || b.chars().count() > 255 {
        return Err(bad("default_branch is not a valid git ref name"));
    }
    Ok(Some(b.to_string()))
}

fn check_defaults(v: Option<&MemberDefaults>) -> Result<MemberDefaults, ProjectError> {
    let Some(d) = v else { return Ok(MemberDefaults::default()) };
    let norm = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let (agent, model, mode) = (norm(&d.agent), norm(&d.model), norm(&d.mode));
    if let Some(a) = &agent {
        if crate::agents::AgentKind::parse(a).is_none() {
            return Err(bad(format!("member_defaults.agent {a:?} is not a known agent")));
        }
    }
    if let Some(m) = &mode {
        if !PERMISSION_MODES.contains(&m.as_str()) {
            return Err(bad(format!("member_defaults.mode must be one of {}", PERMISSION_MODES.join(", "))));
        }
    }
    if let Some(m) = &model {
        if m.chars().count() > 128 || m.chars().any(char::is_control) {
            return Err(bad("member_defaults.model is not a valid model name"));
        }
    }
    Ok(MemberDefaults { agent, model, mode })
}

fn check_style(v: Option<&str>) -> Result<Option<String>, ProjectError> {
    match v {
        None => Ok(None),
        Some(s) if CHARACTER_STYLES.contains(&s) => Ok(Some(s.to_string())),
        Some(_) => Err(bad(format!("character_style must be one of {}", CHARACTER_STYLES.join(", ")))),
    }
}

pub fn validate_name(name: &str) -> Result<(), ProjectError> {
    if name.trim().is_empty() {
        return Err(ProjectError::BadRequest("project name must not be empty".into()));
    }
    Ok(())
}

impl Store {
    /// Every project, by name.
    pub fn projects(&self) -> anyhow::Result<Vec<Project>> {
        let conn = self.conn();
        let sql = format!("SELECT {PROJECT_COLUMNS} FROM projects ORDER BY name");
        let mut stmt = conn.prepare(&sql)?;
        let mut projects: Vec<Project> = stmt.query_map([], project_row)?.collect::<Result<_, _>>()?;
        let mut stmt = conn.prepare("SELECT project, computer_id FROM project_computers ORDER BY computer_id")?;
        let pairs: Vec<(String, String)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        for p in &mut projects {
            p.computers = pairs.iter().filter(|(n, _)| *n == p.name).map(|(_, c)| c.clone()).collect();
        }
        Ok(projects)
    }

    pub fn project(&self, name: &str) -> anyhow::Result<Option<Project>> {
        let conn = self.conn();
        let sql = format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE name = ?1");
        let Some(mut project) = conn.query_row(&sql, params![name], project_row).optional()? else {
            return Ok(None);
        };
        let mut stmt =
            conn.prepare("SELECT computer_id FROM project_computers WHERE project = ?1 ORDER BY computer_id")?;
        project.computers = stmt.query_map(params![name], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(Some(project))
    }

    /// Apply a partial settings change (FR-L26, #384) in one transaction.
    ///
    /// Order of refusals: unknown project (`NotFound`), 기본 프로젝트 rename (`Protected`), bad
    /// values (`BadRequest`), a taken name (`NameTaken`). A rename rewrites the name in every
    /// table that carries it (`sessions`, `channels`, `schedules`, `teams`, `browser_egress`,
    /// `project_computers`) and nothing is changed if any step fails. Channel JSONL files are
    /// keyed by channel id and stay where they are.
    pub fn patch_project(&self, name: &str, patch: &ProjectPatch) -> Result<Project, ProjectError> {
        if self.project(name)?.is_none() {
            return Err(ProjectError::NotFound(name.into()));
        }
        let new_name = match &patch.name {
            Some(n) => Some(check_new_name(n)?),
            None => None,
        };
        let renames = new_name.as_deref().is_some_and(|n| n != name);
        if renames && name == DEFAULT_PROJECT_NAME {
            return Err(ProjectError::Protected(name.into()));
        }
        let repository = patch.repository.as_ref().map(|v| check_repository(v.as_deref())).transpose()?;
        let branch = patch.default_branch.as_ref().map(|v| check_branch(v.as_deref())).transpose()?;
        let defaults = patch.member_defaults.as_ref().map(|v| check_defaults(v.as_ref())).transpose()?;
        let style = patch.character_style.as_ref().map(|v| check_style(v.as_deref())).transpose()?;

        let final_name = {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            if let Some(v) = &repository {
                tx.execute("UPDATE projects SET repository = ?1 WHERE name = ?2", params![v, name])?;
            }
            if let Some(v) = &branch {
                tx.execute("UPDATE projects SET default_branch = ?1 WHERE name = ?2", params![v, name])?;
            }
            if let Some(v) = &defaults {
                let json = serde_json::to_string(v).map_err(anyhow::Error::from)?;
                tx.execute("UPDATE projects SET member_defaults = ?1 WHERE name = ?2", params![json, name])?;
            }
            if let Some(v) = &style {
                tx.execute("UPDATE projects SET character_style = ?1 WHERE name = ?2", params![v, name])?;
            }
            let mut final_name = name.to_string();
            if let (true, Some(new)) = (renames, new_name.as_deref()) {
                let taken: i64 =
                    tx.query_row("SELECT COUNT(*) FROM projects WHERE name = ?1", params![new], |r| r.get(0))?;
                if taken > 0 {
                    return Err(ProjectError::NameTaken(new.into()));
                }
                // The name is the key and two tables reference it, so the new row is created
                // first, the dependants move, then the old row goes.
                tx.execute(
                    "INSERT INTO projects (name, created_at, repository, default_branch, member_defaults,
                                           character_style, archived)
                     SELECT ?1, created_at, repository, default_branch, member_defaults, character_style, archived
                     FROM projects WHERE name = ?2",
                    params![new, name],
                )?;
                for table in RENAMED_TABLES {
                    let verb = if *table == "browser_egress" { "UPDATE OR REPLACE" } else { "UPDATE" };
                    tx.execute(&format!("{verb} {table} SET project = ?1 WHERE project = ?2"), params![new, name])?;
                }
                tx.execute("DELETE FROM projects WHERE name = ?1", params![name])?;
                final_name = new.to_string();
            }
            tx.commit()?;
            final_name
        };
        self.project(&final_name)?.ok_or(ProjectError::NotFound(final_name))
    }

    /// Create `name` if it does not exist. Returns the project and whether it was created.
    pub fn ensure_project(&self, name: &str) -> Result<(Project, bool), ProjectError> {
        validate_name(name)?;
        let created = self.conn().execute(
            "INSERT OR IGNORE INTO projects (name, created_at) VALUES (?1, ?2)",
            params![name, now_ms()],
        )? > 0;
        let p = self.project(name)?.ok_or_else(|| ProjectError::NotFound(name.into()))?;
        Ok((p, created))
    }

    /// Assign `computer_id` (a registered computer or `local`) to `project`. Idempotent.
    pub fn assign_computer(&self, project: &str, computer_id: &str) -> Result<Project, ProjectError> {
        if self.project(project)?.is_none() {
            return Err(ProjectError::NotFound(project.into()));
        }
        if computer_id != LOCAL {
            let known: i64 = self.conn().query_row(
                "SELECT COUNT(*) FROM computers WHERE id = ?1",
                params![computer_id],
                |r| r.get(0),
            )?;
            if known == 0 {
                return Err(ProjectError::ComputerNotFound(computer_id.into()));
            }
        }
        self.conn().execute(
            "INSERT OR IGNORE INTO project_computers (project, computer_id, assigned_at) VALUES (?1, ?2, ?3)",
            params![project, computer_id, now_ms()],
        )?;
        self.project(project)?.ok_or_else(|| ProjectError::NotFound(project.into()))
    }

    /// Unassign `computer_id` from `project`. Idempotent (unassigning what is not assigned is
    /// not an error).
    pub fn unassign_computer(&self, project: &str, computer_id: &str) -> Result<Project, ProjectError> {
        if self.project(project)?.is_none() {
            return Err(ProjectError::NotFound(project.into()));
        }
        self.conn().execute(
            "DELETE FROM project_computers WHERE project = ?1 AND computer_id = ?2",
            params![project, computer_id],
        )?;
        self.project(project)?.ok_or_else(|| ProjectError::NotFound(project.into()))
    }

    /// Projects `computer_id` is assigned to.
    pub fn projects_of_computer(&self, computer_id: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT project FROM project_computers WHERE computer_id = ?1 ORDER BY project")?;
        let rows = stmt.query_map(params![computer_id], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;

    #[test]
    fn sessions_create_projects_and_assignment_is_many_to_many() {
        let store = Store::open_in_memory().unwrap();
        store.create_session("alpha", AgentKind::Scripted, "/tmp", None, "t").unwrap();
        store.create_session("alpha", AgentKind::Scripted, "/tmp", None, "t").unwrap();
        let (beta, created) = store.ensure_project("beta").unwrap();
        assert!(created && beta.computers.is_empty());
        assert!(!store.ensure_project("beta").unwrap().1);
        assert_eq!(store.projects().unwrap().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);

        store.assign_computer("alpha", LOCAL).unwrap();
        store.assign_computer("beta", LOCAL).unwrap();
        let p = store.assign_computer("alpha", LOCAL).unwrap();
        assert_eq!(p.computers, [LOCAL], "assigning twice is a no-op");
        assert_eq!(store.projects_of_computer(LOCAL).unwrap(), ["alpha", "beta"]);
        assert!(matches!(store.assign_computer("alpha", "nope"), Err(ProjectError::ComputerNotFound(_))));
        assert!(matches!(store.assign_computer("gamma", LOCAL), Err(ProjectError::NotFound(_))));
        assert!(store.unassign_computer("alpha", LOCAL).unwrap().computers.is_empty());
        assert!(store.unassign_computer("alpha", LOCAL).is_ok());
        assert!(matches!(store.ensure_project("  "), Err(ProjectError::BadRequest(_))));
    }
}
