//! The server token (SPEC `FR-S11`, `INTENT.md` D37).
//!
//! The server requires `Authorization: Bearer <token>` on its local HTTP and WebSocket routes
//! (`FR-S9`). A client on the same computer finds the token the same way every time: the
//! `EMBER_SERVER_TOKEN` variable, else the `server-token` file in the data folder, else the
//! one in `~/.ember`. No token means no header, which keeps an older server working.

use std::path::Path;

/// The token the server requires from apps on the same computer: `env_token` when set and
/// non-empty, else the first line of `<data_dir>/server-token`, else
/// `<home>/.ember/server-token`. The result is trimmed; an empty value is none.
pub fn server_token(env_token: Option<&str>, data_dir: Option<&Path>, home: Option<&Path>) -> Option<String> {
    // Only the first line counts, so a trailing newline or a note below it is ignored.
    let clean = |s: &str| {
        let s = s.lines().next().unwrap_or_default().trim();
        (!s.is_empty()).then(|| s.to_owned())
    };
    if let Some(token) = env_token.and_then(clean) {
        return Some(token);
    }
    let file = match (data_dir, home) {
        (Some(dir), _) => dir.join("server-token"),
        (None, Some(home)) => home.join(".ember").join("server-token"),
        (None, None) => return None,
    };
    std::fs::read_to_string(file).ok().and_then(|s| clean(&s))
}

/// [`server_token`] over the process environment (`EMBER_SERVER_TOKEN`, `EMBER_DATA_DIR`, `HOME`).
pub fn server_token_from_env() -> Option<String> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let env_token = std::env::var("EMBER_SERVER_TOKEN").ok();
    let data_dir = var("EMBER_DATA_DIR").map(std::path::PathBuf::from);
    let home = var("HOME").map(std::path::PathBuf::from);
    server_token(env_token.as_deref(), data_dir.as_deref(), home.as_deref())
}

/// The `Authorization` header value for `token`.
pub fn bearer_header(token: &str) -> String {
    format!("Bearer {token}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_s11_env_beats_data_dir_beats_home() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("server-token"), "from-data\n").unwrap();
        std::fs::create_dir_all(home.path().join(".ember")).unwrap();
        std::fs::write(home.path().join(".ember").join("server-token"), "from-home\n").unwrap();

        let all = server_token(Some("from-env"), Some(data.path()), Some(home.path()));
        assert_eq!(all.as_deref(), Some("from-env"));
        let no_env = server_token(None, Some(data.path()), Some(home.path()));
        assert_eq!(no_env.as_deref(), Some("from-data"));
        let home_only = server_token(None, None, Some(home.path()));
        assert_eq!(home_only.as_deref(), Some("from-home"));
    }

    #[test]
    fn fr_s11_token_is_trimmed_and_empty_is_none() {
        let data = tempfile::tempdir().unwrap();
        assert_eq!(server_token(Some("  abc \n"), None, None).as_deref(), Some("abc"));
        // A blank env value falls through to the file.
        std::fs::write(data.path().join("server-token"), "  filetok  \nsecond line\n").unwrap();
        assert_eq!(server_token(Some("   "), Some(data.path()), None).as_deref(), Some("filetok"));
        // An empty file is none, and it does not fall back to home once a data dir is given.
        std::fs::write(data.path().join("server-token"), "\n").unwrap();
        assert_eq!(server_token(None, Some(data.path()), None), None);
        assert_eq!(server_token(None, None, None), None);
        // A missing file is none.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(server_token(None, Some(empty.path()), None), None);
    }

    #[test]
    fn fr_s11_bearer_header_format() {
        assert_eq!(bearer_header("abc123"), "Bearer abc123");
    }
}
