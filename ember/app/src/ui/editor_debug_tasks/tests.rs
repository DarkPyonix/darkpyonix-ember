use super::*;
use crate::terminal_stream::TerminalStatus;
use dioxus_compose::html::HtmlDom;
use dioxus_core::use_hook;
use std::cell::RefCell;

thread_local! {
    static CHOSEN: RefCell<Vec<(TaskChoice, bool)>> = const { RefCell::new(Vec::new()) };
    static WAITED: RefCell<Vec<TaskChoice>> = const { RefCell::new(Vec::new()) };
}

const IDLE: TrackStatus = TrackStatus { began: false, ready: false };

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
    assert_eq!(outcome_of("build", &running(), false, Duration::from_secs(60), IDLE), None, "a task that runs is waited for");
    let exited = |code, signal| TerminalState { exit: Some((code, signal)), ..running() };
    assert_eq!(outcome_of("build", &exited(Some(0), None), false, short, IDLE), Some(TaskOutcome::Exited { code: Some(0), errors: 0 }));
    assert_eq!(outcome_of("build", &exited(Some(2), None), false, short, IDLE), Some(TaskOutcome::Exited { code: Some(2), errors: 0 }));
    assert_eq!(outcome_of("build", &exited(None, Some(15)), false, short, IDLE), Some(TaskOutcome::Exited { code: None, errors: 0 }));

    let failed = TerminalState { error: Some("refused".into()), status: TerminalStatus::Disconnected, ..Default::default() };
    assert!(matches!(outcome_of("build", &failed, false, short, IDLE), Some(TaskOutcome::Failed { message }) if message.starts_with("The task 'build' could not run")));
    let retrying = TerminalState { reconnect: Some(crate::terminal_stream::ReconnectLine::Reconnecting), ..failed.clone() };
    assert_eq!(outcome_of("build", &retrying, false, short, IDLE), None, "an automatic reconnect is waited for");

    assert_eq!(outcome_of("watch", &running(), true, short, IDLE), None);
    assert!(matches!(
        outcome_of("watch", &running(), true, BACKGROUND_WAIT, IDLE),
        Some(TaskOutcome::Failed { message }) if message.contains("has not exited and doesn't have a 'problemMatcher' defined")
    ));
    assert_eq!(outcome_of("watch", &exited(Some(0), None), true, BACKGROUND_WAIT, IDLE), Some(TaskOutcome::Exited { code: Some(0), errors: 0 }));
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

