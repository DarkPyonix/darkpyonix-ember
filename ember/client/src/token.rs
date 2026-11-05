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
    read_token_file(&file).and_then(|s| clean(&s))
}

/// Reads the token file if it may be used (unix): a regular file, not a symlink, with no group
/// or other permission bits. A missing file is quietly unusable; a refused one logs why (never
/// the token). The file's owner must be the owner of its folder (the data folder is the user's
/// own); the current uid is not asked for, so `ember-client` needs no `libc`. The environment
/// variable is not subject to this.
///
/// The file is opened once and every check runs on the open handle, then the text is read from
/// that handle, so swapping the path between check and read changes nothing. A swap between the
/// open and the check is caught by comparing the path's own `symlink_metadata` (a symlink, or a
/// different dev/inode, is refused) with the handle's.
fn read_token_file(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let handle = file.metadata().ok()?;
    let refuse = |why: &str| {
        tracing::warn!("ignoring server token file {}: {why}", path.display());
        None
    };
    let on_path = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return refuse("the path changed while it was opened"),
    };
    if on_path.file_type().is_symlink() {
        return refuse("not a regular file (symlinks are refused)");
    }
    if !handle.is_file() || !on_path.is_file() {
        return refuse("not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if handle.dev() != on_path.dev() || handle.ino() != on_path.ino() {
            return refuse("the path was replaced while it was opened");
        }
        if handle.permissions().mode() & 0o077 != 0 {
            return refuse("group or other can access it, set mode 0600");
        }
        let dir_uid = path.parent().and_then(|d| std::fs::metadata(d).ok()).map(|m| m.uid());
        if !owner_matches(handle.uid(), dir_uid) {
            return refuse("its owner is not the owner of its folder");
        }
    }
    // A token is short; a larger file is not one and is not read whole.
    let mut text = String::new();
    file.by_ref().take(64 * 1024).read_to_string(&mut text).ok()?;
    Some(text)
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
    extra_hosts.split(',').map(str::trim).any(|entry| entry_matches(entry, &url, &host))
}

/// One `EMBER_TOKEN_HOSTS` entry against the target. A bare `host` matches that host on any
/// scheme and port. An entry with a scheme (`https://host:8740`) matches only that scheme, host
/// and port; a missing port means the scheme's default port.
fn entry_matches(entry: &str, url: &reqwest::Url, host: &str) -> bool {
    if entry.is_empty() {
        return false;
    }
    if !entry.contains("://") {
        return entry.to_ascii_lowercase() == host;
    }
    let Ok(listed) = reqwest::Url::parse(entry) else { return false };
    let Some(listed_host) = listed.host_str() else { return false };
    let norm = |h: &str| h.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    listed.scheme() == url.scheme()
        && norm(listed_host) == host
        && listed.port_or_known_default() == url.port_or_known_default()
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

    #[test]
    fn fr_s11_token_hosts_entries_may_carry_scheme_and_port() {
        let list = "https://dev.example.com:8740";
        assert!(token_allowed_for_hosts("https://dev.example.com:8740/api", list));
        assert!(token_allowed_for_hosts("https://DEV.example.com:8740", list));
        // Another port, another scheme or another host is refused.
        assert!(!token_allowed_for_hosts("https://dev.example.com:9", list));
        assert!(!token_allowed_for_hosts("https://dev.example.com", list));
        assert!(!token_allowed_for_hosts("http://dev.example.com:8740", list));
        assert!(!token_allowed_for_hosts("https://other.example.com:8740", list));
        // A missing port in the entry means the scheme's default port.
        assert!(token_allowed_for_hosts("https://dev.example.com", "https://dev.example.com"));
        assert!(token_allowed_for_hosts("https://dev.example.com:443", "https://dev.example.com"));
        assert!(!token_allowed_for_hosts("https://dev.example.com:8740", "https://dev.example.com"));
        // A bare host still matches any scheme and port; entries mix freely.
        assert!(token_allowed_for_hosts("http://dev.example.com:1", "dev.example.com"));
        assert!(token_allowed_for_hosts("http://b.org:1", "https://a.org:2, b.org"));
        // An entry that is not a URL never matches; loopback is unchanged.
        assert!(!token_allowed_for_hosts("https://dev.example.com", "https://"));
        assert!(token_allowed_for_hosts("http://127.0.0.1:5", "https://a.org:2"));
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

    #[cfg(unix)]
    #[test]
    fn fr_s11_token_file_read_goes_through_the_open_handle() {
        use std::os::unix::fs::MetadataExt;
        let d = tempfile::tempdir().unwrap();
        let f = token_in(d.path(), 0o600);
        // A regular 0600 file reads, and the handle and the path name the same inode.
        assert_eq!(read_token_file(&f).as_deref(), Some("filetok\n"));
        let handle = std::fs::File::open(&f).unwrap().metadata().unwrap();
        let on_path = std::fs::symlink_metadata(&f).unwrap();
        assert_eq!((handle.dev(), handle.ino()), (on_path.dev(), on_path.ino()));
        // A symlink, a directory and a missing path are refused.
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        assert_eq!(read_token_file(&link), None);
        assert_eq!(read_token_file(d.path()), None);
        assert_eq!(read_token_file(&d.path().join("missing")), None);
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
