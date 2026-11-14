//! FR-P6 and FR-L31.23: the editor's terminal tabs over the node's persistent terminals.
//! Pure model tests: which terminals are listed, reattach on reopen, new, kill, rename, switch.

use ember_app::terminal_list::{
    is_folder_terminal, key_index, tab_key_action, TabKeyAction, kill_url, remove_url, tab_key, TermTab, TerminalTabs, EDITOR_ORIGIN,
};
use ember_app::terminal_stream::{terms_list_url, TermInfo};

const P: &str = "proj";
const F: &str = "/work/app";

fn term(id: &str, key: Option<&str>, created_ms: u64) -> TermInfo {
    TermInfo {
        id: id.into(),
        origin: EDITOR_ORIGIN.into(),
        state: "running".into(),
        key: key.map(str::to_owned),
        created_ms,
        ..Default::default()
    }
}

fn ids(tabs: &TerminalTabs) -> Vec<&str> {
    tabs.tabs().iter().map(|t| t.id.as_str()).collect()
}

#[test]
fn fr_p6_tab_keys_extend_the_session_key() {
    assert_eq!(tab_key(P, F, 0), "proj:/work/app");
    assert_eq!(tab_key(P, F, 3), "proj:/work/app#3");
    assert_eq!(key_index("proj:/work/app", P, F), Some(0));
    assert_eq!(key_index("proj:/work/app#2", P, F), Some(2));
    assert_eq!(key_index("proj:/work/app#0", P, F), None);
    assert_eq!(key_index("proj:/work/app2", P, F), None, "a sibling folder is not this folder");
    assert_eq!(key_index("proj:/work/app#x", P, F), None);
    assert_eq!(key_index("other:/work/app", P, F), None);
}

#[test]
fn fr_p6_only_running_editor_terminals_of_the_folder_are_listed() {
    let mine = term("a", Some("proj:/work/app"), 1);
    assert!(is_folder_terminal(&mine, P, F));
    let mut agent = mine.clone();
    agent.origin = "agent".into();
    assert!(!is_folder_terminal(&agent, P, F));
    let mut exited = mine.clone();
    exited.state = "exited".into();
    assert!(!is_folder_terminal(&exited, P, F));
    let other_folder = term("b", Some("proj:/work/lib"), 1);
    assert!(!is_folder_terminal(&other_folder, P, F));
    let no_key = term("c", None, 1);
    assert!(!is_folder_terminal(&no_key, P, F));
}

#[test]
fn fr_p6_reopen_reattaches_to_surviving_terminals_oldest_first() {
    let list = vec![
        term("t3", Some("proj:/work/app#2"), 30),
        term("t1", Some("proj:/work/app"), 10),
        term("agent", Some("proj:/work/app#9"), 5),
        term("t2", Some("proj:/work/app#1"), 20),
    ];
    let mut list = list;
    list[2].origin = "agent".into();
    let tabs = TerminalTabs::from_list(&list, P, F);
    assert_eq!(ids(&tabs), ["t1", "t2", "t3"]);
    assert_eq!(tabs.active_id(), Some("t1"));
}

#[test]
fn fr_p6_reopen_with_nothing_kept_has_no_tabs() {
    let tabs = TerminalTabs::from_list(&[], P, F);
    assert!(tabs.is_empty());
    assert_eq!(tabs.active_id(), None);
    assert_eq!(tabs.next_key(P, F), tab_key(P, F, 0), "the first terminal keeps the plain key");
}

#[test]
fn fr_p6_new_terminal_is_active_and_takes_the_smallest_free_key() {
    let mut tabs = TerminalTabs::from_list(&[term("t1", Some("proj:/work/app"), 1), term("t3", Some("proj:/work/app#2"), 3)], P, F);
    assert_eq!(tabs.next_key(P, F), "proj:/work/app#1");
    tabs.push(TermTab::from_info(&term("t2", Some("proj:/work/app#1"), 4)));
    assert_eq!(tabs.active_id(), Some("t2"));
    assert_eq!(tabs.next_key(P, F), "proj:/work/app#3");
    // The node re-adopts by key: the same id comes back and is only activated.
    tabs.push(TermTab::from_info(&term("t1", Some("proj:/work/app"), 1)));
    assert_eq!(tabs.len(), 3);
    assert_eq!(tabs.active_id(), Some("t1"));
}

#[test]
fn fr_p6_kill_moves_the_active_tab_like_code_oss() {
    let list = [term("a", Some("proj:/work/app"), 1), term("b", Some("proj:/work/app#1"), 2), term("c", Some("proj:/work/app#2"), 3)];
    let mut tabs = TerminalTabs::from_list(&list, P, F);
    tabs.select("b");
    assert_eq!(tabs.remove("b").map(|t| t.id), Some("b".into()));
    assert_eq!(tabs.active_id(), Some("c"), "the tab now at its place");
    tabs.remove("c");
    assert_eq!(tabs.active_id(), Some("a"), "else the last");
    tabs.remove("a");
    assert_eq!(tabs.active_id(), None);
    assert!(tabs.remove("a").is_none());
}

