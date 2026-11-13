//! FR-L31.23 (#799): the terminal tab strip's + button is there in every state, outside the
//! scrolling tab list, as Code-OSS keeps its new-terminal action beside the tabs.

use std::cell::RefCell;

use dioxus_compose::html::prelude::*;
use dioxus_compose::html::{HtmlDom, NodeId};

use crate::ui::editor_terminals::{Load, StripView};

thread_local! {
    static VIEW: RefCell<Option<StripView>> = const { RefCell::new(None) };
}

fn app() -> Element {
    super::page::strip_of(VIEW.with(|v| v.borrow().clone()), || {})
}

fn attr(dom: &HtmlDom, node: NodeId, name: &str) -> Option<String> {
    dom.document().get_node(node)?.attrs()?.iter().find(|a| &*a.name.local == name).map(|a| a.value.clone())
}

/// The connected nodes whose `name` attribute is `value` (templates are detached subtrees).
fn find(dom: &HtmlDom, name: &str, value: &str) -> Vec<NodeId> {
    let doc = dom.document();
    let root = doc.root_element().id;
    let ids: Vec<NodeId> = doc.tree().iter().map(|(id, _)| id).collect();
    ids.into_iter()
        .filter(|&id| attr(dom, id, name).as_deref() == Some(value))
        .filter(|&id| {
            let mut at = Some(id);
            while let Some(n) = at {
                if n == root {
                    return true;
                }
                at = doc.get_node(n).and_then(|n| n.parent);
            }
            false
        })
        .collect()
}

fn render(view: Option<StripView>) -> HtmlDom {
    VIEW.with(|v| *v.borrow_mut() = view);
    let mut dom = HtmlDom::new(app);
    dom.render();
    dom
}

fn view(n: usize, renaming: bool) -> StripView {
    let tabs: Vec<(String, String)> = (0..n).map(|i| (format!("t{i}"), "zsh".to_string())).collect();
    StripView {
        active: tabs.first().map(|t| t.0.clone()),
        renaming: if renaming { tabs.first().map(|t| (t.0.clone(), t.1.clone())) } else { None },
        tabs,
        load: Load::Ready,
        error: None,
    }
}

#[test]
fn fr_l31_23_plus_is_shown_in_every_strip_state() {
    for (label, v) in [
        ("loading", None),
        ("one tab", Some(view(1, false))),
        ("two tabs", Some(view(2, false))),
        ("renaming", Some(view(2, true))),
    ] {
        let dom = render(v);
        let plus = find(&dom, "aria-label", "새 터미널");
        assert_eq!(plus.len(), 1, "{label}: one + button");
        let list = find(&dom, "role", "tablist");
        assert_eq!(list.len(), 1, "{label}: one tab list");
        let parent = dom.document().get_node(plus[0]).and_then(|n| n.parent);
        assert_ne!(parent, Some(list[0]), "{label}: + sits beside the scrolling list, not in it");
    }
}

#[test]
fn fr_l31_23_plus_never_scrolls_with_the_tabs() {
    let css = include_str!("../../assets/editor/editor.css");
    assert!(css.contains(".ed-tstrip>.ed-tnew{flex:0 0 auto"), "the + keeps its size");
    assert!(css.contains(".ed-tstrip>.ed-tsub{flex:1 1 auto;min-width:0}"), "the tab list takes the room and scrolls");
    assert!(
        css.contains(".ed-tstrip>.ed-tsub>button,.ed-tstrip>.ed-tsub>input{flex:0 0 auto;max-width:240px}"),
        "tabs keep their natural width (label, pencil, trash) instead of shrinking to an icon"
    );
}
