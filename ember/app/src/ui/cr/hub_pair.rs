//! "허브로 연결" in the 이 기기 section of the settings sheet (FR-N9, FR-N2, #1044): the app links
//! its device key to the GitHub account on the hub, shows the code to approve, lists the
//! account's servers and saves the chosen one's peer id and relay into the same prefs fields the
//! address and relay fields write. Logic in `crate::hub_pairing`.
//!
//! Kept to what the SPEC names: no QR code, no approval from inside the app, and a link that
//! is not resumed after an app restart (a new code is started instead).

#![allow(non_snake_case)]

use std::path::PathBuf;

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui::Modifier;
use compose_rust::TypeRole;

use super::components::{copy_notice, AppButton, ButtonKind, Label, Txt, COPY_LABEL};
use super::notice;
use super::theme::*;
use crate::device_link::{self, APPLIES_AFTER_RESTART};
use crate::hub_pairing::{self, ServerRow};
use crate::services::{run_shared, try_services};
use crate::ui::coexist::shared_ui;

pub const TITLE: &str = "허브로 연결";
pub const SUBTITLE: &str = "GitHub 계정에 이 기기를 등록하고, 계정의 서버를 골라 연결합니다. 서버 주소를 직접 입력하지 않아도 됩니다.";
pub const SIGN_IN_LABEL: &str = "GitHub 계정으로 연결";
pub const LIST_LABEL: &str = "서버 목록 불러오기";
pub const STARTING: &str = "허브에 연결하는 중입니다";
/// The line above the code.
pub const APPROVE_HINT: &str = "브라우저에서 아래 주소를 열고 GitHub로 로그인한 다음 이 코드를 승인합니다. 계정의 서버에서 승인해도 됩니다.";
pub const LOADING: &str = "계정의 서버를 불러오는 중입니다";
pub const NO_SERVERS: &str = "이 계정에 등록된 서버가 없습니다. 서버를 먼저 허브에 연결합니다.";
pub const PICK_HINT: &str = "연결할 서버를 고릅니다. 서버에서 이 기기를 허용해야 연결됩니다.";
pub const ONLINE: &str = "온라인";
pub const OFFLINE: &str = "오프라인";

/// Where the flow is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Idle,
    Starting,
    Waiting { code: String, url: String },
    Loading,
    Servers(Vec<ServerRow>),
    Paired(String),
    Failed(&'static str),
}

/// The first button: sign in when the app has no registration, else list the servers.
pub fn first_action(registered: bool) -> &'static str {
    if registered { LIST_LABEL } else { SIGN_IN_LABEL }
}

/// One server row: name, short peer id and whether the hub sees it online.
pub fn server_line(row: &ServerRow) -> String {
    format!("{} · {} · {}", row.name, row.peer.fmt_short(), if row.online { ONLINE } else { OFFLINE })
}

/// The line after pairing.
pub fn paired_line(name: &str) -> String {
    format!("{name} 서버를 저장했습니다. {APPLIES_AFTER_RESTART}")
}

/// Signs in when needed (showing the code in `step` while it waits), then lists the servers.
async fn sign_in_and_list(dir: PathBuf, registered: bool, step: MutableState<Step>) -> Result<Vec<ServerRow>, &'static str> {
    let file = hub_pairing::registration_file(&dir);
    if !registered {
        let hub = hub_pairing::hub_config()?;
        let key = device_link::load_key(&dir).map_err(|_| hub_pairing::FILE_ERROR)?;
        let link = hub_pairing::start(&hub, &key, &hub_pairing::device_name()).await?;
        step.set(Step::Waiting { code: link.user_code().to_string(), url: link.pending().verification_uri_complete.clone() });
        hub_pairing::finish(&link, &file).await?;
        step.set(Step::Loading);
    }
    hub_pairing::servers(&file).await
}

/// Whether `<data dir>/hub.json` holds a registration the hub has not removed.
fn is_registered(dir: &std::path::Path) -> bool {
    matches!(hub_pairing::registration_file(dir).load(), Ok(Some(r)) if !r.is_revoked())
}

