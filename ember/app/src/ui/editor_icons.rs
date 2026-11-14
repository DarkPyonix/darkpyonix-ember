//! Icons from the mockup (icons.js): 24x24 stroke icons, drawn as images because the HTML path
//! has no inline SVG.

fn path(name: &str) -> &'static str {
    match name {
        "folder" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M2.25 12.75V12A2.25 2.25 0 0 1 4.5 9.75h15A2.25 2.25 0 0 1 21.75 12v.75m-8.69-6.44-2.12-2.12a1.5 1.5 0 0 0-1.061-.44H4.5A2.25 2.25 0 0 0 2.25 6v12a2.25 2.25 0 0 0 2.25 2.25h15A2.25 2.25 0 0 0 21.75 18V9a2.25 2.25 0 0 0-2.25-2.25h-5.379a1.5 1.5 0 0 1-1.06-.44Z"/>"#,
        "check" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="m4.5 12.75 6 6 9-13.5"/>"#,
        "git" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M6 3.75v10.5m0 0a2.25 2.25 0 1 0 0 4.5 2.25 2.25 0 0 0 0-4.5Zm12-6a2.25 2.25 0 1 0 0-4.5 2.25 2.25 0 0 0 0 4.5Zm0 0c0 4.5-6 3-12 6"/>"#,
        "spark" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M9.813 15.904 9 18.75l-.813-2.846a4.5 4.5 0 0 0-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 0 0 3.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 0 0 3.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 0 0-3.09 3.09ZM18.259 8.715 18 9.75l-.259-1.035a3.375 3.375 0 0 0-2.455-2.456L14.25 6l1.036-.259a3.375 3.375 0 0 0 2.455-2.456L18 2.25l.259 1.035a3.375 3.375 0 0 0 2.456 2.456L21.75 6l-1.035.259a3.375 3.375 0 0 0-2.456 2.456ZM16.894 20.567 16.5 21.75l-.394-1.183a2.25 2.25 0 0 0-1.423-1.423L13.5 18.75l1.183-.394a2.25 2.25 0 0 0 1.423-1.423l.394-1.183.394 1.183a2.25 2.25 0 0 0 1.423 1.423l1.183.394-1.183.394a2.25 2.25 0 0 0-1.423 1.423Z"/>"#,
        "bell" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M14.857 17.082a23.848 23.848 0 0 0 5.454-1.31A8.967 8.967 0 0 1 18 9.75V9A6 6 0 0 0 6 9v.75a8.967 8.967 0 0 1-2.312 6.022c1.733.64 3.56 1.085 5.455 1.31m5.714 0a24.255 24.255 0 0 1-5.714 0m5.714 0a3 3 0 1 1-5.714 0"/>"#,
        "code" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M17.25 6.75 22.5 12l-5.25 5.25m-10.5 0L1.5 12l5.25-5.25m7.5-3-4.5 16.5"/>"#,
        "play" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M5.25 5.653c0-.856.917-1.398 1.667-.986l11.54 6.347a1.125 1.125 0 0 1 0 1.972l-11.54 6.347a1.125 1.125 0 0 1-1.667-.986V5.653Z"/>"#,
        "warn" => r#"<path d="M12 9.5v4M12 16.75h.01M10.3 3.9 2.4 17.6a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z"/>"#,
        "search" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="m21 21-5.197-5.197m0 0A7.5 7.5 0 1 0 5.196 5.196a7.5 7.5 0 0 0 10.607 10.607Z"/>"#,
        "chev" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="m8.25 4.5 7.5 7.5-7.5 7.5"/>"#,
        "sun" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M12 3v2.25m6.364.386-1.591 1.591M21 12h-2.25m-.386 6.364-1.591-1.591M12 18.75V21m-4.773-4.227-1.591 1.591M5.25 12H3m4.227-4.773L5.636 5.636M15.75 12a3.75 3.75 0 1 1-7.5 0 3.75 3.75 0 0 1 7.5 0Z"/>"#,
        "moon" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M21.752 15.002A9.72 9.72 0 0 1 18 15.75c-5.385 0-9.75-4.365-9.75-9.75 0-1.33.266-2.597.748-3.752A9.753 9.753 0 0 0 3 11.25C3 16.635 7.365 21 12.75 21a9.753 9.753 0 0 0 9.002-5.998Z"/>"#,
        "more" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M6.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0ZM12.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0ZM18.75 12a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0Z"/>"#,
        "refresh" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182m0-4.991v4.99"/>"#,
        "pause" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M15.75 5.25v13.5m-7.5-13.5v13.5"/>"#,
        "plus" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M12 4.5v15m7.5-7.5h-15"/>"#,
        "minus" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M19.5 12h-15"/>"#,
        "edit" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="m16.862 4.487 1.687-1.688a1.875 1.875 0 1 1 2.652 2.652L10.582 16.07a4.5 4.5 0 0 1-1.897 1.13L6 18l.8-2.685a4.5 4.5 0 0 1 1.13-1.897l8.932-8.931Zm0 0L19.5 7.125M18 14v4.75A2.25 2.25 0 0 1 15.75 21H5.25A2.25 2.25 0 0 1 3 18.75V8.25A2.25 2.25 0 0 1 5.25 6H10"/>"#,
        "up" => r#"<path d="M12 19V5M5.5 11.5 12 5l6.5 6.5"/>"#,
        "down" => r#"<path d="M12 5v14M18.5 12.5 12 19l-6.5-6.5"/>"#,
        "close" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M6 18 18 6M6 6l12 12"/>"#,
        // A push pin, for a pinned tab (FR-L31.5); the mockup draws none, so it follows the outline set.
        "pin" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="M15 3.75 20.25 9l-2.25.75-3.75 3.75.75 3.75-1.5 1.5-3-3L5.25 20.25M10.5 15.75l-3-3 1.5-1.5 3.75.75L16.5 8.25l.75-2.25"/>"#,
        "trash" => r#"<path stroke-linecap="round" stroke-linejoin="round" d="m14.74 9-.346 9m-4.788 0L9.26 9m9.968-3.21c.342.052.682.107 1.022.166m-1.022-.165L18.16 19.673a2.25 2.25 0 0 1-2.244 2.077H8.084a2.25 2.25 0 0 1-2.244-2.077L4.772 5.79m14.456 0a48.108 48.108 0 0 0-3.478-.397m-12 .562c.34-.059.68-.114 1.022-.165m0 0a48.11 48.11 0 0 1 3.478-.397m7.5 0v-.916c0-1.18-.91-2.164-2.09-2.201a51.964 51.964 0 0 0-3.32 0c-1.18.037-2.09 1.022-2.09 2.201v.916m7.5 0a48.667 48.667 0 0 0-7.5 0"/>"#,
        "file" => r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8l-5-5Zm0 0v5h5"/>"#,
        "commit" => r#"<circle cx="12" cy="12" r="3.25"/><path d="M3 12h5.75M15.25 12H21"/>"#,
        // Four blocks, one set apart: the extensions window (the mockup's icons.js has none).
        "blocks" => r#"<path stroke-linejoin="round" d="M3.75 3.75h6.5v6.5h-6.5zM3.75 13.75h6.5v6.5h-6.5zM13.75 13.75h6.5v6.5h-6.5zM14.5 3h6.5v6.5h-6.5z"/>"#,
        "dot" => r#"<circle cx="12" cy="12" r="5"/><path stroke-linecap="round" d="M12 3v3M12 18v3M3 12h3M18 12h3"/>"#,
        _ => "",
    }
}

