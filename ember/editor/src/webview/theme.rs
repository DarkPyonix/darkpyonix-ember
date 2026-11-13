//! Theme variables for extension pages (SPEC FR-E8 (3)).
//!
//! Code-OSS gives every webview the workbench theme as CSS custom properties on `<html>`
//! (`contrib/webview/browser/themeing.ts`: `--vscode-<color id with . as ->`, the fonts, and
//! `--text-link-decoration`) and a body class (`vscode-light`, `vscode-dark`, ...). Extension pages
//! style themselves with these names, so Ember produces the same names from its own light and dark
//! tokens (`docs/DESIGN.md` §2), and the page matches the window.
//!
//! [`DEFAULT_STYLE_VARS`] is every variable the webview's own default stylesheet reads
//! (`contrib/webview/browser/pre/index.html`, `defaultStyles` at the pinned commit); [`css_vars`] always
//! defines all of them, plus the common widget colours extensions use most (buttons, inputs,
//! dropdowns, lists, badges, the side bar and panel, error and warning colours).

/// Light or dark, as the window shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Light,
    Dark,
}

impl ThemeKind {
    /// The class Code-OSS puts on the page's `<body>` (`ApiThemeClassName`).
    pub fn body_class(self) -> &'static str {
        match self {
            Self::Light => "vscode-light",
            Self::Dark => "vscode-dark",
        }
    }
}

/// Ember's colour tokens (DESIGN.md §2, the mockup's custom properties), as CSS colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    pub bg: &'static str,
    pub surface: &'static str,
    pub surface_2: &'static str,
    pub line: &'static str,
    pub text: &'static str,
    pub text_2: &'static str,
    pub muted: &'static str,
    pub hover: &'static str,
    pub sel: &'static str,
    pub ink: &'static str,
    pub ink_text: &'static str,
    pub blue: &'static str,
    pub blue_soft: &'static str,
    pub orange: &'static str,
    pub orange_soft: &'static str,
    pub red: &'static str,
    pub green: &'static str,
    /// The large shadow's colour (10% black in light, 45% in dark).
    pub shadow: &'static str,
}

pub const LIGHT: Tokens = Tokens {
    bg: "#f3f2ee",
    surface: "#ffffff",
    surface_2: "#f7f6f2",
    line: "#ebe9e4",
    text: "#1c1b19",
    text_2: "#56534d",
    muted: "#8f8b83",
    hover: "#f0eee9",
    sel: "#ecebe6",
    ink: "#1c1b19",
    ink_text: "#ffffff",
    blue: "#3f74d4",
    blue_soft: "#e6eefb",
    orange: "#e49b55",
    orange_soft: "#fbeedf",
    red: "#d65a4a",
    green: "#4f9a6c",
    shadow: "rgba(0, 0, 0, 0.1)",
};

pub const DARK: Tokens = Tokens {
    bg: "#151514",
    surface: "#1f1f1d",
    surface_2: "#252523",
    line: "#2f2e2b",
    text: "#f0eee9",
    text_2: "#c3bfb7",
    muted: "#8b877f",
    hover: "#2a2927",
    sel: "#2f2e2b",
    ink: "#f0eee9",
    ink_text: "#151514",
    blue: "#6b95e0",
    blue_soft: "#1e2a3f",
    orange: "#e6a76a",
    orange_soft: "#3a2c1d",
    red: "#e07264",
    green: "#6fb287",
    shadow: "rgba(0, 0, 0, 0.45)",
};

impl ThemeKind {
    pub fn tokens(self) -> &'static Tokens {
        match self {
            Self::Light => &LIGHT,
            Self::Dark => &DARK,
        }
    }
}

/// The fonts Code-OSS passes (`vscode-font-*`, `vscode-editor-font-*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fonts {
    pub ui_family: String,
    pub ui_size_px: u32,
    pub editor_family: String,
    pub editor_size_px: u32,
    pub editor_weight: String,
}

impl Default for Fonts {
    /// DESIGN.md: Inter with the Korean fallbacks for the UI, JetBrains Mono for code; 13px as in
    /// Code-OSS's webview default.
    fn default() -> Self {
        Self {
            ui_family: "Inter, -apple-system, \"Apple SD Gothic Neo\", \"Noto Sans KR\", system-ui, sans-serif".into(),
            ui_size_px: 13,
            editor_family: "\"JetBrains Mono\", ui-monospace, monospace".into(),
            editor_size_px: 13,
            editor_weight: "normal".into(),
        }
    }
}

/// The variables `index.html`'s default stylesheet reads, so every page looks right even if the
/// extension sets no colours of its own.
pub const DEFAULT_STYLE_VARS: [&str; 21] = [
    "--vscode-editor-background",
    "--vscode-editor-findMatchBackground",
    "--vscode-editor-findMatchHighlightBackground",
    "--vscode-editor-foreground",
    "--vscode-font-family",
    "--vscode-font-size",
    "--vscode-font-weight",
    "--vscode-keybindingLabel-background",
    "--vscode-keybindingLabel-border",
    "--vscode-keybindingLabel-bottomBorder",
    "--vscode-keybindingLabel-foreground",
    "--vscode-scrollbarSlider-activeBackground",
    "--vscode-scrollbarSlider-background",
    "--vscode-scrollbarSlider-hoverBackground",
    "--vscode-textBlockQuote-background",
    "--vscode-textBlockQuote-border",
    "--vscode-textLink-activeForeground",
    "--vscode-textLink-foreground",
    "--vscode-textPreformat-background",
    "--vscode-textPreformat-foreground",
    "--vscode-widget-shadow",
];

