//! Where a DAP `Source` lives, as Code-OSS's `Source` (`debugSource.ts` `getUriFromSource`) and
//! its URI identity (`uriIdentityService.extUri.isEqual`) decide it (#1151).
//!
//! The adapter names a file by `source.path`, a string. On the way to the window the extension
//! host turns an absolute path into a URI (`debugUtils.convertToVSCPaths(message, true)`, in
//! `extHostDebugService.ts`), and the RPC layer's URI transformer turns its `file` scheme into
//! `vscode-remote://<authority>`. So `source.path` reaches Ember as a marshalled URI object, not
//! as the string the adapter wrote. A frame is matched to an open document by
//! [`same_resource`], never by `==`, so two spellings of one file name the same document.

use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

/// The workspace file a DAP `source` names, on the workspace's server: `None` for a source the
/// adapter serves itself (`sourceReference > 0`, Code-OSS's `debug:` scheme, which Ember does not
/// open yet), a relative path, or a source without a path.
///
/// `source.path` is either the marshalled URI the extension host sends, or a string: a URI
/// (`isUriString`) or an absolute path (`URI.file`). A `file` URI is a file on the server, so it
/// takes the workspace folder's scheme and authority, as the URI transformer would have given it.
pub fn source_uri(source: &Value, workspace: Option<&UriComponents>) -> Option<UriComponents> {
    if source.get("sourceReference").and_then(Value::as_i64).is_some_and(|r| r > 0) {
        return None;
    }
    let path = source.get("path")?;
    let uri = match path {
        Value::Object(_) => serde_json::from_value::<UriComponents>(path.clone()).ok()?,
        Value::String(s) if is_uri_string(s) => parse_uri(s)?,
        Value::String(s) => UriComponents::file(&file_path(s)?),
        _ => return None,
    };
    let uri = if uri.scheme == "file" {
        let ws = workspace?;
        UriComponents::new(ws.scheme.as_str(), ws.authority.as_str(), uri.path.as_str())
    } else {
        uri
    };
    Some(canonical(&uri))
}

/// Whether `a` and `b` name the same resource, as `extUri.isEqual` compares them: the scheme and
/// authority without case, the path after its normalisation ([`canonical`]). The path keeps its
/// case: Ember does not know whether the server's file system ignores it (`ignorePathCasing`).
pub fn same_resource(a: &UriComponents, b: &UriComponents) -> bool {
    if a == b {
        return true;
    }
    let (a, b) = (canonical(a), canonical(b));
    a.scheme == b.scheme
        && a.authority.eq_ignore_ascii_case(&b.authority)
        && a.path == b.path
        && a.query == b.query
        && a.fragment == b.fragment
}

/// `uriIdentityService.asCanonicalUri` for the parts Ember can know without asking the file
/// system: the scheme in lower case, a Windows drive letter in lower case (as `URI.toString`
/// writes it), `.` and `..` segments resolved, repeated and trailing slashes dropped. The
/// authority keeps its spelling, so a frame's file is the editor's own URI; [`same_resource`]
/// compares it without case.
pub fn canonical(uri: &UriComponents) -> UriComponents {
    let mut out = UriComponents::new(
        uri.scheme.to_ascii_lowercase(),
        uri.authority.as_str(),
        normalize_path(&uri.path),
    );
    out.query.clone_from(&uri.query);
    out.fragment.clone_from(&uri.fragment);
    out
}

