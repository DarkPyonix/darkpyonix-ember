//! FR-L31.19 and FR-L31.22: the editor terminal's list, new and kill calls refuse plain http to a
//! non-loopback host before any request, and every failure shows a fixed Korean sentence.

use std::io::{Read, Write};
use std::net::TcpListener;

use ember_app::terminal_list::{self, panel_error, TermAction, AUTH_REFUSED_TEXT};
use ember_app::terminal_stream::{ClientError, RELAY_REFUSED};

const LAN: &str = "http://10.9.8.15:18763";

fn has_latin(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_alphabetic())
}

#[tokio::test]
async fn fr_l31_19_plain_http_lan_is_refused_before_any_request() {
    // 10.9.8.15 is not reachable from CI; a request would time out or fail with a transport error,
    // not with the refusal.
    let list = terminal_list::list(LAN, "local", "proj").await.unwrap_err();
    assert!(matches!(list, ClientError::Refused(_)), "list: {list}");
    let create = terminal_list::create(LAN, "local", "/w", "proj", "k".into()).await.unwrap_err();
    assert!(matches!(create, ClientError::Refused(_)), "create: {create}");
    let kill = terminal_list::kill(LAN, "local", "t1").await.unwrap_err();
    assert!(matches!(kill, ClientError::Refused(_)), "kill: {kill}");
    assert_eq!(panel_error(TermAction::List, &list), RELAY_REFUSED);
    assert_eq!(panel_error(TermAction::Create, &create), RELAY_REFUSED);
}

/// A loopback server that answers every request with `status`.
fn answer_with(status: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn fr_l31_22_401_and_403_show_a_fixed_korean_sentence() {
    for status in ["401 Unauthorized", "403 Forbidden"] {
        let base = answer_with(status);
        let list = terminal_list::list(&base, "local", "proj").await.unwrap_err();
        assert_eq!(panel_error(TermAction::List, &list), AUTH_REFUSED_TEXT, "{status}");
        let create = terminal_list::create(&base, "local", "/w", "proj", "k".into()).await.unwrap_err();
        assert_eq!(panel_error(TermAction::Create, &create), AUTH_REFUSED_TEXT, "{status}");
    }
}

#[tokio::test]
async fn fr_l31_22_other_failures_name_the_action_in_korean() {
    let base = answer_with("500 Internal Server Error");
    let err = terminal_list::kill(&base, "local", "t1").await.unwrap_err();
    let text = panel_error(TermAction::Kill, &err);
    assert!(text.starts_with("터미널을 종료하지 못했습니다."), "{text}");
    let proto = ClientError::Protocol("bad json".into());
    for action in [TermAction::List, TermAction::Create, TermAction::Kill] {
        let text = panel_error(action, &proto);
        assert!(!has_latin(&text), "no transport text on the panel: {text}");
        assert!(text.ends_with("하세요."), "{text}");
    }
    assert!(!has_latin(AUTH_REFUSED_TEXT));
}
