//! Local resources of a webview (SPEC FR-E8 (2)): which request URL maps to which file.
//!
//! An extension turns a file into a URL its page can load with `webview.asWebviewUri`
//! (`contrib/webview/common/webview.ts` at the pinned commit):
//!
//! ```text
//! https://<scheme>+<encoded authority>.vscode-resource.vscode-cdn.net/<path>
//! ```
//!
//! where every character of the authority that is not `[A-Za-z0-9]` is written `-` plus four hex
//! digits. The host's scheme handler hands the URL to [`check`], which decodes it and allows it only
//! when the file is strictly inside one of the webview's `localResourceRoots`
//! (`resourceLoading.ts` `getResourceToLoad` / `containsResource`): a root itself is refused, the
//! query is ignored, `.` and `..` are resolved first (a `..` above the root is refused), and
//! backslashes count as slashes for non-`file` schemes. Anything else is [`Decision::Denied`].
//!
//! Code-OSS rewrites a `vscode-remote` resource to the server's `/vscode-resource` HTTP endpoint;
//! Ember reads the same file through the remote filesystem channel instead, so the allowed URI is
//! the file itself.

use ember_editor_conn::uri::UriComponents;

/// `webviewRootResourceAuthority`.
pub const RESOURCE_AUTHORITY: &str = "vscode-resource.vscode-cdn.net";

/// `webviewGenericCspSource`: what `webview.cspSource` adds to a page's CSP.
pub const CSP_SOURCE: &str = "'self' https://*.vscode-cdn.net";

/// What the host answers a resource request with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceResponse {
    Ok { bytes: Vec<u8>, mime: String },
    /// Outside every root, or not a resource URL (Code-OSS `AccessDenied`, HTTP 401).
    Denied,
    /// Inside a root, but the file could not be read (Code-OSS `Failed`, HTTP 404).
    Failed,
}

impl ResourceResponse {
    /// The HTTP status a scheme handler gives the page.
    pub fn status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::Denied => 401,
            Self::Failed => 404,
        }
    }
}

/// The outcome of [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Read this file and answer with [`mime_type`] of the request path.
    Allowed(UriComponents),
    Denied,
}

/// `asWebviewUri` for a file URI (`http` and `https` stay as they are).
pub fn as_webview_uri(resource: &UriComponents) -> String {
    if resource.scheme == "http" || resource.scheme == "https" {
        return resource.key();
    }
    let mut out = format!("https://{}+{}.{RESOURCE_AUTHORITY}{}", resource.scheme, encode_authority(&resource.authority), resource.path);
    if !resource.query.is_empty() {
        out.push('?');
        out.push_str(&resource.query);
    }
    if !resource.fragment.is_empty() {
        out.push('#');
        out.push_str(&resource.fragment);
    }
    out
}

/// `encodeAuthority`: `[A-Za-z0-9]` as is, anything else `-` and four lower-case hex digits.
pub fn encode_authority(authority: &str) -> String {
    let mut out = String::new();
    for unit in authority.encode_utf16() {
        let c = unit as u32;
        if (c as u8).is_ascii_alphanumeric() && c < 0x80 {
            out.push(c as u8 as char);
        } else {
            out.push_str(&format!("-{c:04x}"));
        }
    }
    out
}

/// `decodeAuthority`: `-XXXX` back to the character.
pub fn decode_authority(encoded: &str) -> Option<String> {
    if !encoded.is_ascii() {
        return None;
    }
    let mut units: Vec<u16> = Vec::new();
    let bytes = encoded.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && i + 5 <= bytes.len() {
            let hex = &encoded[i + 1..i + 5];
            if hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                units.push(u16::from_str_radix(hex, 16).ok()?);
                i += 5;
                continue;
            }
        }
        units.push(bytes[i] as u16);
        i += 1;
    }
    String::from_utf16(&units).ok()
}

/// Percent-decode a URL path; `None` when the bytes are not UTF-8.
pub fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Decode a resource request URL into the file it names. `None` for any other URL.
pub fn parse_resource_url(url: &str) -> Option<UriComponents> {
    let rest = strip_prefix_ci(url, "https://")?;
    let (host, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let suffix = format!(".{RESOURCE_AUTHORITY}");
    if host.len() <= suffix.len() || !host[host.len() - suffix.len()..].eq_ignore_ascii_case(&suffix) {
        return None;
    }
    let label = &host[..host.len() - suffix.len()];
    let (scheme, encoded) = label.split_once('+')?;
    if scheme.is_empty() {
        return None;
    }
    let authority = decode_authority(encoded)?;
    let end = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = percent_decode(&tail[..end])?;
    let query = tail[end..].strip_prefix('?').map(|q| q.split('#').next().unwrap_or("").to_owned()).unwrap_or_default();
    let mut uri = UriComponents::new(scheme.to_ascii_lowercase(), authority, path);
    uri.query = query;
    Some(uri)
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)).then(|| &s[prefix.len()..])
}

/// Resolve `.` and `..`; `None` when a `..` climbs above `/`.
pub fn normalize_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

/// Whether `resource` is strictly inside `root` (same scheme and authority, path below the root's).
pub fn contains(root: &UriComponents, resource: &UriComponents) -> bool {
    if !root.scheme.eq_ignore_ascii_case(&resource.scheme) || !root.authority.eq_ignore_ascii_case(&resource.authority) {
        return false;
    }
    let fix = |p: &str| if root.scheme != "file" { p.replace('\\', "/") } else { p.to_owned() };
    let (Some(root_path), Some(path)) = (normalize_path(&fix(&root.path)), normalize_path(&fix(&resource.path))) else {
        return false;
    };
    if root_path == path {
        return false;
    }
    let base = root_path.trim_end_matches('/');
    path.starts_with(&format!("{base}/"))
}

/// The resource rule: a request URL maps to a file only under one of `roots`.
pub fn check(url: &str, roots: &[UriComponents]) -> Decision {
    let Some(mut uri) = parse_resource_url(url) else { return Decision::Denied };
    if uri.scheme != "file" {
        uri.path = uri.path.replace('\\', "/");
    }
    let Some(path) = normalize_path(&uri.path) else { return Decision::Denied };
    if !roots.iter().any(|r| contains(r, &uri)) {
        return Decision::Denied;
    }
    uri.path = path;
    uri.query.clear();
    Decision::Allowed(uri)
}

/// The content type of a resource, from the request path's extension (`getWebviewContentMimeType`
/// falls back to `application/octet-stream`).
pub fn mime_type(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" | "cjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "xml" => "text/xml",
        _ => "application/octet-stream",
    }
}