#[test]
fn fr_p6_kill_of_another_tab_keeps_the_active_one() {
    let list = [term("a", Some("proj:/work/app"), 1), term("b", Some("proj:/work/app#1"), 2), term("c", Some("proj:/work/app#2"), 3)];
    let mut tabs = TerminalTabs::from_list(&list, P, F);
    tabs.select("c");
    tabs.remove("a");
    assert_eq!(tabs.active_id(), Some("c"));
    assert_eq!(ids(&tabs), ["b", "c"]);
}

#[test]
fn fr_p6_switch_and_rename() {
    let mut a = term("a", Some("proj:/work/app"), 1);
    a.title = "zsh".into();
    let b = term("b", Some("proj:/work/app#1"), 2);
    let mut tabs = TerminalTabs::from_list(&[a, b], P, F);
    assert!(tabs.select("b"));
    assert!(!tabs.select("nope"));
    assert_eq!(tabs.active_id(), Some("b"));
    assert_eq!(tabs.label("a").as_deref(), Some("zsh"));
    assert_eq!(tabs.label("b").as_deref(), Some("터미널 2"), "no title: numbered");
    assert!(tabs.rename("a", "  build  "));
    assert_eq!(tabs.label("a").as_deref(), Some("build"));
    tabs.rename("a", "   ");
    assert_eq!(tabs.label("a").as_deref(), Some("zsh"), "a blank name goes back to the title");
    assert!(!tabs.rename("nope", "x"));
}

#[test]
fn fr_p6_sync_drops_ended_terminals_and_keeps_names() {
    let mut tabs = TerminalTabs::from_list(&[term("a", Some("proj:/work/app"), 1), term("b", Some("proj:/work/app#1"), 2)], P, F);
    tabs.rename("b", "server");
    tabs.select("a");
    let mut b = term("b", Some("proj:/work/app#1"), 2);
    b.title = "node".into();
    let mut ended = term("a", Some("proj:/work/app"), 1);
    ended.state = "exited".into();
    let c = term("c", Some("proj:/work/app#2"), 3);
    tabs.sync(&[ended, b, c], P, F);
    assert_eq!(ids(&tabs), ["b", "c"]);
    assert_eq!(tabs.active_id(), Some("b"));
    assert_eq!(tabs.label("b").as_deref(), Some("server"));
}

#[test]
fn fr_p6_relay_urls_for_list_kill_and_remove() {
    assert_eq!(
        terms_list_url("http://h:1/", "pc 1", P, Some(EDITOR_ORIGIN), true),
        "http://h:1/api/computers/pc%201/terms?project=proj&origin=ide-ember&running=true"
    );
    assert_eq!(kill_url("http://h:1", "pc", "t/1"), "http://h:1/api/computers/pc/terms/t%2F1/kill");
    assert_eq!(remove_url("http://h:1", "pc", "t1"), "http://h:1/api/computers/pc/terms/t1");
}

#[test]
fn fr_p6_list_decodes_the_node_fields_the_tabs_need() {
    let json = r#"[{"id":"t1","key":"proj:/work/app","title":"zsh","origin":"ide-ember","project":"proj",
        "argv":["zsh"],"cwd":"/work/app","pid":7,"state":"running","size":{"rows":24,"cols":80},
        "created_ms":5,"last_activity_ms":6,"tags":{},"clients":[],"controller":null}]"#;
    let list: Vec<TermInfo> = serde_json::from_str(json).unwrap();
    let tabs = TerminalTabs::from_list(&list, P, F);
    assert_eq!(ids(&tabs), ["t1"]);
    assert_eq!(list[0].created_ms, 5);
}

#[test]
fn fr_p6_f2_on_a_focused_tab_renames() {
    assert_eq!(tab_key_action("F2", false, false, false, false), Some(TabKeyAction::Rename));
    assert_eq!(tab_key_action("F2", true, false, false, false), None, "modified F2 is not the rename key");
    assert_eq!(tab_key_action("F2", false, false, true, false), None);
    assert_eq!(tab_key_action("Enter", false, false, false, false), None);
    assert_eq!(tab_key_action("F3", false, false, false, false), None);
    // The rename that F2 starts goes through the same model call.
    let mut tabs = TerminalTabs::from_list(&[term("a", Some("proj:/work/app"), 1)], P, F);
    assert!(tabs.rename("a", "logs"));
    assert_eq!(tabs.label("a").as_deref(), Some("logs"));
}
