//! Paths an explorer edit may name (D38, #438).
//!
//! The OSE file channel writes with the server user's rights and has no folder boundary of its own,
//! so these checks only keep a create, rename or delete inside the project folder and refuse the
//! mistakes that would hurt (the folder itself, a name with a slash, a `..` that climbs out). They are
//! a safeguard against bugs, not a security boundary: a symbolic link inside the folder that points
//! outside it is not seen by a string check.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    /// The name is empty, `.`, `..`, or holds a slash or a NUL.
    BadName,
    /// The path is not below the project folder.
    OutsideFolder,
    /// The path is the project folder itself.
    FolderRoot,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PathError::BadName => "이름이 올바르지 않습니다. 비어 있거나 / 가 들어 있으면 안 됩니다",
            PathError::OutsideFolder => "프로젝트 폴더 밖의 경로입니다",
            PathError::FolderRoot => "프로젝트 폴더 자체는 바꿀 수 없습니다",
        })
    }
}

/// An absolute path with `.` and `..` resolved, duplicate and trailing slashes dropped. `None`
/// for a relative path or one that climbs above the root.
pub fn normalize(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

fn inside(folder: &str, path: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    path == folder || path.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
}

fn valid_name(name: &str) -> bool {
    let trimmed = name.trim();
    !(trimmed.is_empty() || trimmed == "." || trimmed == ".." || name.contains('/') || name.contains('\0'))
}

/// `name` below the directory `parent`, which must be the project `folder` or inside it.
pub fn child_path(folder: &str, parent: &str, name: &str) -> Result<String, PathError> {
    if !valid_name(name) {
        return Err(PathError::BadName);
    }
    let folder = normalize(folder).ok_or(PathError::OutsideFolder)?;
    let parent = normalize(parent).ok_or(PathError::OutsideFolder)?;
    if !inside(&folder, &parent) {
        return Err(PathError::OutsideFolder);
    }
    Ok(if parent == "/" { format!("/{name}") } else { format!("{parent}/{name}") })
}

/// A path to rename or delete: strictly inside the project folder.
pub fn target_path(folder: &str, path: &str) -> Result<String, PathError> {
    let folder = normalize(folder).ok_or(PathError::OutsideFolder)?;
    let path = normalize(path).ok_or(PathError::OutsideFolder)?;
    if path == folder {
        return Err(PathError::FolderRoot);
    }
    if !inside(&folder, &path) {
        return Err(PathError::OutsideFolder);
    }
    Ok(path)
}

/// `path` with its last segment replaced by `new_name`.
pub fn renamed_path(folder: &str, path: &str, new_name: &str) -> Result<String, PathError> {
    let path = target_path(folder, path)?;
    if !valid_name(new_name) {
        return Err(PathError::BadName);
    }
    let parent = path.rsplit_once('/').map(|(p, _)| if p.is_empty() { "/" } else { p }).unwrap_or("/");
    child_path(folder, parent, new_name)
}
