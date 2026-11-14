//! The "이 기기" section of the settings sheet (FR-N9, #856): this device's peer id with a 복사
//! button, so the owner can allow it on the server, and the server's peer address and relay,
//! saved in prefs and read at the next start. Logic in `crate::device_link`.

#![allow(non_snake_case)]

use compose_rust::runtime::{launched_effect, mutable_state_of, remember};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::TypeRole;

use super::components::{copy_notice, AppButton, ButtonKind, EmberField, FieldOptions, InsetPanel, Label, Txt, COPY_LABEL};
use super::notice;
use super::theme::*;
use crate::device_link::{self, PeerSetting, APPLIES_AFTER_RESTART, CONNECTED};
use crate::services::{run_shared, try_services};
use crate::ui::coexist::shared_ui;

/// The section's title and the line under it.
pub const TITLE: &str = "이 기기";
pub const SUBTITLE: &str = "다른 컴퓨터의 서버에 이 기기로 연결합니다. 서버의 소유자가 이 기기의 peer id를 기기로 추가해야 합니다.";
/// The line under the peer id.
pub const ADD_HINT: &str = "서버에서 ember-server owner devices add <peer id> <이름>을 실행하면 이 기기가 등록됩니다.";
pub const PEER_ID_LABEL: &str = "이 기기의 peer id";
pub const ADDR_LABEL: &str = "서버 주소";
pub const ADDR_PLACEHOLDER: &str = "<서버 peer id>@<IP>:<포트> 또는 서버가 출력한 주소";
pub const RELAY_LABEL: &str = "릴레이 (선택)";
pub const RELAY_PLACEHOLDER: &str = "letify://<서버 peer id>@<호스트>:7443";
/// Plain HTTP to a remote host is not offered (FR-S11, D37).
pub const HTTP_NOTE: &str = "다른 컴퓨터의 서버에는 일반 HTTP로 연결하지 않습니다. 주소를 비우고 저장하면 이 컴퓨터의 서버로 돌아갑니다.";
pub const SAVE_LABEL: &str = "저장";
pub const CHECKING: &str = "서버에 연결하는 중입니다";

/// What 저장 does with the two fields: `Ok(None)` clears the reach (back to local HTTP),
/// `Ok(Some((peer, relay)))` keeps both as typed, `Err` is the Korean line to show.
pub fn save_values(peer: &str, relay: &str) -> Result<Option<(String, Option<String>)>, &'static str> {
    let (peer, relay) = (peer.trim(), relay.trim());
    if peer.is_empty() {
        return Ok(None);
    }
    PeerSetting::parse(peer, Some(relay)).map_err(|e| e.text_ko())?;
    Ok(Some((peer.to_string(), (!relay.is_empty()).then(|| relay.to_string()))))
}

/// The peer id in two halves of 32 characters, so it fits a phone's width without breaking.
pub fn id_lines(id: &str) -> (String, String) {
    let split = id.char_indices().nth(32).map_or(id.len(), |(i, _)| i);
    (id[..split].to_string(), id[split..].to_string())
}

