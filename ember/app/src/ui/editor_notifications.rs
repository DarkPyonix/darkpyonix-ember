//! FR-L31.16, #448: extension notifications in the Ember 에디터 window, drawn from
//! [`ember_editor::notifications::Notifications`] (Code-OSS decides the behaviour; the mockup has
//! no notification screen, so the cards follow the island tool window's tokens).

use ember_editor::MessageSeverity;

/// The dot class of a severity (the problems list's dots).
pub fn severity_class(severity: MessageSeverity) -> &'static str {
    match severity {
        MessageSeverity::Error => "ed-pdot err",
        MessageSeverity::Warning => "ed-pdot warn",
        MessageSeverity::Info => "ed-pdot",
    }
}

/// The overlay's distance from the viewport bottom: 12px above the status bar. The page is the
/// viewport on every system (`editor_page_size_style`), so the status bar is at the viewport's
/// bottom edge and nothing else is added.
pub fn overlay_bottom() -> &'static str {
    "calc(var(--sbh) + 12px)"
}

/// Whether the toasts and the notification center are drawn (#802). While a modal message waits
/// they are hidden, so nothing sits undimmed above its scrim, as Code-OSS keeps notifications under
/// a modal dialog; they come back when it closes. The renderer does not order fixed layers by
/// `z-index` yet, so hiding is the reliable way to keep them under it.
pub fn overlay_shown(modal_waiting: bool) -> bool {
    !modal_waiting
}

/// The accessible name of a notification card.
pub fn card_label(severity: MessageSeverity, source: Option<&str>) -> String {
    match source {
        Some(s) => format!("{} 알림, {s}", severity.label()),
        None => format!("{} 알림", severity.label()),
    }
}

pub mod view {
    use dioxus_compose::html::prelude::dioxus_signals::WritableExt;
    use dioxus_compose::html::prelude::*;
    use ember_editor::notifications::{center_title, Notifications};
    use ember_editor::ExtensionMessage;

    use super::{card_label, overlay_bottom, severity_class};
    use crate::editor_host::HostCommand;
    use crate::ui::editor_workbench::page::icon;
    use crate::ui::editor_workbench::Shared;

    fn bump(s: &Shared) {
        let mut rev = s.revision;
        *rev.write() += 1;
    }

    fn answer(s: &Shared, answer: Option<(u64, Option<i64>)>) {
        if let Some((id, handle)) = answer {
            s.host.send(HostCommand::AnswerMessage { id, handle });
        }
        bump(s);
    }

    /// Open or close the notification center (the right stripe's 알림).
    pub fn toggle_center(s: &Shared) {
        let open = !s.notifications.borrow().center_open();
        s.notifications.borrow_mut().set_center_open(open);
        bump(s);
    }