/// `#rrggbb` plus an alpha byte; anything else is returned as is.
fn alpha(hex: &str, a: u8) -> String {
    if hex.len() == 7 && hex.starts_with('#') {
        format!("{hex}{a:02x}")
    } else {
        hex.to_owned()
    }
}

/// The CSS variables of a page in the window's theme, as `(name, value)` with the leading `--`, in
/// a fixed order.
pub fn css_vars(kind: ThemeKind, fonts: &Fonts) -> Vec<(String, String)> {
    let t = kind.tokens();
    let colors: Vec<(&str, String)> = vec![
        // The webview default stylesheet (DEFAULT_STYLE_VARS).
        ("editor-background", t.surface.into()),
        ("editor-foreground", t.text.into()),
        ("editor-findMatchBackground", alpha(t.orange, 0x99)),
        ("editor-findMatchHighlightBackground", alpha(t.orange, 0x55)),
        ("keybindingLabel-background", t.surface_2.into()),
        ("keybindingLabel-foreground", t.text_2.into()),
        ("keybindingLabel-border", t.line.into()),
        ("keybindingLabel-bottomBorder", t.line.into()),
        ("scrollbarSlider-background", alpha(t.muted, 0x44)),
        ("scrollbarSlider-hoverBackground", alpha(t.muted, 0x77)),
        ("scrollbarSlider-activeBackground", alpha(t.muted, 0xaa)),
        ("textBlockQuote-background", t.surface_2.into()),
        ("textBlockQuote-border", t.line.into()),
        ("textLink-foreground", t.blue.into()),
        ("textLink-activeForeground", t.blue.into()),
        ("textPreformat-background", t.surface_2.into()),
        ("textPreformat-foreground", t.text.into()),
        ("widget-shadow", t.shadow.into()),
        // Common workbench colours extension pages read.
        ("foreground", t.text.into()),
        ("descriptionForeground", t.muted.into()),
        ("disabledForeground", t.muted.into()),
        ("errorForeground", t.red.into()),
        ("focusBorder", t.blue.into()),
        ("contrastBorder", "transparent".into()),
        ("widget-border", t.line.into()),
        ("editor-selectionBackground", t.blue_soft.into()),
        ("editor-lineHighlightBackground", t.hover.into()),
        ("editorWidget-background", t.surface.into()),
        ("editorWidget-foreground", t.text.into()),
        ("editorWidget-border", t.line.into()),
        ("editorError-foreground", t.red.into()),
        ("editorWarning-foreground", t.orange.into()),
        ("editorInfo-foreground", t.blue.into()),
        ("button-background", t.ink.into()),
        ("button-foreground", t.ink_text.into()),
        ("button-hoverBackground", t.text_2.into()),
        ("button-border", "transparent".into()),
        ("button-secondaryBackground", t.surface_2.into()),
        ("button-secondaryForeground", t.text.into()),
        ("button-secondaryHoverBackground", t.hover.into()),
        ("input-background", t.surface.into()),
        ("input-foreground", t.text.into()),
        ("input-border", t.line.into()),
        ("input-placeholderForeground", t.muted.into()),
        ("inputOption-activeBorder", t.blue.into()),
        ("dropdown-background", t.surface.into()),
        ("dropdown-foreground", t.text.into()),
        ("dropdown-border", t.line.into()),
        ("checkbox-background", t.surface.into()),
        ("checkbox-foreground", t.text.into()),
        ("checkbox-border", t.line.into()),
        ("list-hoverBackground", t.hover.into()),
        ("list-activeSelectionBackground", t.sel.into()),
        ("list-activeSelectionForeground", t.text.into()),
        ("list-inactiveSelectionBackground", t.sel.into()),
        ("list-focusOutline", t.blue.into()),
        ("badge-background", t.ink.into()),
        ("badge-foreground", t.ink_text.into()),
        ("progressBar-background", t.blue.into()),
        ("sideBar-background", t.surface_2.into()),
        ("sideBar-foreground", t.text.into()),
        ("sideBarTitle-foreground", t.text_2.into()),
        ("sideBarSectionHeader-background", t.surface_2.into()),
        ("panel-background", t.surface.into()),
        ("panel-border", t.line.into()),
        ("tab-activeBackground", t.surface.into()),
        ("tab-inactiveBackground", t.surface_2.into()),
        ("textCodeBlock-background", t.surface_2.into()),
        ("textSeparator-foreground", t.line.into()),
        ("icon-foreground", t.text_2.into()),
        ("charts-foreground", t.text.into()),
        ("charts-red", t.red.into()),
        ("charts-blue", t.blue.into()),
        ("charts-orange", t.orange.into()),
        ("charts-green", t.green.into()),
        ("window-background", t.bg.into()),
    ];
    let mut out: Vec<(String, String)> = vec![
        ("--vscode-font-family".into(), fonts.ui_family.clone()),
        ("--vscode-font-weight".into(), "normal".into()),
        ("--vscode-font-size".into(), format!("{}px", fonts.ui_size_px)),
        ("--vscode-editor-font-family".into(), fonts.editor_family.clone()),
        ("--vscode-editor-font-weight".into(), fonts.editor_weight.clone()),
        ("--vscode-editor-font-size".into(), format!("{}px", fonts.editor_size_px)),
        ("--text-link-decoration".into(), "none".into()),
    ];
    out.extend(colors.into_iter().map(|(k, v)| (format!("--vscode-{k}"), v)));
    out.push(("--vscode-editor-font-feature-settings".into(), "\"liga\" off, \"calt\" off".into()));
    out
}

/// The variables as one `:root { ... }` rule (for a host that injects a stylesheet).
pub fn css_text(vars: &[(String, String)]) -> String {
    let mut s = String::from(":root {");
    for (k, v) in vars {
        s.push_str(&format!(" {k}: {v};"));
    }
    s.push_str(" }");
    s
}
