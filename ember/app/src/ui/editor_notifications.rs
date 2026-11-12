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

/// The overlay's distance from the page bottom: above the status bar, plus the 76px the page is
/// shorter than the window when it is not FullSize (the same allowance as `editor_page_height`).
// TODO(#320): drop the allowance with the fixed page height (compose-rust#467).
pub fn overlay_bottom(full_size: bool) -> &'static str {
    if full_size { "calc(var(--sbh) + 12px)" } else { "calc(var(--sbh) + 88px)" }
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
            div { style: "position:fixed; left:0; top:0; right:0; bottom:0; z-index:45; background:rgba(0,0,0,.25); display:flex; align-items:center; justify-content:center;",
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
        let bottom = overlay_bottom(cfg!(target_os = "macos"));
        let anchor = format!("position:fixed; z-index:35; right:calc(var(--stw) + 12px); bottom:{bottom}; width:min(420px, calc(100vw - 32px)); display:flex; flex-direction:column; gap:var(--sp2);");
        let toasts: Vec<ExtensionMessage> = n.toasts().into_iter().map(|e| e.message.clone()).collect();
        let center: Option<Vec<ExtensionMessage>> = n.center_open().then(|| n.center().into_iter().map(|e| e.message.clone()).collect());
        let pending_modal = n.modal().cloned();
        drop(n);
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
        assert_eq!(overlay_bottom(true), "calc(var(--sbh) + 12px)");
    }
}
