# POST-1-0-MOBILE-NO-NODE.md: Node 없는 모바일 IDE 재기준선 (`FR-W5`, #80)

> **상태: 설계 문서, 2026-11-20. 코드는 없다.** #80은 열려 있고, SPEC 상태표는 `FR-W5`,
> `FR-W5a`부터 `FR-W5h`, `NFR-W5a`를 "Designed only"로 적는다. 이 문서는 10월 설계
> (`docs/design/MOBILE-NO-NODE.md`, 2026-10-03)를 오늘의 코드에 맞춰 다시 읽은 것이다.
> 6장과 7장은 리더에게 내는 제안이며 결정된 것이 아니다. 7장의 문안은 SPEC과 INTENT에
> 적용하지 않았다.
>
> **표기.** "코드에서 확인"은 이 문서를 쓰면서 해당 파일을 읽었다는 뜻이다. "문서에만 있음"은
> SPEC, INTENT, 설계 문서에 적혀 있으나 구현을 읽지 못했거나 구현이 없다는 뜻이다. 빌드와
> 테스트는 돌리지 않았다(AGENTS.md: 문서 서브 에이전트는 빌드하지 않는다).

---

## 1. 범위

**다루는 것.**

- 휴대폰(Android, iOS)에서 기기에 Node 없이 IDE를 여는 네 가지 모드를 오늘 기준으로 다시 정한다.
  (1) 원격 VS Code: 컴퓨터의 OSE를 앱 안 게이트웨이로 연다. (2) 원격 Ember 에디터.
  (3) 오프라인 작업 사본과 동기화. (4) 오프라인 VS Code 웹 빌드.
- 10월 설계에서 아직 맞는 것, 그 아래에서 바뀐 것, 이미 있는 코드 중 재사용할 것을 가른다.
- PR 크기의 작업 순서를 낸다.

**다루지 않는 것.**

- VS Code 내부 프로토콜 조사, 세 접근(A, B, C)의 비교, 플랫폼 사실 표. 10월 설계 3장과 4장이
  그대로 상세 참조다. 다시 쓰면 두 문서가 어긋나기 때문이다.
- 휴대폰에서 에이전트나 터미널 프로세스를 돌리는 일. 대화, 에이전트, 자격 증명은 메인 서버에
  있다(E3, D3).
- 공유 시트(`FR-F7`), 기기 권한 범위(`FR-N8`)의 변경.
- 클라이언트 UI 코드. Ember 클라이언트는 12월에 compose-rust의 builder API에서 call 매크로로
  옮긴다(builder 제거가 먼저다). 이 문서의 UI 단계는 모두 그 이전 뒤에 call 매크로로 쓴다.
  builder API에 맞춘 UI 계획은 곧 버려지기 때문에 세우지 않는다.

**10월 설계와의 관계.** 결론(C를 주 경로로, B를 오프라인 VS Code로, A는 만들지 않는다)은
유지한다. 바뀌는 것은 C의 내부다. 10월 설계는 "게이트웨이가 transport 스트림으로 컴퓨터의
`serve-web`에 닿는다", "에디터 코어가 node 파일 연산(`FR-X1`)으로 파일을 연다"고 썼다. 지금은
노드마다 OSE가 뜨고(D42) 서버가 그 바이트를 중계하며(`FR-W4c`), 에디터가 이미 그 중계를
transport 위에서 쓴다(`FR-N9.1`). 그래서 두 원격 모드 모두 새 프로토콜 없이 같은 중계 경로에
얹힌다.

## 2. 현재 있는 것

### 2.1 코드에서 확인

| 무엇 | 어디 | 테스트 | 모바일 모드에서의 쓸모 |
| ---- | ---- | ------ | ---------------------- |
| Android 진입점. cdylib, `compose_rust::android_application!`, Activity가 `EMBER_DATA_DIR`를 넘긴다 | `ember/app/src/android.rs`, `ember/app/android/MainActivity.kt`, `ember/app/scripts/build-android.sh` | `fr_l36_android_files_dir_is_the_data_dir_and_the_server_url_resolves_as_on_desktop`, `fr_l36_the_activity_passes_exactly_the_listed_intent_settings` | 모든 모드의 바탕. `FR-L36`은 Ember 에디터 창과 VS Code Web을 범위 밖이라고 명시한다 |
| 기기 키로 서버에 붙는 앱. `device.key`, `Api::over_transport`, `Api::open_peer_socket` | `ember/app/src/device_link.rs`, `ember/client/src/api.rs` | `ember/app/tests/fr_n9_device_link.rs` (`fr_n9_app_device_link_over_loopback_letify`, `fr_n9_a_device_with_only_the_relay_line_reaches_the_server` 외) | 휴대폰이 LAN 밖에서 서버에 닿는 유일한 길(D36, D37) |
| 에디터의 OSE 바이트 스트림. 중계 WebSocket 위의 `AsyncRead + AsyncWrite` | `ember/app/src/editor_reach.rs` (`RelayStream`, `dial`) | `ember/app/tests/fr_n9_device_editor.rs` (`fr_n9_a_device_editor_stream_reaches_the_ose_through_the_relay_over_the_transport`) | 원격 Ember 에디터의 연결. 게이트웨이도 같은 스트림을 쓸 수 있다 |
| 노드별 OSE와 중계 경로. `POST /ide/ose`, `GET /ide/ose/{launch_id}/relay`, 자리표시 토큰 치환 | `ember/node/src/ose.rs`, `ember/node/src/ose_api.rs`, `ember/server/src/computers/relay.rs` | SPEC `FR-W4a`부터 `FR-W4c`가 이름을 든다(이 문서에서 테스트 파일은 열지 않았다) | 두 원격 모드의 서버 쪽. Node는 컴퓨터에서만 돈다 |
| 에디터 연결 계층과 세션 계층 | `ember/editor-conn/src/` (`handshake.rs`, `remote_fs.rs`, `exthost.rs`, `webview.rs`), `ember/editor/src/` (`session.rs`, `settings.rs`, `textmate/`) | `FR-E1`부터 `FR-E7` 상태 행 | 원격 Ember 에디터가 그대로 쓴다. 순수 Rust와 tokio다 |
| 확장 웹뷰의 Ember 쪽. `WebviewHost` trait, `WebviewDriver`, `FakeHost` | `ember/editor/src/webview/{host,mod,resource,shim,theme}.rs` | `fr_e8_*` | 플랫폼 웹뷰를 붙이는 자리. 호스트 구현은 없다 |
| 노드 파일 연산. 해시가 붙은 읽기, `expect` 전제 조건이 있는 원자적 쓰기, 감시 | `ember/node/src/fs.rs`, `ember/node/src/proto.rs` (`Expect::Sha256`, `Expect::Absent`, `ErrorCode::PreconditionFailed`), `ember/node/src/fs_watch.rs` | `fr_x1_*` | 작업 사본 동기화의 충돌 검사가 이미 서버 쪽에 있다 |
| 서버의 노드 파일 중계. `POST /api/computers/{id}/fs/{op}` | `ember/server/src/computers/fs.rs` (`READ_OPS`, `WRITE_OPS`, `NAMESPACE_OPS`) | `FR-W4` 행 | 휴대폰이 노드 자격 증명 없이 파일을 읽고 쓴다 |
| 파일 전송. offer, chunk, resume, SHA-256 검증 | `ember/transfer/src/{lib,limits,mode}.rs` | `FR-F*` 상태 행 | 큰 하위 트리 내려받기의 후보. 4.5절에서 쓰지 않기로 한 이유를 적는다 |
| 웹뷰 금지 검사 | `.github/scripts/checks/check-no-webview.sh`, `ember/app/tests/no_webview.rs` | `NFR-L2` | 아래 2.3절 |

