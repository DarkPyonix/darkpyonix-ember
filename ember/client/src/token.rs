//! The server token (SPEC `FR-S11`, `INTENT.md` D37).
//!
//! The server requires `Authorization: Bearer <token>` on its local HTTP and WebSocket routes
//! (`FR-S9`). A client on the same computer finds the token the same way every time: the
//! `EMBER_SERVER_TOKEN` variable, else the `server-token` file in the data folder, else the
//! one in `~/.ember`. No token means no header, which keeps an older server working.

use std::path::Path;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

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
    if !token_file_usable(&file) {
        return None;
    }
    std::fs::read_to_string(file).ok().and_then(|s| clean(&s))
}

/// Whether the token file may be read (unix): a regular file, not a symlink, with no group or
/// other permission bits. A missing file is quietly unusable; a refused one logs why (never the
/// token). The file's owner must be the owner of its folder (the data folder is the user's own);
/// the current uid is not asked for, so `ember-client` needs no `libc`. The environment variable
/// is not subject to this.
fn token_file_usable(path: &Path) -> bool {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let refuse = |why: &str| {
        tracing::warn!("ignoring server token file {}: {why}", path.display());
        false
    };
    if !meta.file_type().is_file() {
        return refuse("not a regular file (symlinks are refused)");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.permissions().mode() & 0o077 != 0 {
            return refuse("group or other can access it, set mode 0600");
        }
        let dir_uid = path.parent().and_then(|d| std::fs::metadata(d).ok()).map(|m| m.uid());
        if !owner_matches(meta.uid(), dir_uid) {
            return refuse("its owner is not the owner of its folder");
        }
    }
    true
}

/// The token file's owner matches its folder's owner; an unreadable folder fails the check.
#[cfg(unix)]
fn owner_matches(file_uid: u32, dir_uid: Option<u32>) -> bool {
    dir_uid == Some(file_uid)
}

/// Whether the token may be sent to `base_url` (FR-S11): its host is loopback (`127.0.0.0/8`,
/// `::1`, `localhost`) or listed in `EMBER_TOKEN_HOSTS` (comma separated). Any other server is
/// reached by device key, not by this token.
pub fn token_allowed_for(base_url: &str) -> bool {
    let extra = std::env::var("EMBER_TOKEN_HOSTS").unwrap_or_default();
    token_allowed_for_hosts(base_url, &extra)
}

/// [`token_allowed_for`] with the extra host list given.
pub fn token_allowed_for_hosts(base_url: &str, extra_hosts: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else { return false };
    let Some(host) = url.host_str() else { return false };
    let host = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
        return true;
    }
    extra_hosts.split(',').map(|h| h.trim().to_ascii_lowercase()).any(|h| !h.is_empty() && h == host)
}

/// Default headers holding `Authorization: Bearer <token>`, marked sensitive so it stays out of
/// debug output. `None` when the token cannot be a header value.
pub fn auth_headers(token: &str) -> Option<HeaderMap> {
    let mut value = HeaderValue::from_str(&bearer_header(token)).ok()?;
    value.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, value);
    Some(headers)
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

    /// Writes a token file the way the server does, mode 0600.
    fn write_token(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn fr_s11_env_beats_data_dir_beats_home() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write_token(&data.path().join("server-token"), "from-data\n");
        std::fs::create_dir_all(home.path().join(".ember")).unwrap();
        write_token(&home.path().join(".ember").join("server-token"), "from-home\n");

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
        write_token(&data.path().join("server-token"), "  filetok  \nsecond line\n");
        assert_eq!(server_token(Some("   "), Some(data.path()), None).as_deref(), Some("filetok"));
        // An empty file is none, and it does not fall back to home once a data dir is given.
        write_token(&data.path().join("server-token"), "\n");
        assert_eq!(server_token(None, Some(data.path()), None), None);
        assert_eq!(server_token(None, None, None), None);
        // A missing file is none.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(server_token(None, Some(empty.path()), None), None);
    }

    #[test]
    fn fr_s11_default_headers_carry_a_sensitive_bearer() {
        let headers = auth_headers("abc").unwrap();
        let v = headers.get(AUTHORIZATION).unwrap();
        assert_eq!(v, "Bearer abc");
        assert!(v.is_sensitive());
        assert!(auth_headers("bad\ntoken").is_none());
    }

    #[test]
    fn fr_s11_token_goes_only_to_loopback_or_listed_hosts() {
        for url in ["http://127.0.0.1:8740", "http://127.1.2.3", "http://[::1]:8740", "http://localhost:1/x"] {
            assert!(token_allowed_for_hosts(url, ""), "{url}");
        }
        assert!(!token_allowed_for_hosts("https://example.com", ""));
        assert!(!token_allowed_for_hosts("http://127.0.0.1.evil.com", ""));
        assert!(!token_allowed_for_hosts("http://127.0.0.1@example.com", ""));
        assert!(!token_allowed_for_hosts("not a url", ""));
        assert!(token_allowed_for_hosts("https://dev.example.com:9", "foo.org, Dev.Example.com"));
        assert!(!token_allowed_for_hosts("https://example.com", "foo.org,,"));
    }

    #[cfg(unix)]
    fn token_in(dir: &Path, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let f = dir.join("server-token");
        std::fs::write(&f, "filetok\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
        f
    }

    #[cfg(unix)]
    #[test]
    fn fr_s11_token_file_0600_is_accepted() {
        let d = tempfile::tempdir().unwrap();
        token_in(d.path(), 0o600);
        assert_eq!(server_token(None, Some(d.path()), None).as_deref(), Some("filetok"));
    }

    #[cfg(unix)]
    #[test]
    fn fr_s11_token_file_0644_is_refused() {
        let d = tempfile::tempdir().unwrap();
        token_in(d.path(), 0o644);
        assert_eq!(server_token(None, Some(d.path()), None), None);
        // The environment variable is not subject to the file checks.
        assert_eq!(server_token(Some("envtok"), Some(d.path()), None).as_deref(), Some("envtok"));
    }

    #[cfg(unix)]
    #[test]
    fn fr_s11_token_file_symlink_is_refused() {
        let real = tempfile::tempdir().unwrap();
        let target = token_in(real.path(), 0o600);
        let d = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(&target, d.path().join("server-token")).unwrap();
        assert_eq!(server_token(None, Some(d.path()), None), None);
    }

    #[test]
    fn fr_s11_bearer_header_format() {
        assert_eq!(bearer_header("abc123"), "Bearer abc123");
    }

    #[cfg(unix)]
    #[test]
    fn fr_s11_token_file_owner_must_match_its_folder() {
        assert!(owner_matches(501, Some(501)));
        assert!(!owner_matches(0, Some(501)));
        assert!(!owner_matches(501, None));
    }
}