    /// A message arrived: keep it and wake the window when its toast is due to hide.
    pub fn push(s: &Shared, message: ExtensionMessage) {
        let wait = ember_editor::notifications::purge_timeout_ms(message.severity);
        s.notifications.borrow_mut().push(message, crate::ui::editor_workbench::now_ms());
        let revision = s.revision;
        crate::services::services().rt.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(wait + 50)).await;
            let mut revision = revision;
            *revision.write() += 1;
        });
    }

    fn card(s: &Shared, m: &ExtensionMessage, raised: bool) -> Element {
        let id = m.id;
        let label = card_label(m.severity, m.source.as_deref());
        let dot = severity_class(m.severity);
        let style = if raised {
            "background:var(--surface); border:1px solid var(--line); border-radius:var(--r); box-shadow:var(--shadow-lg); padding:var(--sp3); display:flex; flex-direction:column; gap:var(--sp2);"
        } else {
            "border-bottom:1px solid var(--line); padding:var(--sp3); display:flex; flex-direction:column; gap:var(--sp2);"
        };
        let close_s = s.clone();
        let actions: Vec<(String, i64, bool)> = m.actions.iter().enumerate().map(|(i, a)| (a.title.clone(), a.handle, i == 0)).collect();
        let source = m.source.clone().unwrap_or_default();
        let message = m.message.clone();
        rsx! {
            div { key: "{id}", role: "alert", "aria-label": "{label}", style: "{style}",
                div { style: "display:flex; align-items:flex-start; gap:var(--sp2);",
                    span { class: "{dot}", style: if dot == "ed-pdot" { "background:var(--c-blue); margin-top:6px;" } else { "margin-top:6px;" } }
                    div { style: "flex:1; min-width:0; white-space:pre-wrap; font-size:var(--fs-sm);", "{message}" }
                    button { class: "ed-ib", title: "알림 지우기", "aria-label": "알림 지우기",
                        onclick: move |_| { let a = close_s.notifications.borrow_mut().dismiss(id); answer(&close_s, a); },
                        {icon("close")} }
                }
                if !source.is_empty() || !actions.is_empty() {
                    div { style: "display:flex; align-items:center; gap:var(--sp2);",
                        span { style: "flex:1; min-width:0; color:var(--muted); font-size:var(--fs-xs);", "{source}" }
                        for (title, handle, primary) in actions {
                            {
                                let act_s = s.clone();
                                rsx! {
                                    button { key: "{handle}", class: "ed-pj",
                                        style: if primary { "background:var(--ink); color:var(--ink-text); height:26px;" } else { "border:1px solid var(--line); height:26px;" },
                                        onclick: move |_| { let a = act_s.notifications.borrow_mut().choose(id, handle); answer(&act_s, a); },
                                        "{title}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn modal(s: &Shared, m: &ExtensionMessage) -> Element {
        let id = m.id;
        let buttons = Notifications::modal_buttons(m);
        let detail = m.detail.clone().unwrap_or_default();
        let source = m.source.clone().unwrap_or_default();
        let message = m.message.clone();
        let label = super::card_label(m.severity, m.source.as_deref());
        let count = buttons.len();
        rsx! {
            // The scrim is the shared `.modal` of the stylesheet, as the unsaved-changes sheet and the
            // trust prompt use it, so no colour is written here.
            div { class: "modal", role: "presentation",
                div { role: "alertdialog", "aria-modal": "true", "aria-label": "{label}",
                    style: "width:min(440px, calc(100vw - 32px)); background:var(--surface); border:1px solid var(--line); border-radius:var(--r-lg); box-shadow:var(--shadow-lg); padding:var(--sp4); display:flex; flex-direction:column; gap:var(--sp3);",
                    div { style: "display:flex; align-items:flex-start; gap:var(--sp2);",
                        span { class: "{severity_class(m.severity)}", style: "margin-top:7px;" }
                        b { style: "flex:1; white-space:pre-wrap;", "{message}" }
                    }
                    if !detail.is_empty() { div { style: "color:var(--text-2); white-space:pre-wrap; font-size:var(--fs-sm);", "{detail}" } }
                    if !source.is_empty() { div { style: "color:var(--muted); font-size:var(--fs-xs);", "{source}" } }
                    div { style: "display:flex; justify-content:flex-end; gap:var(--sp2);",
                        for (i, (title, handle)) in buttons.into_iter().enumerate() {
                            {
                                let btn_s = s.clone();
                                let cancel = i + 1 == count;
                                rsx! {
                                    button { key: "{i}", class: "ed-pj",
                                        style: if i == 0 && !cancel { "background:var(--ink); color:var(--ink-text);" } else { "border:1px solid var(--line);" },
                                        onclick: move |_| {
                                            let a = if cancel {
                                                btn_s.notifications.borrow_mut().cancel_modal(id)
                                            } else {
                                                handle.and_then(|h| btn_s.notifications.borrow_mut().choose(id, h))
                                            };
                                            answer(&btn_s, a);
                                        },
                                        "{title}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The toasts, the center when open, and a modal dialog when one waits. Composed once in the page.
    pub fn layer(s: &Shared) -> Element {
        s.notifications.borrow_mut().tick(crate::ui::editor_workbench::now_ms());
        let n = s.notifications.borrow();
        let bottom = overlay_bottom();
        let anchor = format!("position:fixed; z-index:35; right:calc(var(--stw) + 12px); bottom:{bottom}; width:min(420px, calc(100vw - 32px)); display:flex; flex-direction:column; gap:var(--sp2);");
        let toasts: Vec<ExtensionMessage> = n.toasts().into_iter().map(|e| e.message.clone()).collect();
        let center: Option<Vec<ExtensionMessage>> = n.center_open().then(|| n.center().into_iter().map(|e| e.message.clone()).collect());
        let pending_modal = n.modal().cloned();
        drop(n);
        let (toasts, center) = if super::overlay_shown(pending_modal.is_some()) { (toasts, center) } else { (Vec::new(), None) };
        let clear_s = s.clone();
        let hide_s = s.clone();
        rsx! {
            if let Some(rows) = center {
                section { "aria-label": "알림 센터", style: "{anchor} max-height:60vh; background:var(--surface); border:1px solid var(--line); border-radius:var(--rad); box-shadow:var(--shadow-lg); overflow:hidden; gap:0;",
                    div { class: "ed-twh",
                        b { "{center_title(rows.len())}" }
                        div { class: "spacer" }
                        button { class: "ed-pj", title: "모든 알림 지우기", "aria-label": "모든 알림 지우기", disabled: rows.is_empty(),
                            onclick: move |_| {
                                let answers = clear_s.notifications.borrow_mut().clear_all();
                                for (id, handle) in answers { clear_s.host.send(HostCommand::AnswerMessage { id, handle }); }
                                bump(&clear_s);
                            }, "모두 지우기" }
                        button { class: "ed-ib", title: "알림 센터 숨기기", "aria-label": "알림 센터 숨기기", onclick: move |_| toggle_center(&hide_s), {icon("close")} }
                    }
                    div { style: "overflow-y:auto; min-height:0;",
                        for m in rows.iter() { {card(s, m, false)} }
                    }
                }
            } else if !toasts.is_empty() {
                div { "aria-live": "polite", style: "{anchor}",
                    for m in toasts.iter() { {card(s, m, true)} }
                }
            }
            if let Some(m) = pending_modal { {modal(s, &m)} }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_16_notification_strings_are_korean_and_named() {
        assert_eq!(card_label(MessageSeverity::Error, Some("rust-analyzer")), "오류 알림, rust-analyzer");
        assert_eq!(card_label(MessageSeverity::Info, None), "정보 알림");
        assert_eq!(severity_class(MessageSeverity::Warning), "ed-pdot warn");
    }

    // #301: the page is the viewport, so the overlay sits 12px above the status bar on every
    // system, with no title bar allowance.
    #[test]
    fn fr_l31_37_notifications_sit_above_the_status_bar_with_no_title_bar_allowance() {
        assert_eq!(overlay_bottom(), "calc(var(--sbh) + 12px)");
    }

    #[test]
    fn fr_l31_16_toasts_and_center_hide_while_a_modal_waits() {
        assert!(!overlay_shown(true), "nothing above the modal's scrim");
        assert!(overlay_shown(false), "toasts come back once the modal closes");
    }

    // #477: the window writes no colour of its own. The modal's scrim is the stylesheet's `.modal`.
    #[test]
    fn fr_l31_16_the_modal_scrim_is_the_shared_modal_class() {
        let src = include_str!("editor_notifications.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("tests")];
        assert!(production.contains("div { class: \"modal\", role: \"presentation\","), "the modal message lost the shared scrim");
        assert!(!production.contains("rgba(") && !production.contains("background:#"), "a hard-coded colour in the notifications");
        let css = include_str!("../../assets/editor/style.css");
        assert!(css.contains(".modal{position:fixed;inset:0;z-index:60;background:rgba(0,0,0,.3);display:grid;place-items:center;padding:var(--sp4)}"), "style.css no longer has the .modal scrim");
    }

    // #477 (PR #905's two choices, leader 2026-11-25: the look follows the mockup exactly): the
    // scrim, the bottom-edge dialog at 560px and below, the panel's `surface` and the `muted` tone
    // of tree rows and the find pill are the mockup's own values, in the stylesheets the window
    // loads and in docs/design/mockup, character for character.
    #[test]
    fn fr_l31_16_the_scrim_and_muted_tone_are_the_mockups_values() {
        let mock_style = include_str!("../../../../docs/design/mockup/style.css");
        let mock_editor = include_str!("../../../../docs/design/mockup/editor.css");
        let style = include_str!("../../assets/editor/style.css");
        let editor = include_str!("../../assets/editor/editor.css");
        for rule in [
            // The scrim: black at .3 over the whole window.
            ".modal{position:fixed;inset:0;z-index:60;background:rgba(0,0,0,.3);display:grid;place-items:center;padding:var(--sp4)}",
            // At 560px and below the dialog sits at the bottom edge.
            "@media (max-width:560px){\n.modal{place-items:end center;padding:0}",
            // The tokens, light then dark.
            "--surface:#ffffff",
            "--muted:#8f8b83",
            "--surface:#1f1f1d",
            "--muted:#8b877f",
        ] {
            assert!(mock_style.contains(rule), "the mockup's style.css has `{rule}`");
            assert!(style.contains(rule), "the window's style.css has the mockup's `{rule}`");
        }
        for rule in [
            // The code panel is `surface`.
            "background:var(--surface);border:1px solid var(--line);border-radius:var(--rad);overflow:hidden}",
            // Tree rows' icons and the find pill are `muted`.
            ".ed-tn svg.i:not(.chev){color:var(--muted);width:15px;height:15px}",
            ".ed-tn .chev{width:12px!important;height:12px!important;color:var(--muted);",
            "border-radius:var(--pill);background:var(--surface);color:var(--muted);cursor:pointer;font-size:var(--fs-sm)",
        ] {
            assert!(mock_editor.contains(rule), "the mockup's editor.css has `{rule}`");
            assert!(editor.contains(rule), "the window's editor.css has the mockup's `{rule}`");
        }
        // The muted icon tone strokes in exactly the `--muted` value of each palette.
        assert_eq!(crate::ui::editor_icons::token("--muted", false), Some("#8f8b83"));
        assert_eq!(crate::ui::editor_icons::token("--muted", true), Some("#8b877f"));
        assert_eq!(crate::ui::editor_icons::icon_stroke("folder.muted"), ("folder", "--muted"));
    }
}
