# 1.0 이후 설계: 원격 브라우저 (#12, M6, SPEC §R)

상태: **제안.** 이 문서는 설계 노트이고 코드, SPEC, INTENT를 바꾸지 않는다. 서버와 노드 쪽은
구현되어 있고(#29, #44, #344), 클라이언트 뷰어와 실제 수용 실행은 없다(#12). 6절의 질문은
리더에게 올리는 제안이고 결정된 사실이 아니다. 작성 기준: `develop` 8ea03cb9, 2026-11-20.
읽은 것만 사실로 적고, 확인하지 못한 것은 "확인 못 함"으로 적는다. 빌드와 테스트는 돌리지
않았다(AGENTS.md: 문서 서브 에이전트는 빌드하지 않는다).

## 1. 범위

안에 있는 것:

- `FR-R5`: 클라이언트가 원격 브라우저를 보여 주고 조작한다(프레임 표면, 입력, takeover UI,
  egress 선택, 프로필 지우기). 데스크톱과 Android가 대상이다.
- `FR-R1`부터 `FR-R4`의 수용 실행 세 가지(egress IP, egress 전환 뒤 로그인 유지, 에이전트 작업
  중 takeover)와 그 실행을 막는 서버 쪽 빈 곳.
- 에이전트가 브라우저 도구를 받는 경로의 기본값과 버전 고정.
- 이슈 완료 기준 4번(E1: 런처와 대화 화면에 웹뷰 없음)을 뷰어 설계가 지키는 방법.

밖에 있는 것:

- 에디터 창의 확장 웹뷰(`FR-E8`, D44, `docs/design/EDITOR-WEBVIEWS.md`). 원격 브라우저 뷰어는
  웹뷰가 아니고 그 호스트 모듈을 쓰지 않는다. 섞으면 `check-no-webview.sh`의 예외 경로가
  넓어지기 때문이다.
- 에이전트 자신의 API 트래픽 egress(`FR-X6`, D19, `ember/server/src/egress_proxy.rs`). 같은 노드
  `/egress`를 쓰지만 별개 기능이다.
- 휴대폰을 egress로 쓰는 것. 휴대폰에는 `ember-node`가 없다(`docs/design/MOBILE-NO-NODE.md`).
- 영상 코덱 스트림(H.264, WebRTC), 소리, 여러 사용자가 각자 다른 탭을 보는 것. 4.5절에 기각
  이유를 적는다.
- iOS. iOS 클라이언트가 생긴 뒤에 Android 단계를 따라간다(`FR-L36`이 iOS를 제외한다).

## 2. 현재 있는 것

### 2.1 코드에서 확인

| 무엇 | 어디 | 비고 |
| --- | --- | --- |
| 프로젝트마다 headless Chromium 하나, 1280×800 | `ember/server/src/browser/chrome.rs`, `instance.rs` | `--headless=new`, `--proxy-bypass-list=<-loopback>` |
| 프로필 위치와 지우기 | `browser/mod.rs`, `browser/api.rs` (`DELETE /api/browsers/{project}/data`) | `<EMBER_DATA_DIR>/browser/<project>/profile` |
| egress 선택과 영속화 | `browser/egress.rs`, `store.rs` | `direct`, `proxy`, `computer` 세 종류 |
| 컴퓨터별 loopback SOCKS5 리스너 | `ember/server/src/computers/egress.rs`, `computers/mod.rs` (`egress_proxy`) | 인증 없는 127.0.0.1 바인드 |
| 노드의 SOCKS5 출구 | `ember/node/src/egress.rs` (715줄) | `CONNECT`만, 정책 `EMBER_NODE_EGRESS_DENY` |
| egress 전환 시 재시작 | `instance.rs` (`Browser.close` 뒤 같은 프로필로 재기동) | 해석 실패 시 direct로 떨어지지 않음 |
| 뷰 스트림 | `browser/api.rs` (`GET /api/browsers/{project}/view`, WebSocket) | JSON 상태와 `[u32 길이][JSON 머리][JPEG]` 프레임 |
| 입력 | `instance.rs` (`InputEvent`), `POST …/input` | mouse, wheel, key, text, navigate, reload, back, forward, takeover, select_tab, screencast |
| CDP 릴레이와 takeover 대기 | `browser/cdp.rs`, `instance.rs` (`agent_command`) | takeover 중 에이전트 명령이 대기한다 |
| 에이전트용 MCP 설정 | `browser/agent.rs` | `EMBER_BROWSER_MCP=chrome-devtools\|playwright\|off`, 기본 `off` |
| 에이전트 릴레이 인증 | `ember/server/src/auth.rs` (`agent_cdp_nonce`, `agent_cdp_path_ok`) | 실행마다 nonce를 경로에 넣는다 |
| 기기 scope | `ember/server/src/devices/scope.rs` (95행 근처) | 제한 scope는 `view`, `input`, `cdp`, `agent-config`를 못 쓴다 |
| owner 전용 경로 | `ember/client/src/owner.rs` (`PUT /api/browsers/{project}/egress`) | 브라우저 경로 중 이것 하나만 목록에 있다 |
| Origin 검사 | `browser/api.rs` 195행, 264행 (`check_origin`) | 두 WebSocket 업그레이드 |

테스트(이름은 파일에서 읽음, 실행하지 않음):

- `ember/server/tests/browser.rs`: `cookies_survive_egress_switch_and_clear_removes_them`,
  `view_stream_frames_and_click`, `agent_relay_activity_and_takeover`. 실제 Chrome을 띄우고,
  egress는 테스트 안의 SOCKS5 서버다.
- `ember/server/tests/browser_egress.rs`: `loopback_listener_egresses_through_the_node`,
  `unreachable_node_closes_the_proxied_connection`. 리스너에서 노드를 거쳐 TCP echo까지이고
  Chrome은 없다.
- `ember/node/tests/egress.rs`: `socks5_connect_through_the_node_stream`,
  `token_and_policy_are_enforced`.
- `ember/server/src/browser/chrome.rs`의 단위 테스트 `loopback_goes_through_proxy`.

MCP 서버 목록(`StartRequest::mcp_servers`)을 읽는 어댑터는 `claude_code.rs`, `codex.rs`,
`antigravity.rs`, `acp.rs`다. Antigravity와 ACP가 브라우저 MCP로 실제 페이지를 조작하는지는
확인 못 함(목록을 읽는 코드만 봤다).

### 2.2 문서에만 있음

- `docs/design/REMOTE-BROWSER.md`가 프로토콜을 적는다. 내용은 위 코드와 맞는다. 다만 두 줄이
  오래됐다(3절 G12).
- PROJECT.md 56행과 M6 마일스톤 설명은 "dioxus-compose의 frame-surface 위젯"에 뷰어가 달려
  있다고 적는다. compose-rust와 dioxus-compose의 `docs/SPEC.md`에서 그런 위젯 항목을 찾지
  못했다(`frame surface`, `FrameSurface`, `texture`, `video`로 검색. `Video`는 아이콘 태그 17뿐).

### 2.3 툴킷 쪽에서 쓸 수 있는 것 (compose-rust `docs/SPEC.md`, 문서로만 확인)

- `FR-16` 에셋: `asset(kind, bytes)`, `RegisterAsset`, `ReleaseAsset`. 같은 바이트는 한 번만
  등록된다. `Image`는 `ContentScale`(`Fit`, `Crop`, `FillBounds`)을 받는다.
- `FR-46` 노드 단위 포인터 이벤트(`Move`, `Down`, `Up`, `Wheel`, `ContextMenu`, 캡처), `Agreed`.
- `FR-12` `KeyDown` 소비, `FR-45` `OnKeyUp`과 `OnFocusGained`, `Agreed`. IME 조합 중에는 키가
  Host로 오지 않는다(`FR-12`, D5).
- `FR-78` 클립보드 읽기(`Agreed`), `FR-73` 클립보드 쓰기(`Draft`).
- `FR-81` `NativeView`: 플랫폼 뷰를 노드 사각형에 놓는 자리(`Agreed`, 에디터 웹뷰용).
- `FR-39` `#[Composable]`과 호출 매크로.

`Agreed`는 합의된 설계라는 뜻이고 구현 여부는 항목마다 다르다. 위 항목의 구현 상태는 확인 못
함.

## 3. 빈 곳

| ID | 빈 곳 | 걸리는 SPEC / 완료 기준 |
| --- | --- | --- |
| G1 | 클라이언트에 뷰어가 없다. `ember/app/src`와 `ember/client/src`에서 브라우저 관련 코드는 `owner.rs`의 경로 한 줄뿐이다. | `FR-R5`, 기준 3 |
| G2 | `ember/client`에 `/api/browsers` REST와 뷰 WebSocket 코덱이 없다. | `FR-R5` |
| G3 | 프레임 표면 위젯이 툴킷에 없다(2.2). | `FR-R5` |
| G4 | 실제 Chrome, 실제 리스너, 실제 노드를 한 번에 잇는 테스트가 없다. 조각만 따로 검증된다. | `FR-R1`, 기준 1 |
| G5 | 수용 실행 세 가지가 실행되지 않았다(SPEC 상태표 70행). | 기준 1, 2, 3 |
| G6 | `browser.rs`의 테스트는 Chrome이 없으면 `skipping`을 찍고 통과한다. `.github/workflows`에 Chrome 설치 단계가 없어서 CI에서 실제로 돌았는지는 러너 이미지에 달려 있다. 확인 못 함. | `FR-R1`부터 `FR-R4`의 "CI" 표기 |
| G7 | takeover는 불리언 하나다. 어느 뷰어든 켜고 끌 수 있고, 켠 뷰어가 끊겨도 풀리지 않는다(`input` 밖에서 해제 코드를 찾지 못했다). 에이전트 명령은 무기한 대기한다. | `FR-R3` |
| G8 | 터치(`Input.dispatchTouchEvent`), 뷰어별 뷰포트, 클립보드, 파일 업로드, 다운로드, JS 대화상자 처리가 없다(`instance.rs`에서 해당 CDP 호출을 찾지 못했다). | `FR-R5`, 기준 3 |
| G9 | Chrome 실행 인자에 WebRTC와 QUIC 관련 플래그가 없다. SOCKS5 출구는 `CONNECT`(TCP)만 받으므로 프록시를 거치지 않는 UDP가 서버의 IP를 드러낼 수 있다. 실제로 새는지는 재지 않았다. | `FR-R1` |
| G10 | 브라우저 MCP는 서버 단위 환경 변수이고 기본이 꺼져 있다. 프로젝트별 설정과 UI가 없다. 패키지는 `@latest`로 받는다. | `FR-R3`, AGENTS.md 규칙 4 |
| G11 | 프로필 디렉터리는 `create_dir_all`로 만들고 권한을 따로 좁히지 않는다(stderr 로그만 `0600`). | `FR-R2` |
| G12 | `REMOTE-BROWSER.md`가 "The ember API itself has no auth yet"와 "노드 스트림은 plain HTTP WebSocket"이라고 적는다. `auth.rs`(D37)와 `NodeClient::over_transport`가 이미 있다. | 문서 정합 |
| G13 | `FR-R5`의 "web" 클라이언트가 무엇인지 정해지지 않았다. `ember/` 아래에서 웹 클라이언트를 찾지 못했다. | `FR-R5` |
| G14 | 서버에 Chrome이 없을 때의 경로가 오류 메시지뿐이다. 설치 안내와 클라이언트 표시가 없다. | `FR-R5` |

SPEC과 코드가 어긋나는 곳: `FR-R1`은 egress가 "transport stream(`FR-N5`)으로 운반된다"고 적는다.
리스너는 `NodeClient`의 WebSocket을 쓰고, `NodeClient`는 HTTP와 transport 두 경로를 가진다.
transport 경로로 `/egress`를 검증하는 테스트는 찾지 못했다(`browser_egress.rs`와
`node/tests/egress.rs`는 HTTP 노드를 쓴다).

## 4. 제안 구조

### 4.1 전체 흐름

```text
 ember/app (뷰어 창)                 ember server                        ember node X
 ┌───────────────────────┐          ┌──────────────────────────┐        ┌──────────────┐
 │ BrowserSurface        │◀─frames──│ /api/browsers/{p}/view   │        │              │
 │ (프레임 표면)          │──input──▶│  WebSocket               │        │              │
 │ 툴바: 주소, 탭, egress │──REST───▶│ /api/browsers/{p}[/...]  │        │              │
 │ takeover, 에이전트 표시│          │                          │        │              │
 └──────────┬────────────┘          │ BrowserInstance          │        │              │
            │ ember/client::browser │  └ Chrome (headless)     │        │              │
            │ (코덱, 상태, 재연결)   │       │ socks5://127.0.0.1│        │              │
            └── transport(FR-N5) ───│       ▼                  │        │              │
                                    │ EgressListener ──/egress─┼─FR-N5─▶│ SOCKS5 출구  │─▶ 대상
 agent CLI ── MCP(stdio) ── CDP ───▶│ /api/browsers/{p}/cdp/<n>│        │ (DNS도 여기) │
                                    └──────────────────────────┘        └──────────────┘
```

서버 쪽 프로토콜은 그대로 둔다. 필드는 더하기만 한다(D15: 버전 필드 없음, 모르는 필드 무시).

### 4.2 구성 요소와 위치

1. **`ember/client/src/browser.rs` (새 모듈, UI 없음).** REST 호출, 뷰 WebSocket, 프레임 머리
   파싱, 입력 메시지 직렬화, 재연결을 맡는다. 좌표 환산(`deviceWidth / jpegWidth`)도 여기 둔다.
   UI와 떼어 두는 이유: 툴킷 API 이전과 무관하게 지금 쓸 수 있고, `MemNetwork`와 가짜 서버로
   시험할 수 있다. 메시지 예시는 `ember/vectors/`에 두어 서버 테스트와 같은 벡터를 쓴다.
2. **`ember/app`의 뷰어 화면.** 데스크톱은 별도 창, 휴대폰은 전체 화면 경로를 제안한다(Q-R1).
   구성: 프레임 표면, 주소 줄, 뒤로/앞으로/새로 고침, 탭 목록(`ViewState.tabs`), egress 선택,
   "에이전트 동작 중" 표시(`agent_active`), takeover 버튼, 프로필 지우기, 입력 줄(IME용).
   **이 UI는 12월의 호출 매크로 이전(builder API 제거) 뒤에 호출 매크로로 쓴다.** builder API로
   먼저 쓰면 바로 다시 써야 하기 때문이다.
3. **프레임 표면.** 4.3절.
4. **`ember/server/src/browser/`의 보강.** takeover 보유자, 터치와 대화상자 입력, 누출 차단
   플래그, 프로젝트별 에이전트 설정. 모두 기존 모듈 안의 추가다.
5. **진입점.** 대화 화면 헤더에 "브라우저" 버튼을 두고, 에이전트가 브라우저를 쓰는 동안 세션에
   표시를 단다. 버튼은 뷰어 창을 열 뿐이고 대화 화면 안에 프레임을 그리지 않는다. 대화 화면의
   재구성을 프레임마다 일으키지 않기 위해서다.

### 4.3 프레임 표면: 세 가지 길

| 길 | 방법 | 장점 | 단점 |
| --- | --- | --- | --- |
| A | 프레임마다 `asset(kind, bytes)`로 등록하고 `Image`가 그 id를 가리킨다. 이전 프레임은 `ReleaseAsset`. | 툴킷 변경이 없다(문서 기준). | 프레임마다 JPEG가 배치를 건너고 Host 재구성이 돈다. 비용을 잰 적이 없다. |
| B | compose-rust에 새 위젯(가칭 `FrameSurface`)을 요청한다. Host가 노드에 최신 프레임만 밀어 넣고 Renderer가 디코드해 그린다. | 재구성 없이 표면만 갱신된다. 휴대폰에서도 같은 코드다. | 툴킷 SPEC 항목, 이슈, 구현이 필요하다. 지금 항목이 없다. |
| C | `FR-81` `NativeView`에 Ember가 만든 플랫폼 이미지 레이어를 붙인다. | 웹뷰가 아니다. 프레임 경로가 짧다. | 플랫폼 다섯 개마다 코드가 필요하고, 겹침과 입력이 뷰 쪽으로 넘어간다. |

제안: A를 먼저 재고(작업 S6), 예산(7절 `NFR-RB1` 제안)을 못 맞추면 B를 요청한다. C는 A와 B가
둘 다 안 될 때의 대안으로만 둔다. 플랫폼별 코드가 `NFR-L3`(모든 OS에서 한 설계)과 맞지 않기
때문이다.

세 길 모두 웹뷰를 만들지 않는다. 따라서 E1과 `NFR-L2`는 뷰어가 어느 창에 있든 지켜진다.
`ember/app/tests/no_webview.rs`와 `check-no-webview.sh`는 `ember/app`과 `ember/client` 전체를
훑으므로 새 모듈도 자동으로 검사된다.

### 4.4 입력

- 포인터: `FR-46`의 `Move`, `Down`, `Up`, `Wheel`, `ContextMenu`를 `mouse`와 `wheel` 메시지로
  바꾼다. `Move`는 프레임 간격에 맞춰 솎는다. 릴레이 경로에서 입력이 밀리지 않게 하기 위해서다.
- 키: 표면 노드의 `KeyDown`과 `OnKeyUp`을 `key` 메시지로 바꾼다. 표면 같은 비텍스트 노드가
  포커스와 키를 받는지는 확인 못 함(compose-rust SPEC 1722행은 컨테이너 `on_key_down`이 없다고
  적은 적이 있다). 안 되면 입력 줄이 키도 받는다.
- 글자(IME, 휴대폰 키보드): 툴킷이 조합 중인 글자를 Host에 보내지 않으므로, 뷰어의 입력 줄
  (`TextField`)에서 확정된 글자를 `text` 메시지(`Input.insertText`)로 보낸다.
- 터치: 서버에 `touch` 메시지를 더한다(`Input.dispatchTouchEvent`). 그 전까지 휴대폰은 탭을
  `mouse`로 보낸다(지금 설계 문서의 방식).
- 클립보드: 붙여넣기는 `FR-78`로 읽어 `text`로 보낸다. 복사는 서버에 선택 글자를 돌려주는
  메시지가 필요하다(없음, 작업 S5).
- takeover: 4.6절.

### 4.5 검토한 대안과 기각 이유

- **뷰어를 웹뷰로 만든다.** 기각. 런처 프로세스에 웹뷰가 생기고 E1, `NFR-L2`, AGENTS.md 규칙
  5를 어긴다. 에디터 창의 웹뷰 예외(D44)는 확장 콘텐츠에만 열려 있다.
- **사용자 OS의 브라우저로 서버 페이지를 연다.** 보류(Q-R3). 웹뷰는 아니지만 owner 리스너의
  인증을 브라우저에 넘겨야 하고, 휴대폰에서는 앱을 떠난다.
- **H.264나 WebRTC 스트림.** 기각(지금은). headless Chrome의 screencast는 JPEG와 PNG만 주고,
  인코더 의존성과 영상 표면이 둘 다 새로 필요하다. JPEG로 예산을 못 맞출 때 다시 본다.
- **Chrome을 egress 컴퓨터에서 띄운다.** 기각. 프로필이 그 컴퓨터로 가므로 `FR-R2`와 D6("데이터는
  메인 서버")을 어긴다.
- **Ember가 자체 브라우저 MCP를 만든다.** 기각(지금은). E2(에이전트를 고치지 않고 설정만)와
  기성 MCP 두 개가 이미 연결되어 있다. 버전 고정으로 위험을 줄인다(Q-R5).

### 4.6 takeover 모델

제안: takeover에 보유자(뷰어 연결 id)를 둔다. 보유자만 끌 수 있고, 보유자의 WebSocket이 끊기면
서버가 푼다. 보유한 채 입력이 없는 시간이 한도를 넘으면 상태에 경고를 싣고, 더 지나면 푼다.
이유: 지금은 휴대폰이 잠들면 에이전트의 도구 호출이 끝없이 멈춘다(G7). `ViewState`에
`takeover_by`와 `takeover_since`를 더한다(추가 필드, D15에 맞음).

## 5. 위험

| # | 위험 | 완화 |
| --- | --- | --- |
| 1 | 길 A의 프레임 비용이 커서 입력 지연이 길다. | S6에서 먼저 잰다. 못 맞추면 B를 요청하고 UI 단계를 그 뒤로 미룬다. |
| 2 | 릴레이 경로의 대역폭. 1280×800 JPEG 품질 70은 프레임당 수십에서 수백 KB로 추정한다(재지 않음). | 뷰어가 `screencast` 메시지로 품질과 크기를 낮춘다. 서버는 느린 뷰어의 프레임을 이미 버린다(`broadcast::channel(4)`, `Lagged`). 경로 상태(`FR-N5`)가 relayed면 기본값을 낮춘다. |
| 3 | 실제 사이트가 headless 또는 자동화 Chrome을 막아 로그인 수용 기준이 실패한다. | 수용 실행의 사이트를 소유자가 정한다(Q-R10). 막는 사이트를 기록한다. |
| 4 | UDP 누출로 서버 IP가 보인다(G9). | 프록시를 거치지 않는 UDP를 막는 플래그를 넣고 실제 Chrome에서 누출 검사 페이지로 확인한다(Q-R11). 플래그 동작은 가정하지 않고 통합 테스트로 확인한다(AGENTS.md 규칙 4). |
| 5 | 에이전트가 로그인된 프로필로 사용자의 권한을 쓴다. | 프로젝트별 opt-in, 기본 꺼짐(Q-R4). "에이전트 동작 중" 표시와 takeover. |
| 6 | `@latest` MCP 패키지가 바뀌어 동작이 달라지거나 공급망 위험이 생긴다. | 버전을 고정하고 SPEC에 적는다(Q-R5). |
| 7 | takeover 동안 MCP 도구 호출이 시간 초과로 실패한다. | 4.6의 자동 해제. 에이전트 쪽 시간 초과 값은 실제 CLI로 확인한다(확인 못 함). |
| 8 | loopback SOCKS5 리스너는 인증이 없어 서버의 다른 프로세스가 그 컴퓨터의 egress를 쓸 수 있다. | 지금은 문서화된 신뢰 경계다. 브라우저가 멈추면 리스너를 내리는 것을 S3에 넣는다. |
| 9 | 12월 호출 매크로 이전과 UI 작업이 겹친다. | UI 단계(S8 이후)를 이전 뒤로 고정한다. S1부터 S7은 UI가 없어 먼저 한다. |
| 10 | 공유 뷰포트라 휴대폰에서 데스크톱 배치가 보인다. | v1은 확대와 이동으로 버틴다(Q-R7). |
| 11 | 서버에 Chrome이 없다. | 클라이언트가 이유와 설치 방법을 보여 준다(Q-R9). |

## 6. 소유자 결정 필요

- **Q-R1. 뷰어는 어디에 뜨는가?** (a) 데스크톱은 별도 네이티브 창, 휴대폰은 전체 화면 경로.
  (b) 대화 화면 안의 패널. (c) 최상위 탭 하나. 추천: (a). 이슈 본문이 "native viewer window"라고
  적고, 프레임 갱신이 대화 화면을 재구성하지 않는다.
- **Q-R2. 프레임 표면은 어느 길인가?** A(에셋 교체), B(compose-rust 새 위젯), C(`NativeView`).
  추천: A를 재고 예산을 못 맞추면 B. 툴킷 변경 없이 시작할 수 있고, 숫자가 B의 근거가 된다.
- **Q-R3. `FR-R5`의 "web"은 무엇인가?** (a) SPEC에서 뺀다. (b) 서버가 주는 뷰어 페이지를
  사용자의 브라우저로 연다. (c) 미룬다. 추천: (c), `FR-R5`를 데스크톱과 Android로 좁히고 web은
  별도 행으로 둔다. 웹 클라이언트가 지금 없기 때문이다.
- **Q-R4. 에이전트의 브라우저 사용 기본값은?** (a) 지금처럼 서버 환경 변수, 기본 꺼짐. (b)
  프로젝트별 스위치(owner 전용 경로), 기본 꺼짐. (c) 기본 켜짐. 추천: (b). 로그인된 프로필은
  권한이므로 프로젝트 단위로 사람이 켠다.
- **Q-R5. MCP 패키지 버전은?** (a) `@latest`. (b) 고정 버전을 SPEC에 적고 올릴 때 통합 테스트를
  돌린다. (c) 자체 MCP. 추천: (b). 실제 동작에 맞춰 통합한다는 규칙 4와 맞는다.
- **Q-R6. 뷰어가 여럿일 때 takeover는?** (a) 지금처럼 누구나 켜고 끈다. (b) 보유자 한 명,
  끊기면 자동 해제. 추천: (b). 에이전트가 끝없이 멈추는 경우를 없앤다.
- **Q-R7. 휴대폰의 뷰포트는?** (a) 공유 1280×800을 유지하고 확대와 이동. (b) takeover 보유자가
  뷰포트를 바꾼다(모바일 에뮬레이션). 추천: v1은 (a). 에이전트가 보는 배치가 작업 중에 바뀌지
  않는다.
- **Q-R8. 프로필 지우기는 owner 전용인가?** 지금 owner 전용 목록에는 egress 변경만 있다. (a)
  그대로. (b) `DELETE …/data`도 owner 전용. 추천: (b). 모든 로그인을 지우는 되돌릴 수 없는
  동작이다.
- **Q-R9. Chrome은 어떻게 마련하는가?** (a) 설치된 Chrome만 쓰고 없으면 안내. (b) 처음 쓸 때
  Chrome for Testing을 받는다. (c) 번들. 추천: (a) 먼저. 다운로드와 번들은 배포 크기와 갱신
  책임을 더한다.
- **Q-R10. 로그인 수용 실행은 어느 사이트로 하는가?** (a) 저장소의 테스트 사이트만. (b) 소유자가
  고른 실제 사이트 하나 이상. 추천: (b)와 (a) 둘 다. 실제 사이트가 headless를 막는지가 이 기능의
  쓸모를 정한다.
- **Q-R11. 프록시를 거치지 않는 UDP(WebRTC)를 막는가?** (a) 막는다. 일부 화상 통화 사이트가
  안 된다. (b) 둔다. 추천: (a). `FR-R1`의 뜻이 "페이지가 그 컴퓨터의 IP를 본다"이기 때문이다.
- **Q-R12. 프로필을 암호화하는가?** (a) 디렉터리 권한 `0700`만. (b) 저장 시 암호화. 추천: (a).
  서버의 토큰 저장소와 같은 신뢰 경계이고, Chrome 프로필 암호화는 별도 설계가 필요하다.
- **Q-R13. 지연 예산 숫자를 SPEC에 넣는가?** 7절의 `NFR-RB1` 값은 재지 않은 초안이다. 추천:
  S6의 측정 뒤에 숫자를 정해 넣는다.

## 7. SPEC/INTENT 변경 제안

적용하지 않았다. 아래는 제안 문안이고 모두 [provisional]이다. `NFR-R1`, `NFR-R2`가 이미 릴리스
항목이라 원격 브라우저의 비기능 항목은 `NFR-RB`로 적었다. 접두어는 리더가 정한다.

> **FR-R3a** *[provisional, #12]* Takeover has one holder: the viewer connection that turned it
> on. Only the holder (or an owner-presence request) turns it off. The server releases it when the
> holder's view stream closes, and after 10 minutes with no input from the holder. `ViewState`
> carries `takeover_by` and `takeover_since`. | A viewer takes over and its connection drops: the
> agent's held command proceeds within 5 s (`fr_r3_takeover_releases_when_holder_disconnects`).

> **FR-R1a** *[provisional, #12]* With a computer or proxy egress, the browser sends no traffic
> that bypasses the proxy: non-proxied UDP (WebRTC, QUIC) is disabled. | A WebRTC leak page shows
> no address of the main server; the launch arguments carry the flags (unit test) and a real
> Chrome run confirms it.

> **FR-R5** (changed) *[provisional, #12]* The remote browser is available from the desktop
> client and the Android client. The viewer draws screencast frames on a compose-rust surface and
> never allocates a webview (`NFR-L2`). | Verified from the phone client: open a page, type with
> the on-screen keyboard, take over and hand back.
>
> **FR-R5w** *[provisional, #12]* A web client for the remote browser. Not implemented, no design.

> **FR-R6** *[provisional, #12]* The viewer: frame surface, address bar, back, forward, reload,
> tab list, egress chooser, an "agent is active" indicator, a takeover control, clear data. Input:
> pointer, wheel, keys, committed text (IME and on-screen keyboards), touch on phones, paste. |
> `fr_r6_*` codec tests in `ember/client`; the viewer screen has a `#[Preview]` entry.

> **FR-R7** *[provisional, #12]* Agent browser use is switched per project (owner route), off by
> default. The MCP server package and version are fixed in this row: `chrome-devtools-mcp@<x.y.z>`
> (default), `@playwright/mcp@<x.y.z>`. | With the switch off an agent start carries no
> `ember-browser` server; with it on, an integration test with the real CLI lists the browser
> tools.

> **FR-R8** *[provisional, #12]* JavaScript dialogs, file choosers and downloads have a defined
> behaviour: dialogs are shown in the viewer and answered there; a file chooser takes a file sent
> through `FR-F*` transfers; downloads are saved under the project's browser directory on the main
> server and listed. | One test per case against a real Chrome.

> **NFR-RB1** *[provisional, #12, numbers not measured]* From an input event in the viewer to the
> frame that shows its effect: p50 at most 150 ms on a direct path, at most 400 ms relayed. If the
> desktop viewer misses this, the reason is recorded and `FR-R5` on Android waits.

INTENT 제안:

> **D6 amendment** *[provisional, #12]* The viewer is native: it draws frames the main server
> streams and holds no web engine in the client. Rejected: a webview viewer (E1), running Chrome on
> the egress computer (profile would leave the main server), a video codec stream (no encoder or
> video surface today). Agent browser use is opt-in per project because a logged-in profile is
> authority. Takeover has one holder and releases itself when the holder leaves.

## 8. 작업 분해

추정은 에이전트 세션 하나의 작업일이고 거칠다. 빌드와 테스트 실행은 builder 서브 에이전트나
리더가 dispatch한 CI가 한다. S6의 결과에 따라 S8 이후가 바뀐다.

| # | 무엇 | 크레이트 | 증명하는 테스트 | 일 | 소유자 손 |
| --- | --- | --- | --- | --- | --- |
| S1 | 결정 반영: SPEC 행과 INTENT 수정(7절), `REMOTE-BROWSER.md`의 오래된 두 줄(G12) | `docs/` | 문서 검사(`check-no-em-dash.sh`) | 0.5 | Q-R 답 |
| S2 | takeover 보유자와 자동 해제(4.6) | `ember/server` | `fr_r3_takeover_releases_when_holder_disconnects`, `fr_r3_only_the_holder_hands_back` | 1 | 없음 |
| S3 | UDP 누출 차단 플래그, 프로필 `0700`, 브라우저 정지 시 리스너 내리기, Chrome 없음을 CI에서 실패로 만드는 스위치(가칭 `EMBER_REQUIRE_CHROME`) | `ember/server`, `.github/` | `fr_r1a_launch_args_block_non_proxied_udp`, 스위치가 켜진 CI 실행에서 `browser.rs`가 건너뛰지 않음 | 1 | 없음 |
| S4 | 실제 Chrome에서 리스너와 실제 `ember-node` 프로세스를 거쳐 테스트 사이트까지 가는 종단 테스트. HTTP 노드와 transport 노드 둘 다 | `ember/server`, `ember/node` | `fr_r1_chrome_egresses_through_a_real_node`, `fr_r2_login_survives_a_switch_between_two_nodes` | 1.5 | 없음 |
| S5 | 서버 입력 보강: `touch`, 대화상자, 파일 선택, 다운로드, 선택 글자 복사 | `ember/server` | `fr_r8_*` (실제 Chrome) | 2 | 없음 |
| S6 | 프레임 표면 측정: 길 A로 10, 30 fps에서 CPU와 입력 지연을 잰다. 결과로 Q-R2와 Q-R13을 정하고, 필요하면 compose-rust 이슈를 연다 | `.scratch/`, `ember/app` (예제) | 측정 기록(숫자와 방법) | 1 | 결과 검토 |
| S7 | `ember/client::browser`: REST, 뷰 WebSocket 코덱, 재연결, 공유 벡터 | `ember/client`, `ember/vectors` | `fr_r6_frame_header_round_trips`, `fr_r6_input_messages_match_shared_vectors`, `fr_r6_view_stream_reconnects` | 1.5 | 없음 |
| S8 | 뷰어 창(데스크톱): 표면, 툴바, 탭, egress 선택, 표시, takeover, 지우기. **호출 매크로 이전 뒤, 호출 매크로로 작성** | `ember/app` | `fr_r6_viewer_*` UI 테스트, `no_webview.rs`, `#[Preview]` 항목 | 3 to 4 | 화면 확인 |
| S9 | 입력 연결: 포인터, 휠, 키, 입력 줄(IME), 붙여넣기 | `ember/app`, `ember/client` | `fr_r6_pointer_maps_to_viewport_css_pixels`, `fr_r6_committed_text_is_sent_once` | 2 | 한글 IME 확인 |
| S10 | 진입점: 대화 헤더 버튼, 세션의 에이전트 표시, 프로젝트별 에이전트 스위치(`FR-R7`)와 MCP 버전 고정 | `ember/app`, `ember/server`, `ember/client` | `fr_r7_switch_off_adds_no_browser_server`, 실제 CLI 통합 테스트 | 2 | 없음 |
| S11 | Android: 전체 화면 경로, 터치, 화면 키보드, 확대와 이동 | `ember/app` | `fr_r5_*` 과 Flip 4 실행 | 2 | 휴대폰 확인 |
| S12 | 수용 실행: 공인 IP가 다른 두 번째 컴퓨터로 egress IP와 LAN 페이지, 실제 사이트 로그인과 전환, 에이전트 폼 입력 중 takeover, 휴대폰 | 없음(실행과 기록) | SPEC 수용 기준 그대로, 실제 출력 기록 | 1 | 두 번째 컴퓨터, 실제 계정, 휴대폰 |
| S13 | SPEC 상태표와 `REMOTE-BROWSER.md` 갱신, #12 닫기 | `docs/` | 없음 | 0.5 | 없음 |

의존: S2부터 S5와 S7은 서로 독립이고 지금 시작할 수 있다. S6은 Q-R2 전에 한다. S8은 S6, S7과
12월 이전에 달려 있다. S9, S10, S11은 S8 뒤다. S12는 전부 뒤다. 합계는 대략 19에서 20일이고,
길 B가 필요해지면 툴킷 쪽 작업이 여기에 더해진다(이 문서에서 추정하지 않는다).

검증 메모: S12 전에는 기준 1, 2, 3을 "검증됨"으로 적지 않는다. 두 번째 컴퓨터, 실제 네트워크,
화면이 필요한 확인은 사람이 해야 하고 그 전에는 "아직 검증 안 됨"으로 보고한다(AGENTS.md
Verification).
