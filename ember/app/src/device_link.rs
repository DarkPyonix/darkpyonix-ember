//! The app as a device of a main server over the peer-to-peer transport (SPEC `FR-N9`, #856).
//!
//! - **Device key**: `<data dir>/device.key`, made once by `SecretKey::load_or_generate` (64 hex
//!   characters, mode 0600) and reused. Its peer id is what the owner allows on the server with
//!   `ember-server owner devices add <peer id> <name>` (`FR-N7`). The OS keystore comes later
//!   (`FR-S13`).
//! - **Server address**: the bare peer id, `<peer id>@<ip>:<port>[,<ip>:<port>...]`, or the
//!   `PeerAddr` JSON the server prints at start (`ember server peer addr: {...}`), parsed by
//!   [`parse_server_addr`]. With a fixed transport port (`EMBER_TRANSPORT_LISTEN`, #863) the
//!   server also prints `ember server device address: <peer id>@<host>:<port>`, which is taken
//!   as printed and survives a server restart. The relay is what `EMBER_RELAY_URL` takes ([`parse_relay`]).
//! - **Connection**: [`DeviceLink::bind`] binds the transport with the device key and hands out
//!   an [`Api`] over it ([`DeviceLink::api`]); the `Api` keeps the transport alive.
//! - **Refusals**: [`refusal_text`] turns a refused or failed call into one Korean sentence.
//!
//! No UI here; the 설정 section (`ui/cr/settings.rs`) draws it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ember_client::api::{Api, ApiError};
use ember_transport::{Dialer, PeerAddr, PeerId, RelayConfig, SecretKey, Transport, TransportConfig, TransportError};

/// The device key's file name in the app's data directory.
pub const KEY_FILE: &str = "device.key";

/// Environment variable naming the server's peer address for one run (desktop and debug runs).
pub const SERVER_PEER_ENV: &str = "EMBER_SERVER_PEER";

/// The sentence for a device the server does not admit: not on the allow-list, or revoked.
pub const NOT_ALLOWED: &str = "이 기기가 서버에 등록되어 있지 않거나 등록이 해제되었습니다. 서버에서 ember-server owner devices add <이 기기의 peer id> <이름>으로 이 기기를 추가합니다.";

/// The sentence for a server that could not be reached over the transport.
pub const UNREACHABLE: &str = "서버에 연결하지 못했습니다. 설정의 서버 주소와, 이 기기가 서버에 등록되어 있는지 확인합니다.";

/// The line 설정 shows once a check reached the server.
pub const CONNECTED: &str = "서버에 연결되어 있습니다";

/// A change in 설정 is read at start.
pub const APPLIES_AFTER_RESTART: &str = "앱을 다시 시작하면 적용됩니다";

/// The start of the line the server prints with a fixed transport port (#863).
pub const DEVICE_LINE_PREFIX: &str = "ember server device address:";
/// The start of the server's JSON address line.
pub const PEER_ADDR_PREFIX: &str = "ember server peer addr:";

/// Where the server is on the transport, and the relay to reach it through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSetting {
    pub addr: PeerAddr,
    /// `None` keeps the relay `TransportConfig::from_env` reads (`EMBER_RELAY_URL`, or the
    /// backend's default).
    pub relay: Option<RelayConfig>,
}

impl PeerSetting {
    /// Parses a saved or typed address and relay. An empty relay is `None`.
    pub fn parse(addr: &str, relay: Option<&str>) -> Result<PeerSetting, AddrError> {
        let addr = parse_server_addr(addr)?;
        let relay = match relay.map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) => Some(parse_relay(r)?),
            None => None,
        };
        Ok(PeerSetting { addr, relay })
    }

    /// The base URL the app shows and logs for this reach (`peer:<id>`, as `Api::base_url`).
    pub fn base_url(&self) -> String {
        format!("peer:{}", self.addr.peer)
    }
}

/// Why a server address or relay was not accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddrError {
    #[error("no server address")]
    Empty,
    #[error("not a peer id: {0}")]
    BadPeer(String),
    #[error("not an ip:port address: {0:?}")]
    BadDirect(String),
    #[error("not a peer address JSON: {0}")]
    BadJson(String),
    #[error("not a relay URL letify://<peer id>@host:port: {0:?}")]
    BadRelay(String),
}

impl AddrError {
    /// The Korean line 설정 shows under the field.
    pub fn text_ko(&self) -> &'static str {
        match self {
            AddrError::Empty => "서버 주소를 입력합니다",
            AddrError::BadPeer(_) => "peer id는 64자리 16진수입니다",
            AddrError::BadDirect(_) => "주소는 <peer id>@<IP>:<포트> 형식입니다",
            AddrError::BadJson(_) => "서버가 출력한 주소(JSON)를 읽지 못했습니다",
            AddrError::BadRelay(_) => "릴레이는 letify://<서버 peer id>@<호스트>:<포트> 형식입니다",
        }
    }
}

