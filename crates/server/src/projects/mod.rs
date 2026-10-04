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
use serde::Serialize;

use crate::computers::LOCAL;
use crate::store::{now_ms, Store};

/// A project with the computers assigned to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    pub name: String,
    pub created_at: i64,
    /// Assigned computer ids (`local` = this server), sorted.
    pub computers: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("project {0:?} not found")]
    NotFound(String),
    #[error("computer {0} not found")]
    ComputerNotFound(String),
    #[error("{0}")]
    BadRequest(String),
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
        let mut stmt = conn.prepare("SELECT name, created_at FROM projects ORDER BY name")?;
        let rows: Vec<(String, i64)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        let mut stmt = conn.prepare("SELECT project, computer_id FROM project_computers ORDER BY computer_id")?;
        let pairs: Vec<(String, String)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(name, created_at)| {
                let computers = pairs.iter().filter(|(p, _)| *p == name).map(|(_, c)| c.clone()).collect();
                Project { name, created_at, computers }
            })
            .collect())
    }

    pub fn project(&self, name: &str) -> anyhow::Result<Option<Project>> {
        let conn = self.conn();
        let Some(created_at) = conn
            .query_row("SELECT created_at FROM projects WHERE name = ?1", params![name], |r| r.get::<_, i64>(0))
            .optional()?
        else {
            return Ok(None);
        };
        let mut stmt =
            conn.prepare("SELECT computer_id FROM project_computers WHERE project = ?1 ORDER BY computer_id")?;
        let computers = stmt.query_map(params![name], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(Some(Project { name: name.to_string(), created_at, computers }))
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
