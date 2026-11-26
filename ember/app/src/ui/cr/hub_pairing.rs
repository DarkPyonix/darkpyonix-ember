//! The "허브로 연결" part of the "이 기기" section in 설정 (FR-N9, FR-N2, #1044): register this
//! device with the darkpyonix.dev hub as a `client` device (user code and approval URL), list the
//! account's main servers, and pair with one, which saves the hub record's peer id and the
//! server's `letify://` relay as the server address and relay. The hand-typed fields stay below it
//! under "직접 입력". Logic in `crate::hub_pairing`.

#![allow(non_snake_case)]

use std::path::PathBuf;

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::TypeRole;

use super::components::{copy_notice, AppButton, ButtonKind, InsetPanel, Label, Txt, COPY_LABEL};
use super::notice;
use super::theme::*;
use crate::device_link;
use ember_hub::Registration;
use crate::hub_pairing::{self, Approval, MainServer, DEVICE_NAME, HUB_OFF};
use crate::services::{run_shared, try_services};
use crate::ui::coexist::shared_ui;

pub const TITLE: &str = "허브로 연결";
pub const SUBTITLE: &str = "GitHub 계정의 darkpyonix.dev 허브에 이 기기를 등록하고, 계정의 메인 서버를 골라 연결합니다. 서버 주소와 릴레이를 입력하지 않아도 됩니다.";
pub const REGISTER_LABEL: &str = "허브에 이 기기 등록";
pub const CODE_LABEL: &str = "사용자 코드";
pub const URL_LABEL: &str = "승인 주소";
pub const WAITING: &str = "승인 주소에서 GitHub로 로그인해 이 코드를 승인하면 등록됩니다. 승인을 기다리는 중입니다.";
pub const REGISTERED: &str = "허브에 등록되어 있습니다";
pub const LIST_LABEL: &str = "메인 서버 불러오기";
pub const NO_SERVERS: &str = "계정에 메인 서버가 없습니다.";
pub const PAIR_LABEL: &str = "이 서버와 연결";
pub const NO_RELAY_TAG: &str = "릴레이 없음";
pub const ONLINE_TAG: &str = "온라인";
pub const PAIRED: &str = "서버 주소와 릴레이를 허브에서 받아 저장했습니다. 앱을 다시 시작하면 적용됩니다.";
pub const MANUAL_LABEL: &str = "직접 입력";
const KEY_FAILED: &str = "기기 키를 읽거나 만들지 못했습니다";
const NO_DATA_DIR: &str = "이 기기에는 허브 등록 정보를 둘 폴더가 없습니다";

/// Where the hub part is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Idle,
    Busy,
    Waiting(Approval),
    Servers(Vec<MainServer>),
}