/// Parses the server's peer address: a bare peer id, `<peer id>@<ip>:<port>[,...]`, or the
/// `PeerAddr` JSON `{peer, relays, direct}` the server prints at start. The server's line
/// prefixes (`ember server device address:`, `ember server peer addr:`) are stripped, so a line
/// is taken as printed; a `<host>` placeholder left in it is not an IP and is refused
/// ([`AddrError::BadDirect`]).
pub fn parse_server_addr(text: &str) -> Result<PeerAddr, AddrError> {
    let mut text = text.trim();
    for prefix in [DEVICE_LINE_PREFIX, PEER_ADDR_PREFIX] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
            break;
        }
    }
    if text.is_empty() {
        return Err(AddrError::Empty);
    }
    if text.starts_with('{') {
        return serde_json::from_str::<PeerAddr>(text).map_err(|e| AddrError::BadJson(e.to_string()));
    }
    let (id, direct) = match text.split_once('@') {
        Some((id, rest)) => (id, Some(rest)),
        None => (text, None),
    };
    let peer = parse_peer(id)?;
    let mut addr = PeerAddr::new(peer);
    if let Some(rest) = direct {
        for part in rest.split(',') {
            let part = part.trim();
            let a = part.parse::<SocketAddr>().map_err(|_| AddrError::BadDirect(part.to_string()))?;
            if !addr.direct.contains(&a) {
                addr.direct.push(a);
            }
        }
    }
    Ok(addr)
}

fn parse_peer(id: &str) -> Result<PeerId, AddrError> {
    id.trim().parse::<PeerId>().map_err(|e| AddrError::BadPeer(e.to_string()))
}

/// Parses the relay as `EMBER_RELAY_URL` takes it (`RelayConfig::parse`): `default`, `none`, or
/// a comma-separated list of `letify://<peer id>@host:port`, each checked here.
pub fn parse_relay(text: &str) -> Result<RelayConfig, AddrError> {
    let relay = RelayConfig::parse(text);
    if let RelayConfig::Custom(urls) = &relay {
        for url in urls {
            let bad = || AddrError::BadRelay(url.clone());
            let rest = url.strip_prefix("letify://").ok_or_else(bad)?;
            let (id, host) = rest.split_once('@').ok_or_else(bad)?;
            parse_peer(id).map_err(|_| bad())?;
            let (h, port) = host.rsplit_once(':').ok_or_else(bad)?;
            if h.is_empty() || port.parse::<u16>().is_err() {
                return Err(bad());
            }
        }
    }
    Ok(relay)
}

/// The device key's path in `data_dir`.
pub fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(KEY_FILE)
}

/// Loads the device key from `data_dir`, or makes it there (mode 0600) on first use.
pub fn load_key(data_dir: &Path) -> Result<SecretKey, TransportError> {
    SecretKey::load_or_generate(key_path(data_dir))
}

/// This device's peer id for the app's own data directory, read once per run (the directory
/// does not change while the app runs). The Korean reason when there is no key.
pub fn this_device(data_dir: Option<&Path>) -> Result<PeerId, String> {
    static ID: OnceLock<Result<PeerId, String>> = OnceLock::new();
    let Some(d) = data_dir else {
        return Err("이 기기에는 기기 키를 둘 폴더가 없습니다".to_string());
    };
    ID.get_or_init(|| {
        load_key(d).map(|k| k.peer_id()).map_err(|e| {
            tracing::warn!("device key in {}: {e}", d.display());
            "기기 키를 읽거나 만들지 못했습니다".to_string()
        })
    })
    .clone()
}

/// The transport configuration for `setting`: the environment's (`TransportConfig::from_env`:
/// backend, relay, letify settings), with the setting's relay when it names one.
pub fn transport_config(key: SecretKey, setting: &PeerSetting) -> TransportConfig {
    let mut cfg = TransportConfig::from_env(key);
    if let Some(relay) = &setting.relay {
        cfg.relay = relay.clone();
    }
    cfg
}

/// The bound transport and where the server is. Clones of [`DeviceLink::api`] keep the
/// transport alive.
#[derive(Debug, Clone)]
pub struct DeviceLink {
    pub dialer: Dialer,
    pub server: PeerAddr,
}

impl DeviceLink {
    /// A link over an already bound transport (the in-memory one in tests).
    pub fn new(transport: Transport, server: PeerAddr) -> DeviceLink {
        DeviceLink { dialer: Dialer::new(transport), server }
    }

    /// Binds the transport with `key` for `setting`.
    pub async fn bind(key: SecretKey, setting: &PeerSetting) -> Result<DeviceLink, TransportError> {
        Self::bind_with(transport_config(key, setting), setting.addr.clone()).await
    }

