//! State classes for the workbench chrome (FR-L14, INTENT D23, #143).
//!
//! The renderer's stylo does not parse `:has()`, so `ember/app/assets/code-oss/workbench-has.patch`
//! turns each `:has()` rule into a class on the element that owned the rule. These functions
//! decide which classes an element carries. They are plain, so each class has a unit test without
//! rendering. [`chrome`] is the HTML page that calls them (dioxus-compose FR-34), and
//! [`view::WorkbenchChrome`] places it in a Compose tree with the vendored stylesheets.

/// Vendored Code-OSS CSS, with Ember's `:has()` rewrites applied.
pub const VIEWS_CSS: &str = include_str!("../../assets/code-oss/views.css");
pub const TITLEBAR_CSS: &str = include_str!("../../assets/code-oss/titlebarpart.css");
pub const ACTION_WIDGET_CSS: &str = include_str!("../../assets/code-oss/actionWidget.css");

fn join(base: &str, extra: &[(bool, &str)]) -> String {
    let mut out = String::from(base);
    for (on, class) in extra {
        if *on {
            out.push(' ');
            out.push_str(class);
        }
    }
    out
}

/// `.monaco-list-row` in a custom view tree: `has-actions` when the row draws an action item,
/// `has-decoration` when its label carries a color or badge decoration.
pub fn tree_row_classes(has_actions: bool, has_decoration: bool) -> String {
    join(
        "monaco-list-row",
        &[
            (has_actions, "has-actions"),
            (has_decoration, "has-decoration"),
        ],
    )
}

/// `.viewpane-filter-container`: `is-focused` while the input or a filter control has focus.
pub fn view_filter_classes(focused: bool) -> String {
    join(
        "action-item viewpane-filter-container",
        &[(focused, "is-focused")],
    )
}

/// `.action-item.command-center-center`: `compact-mode` when the command center is compact.
pub fn command_center_classes(compact: bool) -> String {
    join(
        "action-item command-center-center",
        &[(compact, "compact-mode")],
    )
}

/// `.action-widget`: `has-header` when the widget draws a header banner.
pub fn action_widget_classes(has_header: bool) -> String {
    join("action-widget", &[(has_header, "has-header")])
}

use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, WritableExt};
use dioxus_compose::html::prelude::*;

/// The three vendored stylesheets, in the order they are applied.
pub fn stylesheets() -> Vec<String> {
    vec![
        VIEWS_CSS.to_string(),
        TITLEBAR_CSS.to_string(),
        ACTION_WIDGET_CSS.to_string(),
    ]
}

fn tree_row(
    id: &'static str,
    label: &'static str,
    has_actions: bool,
    has_decoration: bool,
) -> Element {
    rsx! {
        div { id: "{id}", class: tree_row_classes(has_actions, has_decoration), "{label}" }
    }
}

fn widget(id: &'static str, has_header: bool) -> Element {
    rsx! {
        div { id: "{id}", class: action_widget_classes(has_header),
            if has_header {
                div { class: "action-list-header", "Header" }
            }
        }
    }
}

/// The workbench chrome page: custom-view tree rows, the view filter, the title bar command
/// center and action widgets. Clicking the filter or the command center toggles `is-focused`
/// and `compact-mode`, standing in for the focus and width events the real chrome will feed.
pub fn chrome() -> Element {
    let mut focused = dioxus_compose::prelude::use_signal(|| false);
    let mut compact = dioxus_compose::prelude::use_signal(|| false);
    let filter_class = view_filter_classes(focused.cloned());
    let center_class = command_center_classes(compact.cloned());
    rsx! {
        div { class: "monaco-workbench",
            div { class: "customview-tree",
                {tree_row("row-plain", "plain", false, false)}
                {tree_row("row-actions", "actions", true, false)}
                {tree_row("row-decoration", "decoration", false, true)}
                {tree_row("row-both", "both", true, true)}
            }
            div { class: "panel",
                div {
                    id: "view-filter",
                    class: filter_class,
                    onclick: move |_| {
                        let now = *focused.peek();
                        focused.set(!now);
                    },
                    "filter"
                }
            }
            div {
                id: "command-center",
                class: center_class,
                onclick: move |_| {
                    let now = *compact.peek();
                    compact.set(!now);
                },
                "center"
            }
            {widget("widget-header", true)}
            {widget("widget-plain", false)}
        }
    }
}

/// The page's configuration: the vendored stylesheets.
pub fn config() -> HtmlConfig {
    HtmlConfig {
        stylesheets: stylesheets(),
        ..HtmlConfig::default()
    }
}

/// The Compose side: the chrome placed in a widget tree.
pub mod view {
    use dioxus_compose::prelude::*;

