//! Korean strings for what the main screens show. `model` still returns English (the Dioxus
//! screens use it and its tests pin it), so the compose-rust screens map from the same values
//! here; when the Dioxus originals go in phase 8 these replace the English ones in `model`.

use ember_client::state::ConnectionState;
use ember_client::LauncherStatus;

use crate::model::Tone;

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
}
