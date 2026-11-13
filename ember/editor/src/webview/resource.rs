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
//! **The URL the page sees depends on the host** ([`ResourceBase`], reported by the host per webview):
//! WKWebView and WebKitGTK can only intercept custom schemes, so there it is
//! `vscode-resource://<scheme>+<encoded authority>/<path>`; WebView2 and the Android WebView can
//! intercept `https://`, so there it stays Code-OSS's form. The extension host always writes Code-OSS's
//! form (its `asWebviewUri` and `cspSource` are computed in the extension host), so the window
//! rewrites the page's HTML and messages into the host's form ([`rewrite_for_host`]) and a request
//! URL back into Code-OSS's form ([`ResourceBase::to_canonical`]) before the session checks it.
//!
//! Code-OSS rewrites a `vscode-remote` resource to the server's `/vscode-resource` HTTP endpoint;
//! Ember reads the same file through the remote filesystem channel instead, so the allowed URI is
//! the file itself.

use ember_editor_conn::uri::UriComponents;

/// `webviewRootResourceAuthority`.
pub const RESOURCE_AUTHORITY: &str = "vscode-resource.vscode-cdn.net";

/// `webviewGenericCspSource`: what `webview.cspSource` adds to a page's CSP in Code-OSS's form (the
/// extension host writes this; the window rewrites it with [`ResourceBase::csp_source`]).
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

/// Where a host serves local resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceBase {
    /// `https://<scheme>+<authority>.<domain>/<path>` (WebView2, Android WebView). With
    /// [`RESOURCE_AUTHORITY`] as the domain it is Code-OSS's own form.
    Https { domain: String },
    /// `<scheme>://<scheme>+<authority>/<path>`, a custom scheme (WKWebView, WebKitGTK).
    Scheme { scheme: String },
}

/// The custom scheme the macOS and Linux hosts register.
pub const CUSTOM_SCHEME: &str = "vscode-resource";

impl ResourceBase {
    /// Code-OSS's form, which the extension host writes.
    pub fn code_oss() -> Self {
        Self::Https { domain: RESOURCE_AUTHORITY.to_owned() }
    }

    /// `vscode-resource://...`.
    pub fn custom_scheme() -> Self {
        Self::Scheme { scheme: CUSTOM_SCHEME.to_owned() }
    }

    /// The URL up to the path for a resource `scheme` and an already encoded authority.
    fn prefix(&self, scheme: &str, encoded_authority: &str) -> String {
        match self {
            Self::Https { domain } => format!("https://{scheme}+{encoded_authority}.{domain}"),
            Self::Scheme { scheme: s } => format!("{s}://{scheme}+{encoded_authority}"),
        }
    }

    /// What the page's CSP allows for resources (Code-OSS `webview.cspSource` is
    /// `'self' https://*.vscode-cdn.net`).
    pub fn csp_host(&self) -> String {
        match self {
            Self::Https { domain } => format!("https://*.{domain}"),
            Self::Scheme { scheme } => format!("{scheme}:"),
        }
    }

    pub fn csp_source(&self) -> String {
        format!("'self' {}", self.csp_host())
    }

    /// `asWebviewUri` in this host's form (`http` and `https` files stay as they are).
    pub fn as_webview_uri(&self, resource: &UriComponents) -> String {
        if resource.scheme == "http" || resource.scheme == "https" {
            return resource.key();
        }
        let mut out = format!("{}{}", self.prefix(&resource.scheme, &encode_authority(&resource.authority)), resource.path);
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

    /// Split a request URL of this form into the label (`<scheme>+<encoded authority>`) and the
    /// rest (path, query, fragment), without decoding anything.
    fn split<'a>(&self, url: &'a str) -> Option<(&'a str, &'a str)> {
        let rest = match self {
            Self::Https { .. } => strip_prefix_ci(url, "https://")?,
            Self::Scheme { scheme } => strip_prefix_ci(url, &format!("{scheme}://"))?,
        };
        let (host, tail) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let label = match self {
            Self::Https { domain } => {
                let suffix = format!(".{domain}");
                if host.len() <= suffix.len() || !host[host.len() - suffix.len()..].eq_ignore_ascii_case(&suffix) {
                    return None;
                }
                &host[..host.len() - suffix.len()]
            }
            Self::Scheme { .. } => host,
        };
        label.contains('+').then_some((label, tail))
    }

    /// Decode a request URL of this form into the file it names.
    pub fn parse(&self, url: &str) -> Option<UriComponents> {
        let (label, tail) = self.split(url)?;
        let (scheme, encoded) = label.split_once('+')?;
        if scheme.is_empty() {
            return None;
        }
        let authority = decode_authority(encoded)?;
        let tail = if tail.starts_with('/') { tail.to_owned() } else { format!("/{tail}") };
        let end = tail.find(['?', '#']).unwrap_or(tail.len());
        let path = percent_decode(&tail[..end])?;
        let query = tail[end..].strip_prefix('?').map(|q| q.split('#').next().unwrap_or("").to_owned()).unwrap_or_default();
        let mut uri = UriComponents::new(scheme.to_ascii_lowercase(), authority, path);
        uri.query = query;
        Some(uri)
    }

    /// The same request in Code-OSS's form, the rest of the URL kept byte for byte.
    pub fn to_canonical(&self, url: &str) -> Option<String> {
        let (label, tail) = self.split(url)?;
        Some(format!("https://{label}.{RESOURCE_AUTHORITY}{tail}"))
    }
}

/// Rewrite Code-OSS's resource URLs and CSP source in `text` (a page's HTML, a message's JSON) into
/// `base`'s form. The replacements contain no quote or backslash, so JSON stays valid.
pub fn rewrite_for_host(text: &str, base: &ResourceBase) -> String {
    if *base == ResourceBase::code_oss() {
        return text.to_owned();
    }
    const CDN_WILDCARD: &str = "*.vscode-cdn.net";
    let suffix = format!(".{RESOURCE_AUTHORITY}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("https://") {
        out.push_str(&rest[..i]);
        let after = &rest[i + "https://".len()..];
        let label_len = after.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'-').count();
        let label = &after[..label_len];
        if let (Some((scheme, encoded)), true) = (label.split_once('+'), after[label_len..].starts_with(&suffix)) {
            out.push_str(&base.prefix(scheme, encoded));
            rest = &after[label_len + suffix.len()..];
        } else if after.starts_with(CDN_WILDCARD) {
            out.push_str(&base.csp_host());
            rest = &after[CDN_WILDCARD.len()..];
        } else {
            out.push_str("https://");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `asWebviewUri` for a file URI in Code-OSS's form (`http` and `https` stay as they are).
pub fn as_webview_uri(resource: &UriComponents) -> String {
    ResourceBase::code_oss().as_webview_uri(resource)
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

/// Decode a request URL in Code-OSS's form into the file it names. `None` for any other URL.
pub fn parse_resource_url(url: &str) -> Option<UriComponents> {
    ResourceBase::code_oss().parse(url)
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
