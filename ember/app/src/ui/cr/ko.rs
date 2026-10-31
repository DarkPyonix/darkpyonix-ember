//! Korean strings for what the main screens show. `model` still returns English (the Dioxus
//! screens use it and its tests pin it), so the compose-rust screens map from the same values
//! here; when the Dioxus originals go in phase 8 these replace the English ones in `model`.

use ember_client::state::{ConnectionState, Reachability, State};
use ember_client::wire::{ForkOrigin, TaskStatus, TeamRole, TeamTask, TeamView};
use ember_client::LauncherStatus;

use crate::model::{TeamMemberRow, Tone};

pub fn status_label(s: LauncherStatus) -> &'static str {
    match s {
        LauncherStatus::Idle => "대기 중",
        LauncherStatus::Running => "실행 중",
        LauncherStatus::WaitingForApproval => "승인 대기",
        LauncherStatus::FinishedUnread => "읽지 않음",
        LauncherStatus::Finished => "완료",
        LauncherStatus::Failed => "실패",
        LauncherStatus::Unknown => "알 수 없음",
    }
}

pub fn reachability_label(r: Reachability) -> &'static str {
    match r {
        Reachability::Online => "온라인",
        Reachability::Offline => "오프라인",
        Reachability::Unknown => "확인 중\u{2026}",
    }
}

/// Banner shown on a conversation while its approve-everything mode is on (FR-A5); `None` when
/// off, so the indicator is visible exactly while the mode is active.
pub fn approve_all_banner(on: bool) -> Option<&'static str> {
    on.then_some("모든 요청 자동 승인이 켜져 있습니다: 이 세션의 도구 요청을 묻지 않고 허용합니다.")
}

/// Menu entry that flips the mode.
pub fn approve_all_menu_label(on: bool) -> &'static str {
    if on { "도구 실행 전 다시 묻기" } else { "모두 자동 승인" }
}

/// Menu entry that forks from the last completed turn (FR-S5).
pub const FORK_MENU_LABEL: &str = "마지막 턴에서 포크";

/// Button under a completed turn.
pub const FORK_TURN_LABEL: &str = "여기서 포크";

/// "<parent>에서 포크됨" line of a forked session; the parent's title when known, else its id.
pub fn fork_origin_label(origin: &ForkOrigin, parent_title: Option<&str>) -> String {
    let name = parent_title.filter(|t| !t.is_empty()).unwrap_or(&origin.parent_id);
    format!("{name}에서 포크됨")
}

/// Korean label for a team task's status.
pub fn task_status_label(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::Open => "열림",
        TaskStatus::InProgress => "진행 중",
        TaskStatus::Blocked => "차단됨",
        TaskStatus::Done => "완료",
        TaskStatus::Cancelled => "취소됨",
        TaskStatus::Unknown => "알 수 없음",
    }
}

/// One task line: `#3 [진행 중] Write tests · alice`.
pub fn task_line(t: &TeamTask) -> String {
    let who = t
        .assignee_name
        .as_deref()
        .unwrap_or(if t.assignee.is_some() { "팀원" } else { "미배정" });
    format!("#{} [{}] {} \u{00b7} {who}", t.number, task_status_label(t.status), t.title)
}

/// The team panel's member rows with Korean status labels: leader first, then active teammates,
/// then ended ones.
pub fn team_member_rows(state: &State, team: &TeamView, this: &str) -> Vec<TeamMemberRow> {
    use ember_client::wire::SessionStatus;
    let mut rows: Vec<TeamMemberRow> = team
        .members
        .iter()
        .map(|m| {
            let status = match state.session(&m.session_id) {
                Some(v) => status_label(v.status),
                None => match m.status {
                    Some(SessionStatus::Running) => status_label(LauncherStatus::Running),
                    Some(SessionStatus::WaitingForApproval) => status_label(LauncherStatus::WaitingForApproval),
                    Some(SessionStatus::Failed) => status_label(LauncherStatus::Failed),
                    Some(SessionStatus::Finished) => status_label(LauncherStatus::Finished),
                    _ => status_label(LauncherStatus::Idle),
                },
            };
            TeamMemberRow {
                session_id: m.session_id.clone(),
                name: m.name.clone(),
                leader: m.role == TeamRole::Leader,
                agent: m.agent.clone(),
                status,
                ended: !m.active(),
                this: m.session_id == this,
                open_tasks: team
                    .tasks
                    .iter()
                    .filter(|t| {
                        t.assignee.as_deref() == Some(m.session_id.as_str())
                            && !matches!(t.status, TaskStatus::Done | TaskStatus::Cancelled)
                    })
                    .count(),
            }
        })
        .collect();
    rows.sort_by_key(|r| (!r.leader, r.ended));
    rows
}

