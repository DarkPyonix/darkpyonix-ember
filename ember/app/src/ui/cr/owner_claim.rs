//! The owner claim sheet (FR-S12, #842): the field for the code the server printed on its
//! console, the refusal with 다시 시도, and another claimant's request to allow or refuse. The
//! decisions and texts are in `crate::owner_claim`; the claim itself runs on a worker and writes
//! `SharedUi::owner_claim`. The typed code lives in this sheet's state only until it is sent.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;

use super::components::{AppButton, ButtonKind, EmberField, EmberSheet, FieldOptions, Label, SheetFooter};
use super::theme::*;
use crate::owner_claim::{self as claim, ClaimView, Prompt};
use crate::ui::coexist::shared_ui;

/// Composed once, in the shell. Mounted only while a prompt is open, so every opening starts
/// with an empty field.
pub fn OwnerClaimSheet() {
    let ui = shared_ui();
    let view = ui.owner_claim.get();
    let dismissed = ui.owner_claim_dismissed.get();
    if !claim::sheet_open(&view, dismissed.as_deref()) {
        return;
    }
    if let Some(prompt) = claim::prompt(&view.state) {
        OwnerClaimForm(&view, prompt);
    }
}

/// Hides the current prompt until the next attempt or claim.
fn dismiss(view: &ClaimView) {
    shared_ui().owner_claim_dismissed.set(claim::prompt_key(view));
}

fn OwnerClaimForm(view: &ClaimView, prompt: Prompt) {
    let p = ember_theme().palette;
    let muted = Some(paint(p.muted));
    let code = remember(|| mutable_state_of(String::new()));
    let title = if matches!(prompt, Prompt::Pending { .. }) { claim::PENDING_TITLE } else { claim::SHEET_TITLE };
    let close = {
        let v = view.clone();
        move || dismiss(&v)
    };
    EmberSheet(title, true, close.clone(), None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| match &prompt {
            Prompt::Code { holders, left, checking } => {
                Label(claim::CODE_LINE, Fs::Sm, Weight::Regular, None, Some(4));
                if *holders > 0 {
                    Label(claim::HOLDERS_LINE, Fs::Xs, Weight::Regular, muted, Some(2));
                }
                let (typed, send) = (code.clone(), code.clone());
                EmberField(FieldOptions {
                    fill_max_width: true,
                    placeholder: claim::CODE_PLACEHOLDER.to_string(),
                    text: Some(code.get_untracked()),
                    enabled: Some(!checking),
                    on_value_change: Some(Box::new(move |v| typed.set(v))),
                    on_submit: Some(Box::new(move |v: String| {
                        submit(&v);
                        send.set(String::new());
                    })),
                    ..Default::default()
                });
                if let Some(left) = left {
                    Label(&claim::wrong_code_text(*left), Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(2));
                }
                if *checking {
                    Label(claim::CHECKING_LINE, Fs::Xs, Weight::Regular, muted, Some(1));
                }
            }
            Prompt::Refused { text } => {
                Label(text, Fs::Sm, Weight::Medium, None, Some(4));
            }
            Prompt::Pending { claim: c, more } => {
                Label(claim::PENDING_LINE, Fs::Sm, Weight::Regular, None, Some(4));
                let who = if c.name.is_empty() { c.exe.clone() } else { c.name.clone() };
                Label(&who, Fs::Md, Weight::Semibold, None, Some(1));
                Label(&format!("{} (PID {})", c.exe, c.pid), Fs::Xs, Weight::Regular, muted, Some(2));
                if *more > 0 {
                    Label(&format!("기다리는 요청이 {more}개 더 있습니다."), Fs::Xs, Weight::Regular, muted, Some(1));
                }
            }
        });
        SheetFooter(|| match &prompt {
            Prompt::Code { checking, .. } => {
                AppButton(claim::LATER, None, ButtonKind::Default, true, true, false, close.clone());
                let typed = code.get();
                let send = code.clone();
                AppButton(claim::SUBMIT, None, ButtonKind::Primary, true, !checking && !typed.trim().is_empty(), false, move || {
                    submit(&send.get_untracked());
                    send.set(String::new());
                });
            }
            Prompt::Refused { .. } => {
                AppButton(claim::CLOSE, None, ButtonKind::Default, true, true, false, close.clone());
                AppButton(claim::RETRY, None, ButtonKind::Primary, true, true, false, move || {
                    if let Some(h) = claim::handle() {
                        h.retry();
                    }
                });
            }
            Prompt::Pending { claim: c, .. } => {
                let id = c.claim;
                AppButton(claim::DENY, None, ButtonKind::Destructive, true, true, false, move || {
                    if let Some(h) = claim::handle() {
                        h.deny(id);
                    }
                });
                AppButton(claim::APPROVE, None, ButtonKind::Primary, true, true, false, move || {
                    if let Some(h) = claim::handle() {
                        h.approve(id);
                    }
                });
            }
        });
    });
}

fn submit(code: &str) {
    if let Some(h) = claim::handle() {
        h.submit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner_claim::{ClaimState, PendingClaim};
    use compose_rust::protocol::decode_batch;
    use compose_rust::{Host, Recomposer};

    fn shown(state: ClaimState) -> String {
        shared_ui().owner_claim.set(ClaimView { state, generation: 7 });
        shared_ui().owner_claim_dismissed.set(None);
        let mut host = Host::new(|| Box::new(Recomposer::new(OwnerClaimSheet)));
        let mut text: String = decode_batch(host.rebuild().expect("rebuild")).expect("decode").iter().map(|m| format!("{m:?}")).collect();
        for _ in 0..2 {
            let batch = decode_batch(host.render_frame(16_000_000).expect("render")).expect("decode");
            text.extend(batch.iter().map(|m| format!("{m:?}")));
        }
        text
    }

    /// The sheet asks for the console code, shows a refusal with 다시 시도, and asks about
    /// another claimant; nothing shows while the key is held (FR-S12, #842).
    #[test]
    fn fr_s12_the_claim_sheet_shows_each_prompt() {
        let _guard = crate::services::test_guard();
        let code = shown(ClaimState::NeedCode { holders: 1 });
        assert!(code.contains(claim::CODE_LINE) && code.contains(claim::HOLDERS_LINE), "{code}");
        assert!(code.contains(claim::SUBMIT) && code.contains(claim::LATER), "{code}");
        let wrong = shown(ClaimState::WrongCode { left: 2, holders: 0 });
        assert!(wrong.contains(&claim::wrong_code_text(2)), "{wrong}");
        let refused = shown(ClaimState::Refused { reason: "agent_tree".into() });
        assert!(refused.contains(claim::refused_text("agent_tree")) && refused.contains(claim::RETRY), "{refused}");
        let pending = shown(ClaimState::PendingOther {
            claims: vec![PendingClaim { claim: 4, pid: 77, exe: "/bin/cli".into(), name: "cli".into() }],
        });
        assert!(pending.contains(claim::PENDING_LINE) && pending.contains(claim::APPROVE) && pending.contains(claim::DENY), "{pending}");
        assert!(pending.contains("PID 77"), "{pending}");
        let held = shown(ClaimState::Held);
        assert!(!held.contains(claim::SHEET_TITLE) && !held.contains(claim::PENDING_TITLE), "{held}");
        shared_ui().owner_claim.set(ClaimView::default());
    }
}
