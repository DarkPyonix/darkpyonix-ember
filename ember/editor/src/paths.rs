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
    /// The directory is a symbolic link: an item made in it would land wherever the link points.
    Symlink,
    /// The new name is the old name.
    SameName,
    /// Moving or copying into itself or a subdirectory.
    IntoSelf,
    /// An open document has unsaved changes.
    Dirty,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PathError::BadName => "이름이 올바르지 않습니다. 비어 있거나 / 가 들어 있으면 안 됩니다",
            PathError::OutsideFolder => "프로젝트 폴더 밖의 경로입니다",
            PathError::FolderRoot => "프로젝트 폴더 자체는 바꿀 수 없습니다",
            PathError::Symlink => "심볼릭 링크 폴더 안에는 만들 수 없습니다",
            PathError::SameName => "이름이 그대로입니다",
            PathError::IntoSelf => "폴더를 자기 자신이나 하위 폴더 안으로 옮길 수 없습니다",
            PathError::Dirty => "저장하지 않은 변경이 있어 이름을 바꿀 수 없습니다. 먼저 저장하세요",
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
    !(trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || name.len() > 255
        || name != trimmed
        || name.ends_with('.')
        || name.chars().any(|c| c == '/' || c == '\\' || c.is_control()))
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
    let (parent, old_name) = path.rsplit_once('/').map(|(p, n)| (if p.is_empty() { "/" } else { p }, n)).unwrap_or(("/", ""));
    if new_name == old_name {
        return Err(PathError::SameName);
    }
    child_path(folder, parent, new_name)
}

/// An absolute path strictly inside or equal to the project folder.
pub fn folder_path(folder: &str, path: &str) -> Result<String, PathError> {
    let folder = normalize(folder).ok_or(PathError::OutsideFolder)?;
    let path = normalize(path).ok_or(PathError::OutsideFolder)?;
    if !inside(&folder, &path) {
        return Err(PathError::OutsideFolder);
    }
    Ok(path)
}

/// Splits a file name into its stem and extension.
/// For example: "main.rs" -> ("main", ".rs"), "README" -> ("README", ""),
/// ".gitignore" -> (".gitignore", ""), "archive.tar.gz" -> ("archive.tar", ".gz").
pub fn split_stem_ext(name: &str) -> (&str, &str) {
    if let Some((stem, _)) = name.rsplit_once('.') {
        if !stem.is_empty() {
            return name.split_at(stem.len());
        }
    }
    (name, "")
}

/// Destination path when copying or moving `src` into `dest_dir`.
/// If `cut` is true, moving to the same directory or with the same name returns `PathError::SameName`.
/// If `cut` is false, colliding names generate a suffix: " copy", " copy 2", " copy 3", etc.
pub fn paste_destination(
    folder: &str,
    src: &str,
    dest_dir: &str,
    existing_names: &[&str],
    cut: bool,
) -> Result<String, PathError> {
    let src = target_path(folder, src)?;
    let folder_norm = normalize(folder).ok_or(PathError::OutsideFolder)?;
    let dest_dir = normalize(dest_dir).ok_or(PathError::OutsideFolder)?;
    if !inside(&folder_norm, &dest_dir) {
        return Err(PathError::OutsideFolder);
    }
    if dest_dir == src || dest_dir.strip_prefix(&src).is_some_and(|r| r.starts_with('/')) {
        return Err(PathError::IntoSelf);
    }
    let name = src.rsplit_once('/').map(|(_, n)| n).unwrap_or(&src);
    if cut || !existing_names.contains(&name) {
        let dest_path = child_path(folder, &dest_dir, name)?;
        if cut && dest_path == src {
            return Err(PathError::SameName);
        }
        return Ok(dest_path);
    }
    let (stem, ext) = split_stem_ext(name);
    for i in 1.. {
        let candidate = if i == 1 {
            format!("{stem} copy{ext}")
        } else {
            format!("{stem} copy {i}{ext}")
        };
        if !existing_names.contains(&candidate.as_str()) {
            return child_path(folder, &dest_dir, &candidate);
        }
    }
    unreachable!()
}