    /// Binds the transport with `config` (tests shorten its timeouts).
    pub async fn bind_with(config: TransportConfig, server: PeerAddr) -> Result<DeviceLink, TransportError> {
        Ok(Self::new(Transport::bind(config).await?, server))
    }

    /// This device's peer id.
    pub fn peer_id(&self) -> PeerId {
        self.dialer.transport().peer_id()
    }

    /// The server's API over the transport, for `Client::with_api` and `ServerApi`.
    pub fn api(&self) -> Api {
        Api::over_transport(self.dialer.clone(), self.server.clone())
    }
}

/// One Korean sentence for a call that failed over the transport, `None` for a failure that is
/// not about the reach (the caller shows its own text). A device the server closes at accept
/// (code 403, not on the allow-list) or after a revocation (code 401) reads [`NOT_ALLOWED`];
/// a 403 `scope_denied` the FR-N8 sentence; any other transport failure [`UNREACHABLE`]
/// (a refused handshake on the default backend names no code, so the sentence names both).
pub fn refusal_text(e: &ApiError) -> Option<&'static str> {
    if e.is_scope_denied() {
        return Some(crate::ui::device_scope::SCOPE_DENIED);
    }
    match e {
        ApiError::Peer(m) | ApiError::Push(m) => Some(if names_refusal(m) { NOT_ALLOWED } else { UNREACHABLE }),
        _ => None,
    }
}

/// True for transport error text that says the server closed this device out.
fn names_refusal(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["code 403", "code 401", "not allowed", "revoked"].iter().any(|n| t.contains(n))
}