/// Runs [`sign_in_and_list`] and puts its result into `step`. Whether to sign in is read from
/// `hub.json` at the click, so a failed list never starts a second link for the same key.
fn run(step: MutableState<Step>) {
    let Some(dir) = try_services().filter(|s| s.demo.is_none()).and_then(|s| s.config.data_dir.clone()) else {
        return;
    };
    let registered = is_registered(&dir);
    step.set(if registered { Step::Loading } else { Step::Starting });
    let st = step.clone();
    run_shared(sign_in_and_list(dir, registered, st), move |r| {
        step.set(match r {
            Ok(rows) => Step::Servers(rows),
            Err(t) => Step::Failed(t),
        })
    });
}

/// Pairs with `row` and saves the result into prefs.
fn pair(step: MutableState<Step>, row: ServerRow) {
    let Some(dir) = try_services().and_then(|s| s.config.data_dir.clone()) else { return };
    step.set(Step::Loading);
    run_shared(
        async move { hub_pairing::pair(&hub_pairing::registration_file(&dir), &row.peer).await.map(|s| (s, row.name)) },
        move |r| match r {
            Ok((saved, name)) => {
                shared_ui().update_prefs(|pr| saved.apply(pr));
                step.set(Step::Paired(name));
            }
            Err(t) => step.set(Step::Failed(t)),
        },
    );
}

pub fn HubPairSection() {
    let p = ember_theme().palette;
    let data_dir = try_services().and_then(|s| s.config.data_dir.clone());
    let registered = remember(|| data_dir.as_deref().is_some_and(is_registered));
    let step = remember(|| mutable_state_of(Step::Idle));

    Label(TITLE, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
    Label(SUBTITLE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
    let busy = matches!(step.get(), Step::Starting | Step::Waiting { .. } | Step::Loading);
    {
        let st = step.clone();
        let label = first_action(registered || matches!(step.get(), Step::Servers(_) | Step::Paired(_)));
        AppButton(label, None, ButtonKind::Default, true, !busy && data_dir.is_some(), false, move || run(st.clone()));
    }
    match step.get() {
        Step::Idle => {}
        Step::Starting => Label(STARTING, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1)),
        Step::Loading => Label(LOADING, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1)),
        Step::Waiting { code, url } => {
            Label(APPROVE_HINT, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
            Txt(&code, TypeRole::Mono, Some(paint(p.text)), Some(1), None, None, None);
            Txt(&url, TypeRole::Mono, Some(paint(p.text_2)), Some(2), None, None, None);
            AppButton(COPY_LABEL, None, ButtonKind::Default, true, true, false, move || {
                notice::post(copy_notice(compose_rust::clipboard::set_text(&url).is_ok()));
            });
        }
        Step::Servers(rows) if rows.is_empty() => Label(NO_SERVERS, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2)),
        Step::Servers(rows) => {
            Label(PICK_HINT, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
            { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                for row in &rows {
                    let (st, row) = (step.clone(), row.clone());
                    let line = server_line(&row);
                    AppButton(&line, None, ButtonKind::Default, true, true, true, move || pair(st.clone(), row.clone()));
                }
            }) };
        }
        Step::Paired(name) => Label(&paired_line(&name), Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2)),
        Step::Failed(t) => Label(t, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(3)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_n9_the_first_button_signs_in_once_then_lists() {
        assert_eq!(first_action(false), SIGN_IN_LABEL);
        assert_eq!(first_action(true), LIST_LABEL);
    }

    #[test]
    fn fr_n9_a_server_row_names_the_server_and_its_short_id() {
        let peer = ember_transport::SecretKey::from_bytes([9; 32]).peer_id();
        let row = ServerRow { peer, name: "mini".into(), online: true };
        let line = server_line(&row);
        assert!(line.starts_with("mini · ") && line.contains(&peer.fmt_short()) && line.ends_with(ONLINE), "{line}");
        assert!(server_line(&ServerRow { online: false, ..row }).ends_with(OFFLINE));
        assert!(paired_line("mini").contains(APPLIES_AFTER_RESTART));
        for t in [TITLE, SUBTITLE, SIGN_IN_LABEL, LIST_LABEL, STARTING, APPROVE_HINT, LOADING, NO_SERVERS, PICK_HINT] {
            assert!(!t.is_ascii() && !t.contains('\u{2014}'), "{t}");
        }
    }
}