/// `방금`, `10분`, `3시간`, `2일`, `1주`.
pub fn relative_time(now: i64, at: i64) -> String {
    if at <= 0 {
        return String::new();
    }
    let secs = (now - at).max(0) / 1000;
    match secs {
        0..=44 => "방금".into(),
        45..=3599 => format!("{}분", (secs / 60).max(1)),
        3600..=86_399 => format!("{}시간", secs / 3600),
        86_400..=604_799 => format!("{}일", secs / 86_400),
        _ => format!("{}주", secs / 604_800),
    }
}

/// The push connection's state, when it is not simply "connected" (PR-1).
pub fn connection_banner(c: &ConnectionState) -> Option<(String, Tone)> {
    match c {
        ConnectionState::Connected => None,
        ConnectionState::Offline => Some(("오프라인: 저장된 데이터를 보여주는 중입니다".into(), Tone::Neutral)),
        ConnectionState::Connecting { .. } => Some(("메인 서버에 연결하는 중입니다\u{2026}".into(), Tone::Neutral)),
        ConnectionState::Reconnecting { retry_in_ms, error, .. } => Some((
            format!("연결이 끊어졌습니다({error}). {}초 후 다시 시도합니다", retry_in_ms.div_ceil(1000)),
            Tone::Warning,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l2_relative_time_is_korean() {
        assert_eq!(relative_time(100_000, 99_000), "방금");
        assert_eq!(relative_time(700_000, 100_000), "10분");
        assert_eq!(relative_time(3 * 3_600_000 + 5, 0), "");
        assert_eq!(relative_time(3 * 3_600_000 + 10, 10), "3시간");
    }

    #[test]
    fn fr_l2_every_status_has_a_korean_label() {
        assert_eq!(status_label(LauncherStatus::WaitingForApproval), "승인 대기");
        assert_eq!(status_label(LauncherStatus::Finished), "완료");
    }

    #[test]
    fn fr_l5_korean_labels_for_reachability_approve_all_fork_and_team() {
        assert_eq!(reachability_label(Reachability::Online), "온라인");
        assert_eq!(reachability_label(Reachability::Offline), "오프라인");
        assert!(approve_all_banner(true).is_some());
        assert!(approve_all_banner(false).is_none());
        assert_eq!(approve_all_menu_label(true), "도구 실행 전 다시 묻기");
        assert_eq!(approve_all_menu_label(false), "모두 자동 승인");
        let origin = ForkOrigin {
            parent_id: "s-parent".into(),
            turn_seq: 3,
            forked_at: 0,
        };
        assert_eq!(fork_origin_label(&origin, Some("기존 세션")), "기존 세션에서 포크됨");
        assert_eq!(fork_origin_label(&origin, None), "s-parent에서 포크됨");
        let task = TeamTask {
            id: "t1".into(),
            team_id: "tm1".into(),
            number: 3,
            title: "테스트 작성".into(),
            detail: String::new(),
            status: TaskStatus::InProgress,
            assignee: Some("s2".into()),
            assignee_name: Some("alice".into()),
            created_by: "s1".into(),
            created_at: 0,
            updated_at: 0,
        };
        assert_eq!(task_line(&task), "#3 [진행 중] 테스트 작성 \u{00b7} alice");
    }
}