    #[component]
    pub fn WorkbenchChrome() -> Element {
        rsx! {
            HtmlView { app: super::chrome, config: Some(super::config as fn() -> dioxus_compose::html::HtmlConfig), fill_max_height: true }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus_compose::html::{HtmlDom, Rgba};

    fn has(s: &str, c: &str) -> bool {
        s.split(' ').any(|x| x == c)
    }

    #[test]
    fn fr_l14_tree_row_has_actions() {
        assert!(has(&tree_row_classes(true, false), "has-actions"));
        assert!(!has(&tree_row_classes(false, true), "has-actions"));
    }

    #[test]
    fn fr_l14_tree_row_has_decoration() {
        assert!(has(&tree_row_classes(false, true), "has-decoration"));
        assert!(!has(&tree_row_classes(true, false), "has-decoration"));
        assert_eq!(tree_row_classes(false, false), "monaco-list-row");
    }

    #[test]
    fn fr_l14_view_filter_is_focused() {
        assert!(has(&view_filter_classes(true), "is-focused"));
        assert!(!has(&view_filter_classes(false), "is-focused"));
    }

    #[test]
    fn fr_l14_command_center_compact_mode() {
        assert!(has(&command_center_classes(true), "compact-mode"));
        assert!(!has(&command_center_classes(false), "compact-mode"));
    }

    #[test]
    fn fr_l14_action_widget_has_header() {
        assert!(has(&action_widget_classes(true), "has-header"));
        assert!(!has(&action_widget_classes(false), "has-header"));
    }

    #[test]
    fn fr_l14_vendored_css_uses_the_classes_and_no_has() {
        assert!(VIEWS_CSS.contains(".monaco-list-row.has-actions"));
        assert!(VIEWS_CSS.contains(".monaco-list-row.has-decoration"));
        assert!(VIEWS_CSS.contains(".viewpane-filter-container.is-focused"));
        assert!(TITLEBAR_CSS.contains(".command-center-center.compact-mode"));
        assert!(ACTION_WIDGET_CSS.contains(".action-widget.has-header"));
        for css in [VIEWS_CSS, TITLEBAR_CSS, ACTION_WIDGET_CSS] {
            assert!(!css.contains(":has("));
        }
    }

    // Render tests. The sheet mirrors the patched selectors with a color per class, so a class
    // that is missing from the element shows up as a missing background.
    const TEST_SHEET: &str = "
        div { display: block; height: 20px; }
        .monaco-list-row.has-actions { background-color: rgb(1, 0, 0); }
        .monaco-list-row.has-decoration { background-color: rgb(0, 1, 0); }
        .viewpane-filter-container.is-focused { background-color: rgb(0, 0, 1); }
        .command-center-center.compact-mode { background-color: rgb(1, 1, 0); }
        .action-widget.has-header { background-color: rgb(0, 1, 1); }
    ";

    fn open() -> HtmlDom {
        let mut dom = HtmlDom::with_config(
            chrome,
            HtmlConfig {
                stylesheets: vec![TEST_SHEET.to_string()],
                ..HtmlConfig::default()
            },
        );
        dom.layout(400.0, 400.0, 1.0);
        dom
    }

    fn background(dom: &HtmlDom, id: &str) -> Option<Rgba> {
        let node = dom.element_by_id(id).unwrap_or_else(|| panic!("no #{id}"));
        dom.display_list()
            .expect("laid out")
            .get(node)
            .unwrap_or_else(|| panic!("no entry for #{id}"))
            .background
    }

    fn click(dom: &mut HtmlDom, id: &str) {
        let node = dom.element_by_id(id).unwrap();
        let r = dom.display_list().unwrap().get(node).unwrap().rect;
        dom.click(r.x + r.width / 2.0, r.y + r.height / 2.0)
            .unwrap_or_else(|| panic!("#{id} not clickable"));
        dom.render();
        dom.layout(400.0, 400.0, 1.0);
    }

    #[test]
    fn fr_l14_render_tree_row_classes() {
        let dom = open();
        assert_eq!(background(&dom, "row-plain"), None);
        assert_eq!(
            background(&dom, "row-actions"),
            Some(Rgba::new(1, 0, 0, 255))
        );
        assert_eq!(
            background(&dom, "row-decoration"),
            Some(Rgba::new(0, 1, 0, 255))
        );
        // Both classes: the later rule wins, so has-decoration shows.
        assert_eq!(background(&dom, "row-both"), Some(Rgba::new(0, 1, 0, 255)));
    }

    #[test]
    fn fr_l14_render_view_filter_is_focused_follows_state() {
        let mut dom = open();
        assert_eq!(background(&dom, "view-filter"), None);
        click(&mut dom, "view-filter");
        assert_eq!(
            background(&dom, "view-filter"),
            Some(Rgba::new(0, 0, 1, 255))
        );
        click(&mut dom, "view-filter");
        assert_eq!(background(&dom, "view-filter"), None);
    }

    #[test]
    fn fr_l14_render_command_center_compact_mode_follows_state() {
        let mut dom = open();
        assert_eq!(background(&dom, "command-center"), None);
        click(&mut dom, "command-center");
        assert_eq!(
            background(&dom, "command-center"),
            Some(Rgba::new(1, 1, 0, 255))
        );
    }

    #[test]
    fn fr_l14_render_action_widget_has_header() {
        let dom = open();
        assert_eq!(
            background(&dom, "widget-header"),
            Some(Rgba::new(0, 1, 1, 255))
        );
        assert_eq!(background(&dom, "widget-plain"), None);
    }
}