### 2.2 문서에만 있음, 또는 없음

- **iOS 진입점은 없다.** `ember/app/Cargo.toml`의 주석이 "The iOS entry point comes later"라고
  적는다. `target_os = "ios"`는 `self_update.rs`, `ui/safe_area.rs`, `ember/client/src/token.rs`의
  조건에만 나온다. compose-rust SPEC은 2026-09-23 기록에서 `dx build --platform ios`가 링크에
  실패한다고 적는다. 그 뒤 고쳐졌는지는 확인하지 못했다.
- **플랫폼 웹뷰 호스트는 없다.** `check-no-webview.sh`가 예외로 두는
  `ember/app/src/ui/editor_webview.rs`와 `editor_webview/`는 아직 존재하지 않는다. 툴킷 쪽은
  compose-rust `FR-81` `NativeView`(Agreed)이고, macOS Kotlin/Native가 먼저이며 Android
  (`AndroidView`)와 iOS(`UIKitView`)는 "뒤따른다"고만 적혀 있다.
- **게이트웨이는 없다.** `ide-gateway`라는 이름의 코드도 크레이트도 없다.
- **작업 사본, 동기화, `ember-fs` 웹 확장, 확장 분류는 없다.** `ember/editor/src/extensions.rs`와
  `ember/app/src/ui/editor_extensions.rs`에서 `browser`나 `extensionKind`를 읽는 코드를 찾지
  못했다.
- **독립 VS Code 웹 빌드는 없다.** `ember/ose/build.sh`는 `vscode-reh-web-<target>-min-ci`만
  만든다. 이것은 Node 서버가 딸린 빌드다.
- **휴대폰에서 확인된 것은 기본 화면뿐이다.** `FR-L36` 상태(2026-11-13): Flip 4에서 데모 모드로
  설치와 실행, 홈과 프로젝트와 세션 캡처. transport를 링크한 APK와 실제 휴대폰의 기기 연결은
  "not yet verified"다(`FR-N9`).

### 2.3 휴대폰에서 "IDE 열기"가 지금 하는 일 (코드에서 확인)

- **VS Code.** `ember/app/src/ide.rs`의 `open_command`는 macOS와 Windows가 아니면 `xdg-open`을
  고른다. Android에는 그 명령이 없으므로 실패한다. 또 서버가 주는 URL은 컴퓨터의 `ide_url`
  (`ember/server/src/api/ide.rs`, `EMBER_IDE_URL`)이고 평문 `http://`다. LAN 밖에서는 닿지
  않고, LAN 안에서도 보안 컨텍스트가 아니어서 서비스 워커 웹뷰가 뜨지 않는다(`FR-N4`).
- **Ember 에디터.** `ember/app/src/ui/coexist.rs`의 `open_editor`가 `current_exe()`를
  `--editor`로 다시 띄운다. Android와 iOS에는 두 번째 프로세스를 띄울 실행 파일이 없다.
  `ide.rs`의 `TODO(dioxus-compose#161)`가 창을 같은 프로세스 안으로 옮기는 일을 가리킨다.
- **JetBrains Gateway.** `jetbrains-gateway://` 링크를 OS에 넘긴다. 휴대폰에는 받을 프로그램이
  없다.

**웹뷰 검사가 실제로 막는 것.** (1) `ember/app`과 `ember/client`의 `.rs` 소스에 나오는 웹뷰 API
이름. 예외는 위의 두 경로뿐이다. (2) 두 크레이트의 `Cargo.toml`에 선언된 웹뷰 크레이트.
(3) `cargo tree --target all`로 `ember-app`에서 닿는 웹뷰 크레이트(`wry`, `objc2-web-kit` 등).
(2)와 (3)에는 예외가 없다. 그래서 D44의 호스트 모듈이 `ember-app`의 의존성으로 웹뷰 크레이트를
들이면 검사가 실패한다. Kotlin 파일은 검사하지 않는다. 검사는 `.rs`만 읽기 때문이다.