/// `debugUtils.isUriString`: a scheme of at least two characters, so `C:` stays a drive letter.
fn is_uri_string(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    scheme.len() >= 2
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `URI.parse` for `scheme://authority/path?query#fragment` and `scheme:path`, with the path
/// percent-decoded.
fn parse_uri(s: &str) -> Option<UriComponents> {
    let (scheme, rest) = s.split_once(':')?;
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = match rest.strip_prefix("//") {
        Some(r) => match r.find('/') {
            Some(i) => (&r[..i], &r[i..]),
            None => (r, ""),
        },
        None => ("", rest),
    };
    let mut uri = UriComponents::new(scheme, percent_decode(authority), percent_decode(path));
    uri.query = percent_decode(query);
    uri.fragment = percent_decode(fragment);
    Some(uri)
}

/// The path part of `URI.file(p)` for an absolute path: backslashes become slashes and a drive
/// path gets its leading slash (`C:\w\a.py` is `/C:/w/a.py`); a relative path has no file.
fn file_path(p: &str) -> Option<String> {
    let p = p.replace('\\', "/");
    let bytes = p.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if drive {
        Some(format!("/{p}"))
    } else if p.starts_with('/') {
        Some(p)
    } else {
        None
    }
}

fn normalize_path(path: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // A drive letter is the root of its path: `..` does not climb above it.
                if parts.last().is_some_and(|p| !is_drive(p)) {
                    parts.pop();
                }
            }
            s => parts.push(s.to_owned()),
        }
    }
    if let Some(first) = parts.first_mut() {
        if is_drive(first) {
            first.make_ascii_lowercase();
        }
    }
    if path.starts_with('/') || parts.first().is_some_and(|p| is_drive(p)) {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

fn is_drive(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ws() -> UriComponents {
        UriComponents::remote("h:1", "/w")
    }

    #[test]
    fn fr_l31_42_source_path_as_a_remote_uri_object_is_that_uri() {
        let s = json!({"name": "main.py", "path": {"$mid": 1, "scheme": "vscode-remote", "authority": "h:1", "path": "/w/main.py", "external": "vscode-remote://h:1/w/main.py"}});
        assert_eq!(source_uri(&s, Some(&ws())), Some(UriComponents::remote("h:1", "/w/main.py")));
    }

    #[test]
    fn fr_l31_42_source_path_as_a_file_uri_takes_the_workspace_scheme_and_authority() {
        let s = json!({"path": {"$mid": 1, "scheme": "file", "authority": "", "path": "/w/main.py"}});
        assert_eq!(source_uri(&s, Some(&ws())), Some(UriComponents::remote("h:1", "/w/main.py")));
        let s = json!({"path": "file:///w/my%20file.py"});
        assert_eq!(source_uri(&s, Some(&ws())), Some(UriComponents::remote("h:1", "/w/my file.py")));
        assert_eq!(source_uri(&s, None), None);
    }

    #[test]
    fn fr_l31_42_source_path_strings_map_as_uri_file_does() {
        let at = |p: &str| source_uri(&json!({ "path": p }), Some(&ws()));
        assert_eq!(at("/w/main.py"), Some(UriComponents::remote("h:1", "/w/main.py")));
        assert_eq!(at("/w/./src//main.py"), Some(UriComponents::remote("h:1", "/w/src/main.py")));
        assert_eq!(at("/w/src/../main.py"), Some(UriComponents::remote("h:1", "/w/main.py")));
        assert_eq!(at(r"C:\w\main.py"), Some(UriComponents::remote("h:1", "/c:/w/main.py")));
        assert_eq!(at("vscode-remote://h:1/w/main.py"), Some(UriComponents::remote("h:1", "/w/main.py")));
        assert_eq!(at("main.py"), None, "a relative path is the adapter's to serve");
    }

    #[test]
    fn fr_l31_42_a_source_the_adapter_serves_is_not_a_workspace_file() {
        let s = json!({"name": "<string>", "path": "/w/gen.py", "sourceReference": 7});
        assert_eq!(source_uri(&s, Some(&ws())), None);
        assert_eq!(source_uri(&json!({"name": "x"}), Some(&ws())), None);
    }

    #[test]
    fn fr_l31_42_same_resource_ignores_scheme_case_drive_case_and_trailing_slashes() {
        let a = UriComponents::remote("h:1", "/w/main.py");
        assert!(same_resource(&a, &UriComponents::new("VSCODE-REMOTE", "H:1", "/w//main.py")));
        assert!(same_resource(&UriComponents::remote("h", "/C:/w/"), &UriComponents::remote("h", "/c:/w")));
        assert!(!same_resource(&a, &UriComponents::remote("h:1", "/w/other.py")));
        assert!(!same_resource(&a, &UriComponents::remote("h:2", "/w/main.py")));
        assert!(!same_resource(&a, &UriComponents::file("/w/main.py")));
    }
}
