//! Workspace trust in the Ember 에디터 window (FR-L31.18, #450).
//!
//! Code-OSS 1.139 (`workspace.contribution.ts`, `workspaceTrust.ts` at the pinned commit) asks on
//! the first open of an untrusted folder whether the user trusts its authors, remembers the answer
//! per folder, and runs an untrusted folder in restricted mode with a banner. Ember does the same:
//! the answer is kept per device in the prefs file (`Prefs::workspace_trust`, beside the keymap),
//! keyed by the computer and the folder, and a trusted folder also trusts the folders below it (the
//! longest remembered folder that contains the opened one decides, as `doGetUriTrustInfo` does).
//!
//! What trust protects and what it does not is `docs/INTENT.md` D45: it keeps extensions that have
//! not declared untrusted-workspace support from running code from the folder, and blocks tasks
//! and debugging. It does not limit agents or terminals, which run with the full authority of the
//! computer (D37) whatever the answer.

use std::collections::BTreeMap;

/// The prefs key of a folder on a computer. An empty computer id is the main server itself.
pub fn trust_key(computer: &str, folder: &str) -> String {
    format!("{computer}\n{}", normalize(folder))
}

fn normalize(folder: &str) -> &str {
    let t = folder.trim_end_matches('/');
    if t.is_empty() { "/" } else { t }
}

fn contains(parent: &str, child: &str) -> bool {
    parent == child || parent == "/" || child.strip_prefix(parent).is_some_and(|rest| rest.starts_with('/'))
}

/// The remembered answer for `folder`: the entry of the longest remembered folder on the same
/// computer that is `folder` or contains it. `None`: never answered, ask.
pub fn decision(saved: &BTreeMap<String, bool>, computer: &str, folder: &str) -> Option<bool> {
    let folder = normalize(folder);
    saved
        .iter()
        .filter_map(|(k, v)| {
            let (c, f) = k.split_once('\n')?;
            (c == computer && contains(f, folder)).then_some((f.len(), *v))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, v)| v)
}

/// The trust state of an editor window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrustState {
    pub trusted: bool,
    /// The prompt is showing.
    pub prompt: bool,
    /// The banner was closed for this window (Code-OSS `untilDismissed`; it comes back next open).
    pub banner_dismissed: bool,
}

impl TrustState {
    /// The state a window opens with. A folder never answered starts in restricted mode with the
    /// prompt up, as Code-OSS's startup prompt does (the extension host starts restricted and is
    /// granted trust if the user says yes).
    pub fn at_open(saved: Option<bool>) -> Self {
        match saved {
            Some(trusted) => Self { trusted, prompt: false, banner_dismissed: false },
            None => Self { trusted: false, prompt: true, banner_dismissed: false },
        }
    }

    /// The user's answer. Restricted mode after a grant needs a new window (the extension host
    /// cannot unload an extension), so `false` never takes trust back here.
    pub fn answer(&mut self, trusted: bool) {
        self.prompt = false;
        if trusted {
            self.trusted = true;
        }
    }

    /// An extension asked for trust while restricted.
    pub fn requested(&mut self) {
        if !self.trusted {
            self.prompt = true;
        }
    }

    pub fn banner_shown(&self) -> bool {
        !self.trusted && !self.prompt && !self.banner_dismissed
    }
}

/// A feature restricted mode blocks (Code-OSS: tasks and debugging need a trusted workspace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restricted {
    Run,
    Debug,
    Task,
}

impl Restricted {
    fn label(self) -> &'static str {
        match self {
            Restricted::Run => "실행",
            Restricted::Debug => "디버그",
            Restricted::Task => "작업",
        }
    }
}

/// `None` when the feature may run; else the notice to show.
pub fn blocked(state: &TrustState, feature: Restricted) -> Option<String> {
    (!state.trusted).then(|| format!("제한 모드에서는 {}을(를) 사용할 수 없습니다. 이 폴더를 신뢰하면 사용할 수 있습니다.", feature.label()))
}

pub const PROMPT_TITLE: &str = "이 폴더에 있는 파일의 작성자를 신뢰합니까?";
pub const PROMPT_DETAIL: &str = "Ember 에디터의 확장은 이 폴더의 파일을 자동으로 실행할 수 있습니다.";
pub const PROMPT_ADVICE: &str =
    "작성자를 신뢰하지 않으면 제한 모드로 계속하는 것이 좋습니다. 악성 파일일 수 있습니다.";
