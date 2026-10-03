//! `UriComponents` as Code-OSS marshals them.
//!
//! A `URI` serializes via `toJSON()` to `{ $mid: 1, scheme, authority, path, query, fragment }`
//! plus optional caches (`external`, `fsPath`, `_sep`) that receivers ignore unless consistent
//! (`src/vs/base/common/uri.ts` `Uri.toJSON` L479, `URI.revive` L408;
//! `MarshalledId.Uri = 1` in `marshallingIds.ts`).
//!
//! Both the server's IPC channels and the remote extension host run a URI transformer
//! (`src/vs/base/common/uriTransformer.ts` L18-47): incoming `vscode-remote://<authority>/p`
//! becomes `file:///p` on the server, and outgoing `file` URIs come back as `vscode-remote`.
//! So the editor always addresses server files as `vscode-remote://<authority><path>`.

use serde::{Deserialize, Serialize};

/// `Schemas.vscodeRemote`.
pub const SCHEME_REMOTE: &str = "vscode-remote";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UriComponents {
    #[serde(rename = "$mid", default = "mid_uri", skip_deserializing)]
    mid: u8,
    pub scheme: String,
    #[serde(default)]
    pub authority: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub fragment: String,
}

fn mid_uri() -> u8 {
    1
}

impl UriComponents {
    pub fn new(scheme: impl Into<String>, authority: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            mid: 1,
            scheme: scheme.into(),
            authority: authority.into(),
            path: path.into(),
            query: String::new(),
            fragment: String::new(),
        }
    }

    /// `vscode-remote://<authority><path>`: a file on the server.
    pub fn remote(authority: &str, path: &str) -> Self {
        Self::new(SCHEME_REMOTE, authority, path)
    }

    /// `file://<path>`: what a server-side URI looks like before the transformer maps it back.
    pub fn file(path: &str) -> Self {
        Self::new("file", "", path)
    }

    /// A stable string key (not the full RFC 3986 `toString` with percent-encoding; use only as a
    /// map key inside Ember).
    pub fn key(&self) -> String {
        let mut s = format!("{}://{}{}", self.scheme, self.authority, self.path);
        if !self.query.is_empty() {
            s.push('?');
            s.push_str(&self.query);
        }
        if !self.fragment.is_empty() {
            s.push('#');
            s.push_str(&self.fragment);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_mid() {
        let u = UriComponents::remote("localhost:8000", "/w/a.rs");
        assert_eq!(
            serde_json::to_value(&u).unwrap(),
            serde_json::json!({"$mid":1,"scheme":"vscode-remote","authority":"localhost:8000","path":"/w/a.rs","query":"","fragment":""})
        );
    }

    #[test]
    fn deserializes_server_shape_with_caches() {
        let u: UriComponents = serde_json::from_value(serde_json::json!({
            "$mid":1,"external":"vscode-remote://h/x","path":"/x","scheme":"vscode-remote","authority":"h"
        }))
        .unwrap();
        assert_eq!(u, UriComponents::remote("h", "/x"));
    }
}
