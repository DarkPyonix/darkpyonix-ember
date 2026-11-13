//! The script that runs before the page's own scripts (SPEC FR-E8 (3)).
//!
//! It does what Code-OSS's `pre/index.html` does for the inner frame (`getVsCodeApiScript`,
//! `applyStyles`): defines `acquireVsCodeApi()` (once per page, frozen object with `postMessage`,
//! `setState`, `getState`; the state a reload starts with is embedded), puts the theme's CSS variables
//! on `<html>` and the theme class on `<body>`.
//!
//! The host node supplies the channel as `globalThis.<ipc global>.post(kind, payload)`, the global
//! named by [`IPC_GLOBAL`] (the host is configured with it, compose-rust `.ipc_global(...)`), e.g.
//! `globalThis.__emberWebviewHost.post(kind, payload)`
//! (`"message"` with the page's value, `"state"` with the JSON text of the new state); how bytes
//! cross the OS's IPC is the node's business. Messages to the page arrive as ordinary `message`
//! events on `window`, as in Code-OSS.

use super::theme::ThemeKind;

/// The name of the global the host node injects before this script runs; Ember configures the host
/// with it ([`super::host::HostWebview::ipc_global`]).
pub const IPC_GLOBAL: &str = "__emberWebviewHost";

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The init script for one page: `state` is the JSON text the page last saved with `setState`, and
/// `ipc_global` the name of the host's channel global (normally [`IPC_GLOBAL`]).
pub fn init_script(state: Option<&str>, vars: &[(String, String)], kind: ThemeKind, ipc_global: &str) -> String {
    let state_expr = match state {
        Some(s) => format!("JSON.parse({})", js_string(s)),
        None => "undefined".into(),
    };
    let mut style = String::new();
    for (k, v) in vars {
        style.push_str(&format!("    root.setProperty({}, {});\n", js_string(k), js_string(v)));
    }
    let class = js_string(kind.body_class());
    let host_global = js_string(ipc_global);
    format!(
        r#"(function () {{
  const host = globalThis[{host_global}];
  let state = {state_expr};
  let acquired = false;
  globalThis.acquireVsCodeApi = function () {{
    if (acquired) {{
      throw new Error('An instance of the VS Code API has already been acquired');
    }}
    acquired = true;
    return Object.freeze({{
      postMessage: function (message) {{ host.post('message', message); }},
      setState: function (newState) {{ state = newState; host.post('state', JSON.stringify(newState)); return newState; }},
      getState: function () {{ return state; }}
    }});
  }};
  const applyTheme = function () {{
    const root = document.documentElement.style;
{style}  }};
  applyTheme();
  const applyClass = function () {{
    const body = document.body;
    if (!body) {{ return; }}
    body.classList.remove('vscode-light', 'vscode-dark', 'vscode-high-contrast', 'vscode-high-contrast-light');
    body.classList.add({class});
    body.dataset.vscodeThemeKind = {class};
  }};
  if (document.body) {{ applyClass(); }} else {{ document.addEventListener('DOMContentLoaded', applyClass); }}
}})();
"#
    )
}

/// A script that replaces the theme on a live page (theme change in the window).
pub fn theme_update_script(vars: &[(String, String)], kind: ThemeKind) -> String {
    let mut s = String::from("(function () {\n  const root = document.documentElement.style;\n");
    s.push_str("  for (let i = root.length - 1; i >= 0; i--) { const p = root[i]; if (p && p.startsWith('--vscode-')) { root.removeProperty(p); } }\n");
    for (k, v) in vars {
        s.push_str(&format!("  root.setProperty({}, {});\n", js_string(k), js_string(v)));
    }
    let class = js_string(kind.body_class());
    s.push_str(&format!(
        "  if (document.body) {{ document.body.classList.remove('vscode-light', 'vscode-dark'); document.body.classList.add({class}); document.body.dataset.vscodeThemeKind = {class}; }}\n}})();\n"
    ));
    s
}