/// The mockup's shared stylesheet, where the palette's tokens are declared for both themes.
const TOKENS: &str = include_str!("../../assets/editor/style.css");

/// The value the shared stylesheet declares for the token `name` (e.g. `--text-2`) in the light
/// palette (`:root`) or the dark one (`:root[data-theme=dark]`). An icon is drawn as an image,
/// which cannot read a CSS variable, so its stroke is looked up here and no colour is written in
/// this file.
pub(crate) fn token(name: &str, dark: bool) -> Option<&'static str> {
    let open = if dark { ":root[data-theme=dark]{" } else { ":root{" };
    let mut rest = TOKENS;
    while let Some(at) = rest.find(open) {
        let block = &rest[at + open.len()..];
        let end = block.find('}')?;
        for declaration in block[..end].split(';') {
            if let Some((key, value)) = declaration.split_once(':') {
                if key.trim() == name {
                    return Some(value.trim());
                }
            }
        }
        rest = &block[end..];
    }
    None
}

/// The shape and the stroke token of the icon `icon`, written `<name>` or `<name>.<tone>`. The
/// mockup strokes an icon in `currentColor`, so an icon takes the colour editor.css gives its place:
/// * no tone: `--text-2`, an icon button, a tab or a status item (`.ed-ib`, `.ed-sb`, `.ed-tab`,
///   `.ed-status`);
/// * `muted`: `--muted`, a tree row's icons, a breadcrumb chevron, a pill's chevron and the find
///   pill (`.ed-tn .chev`, `.ed-tn svg.i:not(.chev)`, `.ed-crumbs`, `.ed-pj .chev`, `.ed-find`);
/// * `green`: `--c-green`, the run button (`.ed-ib.run`);
/// * `text`: `--text`, a selected stripe button, the selected terminal tab, a menu item and a
///   project pill (`.ed-sb.on`, `.ed-tsub>button.on`, `.ed-menu button`, `.ed-pj`);
/// * `<name>-w`: `--ink-text`, the icon on the ink pill of the active tab (`.ed-tab.on`).
pub(crate) fn icon_stroke(icon: &str) -> (&str, &'static str) {
    let (name, tone) = icon.split_once('.').unwrap_or((icon, ""));
    if let Some(shape) = name.strip_suffix("-w") {
        return (shape, "--ink-text");
    }
    let token = match tone {
        "muted" => "--muted",
        "green" => "--c-green",
        "text" => "--text",
        _ => "--text-2",
    };
    (name, token)
}

