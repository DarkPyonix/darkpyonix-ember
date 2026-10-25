//! State classes for the workbench chrome (FR-L14, INTENT D23, #143).
//!
//! The renderer's stylo does not parse `:has()`, so `ember/app/assets/code-oss/workbench-has.patch`
//! turns each `:has()` rule into a class on the element that owned the rule. These functions
//! decide which classes an element carries. They are plain, so each class has a unit test without
//! rendering. The rsx that calls them needs HTML elements (`div`, `class`) in dioxus-compose,
//! which its FR-34 lists as Draft; until then the CSS is only embedded here.

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
    join("monaco-list-row", &[(has_actions, "has-actions"), (has_decoration, "has-decoration")])
}

/// `.viewpane-filter-container`: `is-focused` while the input or a filter control has focus.
pub fn view_filter_classes(focused: bool) -> String {
    join("action-item viewpane-filter-container", &[(focused, "is-focused")])
}

/// `.action-item.command-center-center`: `compact-mode` when the command center is compact.
pub fn command_center_classes(compact: bool) -> String {
    join("action-item command-center-center", &[(compact, "compact-mode")])
}

/// `.action-widget`: `has-header` when the widget draws a header banner.
pub fn action_widget_classes(has_header: bool) -> String {
    join("action-widget", &[(has_header, "has-header")])
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