### 2.4 `FR-W5a`부터 `FR-W5h`, `NFR-W5a`의 판정

| ID | 판정 | 이유 |
| -- | ---- | ---- |
| `FR-W5a` | 수정 필요 | "컴퓨터의 `serve-web`(OSE 또는 VSC)을 transport로"라고 쓴다. 지금 transport로 중계되는 것은 노드의 OSE뿐이다(D42). VSC와 `ember/proxy/`의 `ide_url`은 중계 경로가 없다 |
| `FR-W5b` | 수정 필요 | 수용 기준이 "`iroh` 타입이 transport 밖에 없다"만 말한다. 기본 백엔드는 letify-link다(D36, `FR-N5`). 게이트웨이의 역할도 "HTTP와 WS 역방향 프록시"에서 "루프백 TCP를 중계 스트림에 잇는 관"으로 줄어든다 |
| `FR-W5c` | 대체됨(방식), 유지(목표) | "node 파일 연산(`FR-X1`)으로"가 아니라 OSE 중계(`FR-N9.1`)로 연다. "No webview, no JS engine"도 D44로 좁혀졌다. 확장 내용에 한해 플랫폼 웹뷰가 들어온다 |
| `FR-W5d` | 유효 | 바뀐 것이 없다. 구현이 없다 |
| `FR-W5e` | 유효 | "양쪽에서 바뀐 파일은 덮어쓰지 않는다"는 노드의 `Expect::Sha256`으로 그대로 구현된다 |
| `FR-W5f` | 유효, 보완 필요 | 묶을 "고정된 OSE 웹 빌드"가 산출물로 없다(2.2절). 웹뷰 호스트 페이지가 `vscode-cdn.net`에서 온다는 점(`ember/ose/README.md`)도 오프라인과 충돌한다 |
| `FR-W5g` | 유효 | 구현이 없다. 분류 근거(매니페스트의 `browser`, `main`, `extensionKind`)는 그대로다 |
| `FR-W5h` | 유효 | Q-M3이 열려 있는 동안 그대로 둔다 |
| `NFR-W5a` | 수정 필요 | "직접 경로"만 잰다. 메인 서버 중계 경로(D36)가 휴대폰의 흔한 경로이므로 따로 재야 한다. 기준 기기도 정해지지 않았다 |

## 3. 빈 곳

**3.1 원격 VS Code (앱 안 게이트웨이).**
- 게이트웨이 크레이트가 없다. `RelayStream`은 `ember/app` 안에 있어서 다른 크레이트가 쓸 수 없다.
- 노드의 토큰 치환은 스트림 첫 구간의 한 메시지 안에서만 일어난다(`ember/node/src/ose_api.rs`).
  브라우저는 keep-alive 연결 하나로 여러 요청을 보내고, 뒤 요청의 쿠키에도 토큰이 실린다.
  게이트웨이가 요청 하나에 스트림 하나를 쓰거나, 노드의 치환 규칙을 HTTP에 맞게 넓혀야 한다.
  어느 쪽인지 정해지지 않았다.
- 휴대폰에 웹뷰를 띄울 화면이 없다(2.2절).
- `ember/proxy/`의 전화 폭 레이아웃과 주입 타이틀바(`FR-W2`)는 Python 프록시가 컴퓨터에서
  만든다. OSE에 직접 붙으면 그 겹이 빠진다.
- OSE 웹 워크벤치의 확장 웹뷰는 `https://{{uuid}}.vscode-cdn.net/`에서 호스트 페이지를 받는다.
  인터넷이 있어야 하고, 부모 페이지인 루프백 출처가 보안 컨텍스트로 취급되어야 한다. Android
  WebView와 WKWebView에서의 동작은 확인되지 않았다(10월 설계의 P0 스파이크가 돌지 않았다).

