//! Korean user-facing strings for the main screens, re-exported from [`crate::model`] (NFR-L4).

pub use crate::model::{
    approve_all_banner, approve_all_menu_label, connection_banner, fork_origin_label,
    reachability_label, relative_time, status_label, task_line, task_status_label,
    team_member_rows, FORK_MENU_LABEL, FORK_TURN_LABEL,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tone;
    use ember_client::state::{ConnectionState, Reachability};
    use ember_client::wire::{ForkOrigin, TaskStatus, TeamTask};
    use ember_client::LauncherStatus;

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

    #[test]
    fn fr_s11_an_unauthorized_push_says_where_the_token_was_looked_for() {
        let c = ConnectionState::Unauthorized {
            error: "missing or invalid server token".into(),
            looked_in: vec!["EMBER_SERVER_TOKEN".into(), "C:/Users/me/AppData/Roaming/ember/server-token".into()],
        };
        let (text, tone) = connection_banner(&c).unwrap();
        assert_eq!(tone, Tone::Error);
        assert!(text.starts_with("서버가 토큰을 거부했습니다(missing or invalid server token)."), "{text}");
        assert!(text.contains("토큰을 찾은 곳: EMBER_SERVER_TOKEN, C:/Users/me/AppData/Roaming/ember/server-token."), "{text}");
        assert!(!text.contains("다시 시도합니다"), "no retry loop: {text}");
        // A token given by the caller has no place to name.
        let given = ConnectionState::Unauthorized { error: "x".into(), looked_in: Vec::new() };
        assert!(!connection_banner(&given).unwrap().0.contains("찾은 곳"));
    }
}