fn error_sheet() -> Element {
    let prompt = use_hook(|| TaskPrompt {
        start: 4,
        message: "Could not find the task 'build'.".into(),
        choices: vec![TaskChoice::DebugAnyway, TaskChoice::ConfigureTask, TaskChoice::Cancel],
        error: true,
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
        [(TaskChoice::DebugAnyway, false), (TaskChoice::ShowErrors, false), (TaskChoice::Abort, false)]
    );
    assert_eq!(
        [TaskChoice::DebugAnyway, TaskChoice::ShowErrors, TaskChoice::ConfigureTask, TaskChoice::Abort, TaskChoice::Cancel].map(choice_label),
        ["그래도 디버그", "오류 표시", "작업 구성", "중단", "취소"]
    );
}

fn status(began: bool, ready: bool) -> TrackStatus {
    TrackStatus { began, ready }
}

// A background task is ready when a pass of its matcher ends; one whose matcher began a pass is
// tracked and gets no untracked-task error after 5 s; one whose matcher never began does.
#[test]
fn fr_l31_42_a_background_task_is_ready_at_its_ends_pattern() {
    let short = Duration::from_millis(100);
    assert_eq!(outcome_of("watch", &running(), true, short, status(true, true)), Some(TaskOutcome::Ready { errors: 0 }));
    assert_eq!(outcome_of("watch", &running(), true, Duration::from_secs(60), status(true, false)), None, "active: waited for");
    assert!(matches!(
        outcome_of("watch", &running(), true, BACKGROUND_WAIT, status(false, false)),
        Some(TaskOutcome::Failed { message }) if message == "The task 'watch' has not exited and doesn't have a 'problemMatcher' defined. Make sure to define a problem matcher for watch tasks."
    ));
    assert_eq!(outcome_of("watch", &running(), true, BACKGROUND_WAIT - short, status(false, false)), None, "not before 5 s");
    // A task that is not background ends with its process, whatever its matcher says.
    assert_eq!(outcome_of("build", &running(), false, short, status(true, true)), None);
}

// The waiting card is due after 10 s unless `debug.hideSlowPreLaunchWarning` is on; Debug Anyway
// is a success, Configure Task and Abort a cancelled task.
#[test]
fn fr_l31_42_the_waiting_card_comes_after_10_s() {
    assert!(!slow_warning_due(Duration::from_secs(9), false));
    assert!(slow_warning_due(SLOW_WAIT, false));
    assert!(!slow_warning_due(Duration::from_secs(60), true));
    assert_eq!(waiting_message("build"), "Waiting for preLaunchTask 'build'...");
    assert_eq!(WAITING_CHOICES, [TaskChoice::DebugAnyway, TaskChoice::ConfigureTask, TaskChoice::Abort]);
    assert_eq!(waiting_outcome(TaskChoice::DebugAnyway), TaskOutcome::Exited { code: Some(0), errors: 0 });
    assert_eq!(waiting_outcome(TaskChoice::Abort), TaskOutcome::Exited { code: None, errors: 0 });
    assert_eq!(waiting_outcome(TaskChoice::ConfigureTask), TaskOutcome::Exited { code: None, errors: 0 });
}

fn waiting() -> Element {
    rsx! { WaitingCard { label: "build".to_string(), choose: move |c| WAITED.with_borrow_mut(|v| v.push(c)) } }
}

#[test]
fn fr_l31_42_the_waiting_card_offers_code_oss_buttons() {
    let mut dom = HtmlDom::new(waiting);
    dom.layout(1000.0, 800.0, 1.0);
    assert!(dom.element_by_id("debug-task-waiting").is_some());
    for i in 0..3 {
        click(&mut dom, &format!("debug-task-waiting-{i}"));
    }
    assert_eq!(WAITED.with_borrow_mut(std::mem::take), WAITING_CHOICES);
}

// "Remember my choice in user settings": the warning's checkbox goes with the button, and the
// answer writes `debug.onTaskErrors` as Code-OSS does; the error dialog's box is "Remember my
// choice for this task".
#[test]
fn fr_l31_42_remember_my_choice_goes_with_the_answer_and_names_the_setting() {
    let mut dom = HtmlDom::new(sheet);
    dom.layout(1000.0, 800.0, 1.0);
    let label = dom.element_by_id("debug-task-remember").and_then(|n| dom.document().get_node(n).map(|node| node.text_content())).unwrap_or_default();
    assert!(label.contains(REMEMBER_LABEL), "{label}");
    click(&mut dom, "debug-task-remember");
    click(&mut dom, "debug-task-choice-2");
    assert_eq!(CHOSEN.with_borrow_mut(std::mem::take), [(TaskChoice::Abort, true)]);

    let mut dom = HtmlDom::new(error_sheet);
    dom.layout(1000.0, 800.0, 1.0);
    let label = dom.element_by_id("debug-task-remember").and_then(|n| dom.document().get_node(n).map(|node| node.text_content())).unwrap_or_default();
    assert!(label.contains(REMEMBER_TASK_LABEL), "{label}");
    click(&mut dom, "debug-task-remember");
    click(&mut dom, "debug-task-choice-1");
    assert_eq!(CHOSEN.with_borrow_mut(std::mem::take), [(TaskChoice::ConfigureTask, true)]);

    assert_eq!(remembered_setting(TaskChoice::DebugAnyway), ("debug.onTaskErrors", "debugAnyway"));
    assert_eq!(remembered_setting(TaskChoice::ShowErrors), ("debug.onTaskErrors", "showErrors"));
    assert_eq!(remembered_setting(TaskChoice::Abort), ("debug.onTaskErrors", "abort"));
}

// "Remember my choice for this task": the error dialog's answers are kept by message as
// `debugTaskRunner`'s `DebugChoice` numbers, and a remembered one answers the same message again.
#[test]
fn fr_l31_42_remember_my_choice_for_this_task_keeps_the_answer_by_message() {
    assert_eq!(task_choice_code(TaskChoice::DebugAnyway), Some(0));
    assert_eq!(task_choice_code(TaskChoice::ConfigureTask), Some(1));
    assert_eq!(task_choice_code(TaskChoice::Cancel), Some(2));
    assert_eq!(task_choice_code(TaskChoice::Abort), None, "the warning's answers go to the setting instead");
    assert_eq!(task_choice_code(TaskChoice::ShowErrors), None);
    let mut kept = BTreeMap::new();
    let message = "Could not find the task 'build'.";
    assert_eq!(remembered_task_choice(&kept, message), None);
    for choice in [TaskChoice::DebugAnyway, TaskChoice::ConfigureTask, TaskChoice::Cancel] {
        kept.insert(message.to_owned(), task_choice_code(choice).unwrap());
        assert_eq!(remembered_task_choice(&kept, message), Some(choice));
    }
    assert_eq!(remembered_task_choice(&kept, "Could not find the task 'other'."), None, "per message");
    kept.insert(message.to_owned(), 9);
    assert_eq!(remembered_task_choice(&kept, message), None, "an unknown number asks again");
}