/// Asks the server once (`GET /api/health`); `Err` carries the sentence to show.
pub async fn check(api: &Api) -> Result<(), &'static str> {
    match api.health().await {
        Ok(_) => Ok(()),
        Err(e) => Err(refusal_text(&e).unwrap_or(UNREACHABLE)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> PeerId {
        SecretKey::from_bytes([7; 32]).peer_id()
    }

    fn scratch() -> tempfile::TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }

    #[test]
    fn fr_n9_server_address_forms_parse() {
        let peer = id();
        assert_eq!(parse_server_addr(&format!("  {peer} ")).unwrap(), PeerAddr::new(peer));
        let upper = peer.to_string().to_ascii_uppercase();
        assert_eq!(parse_server_addr(&upper).unwrap().peer, peer, "hex in either case");

        let a = parse_server_addr(&format!("{peer}@192.168.1.5:41234, 10.0.0.2:7443")).unwrap();
        assert_eq!(a.peer, peer);
        assert_eq!(a.direct, vec!["192.168.1.5:41234".parse().unwrap(), "10.0.0.2:7443".parse().unwrap()]);
        assert!(a.relays.is_empty());

        // The line the server prints at start: `ember server peer addr: {...}`.
        let printed = serde_json::to_string(&PeerAddr {
            peer,
            relays: vec![format!("letify://{peer}@203.0.113.4:7443")],
            direct: vec!["203.0.113.4:5000".parse().unwrap()],
        })
        .unwrap();
        let j = parse_server_addr(&printed).unwrap();
        assert_eq!(j.relays.len(), 1);
        assert_eq!(j.direct.len(), 1);
    }

    #[test]
    fn fr_n9_bad_server_addresses_are_refused_with_a_korean_line() {
        assert_eq!(parse_server_addr("  "), Err(AddrError::Empty));
        assert!(matches!(parse_server_addr("abc"), Err(AddrError::BadPeer(_))));
        assert!(matches!(parse_server_addr(&"0".repeat(63)), Err(AddrError::BadPeer(_))));
        let peer = id();
        assert!(matches!(parse_server_addr(&format!("{peer}@mini.local:8740")), Err(AddrError::BadDirect(_))));
        assert!(matches!(parse_server_addr(&format!("{peer}@")), Err(AddrError::BadDirect(_))));
        assert!(matches!(parse_server_addr("{\"peer\": 1}"), Err(AddrError::BadJson(_))));
        // A plain HTTP address is not a peer address (FR-N9: no plain HTTP to a remote host).
        assert!(parse_server_addr("http://192.0.2.1:8740").is_err());
        for e in [
            AddrError::Empty,
            AddrError::BadPeer(String::new()),
            AddrError::BadDirect(String::new()),
            AddrError::BadJson(String::new()),
            AddrError::BadRelay(String::new()),
        ] {
            assert!(!e.text_ko().is_ascii(), "{e:?} reads in Korean");
        }
    }

    #[test]
    fn fr_n9_relay_takes_what_ember_relay_url_takes() {
        let peer = id();
        let url = format!("letify://{peer}@relay.example:7443");
        assert_eq!(parse_relay(&url).unwrap(), RelayConfig::Custom(vec![url.clone()]));
        assert_eq!(parse_relay("none").unwrap(), RelayConfig::Disabled);
        assert_eq!(parse_relay("default").unwrap(), RelayConfig::Default);
        let bad = [
            "https://relay.example".to_string(),
            "letify://nothex@h:1".to_string(),
            format!("letify://{peer}@h"),
            format!("letify://{peer}@:1"),
        ];
        for bad in &bad {
            assert!(matches!(parse_relay(bad), Err(AddrError::BadRelay(_))), "{bad}");
        }
        let s = PeerSetting::parse(&peer.to_string(), Some("  ")).unwrap();
        assert_eq!(s.relay, None, "an empty relay keeps the environment's");
        assert_eq!(s.base_url(), format!("peer:{peer}"));
        let s = PeerSetting::parse(&peer.to_string(), Some(&url)).unwrap();
        assert_eq!(s.relay, Some(RelayConfig::Custom(vec![url])));
        assert!(PeerSetting::parse(&peer.to_string(), Some("https://x")).is_err());
    }

    #[test]
    fn fr_n9_the_printed_device_line_parses_as_printed() {
        let peer = id();
        let line = format!("ember server device address: {peer}@192.168.0.10:7444");
        for text in [line.clone(), format!("  {line}\n")] {
            let a = parse_server_addr(&text).unwrap();
            assert_eq!(a.peer, peer);
            assert_eq!(a.direct, vec!["192.168.0.10:7444".parse().unwrap()]);
        }
        let json = serde_json::to_string(&PeerAddr::new(peer)).unwrap();
        assert_eq!(parse_server_addr(&format!("ember server peer addr: {json}")).unwrap(), PeerAddr::new(peer));
        assert_eq!(PeerSetting::parse(&line, None).unwrap().addr.direct.len(), 1);
        // The placeholder the server prints without a known address is refused until replaced.
        let placeholder = format!("ember server device address: {peer}@<host>:7444");
        assert!(matches!(parse_server_addr(&placeholder), Err(AddrError::BadDirect(_))));
        let noted = format!("{placeholder} (replace <host> with this computer's address or name)");
        assert!(matches!(parse_server_addr(&noted), Err(AddrError::BadDirect(_))));
        assert!(matches!(parse_server_addr("ember server device address: "), Err(AddrError::Empty)));
    }

    #[test]
    fn fr_n9_device_key_is_made_once_with_0600_and_reused() {
        let dir = scratch();
        let data = dir.path().join("ember");
        assert!(!key_path(&data).exists());
        let first = load_key(&data).unwrap();
        assert!(key_path(&data).exists(), "made on first use, the data dir with it");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(key_path(&data)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let text = std::fs::read_to_string(key_path(&data)).unwrap();
        assert_eq!(text.trim().len(), 64);
        let again = load_key(&data).unwrap();
        assert_eq!(first.peer_id(), again.peer_id(), "reused, not replaced");
        assert_eq!(std::fs::read_to_string(key_path(&data)).unwrap(), text);
    }

    #[test]
    fn fr_n9_transport_config_uses_the_device_key_and_the_saved_relay() {
        let key = SecretKey::from_bytes([9; 32]);
        let peer = id();
        let url = format!("letify://{peer}@relay.example:7443");
        let s = PeerSetting::parse(&peer.to_string(), Some(&url)).unwrap();
        let cfg = transport_config(key.clone(), &s);
        assert_eq!(cfg.secret_key.peer_id(), key.peer_id());
        assert_eq!(cfg.relay, RelayConfig::Custom(vec![url]));
        let s = PeerSetting { relay: Some(RelayConfig::Disabled), ..s };
        assert_eq!(transport_config(key, &s).relay, RelayConfig::Disabled);
    }

    #[test]
    fn fr_n9_refusals_read_as_korean_sentences() {
        let closed = ApiError::Peer("connection closed: closed by peer (code 403: peer not allowed)".into());
        assert_eq!(refusal_text(&closed), Some(NOT_ALLOWED));
        let revoked = ApiError::Peer("connection closed: closed by peer (code 401: device revoked)".into());
        assert_eq!(refusal_text(&revoked), Some(NOT_ALLOWED));
        let lost = ApiError::Peer("cannot connect to PeerId(ab): no address is known for the peer".into());
        assert_eq!(refusal_text(&lost), Some(UNREACHABLE));
        let scope = ApiError::Status { status: 403, message: "x".into(), code: Some("scope_denied".into()) };
        assert_eq!(refusal_text(&scope), Some(crate::ui::device_scope::SCOPE_DENIED));
        let other = ApiError::Status { status: 500, message: "boom".into(), code: None };
        assert_eq!(refusal_text(&other), None);
    }
}
