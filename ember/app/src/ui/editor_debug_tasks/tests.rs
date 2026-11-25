use super::*;
use crate::terminal_stream::TerminalStatus;
use dioxus_compose::html::HtmlDom;
use dioxus_core::use_hook;
use std::cell::RefCell;

thread_local! {
    static CHOSEN: RefCell<Vec<TaskChoice>> = const { RefCell::new(Vec::new()) };
}

const TASKS: &str = r#"{
    "version": "2.0.0",
    "tasks": [
        { "label": "build", "type": "shell", "command": "make" },
        { "label": "watch", "type": "shell", "command": "tsc -w", "isBackground": true },
        { "label": "npm: lint", "type": "npm", "script": "lint" },
    ],
}"#;

fn tasks() -> TasksLoad {
    TasksLoad::from_read(Ok(TASKS.to_owned()))
}

// The task is looked up by label in the tasks.json just read; one that is not there gets
// Code-OSS's "Could not find the task" and one this runner cannot run says why.
#[test]
fn fr_l31_42_the_pre_launch_task_is_found_by_label() {
    match find_task(&tasks(), "build") {
        Lookup::Found(t) => assert!(!t.is_background),
        other => panic!("{other:?}"),
    }
    match find_task(&tasks(), "watch") {
        Lookup::Found(t) => assert!(t.is_background, "isBackground is read"),
        other => panic!("{other:?}"),
    }
    assert_eq!(find_task(&tasks(), "nope"), Lookup::Missing("Could not find the task 'nope'.".into()));
    assert_eq!(find_task(&TasksLoad::NoFile, "build"), Lookup::Missing("Could not find the task 'build'.".into()));
    assert!(matches!(find_task(&tasks(), "npm: lint"), Lookup::Missing(why) if why.contains("npm")));
    assert_eq!(find_task(&TasksLoad::Loading, "build"), Lookup::Wait);
    assert!(matches!(find_task(&TasksLoad::Failed("bad json".into()), "build"), Lookup::Missing(why) if why == "bad json"));
}

fn running() -> TerminalState {
    TerminalState { status: TerminalStatus::Connected, ..Default::default() }
}

// How the terminal says the task ended: its exit code; no code after a signal (a cancelled task);
// a terminal that failed for good; a background task still running after Code-OSS's 5 s wait.
#[test]
fn fr_l31_42_the_task_terminal_gives_the_outcome() {
    let short = Duration::from_millis(100);
    assert_eq!(outcome_of("build", &running(), false, Duration::from_secs(60)), None, "a task that runs is waited for");
    let exited = |code, signal| TerminalState { exit: Some((code, signal)), ..running() };
    assert_eq!(outcome_of("build", &exited(Some(0), None), false, short), Some(TaskOutcome::Exited { code: Some(0), errors: 0 }));
    assert_eq!(outcome_of("build", &exited(Some(2), None), false, short), Some(TaskOutcome::Exited { code: Some(2), errors: 0 }));
    assert_eq!(outcome_of("build", &exited(None, Some(15)), false, short), Some(TaskOutcome::Exited { code: None, errors: 0 }));

    let failed = TerminalState { error: Some("refused".into()), status: TerminalStatus::Disconnected, ..Default::default() };
    assert!(matches!(outcome_of("build", &failed, false, short), Some(TaskOutcome::Failed { message }) if message.starts_with("The task 'build' could not run")));
    let retrying = TerminalState { reconnect: Some(crate::terminal_stream::ReconnectLine::Reconnecting), ..failed.clone() };
    assert_eq!(outcome_of("build", &retrying, false, short), None, "an automatic reconnect is waited for");

    assert_eq!(outcome_of("watch", &running(), true, short), None);
    assert!(matches!(
        outcome_of("watch", &running(), true, BACKGROUND_WAIT),
        Some(TaskOutcome::Failed { message }) if message.contains("has not exited and doesn't have a 'problemMatcher' defined")
    ));
    assert_eq!(outcome_of("watch", &exited(Some(0), None), true, BACKGROUND_WAIT), Some(TaskOutcome::Exited { code: Some(0), errors: 0 }));
}

fn sheet() -> Element {
    let prompt = use_hook(|| TaskPrompt {
        start: 3,
        message: "The preLaunchTask 'build' terminated with exit code 2.".into(),
        choices: vec![TaskChoice::DebugAnyway, TaskChoice::ShowErrors, TaskChoice::Abort],
        error: false,
    });
    rsx! { TaskPromptSheet { prompt, choose: move |c| CHOSEN.with_borrow_mut(|v| v.push(c)) } }
}

fn click(dom: &mut HtmlDom, id: &str) {
    dom.layout(1000.0, 800.0, 1.0);
    let node = dom.element_by_id(id).unwrap_or_else(|| panic!("no #{id}"));
    dom.click_node(node);
    dom.render();
}

// Code-OSS's dialog: the message, Debug Anyway first, Show Errors, and Abort as the cancel button.
#[test]
fn fr_l31_42_the_task_question_offers_code_oss_buttons() {
    let mut dom = HtmlDom::new(sheet);
    dom.layout(1000.0, 800.0, 1.0);
    assert!(dom.element_by_id("debug-task-prompt").is_some());
    click(&mut dom, "debug-task-choice-0");
    click(&mut dom, "debug-task-choice-1");
    click(&mut dom, "debug-task-choice-2");
    assert_eq!(
        CHOSEN.with_borrow_mut(std::mem::take),
        [TaskChoice::DebugAnyway, TaskChoice::ShowErrors, TaskChoice::Abort]
    );
    assert_eq!(
        [TaskChoice::DebugAnyway, TaskChoice::ShowErrors, TaskChoice::ConfigureTask, TaskChoice::Abort, TaskChoice::Cancel].map(choice_label),
        ["그래도 디버그", "오류 표시", "작업 구성", "중단", "취소"]
    );
}
