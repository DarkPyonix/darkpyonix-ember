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
        "file" => r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8l-5-5Zm0 0v5h5"/>"#,
        "commit" => r#"<circle cx="12" cy="12" r="3.25"/><path d="M3 12h5.75M15.25 12H21"/>"#,
        // Four blocks, one set apart: the extensions window (the mockup's icons.js has none).
        "blocks" => r#"<path stroke-linejoin="round" d="M3.75 3.75h6.5v6.5h-6.5zM3.75 13.75h6.5v6.5h-6.5zM13.75 13.75h6.5v6.5h-6.5zM14.5 3h6.5v6.5h-6.5z"/>"#,
        "dot" => r#"<circle cx="12" cy="12" r="5"/><path stroke-linecap="round" d="M12 3v3M12 18v3M3 12h3M18 12h3"/>"#,
        _ => "",
    }
}

/// The SVG document of an icon stroked in `color` (e.g. `#56534d`).
fn svg(name: &str, color: &str) -> String {
    // `<name>-w` is the icon in white, for a dark pill (the active tab).
    // The ink pill is light in the dark palette, so its icon is the dark ink colour there.
    if let Some(base) = name.strip_suffix("-w") {
        return svg(base, if color == "#c3bfb7" { "#151514" } else { "#ffffff" });
    }
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

/// Answers the `src` of an icon `img` (`icon:<name>`) with an SVG asset registered with the
/// renderer. The HTML path draws images only through a resolver.
// TODO(editor-redesign step 4): one asset per theme colour once the dark palette is switched on.
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
        } else if dark {
            svg(base, "#c3bfb7")
        } else {
            svg(base, "#56534d")
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