/// D37: trust does not reach agents and terminals.
pub const PROMPT_SCOPE: &str =
    "신뢰 여부는 확장, 실행과 디버그에만 적용됩니다. 에이전트와 터미널은 이 컴퓨터에서 언제나 모든 권한으로 실행됩니다.";
pub const TRUST_LABEL: &str = "예, 작성자를 신뢰합니다";
pub const TRUST_SUB: &str = "폴더를 신뢰하고 모든 기능을 사용합니다";
pub const RESTRICT_LABEL: &str = "아니요, 작성자를 신뢰하지 않습니다";
pub const RESTRICT_SUB: &str = "제한 모드에서 폴더를 안전하게 둘러봅니다";
pub const BANNER_TEXT: &str = "제한 모드는 안전하게 코드를 둘러보기 위한 모드입니다. 모든 기능을 사용하려면 이 폴더를 신뢰하십시오.";
pub const BANNER_TRUST: &str = "신뢰";
pub const BANNER_CLOSE: &str = "배너 닫기";

pub mod view {
    //! The prompt and the banner. The state lives in the editor host (`HostState::trust`), so the
    //! window's shared state does not grow.

    use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
    use dioxus_compose::html::prelude::*;

    use super::*;
    use crate::editor_host::HostCommand;
    use crate::ui::editor_workbench::Shared;

    fn state(s: &Shared) -> TrustState {
        s.host.state.lock().unwrap().trust
    }

    /// Remembers the answer for this folder on this device, updates the window at once and tells
    /// the session through the host (queued until the session is connected).
    fn answer(s: &Shared, trusted: bool) {
        let key = trust_key(&s.computer, &s.folder);
        crate::services::services().ui.update_prefs(|p| {
            p.workspace_trust.insert(key, trusted);
        });
        s.host.state.lock().unwrap().trust.answer(trusted);
        s.host.send(HostCommand::DecideTrust(trusted));
        let mut revision = s.revision;
        *revision.write() += 1;
    }

    /// `None` when the feature may run; else the notice (see [`super::blocked`]).
    pub fn blocked(s: &Shared, feature: Restricted) -> Option<String> {
        super::blocked(&state(s), feature)
    }

    /// The restricted-mode banner, above the tabs.
    pub fn banner(s: &Shared) -> Element {
        if !state(s).banner_shown() {
            return rsx! {};
        }
        let trust_s = s.clone();
        let close_s = s.clone();
        rsx! {
            div { role: "status", "aria-label": "{BANNER_TEXT}",
                style: "display:flex; align-items:center; gap:var(--sp2); padding:6px var(--sp2); border-radius:8px; background:var(--accent-soft, var(--hover)); color:var(--fg); font-size:12px;",
                span { style: "flex:1; min-width:0;", "{BANNER_TEXT}" }
                button { class: "ed-pj", title: "{BANNER_TRUST}", "aria-label": "{BANNER_TRUST}", onclick: move |_| answer(&trust_s, true), "{BANNER_TRUST}" }
                button { class: "ed-ib", title: "{BANNER_CLOSE}", "aria-label": "{BANNER_CLOSE}",
                    onclick: move |_| {
                        close_s.host.state.lock().unwrap().trust.banner_dismissed = true;
                        let mut revision = close_s.revision;
                        *revision.write() += 1;
                    },
                    {crate::ui::editor_workbench::page::icon("close")}
                }
            }
        }
    }