/// Drawn by `this_device::ThisDeviceSection` above the hand-typed fields. `typed_peer`,
/// `typed_relay` and `saved` are that section's field states, set on a pairing so the fields show
/// what was saved.
pub fn HubPairingSection(typed_peer: MutableState<String>, typed_relay: MutableState<String>, saved: MutableState<bool>) {
    let p = ember_theme().palette;
    let data_dir: Option<PathBuf> = try_services().and_then(|s| s.config.data_dir.clone());
    let live = try_services().is_some_and(|s| s.demo.is_none());
    let reg = {
        let d = data_dir.clone();
        remember(move || mutable_state_of(d.as_deref().and_then(hub_pairing::load)))
    };
    let step = remember(|| mutable_state_of(Step::Idle));
    let error = remember(|| mutable_state_of(None::<&'static str>));
    let paired = remember(|| mutable_state_of(false));

    Label(TITLE, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
    Label(SUBTITLE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
    InsetPanel(true, || {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
            let current = reg.get();
            let busy = matches!(step.get(), Step::Busy | Step::Waiting(_));
            match &current {
                None => {
                    let (dir, st, e, r) = (data_dir.clone(), step.clone(), error.clone(), reg.clone());
                    AppButton(REGISTER_LABEL, None, ButtonKind::Default, true, live && !busy, false, move || {
                        let Some(dir) = dir.clone() else {
                            e.set(Some(NO_DATA_DIR));
                            return;
                        };
                        e.set(None);
                        st.set(Step::Busy);
                        let shown = st.clone();
                        let (st, e, r) = (st.clone(), e.clone(), r.clone());
                        run_shared(
                            register(dir, shown),
                            move |res| {
                                st.set(Step::Idle);
                                match res {
                                    Ok(registration) => r.set(Some(registration)),
                                    Err(t) => e.set(Some(t)),
                                }
                            },
                        );
                    });
                }
                Some(registration) if registration.is_revoked() => {
                    Label(hub_pairing::REMOVED, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(3));
                }
                Some(registration) => {
                    let line = format!("{REGISTERED}: {}", registration.endpoint_id().fmt_short());
                    Label(&line, Fs::Xs, Weight::Medium, Some(paint(p.text)), Some(1));
                    let (dir, registration) = (data_dir.clone(), registration.clone());
                    let (st, e, r) = (step.clone(), error.clone(), reg.clone());
                    AppButton(LIST_LABEL, None, ButtonKind::Default, true, live && !busy, false, move || {
                        e.set(None);
                        st.set(Step::Busy);
                        let (dir, registration) = (dir.clone(), registration.clone());
                        let file = dir.as_deref().map(hub_pairing::registration_file);
                        let (st, e, r) = (st.clone(), e.clone(), r.clone());
                        run_shared(
                            async move {
                                hub_pairing::main_servers(&registration, file.as_ref()).await
                            },
                            move |res| match res {
                                Ok(list) => st.set(Step::Servers(list)),
                                Err(err) => {
                                    st.set(Step::Idle);
                                    if matches!(err, hub_pairing::HubPairError::Removed) {
                                        r.set(dir.as_deref().and_then(hub_pairing::load));
                                    }
                                    e.set(Some(err.text_ko()));
                                }
                            },
                        );
                    });
                }
            }

            if let Step::Waiting(approval) = step.get() {
                Label(CODE_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                Txt(&approval.user_code, TypeRole::Mono, Some(paint(p.text)), Some(1), None, None, None);
                Label(URL_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                Txt(&approval.url, TypeRole::Mono, Some(paint(p.text)), Some(2), None, None, None);
                let url = approval.url.clone();
                AppButton(COPY_LABEL, None, ButtonKind::Default, true, true, false, move || {
                    notice::post(copy_notice(compose_rust::clipboard::set_text(&url).is_ok()));
                });
                Label(WAITING, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
            }

            if let (Step::Servers(list), Some(registration)) = (step.get(), current.clone()) {
                if list.is_empty() {
                    Label(NO_SERVERS, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                }
                for server in list {
                    ServerRow(
                        &server,
                        live,
                        {
                            let (dir, registration, peer) = (data_dir.clone(), registration.clone(), server.peer);
                            let (e, r, done) = (error.clone(), reg.clone(), paired.clone());
                            let (tp, tr, s) = (typed_peer.clone(), typed_relay.clone(), saved.clone());
                            move || {
                                e.set(None);
                                let (dir, registration) = (dir.clone(), registration.clone());
                                let file = dir.as_deref().map(hub_pairing::registration_file);
                                let (e, r, done) = (e.clone(), r.clone(), done.clone());
                                let (tp, tr, s) = (tp.clone(), tr.clone(), s.clone());
                                run_shared(
                                    async move {
                                        hub_pairing::pair(&registration, file.as_ref(), &peer).await
                                    },
                                    move |res| match res {
                                        Ok(pairing) => {
                                            shared_ui().update_prefs(|pr| pairing.apply(pr));
                                            tp.set(pairing.peer.clone());
                                            tr.set(pairing.relay.clone());
                                            s.set(false);
                                            done.set(true);
                                        }
                                        Err(err) => {
                                            if matches!(err, hub_pairing::HubPairError::Removed) {
                                                r.set(dir.as_deref().and_then(hub_pairing::load));
                                            }
                                            done.set(false);
                                            e.set(Some(err.text_ko()));
                                        }
                                    },
                                );
                            }
                        },
                    );
                }
            }

            if let Some(t) = error.get() {
                Label(t, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(3));
            }
            if paired.get() {
                Label(PAIRED, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
            }
        }) };
    });
    Label(MANUAL_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
}

/// Registers this device with the hub as a `client` device, showing the approval in `shown`
/// while it waits, and keeps the registration in `<data dir>/hub.json`.
async fn register(dir: PathBuf, shown: MutableState<Step>) -> Result<Registration, &'static str> {
    let hub = hub_pairing::hub_from_env().ok_or(HUB_OFF)?;
    let key = device_link::load_key(&dir).map_err(|_| KEY_FAILED)?;
    let link = hub_pairing::start(&hub, &key, DEVICE_NAME).await.map_err(|e| e.text_ko())?;
    shown.set(Step::Waiting(Approval::of(&link)));
    hub_pairing::finish(&link, &hub_pairing::registration_file(&dir)).await.map_err(|e| e.text_ko())
}

/// One main server: its name, short peer id, whether it is online and names a relay, and 연결.
fn ServerRow(server: &MainServer, live: bool, on_pair: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Column; Column!(spacing = 2.0, modifier = Modifier::new().fill_max_width(), {
        let mut line = format!("{} ({})", server.name, server.peer.fmt_short());
        if server.online {
            line.push_str(&format!(" · {ONLINE_TAG}"));
        }
        if server.relay.is_none() {
            line.push_str(&format!(" · {NO_RELAY_TAG}"));
        }
        Label(&line, Fs::Xs, Weight::Medium, Some(paint(p.text)), Some(1));
        AppButton(PAIR_LABEL, None, ButtonKind::Default, true, live && server.relay.is_some(), false, on_pair);
    }) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_n9_hub_lines_are_korean_sentences() {
        for t in [SUBTITLE, WAITING, NO_SERVERS, PAIRED] {
            assert!(t.ends_with("다."), "{t}");
        }
        assert!(!TITLE.is_ascii() && !REGISTER_LABEL.is_ascii() && !PAIR_LABEL.is_ascii());
    }
}