/// The colour of the token `name` in the palette asked for. A token the dark palette does not
/// redefine keeps its light value, as the cascade gives it.
fn stroke(name: &str, dark: bool) -> &'static str {
    token(name, dark).or_else(|| token(name, false)).unwrap_or("currentColor")
}

/// The SVG document of the icon `name` stroked in `color`.
fn svg(name: &str, color: &str) -> String {
    if name == "chev-dn" {
        // The mockup turns the right chevron a quarter turn for an open node.
        return format!(
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='{color}' stroke-width='1.5'><g transform='rotate(90 12 12)'>{}</g></svg>",
            path("chev").replace('"', "'")
        );
    }
    format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='{color}' stroke-width='1.5'>{}</svg>",
        path(name).replace('"', "'")
    )
}

/// Answers the `src` of an icon `img` (`icon:<name>` or `icon:<name>.<tone>`, with `@dark` after it
/// in the dark palette) with an SVG asset registered with the renderer, one per icon, tone and palette. The HTML path draws images
/// only through a resolver.
#[derive(Default)]
pub struct IconResolver {
    ids: std::collections::HashMap<String, dioxus_compose::html::prelude::AssetId>,
}

impl dioxus_compose::html::prelude::ImageResolver for IconResolver {
    fn resolve(&mut self, url: &str) -> Option<dioxus_compose::html::prelude::AssetId> {
        let name = url.strip_prefix("icon:")?;
        if let Some(id) = self.ids.get(name) {
            return Some(*id);
        }
        let (base, dark) = match name.strip_suffix("@dark") {
            Some(base) => (base, true),
            None => (name, false),
        };
        let doc: String = if base == "mark" {
            include_str!("../../assets/brand/mark-sm.svg").to_string()
        } else {
            let (shape, stroke_token) = icon_stroke(base);
            svg(shape, stroke(stroke_token, dark))
        };
        let bytes: &'static [u8] = Box::leak(doc.into_boxed_str().into_boxed_bytes());
        let id = dioxus_compose::html::prelude::AssetId(compose_rust::asset(compose_rust::schema::AssetKind::Svg, bytes));
        self.ids.insert(name.to_string(), id);
        Some(id)
    }