pub fn ThisDeviceSection() {
    let p = ember_theme().palette;
    let prefs = shared_ui().prefs.get();
    let data_dir = try_services().and_then(|s| s.config.data_dir.clone());
    let me = device_link::this_device(data_dir.as_deref());
    let reach = try_services().and_then(|s| s.config.server_peer.clone());
    let typed_peer = remember(|| mutable_state_of(prefs.server_peer.clone().unwrap_or_default()));
    let typed_relay = remember(|| mutable_state_of(prefs.server_relay.clone().unwrap_or_default()));
    let error = remember(|| mutable_state_of(None::<&'static str>));
    let saved = remember(|| mutable_state_of(false));
    let status = remember(|| mutable_state_of(None::<Result<(), &'static str>>));

    // Over the transport, ask the server once whether it admits this device (FR-N9).
    if reach.is_some() {
        let st = status.clone();
        launched_effect(("this-device",), move || {
            let st = st.clone();
            async move {
                if try_services().is_some_and(|s| s.demo.is_none()) {
                    run_shared(
                        async move { device_link::check(&crate::services::services().client.api()).await },
                        move |r| st.set(Some(r)),
                    );
                }
            }
        });
    }

    InsetPanel(true, || {
        cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
            Label(PEER_ID_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
            match &me {
                Ok(id) => {
                    let id = id.to_string();
                    let (a, b) = id_lines(&id);
                    Txt(&a, TypeRole::Mono, Some(paint(p.text)), Some(1), None, None, None);
                    Txt(&b, TypeRole::Mono, Some(paint(p.text)), Some(1), None, None, None);
                    AppButton(COPY_LABEL, None, ButtonKind::Default, true, true, false, move || {
                        notice::post(copy_notice(compose_rust::clipboard::set_text(&id).is_ok()));
                    });
                    Label(ADD_HINT, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
                }
                Err(t) => Label(t, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(2)),
            }
            if let Some(setting) = &reach {
                let line = format!("지금 전송 계층으로 연결합니다: {}", setting.addr.peer.fmt_short());
                Label(&line, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                match status.get() {
                    None => Label(CHECKING, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1)),
                    Some(Ok(())) => Label(CONNECTED, Fs::Xs, Weight::Regular, Some(paint(p.text)), Some(1)),
                    Some(Err(t)) => Label(t, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(3)),
                }
            }
        });
    });

    Label(ADDR_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
    {
        let (t, e, s) = (typed_peer.clone(), error.clone(), saved.clone());
        EmberField(FieldOptions {
            fill_max_width: true,
            placeholder: ADDR_PLACEHOLDER.to_string(),
            text: Some(typed_peer.get_untracked()),
            on_value_change: Some(Box::new(move |v| {
                e.set(None);
                s.set(false);
                t.set(v)
            })),
            ..Default::default()
        });
    }
    Label(RELAY_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
    {
        let (t, e, s) = (typed_relay.clone(), error.clone(), saved.clone());
        EmberField(FieldOptions {
            fill_max_width: true,
            placeholder: RELAY_PLACEHOLDER.to_string(),
            text: Some(typed_relay.get_untracked()),
            on_value_change: Some(Box::new(move |v| {
                e.set(None);
                s.set(false);
                t.set(v)
            })),
            ..Default::default()
        });
    }
    Label(HTTP_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
    {
        let (tp, tr, e, s) = (typed_peer.clone(), typed_relay.clone(), error.clone(), saved.clone());
        AppButton(SAVE_LABEL, None, ButtonKind::Default, true, true, false, move || {
            match save_values(&tp.get_untracked(), &tr.get_untracked()) {
                Ok(values) => {
                    let (peer, relay) = match values {
                        Some((peer, relay)) => (Some(peer), relay),
                        None => (None, None),
                    };
                    shared_ui().update_prefs(|pr| {
                        pr.server_peer = peer;
                        pr.server_relay = relay;
                    });
                    e.set(None);
                    s.set(true);
                }
                Err(t) => {
                    s.set(false);
                    e.set(Some(t));
                }
            }
        });
    }
    if let Some(t) = error.get() {
        Label(t, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(2));
    }
    if saved.get() {
        Label(APPLIES_AFTER_RESTART, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_n9_save_values_clear_keep_or_refuse() {
        assert_eq!(save_values("  ", "letify://x"), Ok(None), "an empty address goes back to local HTTP");
        let peer = ember_transport::SecretKey::from_bytes([5; 32]).peer_id().to_string();
        assert_eq!(save_values(&format!(" {peer} "), " "), Ok(Some((peer.clone(), None))));
        let relay = format!("letify://{peer}@relay.example:7443");
        assert_eq!(save_values(&peer, &relay), Ok(Some((peer.clone(), Some(relay)))));
        assert_eq!(save_values("abc", ""), Err(device_link::AddrError::BadPeer(String::new()).text_ko()));
        assert_eq!(save_values(&peer, "https://x"), Err(device_link::AddrError::BadRelay(String::new()).text_ko()));
        assert!(save_values("http://192.0.2.1:8740", "").is_err(), "no plain HTTP to a remote host");
    }

    #[test]
    fn fr_n9_the_peer_id_shows_in_two_halves() {
        let id = "a".repeat(32) + &"b".repeat(32);
        assert_eq!(id_lines(&id), ("a".repeat(32), "b".repeat(32)));
        assert_eq!(id_lines("short"), ("short".to_string(), String::new()));
    }
}