    /// The trust prompt: a modal with the two choices; there is no close action (Code-OSS
    /// `disableCloseAction`).
    pub fn prompt(s: &Shared) -> Element {
        if !state(s).prompt {
            return rsx! {};
        }
        let yes = s.clone();
        let no = s.clone();
        rsx! {
            div { style: "position:fixed; left:0; top:0; right:0; bottom:0; z-index:49; background:rgba(0,0,0,0.25);" }
            div { class: "ed-menu", role: "dialog", "aria-modal": "true", "aria-label": "{PROMPT_TITLE}",
                style: "position:fixed; left:50%; top:120px; transform:translateX(-50%); width:440px; max-width:calc(100% - 32px); z-index:50; padding:var(--sp3); display:flex; flex-direction:column; gap:var(--sp2);",
                b { "{PROMPT_TITLE}" }
                p { style: "margin:0;", "{PROMPT_DETAIL}" }
                p { class: "muted", style: "margin:0;", "{PROMPT_ADVICE}" }
                p { class: "muted", style: "margin:0;", "{PROMPT_SCOPE}" }
                button { class: "ed-pj", style: "flex-direction:column; align-items:flex-start; height:auto; padding:8px 10px;", "aria-label": "{TRUST_LABEL}",
                    onclick: move |_| answer(&yes, true),
                    span { "{TRUST_LABEL}" }
                    span { class: "muted", "{TRUST_SUB}" }
                }
                button { class: "ed-pj", style: "flex-direction:column; align-items:flex-start; height:auto; padding:8px 10px;", "aria-label": "{RESTRICT_LABEL}",
                    onclick: move |_| answer(&no, false),
                    span { "{RESTRICT_LABEL}" }
                    span { class: "muted", "{RESTRICT_SUB}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_18_answer_is_remembered_per_computer_and_folder() {
        let mut saved = BTreeMap::new();
        assert_eq!(decision(&saved, "mac", "/w/ember"), None);
        saved.insert(trust_key("mac", "/w/ember/"), true);
        assert_eq!(decision(&saved, "mac", "/w/ember"), Some(true));
        assert_eq!(decision(&saved, "pc", "/w/ember"), None, "another computer asks again");
        assert_eq!(decision(&saved, "mac", "/w/ember-other"), None, "a sibling with the same prefix is not inside");
    }

    #[test]
    fn fr_l31_18_trusted_parent_covers_subfolders_longest_wins() {
        let mut saved = BTreeMap::new();
        saved.insert(trust_key("", "/w"), true);
        saved.insert(trust_key("", "/w/vendor"), false);
        assert_eq!(decision(&saved, "", "/w/app"), Some(true));
        assert_eq!(decision(&saved, "", "/w/vendor/lib"), Some(false));
        saved.insert(trust_key("", "/"), true);
        assert_eq!(decision(&saved, "", "/x"), Some(true));
    }

    #[test]
    fn fr_l31_18_first_open_prompts_in_restricted_mode() {
        let s = TrustState::at_open(None);
        assert!(s.prompt && !s.trusted && !s.banner_shown());
        let mut s = TrustState::at_open(Some(false));
        assert!(!s.prompt && s.banner_shown());
        s.banner_dismissed = true;
        assert!(!s.banner_shown());
        let s = TrustState::at_open(Some(true));
        assert!(!s.prompt && s.trusted && !s.banner_shown());
    }

    #[test]
    fn fr_l31_18_answers_and_requests() {
        let mut s = TrustState::at_open(None);
        s.answer(false);
        assert!(!s.prompt && !s.trusted && s.banner_shown());
        s.requested();
        assert!(s.prompt, "an extension's request shows the prompt again");
        s.answer(true);
        assert!(s.trusted && !s.prompt && !s.banner_shown());
        s.requested();
        assert!(!s.prompt, "a trusted folder is not asked");
    }

    #[test]
    fn fr_l31_18_restricted_mode_blocks_run_debug_and_tasks() {
        let restricted = TrustState::at_open(Some(false));
        for f in [Restricted::Run, Restricted::Debug, Restricted::Task] {
            let text = blocked(&restricted, f).unwrap();
            assert!(text.starts_with("제한 모드에서는"));
            assert!(text.ends_with("니다."), "합니다체: {text}");
        }
        assert_eq!(blocked(&TrustState::at_open(Some(true)), Restricted::Run), None);
    }

    #[test]
    fn fr_l31_18_strings_have_no_em_dash() {
        for t in [PROMPT_TITLE, PROMPT_DETAIL, PROMPT_ADVICE, PROMPT_SCOPE, TRUST_LABEL, TRUST_SUB, RESTRICT_LABEL, RESTRICT_SUB, BANNER_TEXT] {
            assert!(!t.contains('\u{2014}'));
        }
    }
}