    fn size(&mut self, url: &str) -> Option<dioxus_compose::html::prelude::ImageSize> {
        url.starts_with("icon:").then(|| dioxus_compose::html::prelude::ImageSize::new(16, 16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-L31.31 (#895, design review E6): the run button's icon takes `c-green`, the colour the
    /// mockup's `.ed-ib.run` rule gives it, in both palettes; the stop button beside it does not.
    #[test]
    fn fr_l31_31_the_run_icon_is_c_green() {
        assert!(include_str!("../../assets/editor/editor.css").contains(".ed-ib.run{color:var(--c-green)}"));
        assert_eq!(icon_stroke("play.green"), ("play", "--c-green"));
        assert_eq!(icon_stroke("pause"), ("pause", "--text-2"));
        for dark in [false, true] {
            assert!(token("--c-green", dark).is_some_and(|green| Some(green) != token("--text-2", dark)));
        }
        let page = include_str!("editor_workbench.rs");
        let run = &page[page.find(r#"button { class: "ed-ib run", title: "실행", "aria-label": "실행","#).expect("the run button")..];
        let run = &run[..run.find('\n').unwrap()];
        assert!(run.ends_with(r#"{icon_tone("play", 16, "green")} }"#), "{run}");
    }

    // #477: the icon strokes are the shared tokens of the stylesheet the page loads, in both palettes.
    #[test]
    fn fr_l31_icon_strokes_are_read_from_the_shared_tokens() {
        assert_eq!(token("--text-2", false), Some("#56534d"));
        assert_eq!(token("--text-2", true), Some("#c3bfb7"));
        assert_eq!(token("--ink-text", false), Some("#ffffff"));
        assert_eq!(token("--ink-text", true), Some("#151514"));
        // A name is matched whole: `--text` is not `--text-2`, and `--c-green` is not `--c-green-soft`.
        assert_eq!(token("--text", false), Some("#1c1b19"));
        assert_eq!(token("--c-green", true), Some("#6fb287"));
        assert_eq!(token("--no-such-token", false), None);
        // A token only the light palette declares keeps its value in the dark one.
        assert_eq!(token("--pill", true), None);
        assert_eq!(stroke("--pill", true), "999px");
    }

    #[test]
    fn fr_l31_an_icon_is_text_2_and_ink_text_on_the_active_pill() {
        assert_eq!(icon_stroke("file"), ("file", "--text-2"));
        assert_eq!(icon_stroke("chev-dn"), ("chev-dn", "--text-2"));
        assert_eq!(icon_stroke("file-w"), ("file", "--ink-text"));
        assert!(svg("file", stroke("--text-2", false)).contains("stroke='#56534d'"));
        assert!(svg("file", stroke("--ink-text", true)).contains("stroke='#151514'"));
        assert!(svg("chev-dn", stroke("--text-2", true)).contains("rotate(90 12 12)"));
    }

    // #477: an icon takes the colour editor.css gives its place, since an image has no currentColor.
    #[test]
    fn fr_l31_an_icon_tone_is_the_token_editor_css_gives_its_place() {
        assert_eq!(icon_stroke("folder.muted"), ("folder", "--muted"));
        assert_eq!(icon_stroke("chev-dn.muted"), ("chev-dn", "--muted"));
        assert_eq!(icon_stroke("play.green"), ("play", "--c-green"));
        assert_eq!(icon_stroke("folder.text"), ("folder", "--text"));
        assert_eq!(icon_stroke("file-w.muted"), ("file", "--ink-text"));
        assert_eq!(icon_stroke("file.unknown"), ("file", "--text-2"));
        // The rules the tones stand for are the mockup's.
        let css = include_str!("../../assets/editor/editor.css");
        for rule in [
            ".ed-ib.run{color:var(--c-green)}",
            ".ed-pj .chev{transform:rotate(90deg);width:12px!important;height:12px!important;color:var(--muted)}",
            ".ed-tn svg.i:not(.chev){color:var(--muted);width:15px;height:15px}",
            ".ed-sb.on{background:var(--sel);color:var(--text)}",
            ".ed-tsub>button.on{background:var(--surface-2);color:var(--text)}",
        ] {
            assert!(css.contains(rule), "editor.css no longer has {rule}");
        }
        assert!(svg("play", stroke("--c-green", false)).contains("stroke='#4f9a6c'"));
        assert!(svg("play", stroke("--c-green", true)).contains("stroke='#6fb287'"));
        assert!(svg("folder", stroke("--muted", false)).contains("stroke='#8f8b83'"));
        assert!(svg("folder", stroke("--muted", true)).contains("stroke='#8b877f'"));
    }

    // #477: the page asks for the tone at each place the mockup colours.
    #[test]
    fn fr_l31_the_page_asks_for_the_mockup_tone_of_each_icon() {
        let page = include_str!("editor_workbench.rs");
        for call in [
            // The top bar: pill chevrons, the find pill and the run button.
            "\"{name}\" {icon_tone(\"chev-dn\", 12, \"muted\")}",
            "{icon_tone(\"git\", 16, \"text\")} \"{branch}\" {icon_tone(\"chev-dn\", 12, \"muted\")}",
            "{icon_tone(\"play\", 16, \"text\")} \"실행 구성\" {icon_tone(\"chev-dn\", 12, \"muted\")}",
            "{icon_tone(\"search\", 16, \"muted\")}\n                    span { class: \"hide-md\"",
            "{icon_tone(\"play\", 16, \"green\")} }",
            // The stripes and the terminal tabs: the selected one is in the text colour.
            "{icon_tone(icon_name(w), 16, if selected { \"text\" } else { \"\" })}",
            "{icon_tone(\"code\", 14, if on { \"text\" } else { \"\" })}",
            // The tree rows and the breadcrumbs.
            "{icon_tone(\"chev-dn\", 12, \"muted\")} {icon_tone(\"folder\", 15, \"muted\")} span { class: \"ellipsis\", b { \"{root_name}\" } }",
            "if is_dir { if row.open { {icon_tone(\"chev-dn\", 12, \"muted\")} } else { {icon_tone(\"chev\", 12, \"muted\")} } }",
            "if is_dir { {icon_tone(\"folder\", 15, \"muted\")} } else { {icon_tone(\"file\", 15, \"muted\")} }",
            "{icon_tone(icon_name, 15, \"muted\")}",
            "if i > 0 { {icon_tone(\"chev\", 11, \"muted\")} }",
            // The checked item of a menu.
            "span { class: \"ed-mchk\", if selected { {icon_tone(\"check\", 14, \"text\")} } }",
        ] {
            assert!(page.contains(call), "the page lost {call}");
        }
        assert!(include_str!("editor_language.rs").contains("{icon_tone(\"chev\", 11, \"muted\")}"), "the symbol crumbs' chevrons are not muted");
        assert!(include_str!("editor_states.rs").contains("{icon_tone(\"check\", 14, \"text\")}"), "the 자동 저장 submenu's check is not in the text colour");
        assert!(include_str!("editor_tasks.rs").contains("{icon_tone(\"play\", 14, \"text\")}"), "the running task's tab icon is not in the text colour");
        // The rows of the other tool windows are `.ed-tn` rows too, so the same rule colours them.
        assert!(include_str!("editor_search.rs").contains("{icon_tone(\"file\", 15, \"muted\")}"), "a search result's file icon is not muted");
        assert!(include_str!("editor_scm.rs").contains("{icon_tone(\"file\", 15, \"muted\")}"), "a change row's file icon is not muted");
        assert!(include_str!("editor_extensions.rs").contains("{icon_tone(\"chev-dn\", 12, \"muted\")}"), "the built-in group's chevron is not muted");
        // Every `.ed-find` pill has its icon on the line after it, and `.ed-find` is muted.
        let pills = [page, include_str!("editor_extensions.rs"), include_str!("editor_settings.rs"), include_str!("editor_language.rs")];
        for src in pills {
            let lines: Vec<&str> = src.lines().collect();
            for (at, line) in lines.iter().enumerate() {
                let pill = line.contains("class: \"ed-find\"") && line.trim_start().starts_with("div {");
                let next = lines.get(at + 1).map_or("", |l| l.trim_start());
                if pill && next.starts_with("{icon") {
                    assert!(next.starts_with("{icon_tone(") && next.ends_with("\"muted\")}"), "a find pill's icon is not muted: {next}");
                }
            }
        }
    }

    // #477: no colour is written in the icon code; the palette lives in the stylesheet alone.
    #[test]
    fn fr_l31_the_icon_code_holds_no_colour_of_its_own() {
        let src = include_str!("editor_icons.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for line in code.lines() {
            let hex = line.match_indices('#').any(|(at, _)| {
                let after = &line[at + 1..];
                let digits = after.chars().take_while(|c| c.is_ascii_hexdigit()).count();
                matches!(digits, 3 | 6 | 8) && after[digits..].starts_with(['"', '\''])
            });
            assert!(!hex, "a hard-coded colour in the icon code: {line}");
        }
    }
}