**3.2 원격 Ember 에디터.**
- 에디터 창이 별도 프로세스다. 한 프로세스 안의 화면으로 옮기는 일(dioxus-compose#161)이 먼저다.
- 전화 폭 레이아웃이 없다. 목업 `editor.css`에는 560px 이하 규칙이 한 줄 있다(둘째 프로젝트
  버튼과 찾기를 숨긴다). 도구 창, 탭, 하단 패널을 전화에서 어떻게 놓을지는 그려지지 않았다.
- `CodeEditor`(compose-rust `FR-38`, Draft)의 터치 선택과 화면 키보드 동작을 확인한 기록을 찾지
  못했다. compose-rust `FR-5`의 IME 점검표는 9개 중 4개가 미확인이다.
- `ember-editor`는 `onig`(C 라이브러리)에 의존한다. Android NDK와 iOS 교차 빌드가 되는지는
  릴리스 워크플로의 Android 잡이 돌아야 안다. 그 잡은 아직 돌지 않았다(`FR-L36`).
- 확장 웹뷰 호스트(D44)가 없다. 없어도 편집은 되지만 웹뷰 패널은 자리표시만 보인다.

**3.3 오프라인 작업 사본과 동기화.**
- 저장소 형식, 제외 규칙, 크기 한도, 변경 집합, 충돌 표시가 모두 없다.
- 오프라인에서는 OSE가 없다. 에디터 세션(`EditorSession`)은 OSE 연결을 전제로 한다. OSE 없이
  로컬 파일을 여는 세션 경로가 필요하고, 그때 쓸 수 있는 기능(문법 강조용 문법 파일은 설치된
  확장에서만 온다, `FR-E6`)을 정해야 한다.
- D38은 "기능은 OSS와 동일"이다. 확장 호스트 없는 오프라인 에디터는 그 기준에 못 미친다.
  소유자의 확인이 필요하다(Q-M2).

**3.4 오프라인 VS Code 웹 빌드.**
- 독립 웹 빌드 산출물, 그 고정과 diff 검사, `ember-fs` 웹 확장, 게이트웨이의 정적 서빙 모드,
  오프라인 웹뷰 출처, Open VSX에서 웹 확장을 받아 두는 방법이 모두 없다. 번들 크기도 재지
  않았다.

**3.5 확장 분류.** 매니페스트를 읽어 "휴대폰에서 돈다"와 "컴퓨터가 필요하다"를 가르는 코드가
없다. 원격 모드에서는 모든 확장이 컴퓨터에서 돌므로 이 분류는 오프라인 VS Code 모드에서만
필요하다.

**3.6 iOS App Review.** 10월 설계의 판단(2.5.2, 4.7, 4.7.2)은 사실 확인이 아니라 위험 판단이고
그대로다. 추가로, iOS는 진입점부터 없다. 심사 이전에 빌드와 서명과 기기가 필요하다.

**3.7 측정(`NFR-W5a`).** 측정 하네스가 없다. 기준 기기가 없다. Flip 4는 실행에 쓰고 있으나
기준 기기로 정해진 적은 없다. 휴대폰 실행은 기기 잠금 규칙을 따른다.

## 4. 제안 구조

### 4.1 그림

```
┌──────────────────────── ember 앱 (Android / iOS) ────────────────────────┐
│ 런처 프로세스: compose-rust 기본 화면 (웹뷰 없음, E1)                    │
│   IDE 열기 ──┬─ VS Code ──────────────┐                                  │
│              ├─ Ember 에디터 ───┐     │                                  │
│              └─ Gateway (비활성)│     ▼                                  │
│                                 │  IDE 화면 (웹뷰가 있는 유일한 곳)      │
│  ┌──────────────────────────┐   │  Android: 별도 Activity + WebView      │
│  │ Ember 에디터 화면        │◀──┘  iOS: WKWebView 화면 (뒤 단계)         │
│  │ dioxus-compose, 같은 프로│          │ http + ws, 127.0.0.1, 실행별 토큰│
│  │ 세스 (dioxus-compose#161)│          ▼                                 │
│  │ EditorSession            │   ┌─────────────────────────────┐          │
│  └───────┬──────────────────┘   │ ember-gateway (Rust)        │          │
│          │ RelayStream           │  루프백 TCP ↔ RelayStream   │          │
│          │ (FR-N9.1)             │  [뒤 단계] 정적 웹 빌드,    │          │
│          │                       │           /__fs/* 작업 사본 │          │
│  ┌───────┴───────────────────────┴──────────┐                            │
│  │ ember-client Api::open_peer_socket       │   작업 사본 저장소         │
│  │ ember-transport (letify-link, 기기 키)   │   (샌드박스, 기준 해시)    │
│  └───────────────────┬──────────────────────┘        │ 온라인일 때 동기화│
└──────────────────────┼───────────────────────────────┼───────────────────┘
                       │ 직접, 또는 메인 서버 중계 (D36)│
                       ▼                               ▼
        ember server: /api/computers/{id}/ide/relay/{launch_id}   (FR-W4c)
                      /api/computers/{id}/fs/{op}                 (FR-W4)
                       │
                       ▼
        컴퓨터의 ember node: dpx-ose-server (Node와 확장 호스트는 여기)
                             /fs/* (해시 전제 조건이 있는 쓰기)
```

### 4.2 구성 요소와 크레이트

| 구성 요소 | 위치(제안) | 새로 쓰는 것 | 재사용하는 것 |
| --------- | ---------- | ------------ | ------------- |
| 중계 스트림 | `ember/client/` (옮김) | 없음. `RelayStream`과 `dial`을 `ember/app/src/editor_reach.rs`에서 옮긴다 | 코드와 `fr_n9_*` 테스트 그대로 |
| 게이트웨이 | `ember/gateway/` (새 크레이트) | 루프백 리스너, 실행별 토큰 검사, TCP와 중계 스트림 잇기 | 중계 스트림, `Api::open_peer_socket` |
| IDE 화면 호스트 | `ember/app/android/IdeActivity.kt`, 뒤에 iOS | Activity, WebView 설정, 뒤로 가기 | 없음. 툴킷 노드를 기다리지 않는다 |
| Ember 에디터 화면 | `ember/app/src/ui/editor*` | 한 프로세스 안의 창, 전화 레이아웃 | `EditorSession`, `editor_host.rs`, 설정(D47), 키맵 |
| 작업 사본 | `ember/workcopy/` (새 크레이트) | 매니페스트, 제외, 한도, 변경 집합, 충돌 | 노드 `read`의 해시, `write`의 `Expect` |
| 오프라인 VS Code | `ember/ose/` (웹 빌드 잡), `extensions/ember-fs/`, 게이트웨이의 정적 모드 | 전부 | 게이트웨이, 작업 사본 |

게이트웨이는 transport 타입을 직접 쓰지 않는다. `ember-client`의 API만 쓴다. `FR-N5`의 격리
검사(`check-transport-isolation.sh`)가 iroh와 letify-link 타입을 transport 크레이트 밖에서
금지하기 때문이다.

### 4.3 데이터 흐름 (원격 VS Code)

1. 앱이 `GET /api/sessions/{sid}/ide`를 부른다. 답의 Ember 대상에 `ose.relay`와 자리표시 토큰이
   있다(`FR-W4c`). VS Code 대상의 `url`(`ide_url`)은 휴대폰에서 쓰지 않는다.
2. 게이트웨이가 `127.0.0.1:0`에 묶이고 실행별 토큰을 만든다. 루프백만 받는 이유는 같은 망의
   다른 기기가 워크벤치에 닿지 못하게 하기 위해서다. 토큰을 두는 이유는 같은 휴대폰의 다른
   앱이 루프백 포트에 닿을 수 있기 때문이다.
3. IDE 화면이 `http://127.0.0.1:<port>/?tkn=<자리표시 토큰>&folder=<폴더>`를 연다.
4. 게이트웨이는 받은 TCP 연결마다 중계 WebSocket 하나를 열고 바이트를 그대로 잇는다. 노드가
   자리표시 토큰을 진짜 토큰으로 바꾼다. 진짜 토큰은 컴퓨터를 떠나지 않는다(D42).
5. 워크벤치의 관리 연결과 확장 호스트 연결은 같은 길로 간다. 확장 호스트는 컴퓨터의 Node다.

HTTP 요청과 스트림의 대응(3.1절의 치환 문제)은 구현 전에 정한다. 제안: 게이트웨이가 요청마다
`Connection: close`로 새 스트림을 쓴다. 노드의 치환 규칙(보안 경계)을 건드리지 않기 때문이다.
정적 자산은 웹뷰의 HTTP 캐시가 받으므로 재방문 비용은 작다(`FR-W1`). 첫 방문의 연결 수가
문제가 되면 측정값을 들고 노드 규칙을 다시 본다.

### 4.4 무엇을 먼저 내는가

**1순위: Android의 원격 VS Code(OSE).** 이유는 세 가지다.
- 서버 쪽이 이미 있다. D42 중계와 `RelayStream`이 있고, 새로 쓰는 것은 작은 Rust 크레이트와
  Kotlin Activity 하나다.
- 12월의 call 매크로 이전에 걸리는 UI가 거의 없다. IDE 선택기의 동작 한 갈래뿐이다.
- `FR-W5`의 수용 기준(기기에 Node 없이 프로젝트를 열고 고친다)을 모든 확장이 도는 상태로
  채운다.

**2순위: 원격 Ember 에디터.** 연결은 이미 transport 위에서 테스트된다. 막는 것은 툴킷
(dioxus-compose#161), call 매크로 이전, 전화 레이아웃이다. 셋 다 이 문서 밖의 일정에 달려
있어서 1순위가 될 수 없다.

**3순위: 작업 사본과 동기화.** 저장소와 동기화 로직은 UI 없이 데스크톱에서 테스트할 수 있으므로
2순위와 나란히 쓸 수 있다. 화면은 2순위 뒤다.

**4순위: 오프라인 VS Code(Android).** 산출물, 확장, 웹뷰 출처까지 새로 만들 것이 가장 많다.

**iOS는 모든 모드에서 Android 뒤다.** 진입점, 패키징, 서명이 없기 때문이다(Q-M9).

### 4.5 검토한 대안과 버린 이유

| 대안 | 버린 이유 |
| ---- | --------- |
| 휴대폰에 `serve-web` 호환 Rust 서버(10월의 A) | 10월 설계 5장의 이유 그대로다. 커밋마다 바뀌는 내부 프로토콜이고, 확장 호스트 연결이 E4와 충돌한다 |
| 게이트웨이가 `ember/proxy/`(Python, `ide_url`)로 붙는다 | 그 URL은 transport 중계가 없다. 새 범용 포트 중계가 필요하고, 프록시의 자체 로그인이 기기 키 인증(D37)과 겹친다 |
| 툴킷의 `NativeView`(compose-rust `FR-81`)가 Android에 올 때까지 기다려 런처 창 안에 웹뷰를 놓는다 | 일정이 툴킷에 묶인다. 또 런처 프로세스가 웹뷰를 갖게 되어 `FR-L7`의 "런처 프로세스는 웹뷰를 만들지 않는다"와 부딪친다. 별도 Activity는 별도 프로세스로 둘 수 있다 |
| 시스템 브라우저(Chrome, Safari)로 게이트웨이 URL을 연다 | 앱이 배경으로 가면 iOS는 프로세스를 멈추고 Android도 보장하지 않는다. 게이트웨이가 죽는다. Android 스파이크의 임시 수단으로만 쓴다 |
| 작업 사본을 `ember/transfer`로 내려받는다 | 전송은 사람이 수락하는 offer 흐름이고(`FR-F*`) 파일별 기준 해시를 남기지 않는다. 노드 `read`가 해시를 함께 주므로 fs 중계가 맞다 |
| 원격 Ember 에디터를 node 파일 연산으로 연다(10월의 `FR-W5c`) | 데스크톱 에디터와 다른 두 번째 경로가 생긴다. 언어 기능이 없어지고 D38(기능은 OSS와 동일)에 못 미친다 |
| 에디터 코어용 LSP를 노드가 따로 돌린다(10월의 P6) | 에디터가 OSE의 확장 호스트와 직접 말하므로(D9a, D42) 언어 기능은 확장에서 온다. 필요가 없어졌다 |

## 5. 위험

| 위험 | 내용 | 완화 |
| ---- | ---- | ---- |
| iOS WKWebView의 서비스 워커와 보안 컨텍스트 | 서비스 워커는 App-Bound Domains에서만 된다. 루프백이 그 도메인이 될 수 있는지 확인되지 않았다 | iOS 작업 전에 스파이크로 확인한다. 안 되면 iOS의 VS Code 모드는 보류하고 Ember 에디터만 낸다. Ember 에디터의 확장 웹뷰는 패널별 출처를 호스트가 정하므로(`WebviewHost::origin`) 같은 제약을 받지 않을 수 있다(미확인) |
| App Review 2.5.2, 4.7 | 내려받은 확장 코드를 기기에서 실행하면 심사 위험이 있다 | 원격 모드와 오프라인 Ember 에디터는 코드를 내려받지 않는다. 오프라인 VS Code는 iOS에서 플래그 뒤에 둔다(`FR-W5h`). 결정은 Q-M3 |
| Android WebView 버전 | 기기마다 WebView 버전이 다르다. 워크벤치가 요구하는 기능이 없을 수 있다 | 최소 WebView 버전을 정하고, 낮으면 IDE 화면이 이유를 말하고 열지 않는다. 최소값은 스파이크에서 잰다 |
| 루프백 보안 컨텍스트(Android) | `http://127.0.0.1`이 WebView에서 보안 컨텍스트인지 확인되지 않았다 | 스파이크. 안 되면 `WebViewAssetLoader` 출처는 WebSocket을 나르지 못하므로 정적 자산만 그쪽으로 옮기는 혼합 구성을 잰다 |
| 배경 종료 | 앱이 배경으로 가면 게이트웨이와 transport가 끊긴다 | 포그라운드 전용으로 설계한다. 돌아오면 워크벤치의 자체 재연결(`reconnectionToken`)과 `Api::resume_transport`에 맡긴다. 저장하지 않은 편집은 OSE 쪽 백업과 D43(에디터 핫 엑시트)을 따른다 |
| 중계 처리량 | 메인 서버 중계 경로는 서버의 업링크에 묶인다(D36). 워크벤치 첫 로드는 수십 MB다 | 정적 자산을 웹뷰 캐시에 둔다. `NFR-W5a`에 중계 경로 측정을 넣는다 |
| 저장 공간 | 작업 사본과 오프라인 웹 빌드가 앱 샌드박스를 채운다 | 크기 한도와 제외 경로를 기본값으로 둔다(`FR-W5d`). 사본별 크기를 보여 주고 지울 수 있게 한다 |
| 동기화 충돌 | 오프라인 편집과 컴퓨터 쪽 변경(에이전트가 그 사이 고친다)이 겹친다 | `Expect::Sha256` 전제 조건으로 절대 덮어쓰지 않는다. 충돌은 양쪽 내용을 남기고 사람에게 묻는다. 삭제와 이름 바꾸기는 내용 변경과 따로 다룬다 |
| 권한 범위 | `read_only` 기기는 쓰기 중계가 403이다(`FR-N8`) | IDE 화면을 열기 전에 `Api::whoami`의 범위를 보고 읽기 전용임을 미리 말한다 |
| 내부 프로토콜 고정 | 에디터는 OSE 커밋 하나에 고정된다(D9a) | 모바일 고유 위험이 아니다. 데스크톱과 같은 고정을 쓴다 |
| 웹뷰 검사 우회 | Kotlin Activity는 `check-no-webview.sh`에 걸리지 않는다 | 검사에 Kotlin 규칙을 더한다. `MainActivity.kt`에는 WebView가 없어야 하고 IDE Activity 파일 하나만 허용한다 |

## 6. 소유자 결정 필요

모두 리더에게 내는 제안이다. 추천은 추천일 뿐이다.

**10월 설계에서 넘어온 질문.**

- **Q-M1 (열림).** `FR-W5`의 "`serve-web` 호환 Rust 백엔드"를, 휴대폰이 Rust 게이트웨이로
  컴퓨터의 실제 OSE에 닿는 것으로 채워도 되는가. 선택지: (a) 된다. (b) 휴대폰에 Rust 서버를
  다시 구현한다. (c) 컴퓨터용 Rust `serve-web` 대체를 뜻한 것이다. 추천: (a). D42가 이미 OSE를
  노드에 두기로 했고, (b)는 E4와 부딪친다.
- **Q-M2 (열림, 범위가 넓어짐).** 오프라인의 첫 출시는 작업 사본 위의 Ember 에디터로 충분한가.
  오프라인에는 확장 호스트가 없어서 D38의 "기능은 OSS와 동일"에 못 미친다. 선택지: (a) 충분하다.
  오프라인은 "제한 모드"라고 화면에 밝힌다. (b) 오프라인도 확장이 도는 VS Code여야 한다.
  추천: (a). 코드를 내려받지 않아 iOS에서도 낼 수 있기 때문이다.
- **Q-M3 (열림).** iOS에서 내려받은 웹 확장을 실행할 것인가. 선택지: (a) 심사 위험을 지고 낸다.
  (b) Android 전용으로 둔다. (c) EU 대체 배포로만 낸다. 추천: (b). 위험이 사실이 아니라 판단이고,
  원격 모드가 같은 필요를 채운다.
- **Q-M4 (열림, 선택지가 줄어듦).** 작업 사본은 어디에서 와서 어디로 돌아가는가. D42 이후
  프로젝트 파일은 노드에만 있고 서버는 사본을 갖지 않는다. 선택지: (a) 사본을 뜬 컴퓨터로만.
  (b) 세션의 현재 컴퓨터를 고를 수 있다. 추천: (a). 기준 해시가 그 컴퓨터의 파일에 대한 것이어서
  다른 컴퓨터로 보내면 충돌 검사가 뜻을 잃는다.
- **Q-M5 (이후 결정으로 답이 나옴).** 에디터 코어의 언어 기능을 원격 언어 서버에서 받을 것인가.
  D9a, D38, D42로 답이 나왔다. 에디터는 OSE의 확장 호스트와 직접 말하고 언어 기능은 확장에서
  온다. 10월의 P6은 없앤다. 확인만 필요하다.

**새 질문.**

- **Q-M6.** Android의 IDE 화면을 무엇으로 띄우는가. 선택지: (a) 별도 프로세스의 Activity와
  Android WebView(`android:process`). (b) 툴킷 `NativeView`를 기다려 런처 창 안에. (c) 시스템
  브라우저. 추천: (a). 런처 프로세스에 웹뷰가 없다는 `FR-L7`의 문장을 글자 그대로 지키고 툴킷
  일정에 묶이지 않는다. dx가 쓰는 매니페스트에 Activity를 더할 수 있는지는 스파이크로 확인한다.
- **Q-M7.** 휴대폰의 VS Code 대상은 OSE만으로 시작해도 되는가. `FR-W5a`는 "OSE 또는 VSC"다.
  선택지: (a) OSE만. VSC와 `ember/proxy/` 겹은 뒤로. (b) 범용 포트 중계를 먼저 만들어 둘 다.
  추천: (a). 중계가 있는 것이 OSE뿐이고 D10의 기본 런타임이 OSE다.
- **Q-M8.** 휴대폰의 IDE 선택기에서 JetBrains Gateway를 어떻게 보이는가. D32는 "정확히 셋"이다.
  선택지: (a) 셋을 보이되 Gateway는 이유와 함께 비활성. (b) 휴대폰에서는 숨긴다. 추천: (a).
  D32의 문장을 바꾸지 않기 때문이다.
- **Q-M9.** iOS를 언제 시작하는가. 선택지: (a) Android의 1순위와 2순위가 휴대폰에서 통과한 뒤.
  (b) 나란히. 추천: (a). iOS는 진입점, 서명, 기기부터 필요하고 소유자의 손이 든다.
- **Q-M10.** `NFR-W5a`의 기준 기기. 선택지: (a) Android는 Flip 4, iPhone은 iOS 시작 때 정한다.
  (b) 중급 Android 기기를 따로 정한다. 추천: (a). 지금 실행에 쓰는 기기가 그것뿐이다.
- **Q-M11.** `FR-W2`의 전화 폭 겹(주입 타이틀바, 키보드 정책)을 OSE 직결 경로에서 어떻게 할
  것인가. 선택지: (a) 첫 출시는 겹 없이 IDE Activity의 자체 상단 바만. (b) 게이트웨이가
  `ember/proxy/static`의 겹을 응답에 주입한다. 추천: (a). (b)는 Python 프록시의 일을 Rust로 한
  번 더 쓰는 것이고 E4의 경계를 다시 따져야 한다.

## 7. SPEC/INTENT 변경 제안

적용하지 않은 제안이다. 모두 **[provisional]**이며 `FR-W5`의 [user] 태그는 건드리지 않는다.

> **FR-W5a** *[provisional, `docs/design/POST-1-0-MOBILE-NO-NODE.md`]* On Android and iOS,
> "Open IDE → VS Code" opens the session's project on its current computer: the IDE screen loads
> that computer's OSE (`FR-W4a`) through the in-app gateway (`FR-W5b`) and the server's relay
> route (`FR-W4c`). Node runs only on the computer. The VSC runtime and the `ember/proxy/` layer
> are not reached on a phone yet (Q-M7, Q-M11).
> Acceptance: unchanged, plus "the real connection token never reaches the phone".

> **FR-W5b** *[provisional]* One Rust gateway (`ember/gateway`) in the app process listens on a
> loopback address, rejects requests without the per-launch token, and joins each accepted TCP
> connection to one relay stream opened through `ember-client`. It names no iroh type and no
> letify-link type (`FR-N5`). Static serving and the working-copy file API are added with
> `FR-W5f`.
> Acceptance: an integration test with an in-process server and a real `dpx-ose-server` loads
> the workbench HTML and opens the management connection through the gateway; a request without
> the token is refused; a Wi-Fi to LTE switch on a phone keeps the workbench connected within
> `NFR-N1`'s stall bound.

> **FR-W5c** *[provisional]* On Android and iOS, "Open IDE → Ember 에디터" opens the Ember editor
> as a screen of the app process, connected to the computer's OSE through the relay route
> (`FR-N9.1`), with the functions of the desktop window (`FR-L31`). Extension webviews follow
> `FR-E8`. The workbench has no webview and no JavaScript engine.
> Acceptance: open, edit and save a remote file from a phone over a direct path and over the
> main-server relay; diagnostics from an extension on the computer appear.

> **FR-W5d, FR-W5e** *[provisional]* Text unchanged. Add to `FR-W5e`: "the write uses the node's
> SHA-256 precondition (`Expect::Sha256`); a failed precondition is a conflict."

> **FR-W5f** *[form provisional]* Add: "The bundled web build is a pinned artefact of
> `ember/ose/`, and its webview host page is served from a second local origin, because the OSE
> default loads it from `vscode-cdn.net`."

> **FR-W5i** *[provisional, new]* The IDE screen is the only place a webview exists on a phone.
> On Android it is its own Activity in its own process; the launcher process never creates a
> webview. `check-no-webview.sh` also checks the Kotlin sources.
> Acceptance: the check fails when `MainActivity.kt` names a WebView.

> **NFR-W5a** *[provisional]* Add a row: "`FR-W5a` cold open to editable over the main-server
> relay, recorded (no target until the first measurement)". Name the reference Android phone.

> **INTENT, new decision (번호는 적용할 때 배정)** *[provisional]* "The phone reaches the IDE
> through the per-node OSE relay." The phone's two online IDE modes use D42's relay and D37's
> device key. Rejected: a `serve-web` protocol server on the phone; reaching `ide_url` from a
> phone; node file operations as the editor's file path; language servers run by the node for
> the editor core.

SPEC 상태표의 `FR-W5` 행은 구현이 들어올 때까지 "Designed only"로 둔다. 문서 이름만 이 문서를
함께 가리키게 바꾼다.

## 8. 작업 분해

추정은 에이전트 세션 하나의 작업일이고 거칠다. 빌드와 테스트 실행은 빌더가 하므로 대기 시간은
넣지 않았다. "휴대폰"은 실제 기기와 기기 잠금이 필요하다는 뜻이다. "소유자"는 소유자의 손이나
승인이 필요하다는 뜻이다.

| # | 무엇 | 건드리는 곳 | 증명하는 테스트 | 추정 | 필요 |
| - | ---- | ----------- | --------------- | ---- | ---- |
| 1 | SPEC과 INTENT 반영(7장, 리더 확인 뒤) | `docs/SPEC.md`, `docs/INTENT.md` | 문서 검사(em-dash) | 0.5 | 리더 |
| 2 | 스파이크: Android WebView에서 루프백 출처의 보안 컨텍스트와 서비스 워커, 최소 WebView 버전, dx 매니페스트에 별도 프로세스 Activity 추가 | `.scratch/` | 결과를 이 문서에 기록 | 1 | 휴대폰 |
| 3 | `RelayStream`과 `dial`을 `ember/client/`로 옮긴다 | `ember/client`, `ember/app` | 기존 `fr_n9_*`가 그대로 통과 | 0.5 | |
| 4 | 게이트웨이 크레이트: 루프백 리스너, 토큰, TCP와 중계 스트림 잇기 | `ember/gateway` (새 크레이트) | `fr_w5b_*`: in-process 서버와 가짜 OSE로 요청 왕복, 토큰 없는 요청 거절, 루프백 외 바인드 거절 | 2 | |
| 5 | 실제 OSE 통합: 요청당 스트림 규칙과 토큰 치환 | `ember/gateway`, 필요하면 `ember/node/src/ose_api.rs` | `fr_w5a_*`: 실제 `dpx-ose-server`에서 워크벤치 HTML과 관리 연결. 진짜 토큰이 게이트웨이 쪽에 나타나지 않는다 | 2 | |
| 6 | Android IDE Activity와 게이트웨이 시작, 휴대폰의 VS Code 동작 | `ember/app/android/IdeActivity.kt`, `ember/app/src/ide.rs`, `check-no-webview.sh` | `fr_w5i_*`(검사 스크립트), `fr_l7_*`에 Android 갈래 | 2 | 휴대폰 |
| 7 | 휴대폰 수용: 열기, 고치기, 저장, `main`만 있는 확장, 확장 웹뷰, Wi-Fi와 LTE 전환 | 없음 | `FR-W5a` 수용 기준을 실제 출력으로 보고 | 1 | 휴대폰, 소유자(모바일 데이터) |
| 8 | 측정 하네스와 첫 측정 | `.github/scripts/`, `ember/gateway` | `NFR-W5a` 값 기록 | 1 | 휴대폰 |
| 9 | 작업 사본 저장소: 매니페스트, 기준 해시, 제외, 한도, 변경 집합 | `ember/workcopy` (새 크레이트) | `fr_w5d_*`: 재시작 뒤 편집 유지, 제외와 한도 | 3 | |
| 10 | 동기화와 충돌 | `ember/workcopy`, `ember/client` | `fr_w5e_*`: in-process 서버와 노드로 X는 쓰이고 Y는 충돌, 어느 쪽도 잃지 않는다 | 2 | |
| 11 | Ember 에디터를 앱 프로세스 안의 화면으로(dioxus-compose#161 뒤) | `ember/app/src/ui/editor*`, `ember/app/src/ide.rs` | 데스크톱 `fr_l31_*` 유지, 새 `fr_w5c_*` | 3 | 툴킷, call 매크로 이전 뒤 |
| 12 | 에디터 전화 레이아웃과 터치(목업 확정 뒤) | `ember/app/src/ui/editor*` | 레이아웃 단위 테스트, 휴대폰 캡처 | 4 | 휴대폰, 소유자(목업), call 매크로 이전 뒤 |
| 13 | OSE 없는 로컬 세션과 오프라인 화면, 충돌 화면 | `ember/editor`, `ember/app` | `fr_w5d_*` 화면 쪽, 비행기 모드 실행 | 4 | 휴대폰, call 매크로 이전 뒤, Q-M2 |
| 14 | OSE 독립 웹 빌드 산출물과 고정 | `ember/ose`, CI 워크플로 | 번들 diff 검사, 크기 기록 | 2 | 리더(CI 실행) |
| 15 | `ember-fs` 웹 확장, 게이트웨이 정적 모드, 로컬 웹뷰 출처 | `extensions/ember-fs`, `ember/gateway` | `fr_w5f_*`: 헤드리스 브라우저에서 작업 사본 열기와 저장 | 5 | 휴대폰(마지막 확인) |
| 16 | 확장 분류 | `ember/editor` 또는 `ember/workcopy` | `fr_w5g_*`: 매니페스트 표본으로 분류 | 1 | |
| 17 | iOS: 진입점, 패키징, WKWebView 스파이크와 IDE 화면 | `ember/app`, 툴킷 | 시뮬레이터 실행, 기기 실행 | 8 | 소유자(Apple 계정, 서명, 기기), Q-M9 |

순서의 근거. 3부터 8까지가 1순위(Android 원격 VS Code)이고 UI 이전과 무관하게 지금 시작할 수
있다. 9와 10은 UI가 없어서 나란히 갈 수 있다. 11부터 13은 12월의 call 매크로 이전과 툴킷
이슈가 끝난 뒤다. 14부터 16은 Q-M2와 Q-M3의 답에 따라 범위가 달라진다. 17은 Q-M9에 달렸다.

합계는 약 42 작업일이다. 스파이크 결과(특히 2번과 17번의 WKWebView)에 따라 크게 달라진다.

### 확인하지 못한 것

- 10월 설계의 미확인 사실 목록 전부. P0 스파이크가 돌았다는 기록을 찾지 못했다.
- `ember-editor`(`onig`)와 letify-link를 링크한 APK의 빌드와 실제 휴대폰 실행.
- dx가 쓰는 Android 매니페스트에 Activity와 `android:process`를 더할 수 있는지.
- compose-rust의 iOS `dx` 경로가 9월 이후 고쳐졌는지, `FR-81`의 Android와 iOS 이슈 번호.
- `CodeEditor`의 터치 선택과 화면 키보드 동작.
- 노드의 토큰 치환 구간이 브라우저의 실제 요청(쿠키, 쿼리)에서 충분한지.
- `docs/design/mobile-ide-support.pptx`의 내용. 열지 않았다.
- #80의 댓글. `gh issue view`가 본문을 돌려주지 않아 과제 설명의 인용에 기댔다.
