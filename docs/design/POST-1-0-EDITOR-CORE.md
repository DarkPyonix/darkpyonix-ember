# POST-1-0-EDITOR-CORE.md: 에디터 코어(M8, #32)의 남은 일과 1.0 이후 계획

상태: 제안이다. 이 문서는 코드를 바꾸지 않는다. 아래의 결정은 모두 리더에게 올리는 제안이고
확정된 것이 아니다. `develop` `8ea03cb9`의 코드와 문서를 읽고 썼다. 컴파일하지 않았고, 테스트를
돌리지 않았고, 화면을 보지 않았다. "1.0"은 리더가 정하는 첫 정식 릴리스를 가리킨다. `PROJECT.md`에
1.0의 정의는 없다.

표기: **코드에서 확인**은 이 문서를 쓰면서 그 파일을 읽었다는 뜻이다. **문서에만 있음**은 SPEC이나
설계 문서가 말하지만 코드로 대조하지 않았다는 뜻이다.

## 1. 범위

**안.**

- 이슈 #32의 완료 기준 네 개가 지금 어디까지 왔는지, 이슈를 닫으려면 무엇이 남았는지.
- `SPEC.md` §E(`FR-E1`부터 `FR-E8`)의 상태와, §E가 다루지 않는 구조적 빈 곳: 프로토콜 범위,
  재연결, 버전 고정과 폴백, 성능, 사람 없이 도는 검증.
- 남은 큰 조각의 구조와 PR 단위 분해.

**밖.**

- 창의 모양과 목업 대조(`FR-L31`, D38, #477, #895). 이 문서는 창이 쓰는 세션과 연결만 다룬다.
- 확장 웹뷰의 플랫폼 호스트(`FR-E8`, D44, `EDITOR-WEBVIEWS.md`). 호스트 노드는 compose-rust의
  일이다. 여기서는 검증 단계에 한 줄로만 넣는다.
- 터미널(`TERMINALS.md`), 디버깅(#445), 모바일의 Node 없는 경로(`MOBILE-NO-NODE.md`, #80).
- VS Code를 고치는 일과 Extension Host를 다시 만드는 일. E4와 D11이 막는다. 이 문서의 어떤 단계도
  Code-OSS 소스에 패치를 넣지 않고, 확장 호스트 쪽 로직을 Rust로 옮기지 않는다.

**#32와 §E의 관계.** #32는 10월 3일의 기준 네 개(연결 계층, 오버레이 다리 네 개, 열고 고치고
저장하기, 버전 확인과 폴백)를 가진다. 그 뒤 D38("기능은 OSS와 같고 디자인은 목업")이 범위를 Code-OSS
전체로 넓혔고, 그 일은 `FR-E6`부터 `FR-E8`, `FR-L31.1`부터 `FR-L31.41`, `EDITOR-PARITY.md`의 행으로
갔다. 그래서 #32는 "첫 관문" 하나와 "끝없는 우산" 하나를 함께 지고 있다. 이 문서는 둘을 나눈다:
관문(아래의 A 단계)이 닫히면 #32를 닫고, 나머지는 좁은 이슈로 추적한다(Q-E1).

## 2. 현재 있는 것

### 2.1 계층

| 계층 | 위치 | 하는 일 | 근거 |
| --- | --- | --- | --- |
| 연결 | `ember/editor-conn/src` (16개 모듈, 약 7,000줄) | `frame.rs`, `protocol.rs`, `connection.rs`(PersistentProtocol), `handshake.rs`(업그레이드, 핸드셰이크, `verify_server`, `reconnect_loop`), `ipc.rs`, `remote_fs.rs`, `management.rs`, `rpc.rs`, `rpc_ids.rs`, `exthost.rs`(부트스트랩, 호출 만들기, `MainThreadCall::parse`, `default_reply`), `document.rs`, `language.rs`, `search.rs`, `trust.rs`, `webview.rs` | 코드에서 확인 |
| 세션 | `ember/editor/src` (약 19,000줄) | `session.rs`, `engine.rs`, 다리(`diagnostics.rs`, `codelens.rs`, `hover.rs`, `inline.rs`), `bridge.rs`와 `widget.rs`(위젯 다리), `textmate/`, `highlight.rs`, `semantic.rs`, `folding.rs`, `search.rs`, `settings.rs`, `trust.rs`, `webview/`, `fallback.rs` | 코드에서 확인 |
| 창 | `ember/app/src/editor_host.rs`, `editor_reach.rs`, `ui/editor*.rs` (약 20,000줄) | `--editor <project> <folder> <endpoint>` 프로세스, `EditorSession`을 서버 릴레이로 연결, `CodeEditor`에 `text`, `decorations`, `syntax_spans`, `folds`를 넘김 | 코드에서 확인(`editor_host.rs` 355-392행, `editor_workbench.rs` 376-395행) |
| OSE | `ember/ose` | 빌드, 스모크, 확장 회귀 하네스(`regress.sh`, `regression/`), `VERSION`은 `1.139.1` | 코드에서 확인(파일 목록과 `VERSION`만) |

### 2.2 고정(pin)

- `PINNED_VERSION = "1.139.1"`, `PINNED_COMMIT = "04c0d99f..."`(`ember/editor-conn/src/lib.rs` 62,
  65행). 단위 테스트 `pin_matches_ose_version`이 `ember/ose/VERSION`과 맞춘다. 코드에서 확인.
- `rpc_ids.rs`의 `PROXY_IDS`는 168개다(`MainThread*` 87개, `ExtHost*` 81개). `scripts/gen_rpc_ids.sh`가
  `extHost.protocol.ts`의 `createProxyIdentifier` 줄을 순서대로 뽑아 표준 출력에 찍고, 사람이
  붙여 넣는다. 다른 파일이 `createProxyIdentifier`를 부르면 스크립트가 실패한다. 코드에서 확인.
- CI 작업 `editor-conn-pin`(`.github/workflows/test.yml` 132행)이 그 태그에서 다시 만들어 비교한다.
  코드에서 확인(작업 정의만, 실행 결과는 보지 않았다).
- 생성되는 것은 **id 순서뿐**이다. 메서드 이름, 인자 수, DTO 모양은 손으로 썼고, 주석의 줄 번호로만
  원본과 이어진다. 코드에서 확인.

### 2.3 액터 범위

`MainThread*` 87개 가운데 메서드를 하나라도 해석하거나 상태로 답하는 것은 16개다(`exthost.rs`의
`MainThreadCall`, `engine.rs` 1644-1650행, `replies.rs`를 grep으로 확인): `Diagnostics`,
`LanguageFeatures`, `Commands`, `OutputService`, `MessageService`, `Search`, `Webviews`,
`WebviewPanels`, `WebviewViews`, `CustomEditors`, `Workspace`(신뢰와 `$checkExists`), `Window`
(`$getInitialState`), `Languages`(`$getLanguages`), `Documents`(`$tryOpenDocument`), `BulkEdits`
(`$tryApplyWorkspaceEdit`), `Storage`(빈 답). 나머지 71개는 `default_reply`가 `Reply::Empty`
(`undefined`)로 답한다. 확장 호스트가 멈추지 않게 하려는 답이고, 기능은 없다.

### 2.4 테스트

| 테스트 | 무엇을 증명하나 | 어디서 도나 |
| --- | --- | --- |
| `ember/editor-conn/src/*`의 단위 테스트, `tests/fr_e8_webview_decode.rs` | 프레임, 핸드셰이크, IPC, RPC 코덱, 호출 해석 | CI `rust` 작업 |
| `ember/editor/tests/connect_and_fallback.rs` (5개) | 커밋 불일치, `/version` 없음, 핸드셰이크 불일치는 폴백. 네트워크와 인증 오류는 폴백 아님. 가짜 서버로 열기, 고치기, 진단, 저장 | CI |
| `ember/editor/tests/widget_bridge.rs`, `engine_flow.rs` | 세션 코어와 위젯 다리의 흐름, 44바이트 장식 레코드, 인라인 완성 수락 | CI |
| `fr_e6_*`, `fr_e7_search_flow.rs`, `fr_e8_webviews.rs`, `fr_l31_*` | 각 행의 단위 흐름 | CI |
| `editor-conn/tests/live_ose.rs` `live_ose_end_to_end` | 실제 OSE: 관리 연결, 파일 채널, 확장 스캔, 부트스트랩, JSON 진단 | `#[ignore]`. `test-ose.yml`의 `live` 작업(풀 리퀘스트에서는 돌지 않음) |
| `editor/tests/live_session.rs` (3개: `live_session_json_diagnostics_save_and_reload`, `live_fr_l31_24_external_change_under_ws_root`, `live_session_explorer_create_rename_delete`) | 실제 OSE에서 세션 계층의 진단, 저장, 다시 읽기, 외부 변경, 탐색기 조작 | 같음 |
| `fr_l31_14_live_extensions.rs`, `nfr_w1_extension_regression.rs`(2개), `textmate_real_grammars.rs`, `fr_e6_textmate_perf.rs` | 확장 스캔, 회귀 클라이언트, 실제 문법, 토크나이저 시간 | `#[ignore]` |

실제 OSE 테스트에 Hover, CodeLens, 인라인 완성, 검색, 웹뷰의 경우는 없다(두 live 파일을 grep으로
확인). `ember/vectors/`에 `editor-conn` 폴더는 없다. `EDITOR-CONNECTION.md` §6의 "실제 프레임을
받아 단위 테스트에서 다시 돌리기"는 아직 하지 않은 일이다.

### 2.5 #32의 기준 네 개

| # | 기준 | 상태 | 근거 |
| --- | --- | --- | --- |
| 1 | `ember-editor-conn`: 프레이밍과 핸드셰이크, IPC, 원격 파일 시스템, rpcProtocol 코덱과 고정된 생성 표, 부트스트랩, 문서 동기화, 실제 서버로 검증 | **됨**, 단서 둘 | 모듈과 `live_ose.rs`는 코드에서 확인. `live` 작업의 첫 초록 실행(43670763653)은 문서에만 있음. 단서: 표는 id만 생성된다. 재생용 벡터가 없다 |
| 2 | 진단, CodeLens, Hover, 인라인 완성을 위젯 장식에 연결(compose-rust FR-38) | **세션까지 됨, 화면은 부분** | 다리 네 개와 `widget_bridge.rs`는 코드에서 확인. 실제 OSE로 검증된 것은 진단뿐이다. Hover 팝업은 `bridge.rs`가 `doc.hover`에 넣지만 `ember/app/src`에서 그것을 읽는 코드를 찾지 못했다. CodeLens와 고스트 텍스트는 장식 바이트로 위젯에 가지만 화면에서 본 기록이 없다 |
| 3 | "Open IDE → Ember"로 열고, 고치고, 저장한다. 수정하지 않은 확장(예: 린터)이 진단을 보여 준다 | **부분** | 창과 연결은 코드에서 확인. 탐색기에서 파일 열기는 Mac에서 손으로 확인했다고 Status 표가 말한다(문서에만 있음). 창에서 타이핑과 저장, 화면의 확장 진단은 Status 표가 "수동 확인 대기"라고 한다. 세션 계층에서는 내장 JSON 확장으로 확인됐다. Open VSX의 린터로 한 실행은 없다(`NFR-W1`: "Written, no real run yet") |
| 4 | 연결할 때 버전 확인. 모르는 버전이면 VS Code 대상으로 폴백 | **확인은 됨, 폴백은 창에 연결되지 않음** | `handshake::verify_server`와 `check_server_commit`, `fallback.rs`, `EditorSession::connect_or_fallback`은 코드에서 확인. 그러나 창은 `connect_tcp`와 `connect`를 부르고(`editor_host.rs` 365, 369행) `connect_or_fallback`을 부르지 않는다. 버전이 다르면 창은 일반 연결 실패 문구만 보인다(`connect_failed`). `ember/app/src`에 `Fallback`이나 `UnsupportedServerVersion`을 다루는 곳이 없다 |

### 2.6 §E 행

| 행 | 상태 | 비고 |
| --- | --- | --- |
| `FR-E1` 버퍼와 IME | 위젯은 compose-rust FR-38(조합 중인 글자는 Host로 가지 않음, 38.1). Ember 쪽 다리는 있음 | 화면의 한글 조합은 확인 기록이 없다 |
| `FR-E2` 진단, 출력, 알림 | 코드에 있음. 진단은 실제 OSE로 확인 | 문제 패널과 출력 패널의 화면 확인 없음 |
| `FR-E3` CodeLens, Hover | 세션에 있음 | 실제 OSE 경우 없음. Hover 팝업은 창에서 그리지 않는 것으로 보인다 |
| `FR-E4` 인라인 완성 | 세션에 있음 | 실제 제공자로 한 실행 없음. `$registerInlineCompletionsSupport`는 가장 자주 바뀌는 서명이다 |
| `FR-E5`, `FR-E8` 웹뷰 | 프로토콜, 모델, 호스트 트레이트(#823) | 그리기는 compose-rust 호스트 노드를 기다린다 |
| `FR-E6` 구문 색, 의미 토큰, 접기 | 코드에 있음, 골든 테스트 | 실제 OSE와 화면 확인 없음 |
| `FR-E7` 파일 검색과 바꾸기 | 코드에 있음 | 행 스스로 "Not verified: a real OSE, the rendered window"라고 적는다 |

## 3. 빈 곳

### 3.1 #32의 기준에 대해

1. **폴백이 창에 없다**(기준 4). 세션 계층의 `ConnectOutcome::Fallback`을 받는 쪽이 없다.
2. **오버레이 네 개 가운데 셋이 실제 확장 호스트로 검증되지 않았다**(기준 2).
3. **수정하지 않은 린터가 화면에 진단을 그리는 것을 본 기록이 없다**(기준 3).
4. **Hover 팝업이 화면에 없다**(기준 2). `EDITOR-PARITY.md`의 Hover 행도 "부분(no popup), #442"인데
   #442는 닫혀 있다.

### 3.2 §E 행에 대해

- 표의 머리는 두 칸(`ID`, `Requirement`)인데 `FR-E7`과 `FR-E8`만 세 번째 칸(검증)을 가진다.
  `FR-E1`부터 `FR-E6`에는 승인 기준 칸이 없다. 그래서 "행이 끝났다"를 판정할 근거가 행에 없다.
- §E의 머리말은 "에디터가 화면에 프로젝트를 열 때까지 초안"이라고 하는데, Status 표는 이미 손으로
  열었다고 한다. 머리말이 낡았다.
- §E에 없는 요구: 프로토콜 범위, 재연결, 지원하는 서버 버전의 집합, 성능, 자동 검증.

### 3.3 구조

- **프로토콜 범위.** 71개 액터가 빈 답이다. 사용자에게 보이는 것부터 적으면: `Storage`
  (`$initializeExtensionStorage`가 빈 답이라 확장의 `globalState`와 `workspaceState`가 세션마다
  비어 있다), `SecretState`, `StatusBar`, `Progress`와 `QuickOpen`(자동으로 닫힘, PARITY 문서),
  `TreeViews`, `TextEditors`(선택 전달, `showTextDocument`), `Configuration`(확장이 설정을 쓰는
  경로), `FileSystemEventService`, `Dialogs`, `Clipboard`, `Decorations`, `DocumentContentProviders`,
  `SCM`과 `QuickDiff`(지금의 커밋 창은 서버의 git 릴레이를 쓴다), `TerminalService`, `Task`,
  `DebugService`(#445), `Testing`, `Comments`, `Timeline`, `Authentication`, `Notebook*` 다섯 개.
- **빈 답의 위험.** 값을 돌려주는 메서드에 `undefined`를 주면 확장이 조용히 다르게 동작한다.
  어떤 확장이 어떤 미구현 메서드를 부르는지 세는 곳이 없다.
- **재연결.** 연결이 끊기면 세션이 `ConnectionLost`로 끝나고 창이 새 세션을 만든다(#451). 확장
  호스트와 언어 서버가 처음부터 다시 뜬다. `handshake::reconnect_loop`는 있지만 부르는 곳이 없다
  (`session.rs` 19-21행의 주석이 그렇게 적는다).
- **버전 어긋남.** 클라이언트는 스스로 갱신한다(`NFR-R2`). OSE는 컴퓨터마다 따로 깔린다(D42).
  그래서 1.0 뒤에는 "새 클라이언트, 옛 OSE"가 보통의 상태가 된다. 지금은 커밋 하나만 받는다.
  고정을 올리는 절차는 `gen_rpc_ids.sh`의 주석에만 있고, 새 업스트림 태그를 미리 보는 카나리는
  없다(`.github`, `ember/ose`, `ember/editor-conn`에서 `canary`를 찾지 못했다).
- **서버가 OSE의 커밋을 말해 주지 않는다.** `ember/server/src/api/ide.rs`의 대상 정보에 커밋이
  없다. 그래서 창을 띄우고 연결해 본 뒤에야 맞지 않음을 안다.
- **성능.** 편집 한 번의 왕복, 릴레이를 거친 지연, 큰 파일에 대한 요구가 §E에 없다.
  `fr_e6_textmate_perf.rs`는 시간을 찍기만 한다. 검색은 가상 목록이 없어 2,000건에서 자른다.
- **자동 검증.** live 테스트는 풀 리퀘스트에서 돌지 않고, 개발 CI는 리더가 명시적으로 띄울 때만
  돈다(AGENTS.md, 2026-11-14). 창을 통한 흐름은 사람의 키보드를 기다린다.
- **낡은 문서와 이름.** `IMPLEMENTATION.md`의 2026-10-04 상태("오버레이가 클라이언트에 아직 안
  보임"), `PROJECT.md`의 M8 행("planned"), `ember/app/src/ide.rs`의 `IdeAction::EmberPlaceholder`와
  "코드 영역은 M8을 기다린다"는 주석.

## 4. 제안 구조

### 4.1 "에디터 코어 완료"의 뜻

두 단계로 나눈다. 한 이슈가 끝없는 범위를 지면 닫을 수 없기 때문이다.

- **A 단계(#32를 닫는 조건).** 기준 네 개가 각각 자동 증거를 가진다. (1) 고정된 OSE에서
  `live_ose`가 초록이다. (2) 다리 네 개가 각각 실제 확장 호스트 경우를 가진다. (3) 위젯이 내는
  이벤트로 "열기, 치기, 저장, Open VSX 린터의 진단"을 끝까지 도는 테스트가 초록이고, 화면 확인이
  한 번 기록된다. (4) 버전이 다른 서버에서 창이 폴백을 제안하고 그 테스트가 있다.
- **B 단계(1.0 이후의 동등성).** `EDITOR-PARITY.md`의 행과 아래 4.3의 범위 장부로 추적한다.
  끝은 "장부의 모든 액터가 `bridged`, `acked`, `out` 가운데 하나로 분류되고, 회귀 목록의 확장이
  미분류 호출을 하나도 내지 않는다"이다.

### 4.2 계층은 그대로 둔다

`editor-conn`(와이어와 생성된 표), `editor`(세션, 상태, 다리), `app`(창)의 세 층을 유지한다.
옮기는 것은 둘이다.

- 폴백의 판정은 `editor`에 있고(`fallback.rs`), 창은 그 결과를 상태로 받기만 한다. 창이 오류
  문자열을 해석하지 않게 하려는 것이다.
- 재연결의 정책은 `app`(`editor_host.rs`의 다시 연결 루프)에서 `editor`로 내려간다. 창은
  "이어 붙음"과 "새 세션"의 두 결과만 안다.

### 4.3 액터 범위를 손으로 하나씩 쓰지 않고 넓히는 방법

생성할 수 있는 것과 없는 것을 나눈다. 의미(상태 기계, 화면)는 생성할 수 없다. 서명은 할 수 있다.

1. **프로토콜 명세 파일.** OSE 태그마다 `ember/vectors/editor-conn/<tag>/protocol.json`을 만든다:
   액터의 순서, 각 `MainThread*Shape`와 `ExtHost*Shape`의 메서드 이름, 인자 수, 반환이 `void`인지,
   원본 줄 번호. `rpc_ids.rs`와 새 `rpc_methods.rs`는 이 파일에서 나온다. 붙여 넣기를 없애려고
   생성기가 파일을 직접 쓴다.
2. **범위 장부.** `ember/editor-conn/coverage.toml`에 87개 `MainThread*` 액터마다 상태를 적는다:
   `bridged`(해석하고 상태로 답함), `acked`(빈 답이 맞다고 사람이 확인함), `out`(범위 밖, 이유),
   `todo`. 단위 테스트가 장부와 `protocol.json`을 대조해 장부에 없는 액터나 메서드가 생기면
   실패한다. 고정을 올릴 때 새 메서드가 조용히 빈 답으로 들어가지 않게 하려는 것이다.
3. **기본 답을 서명에서 고른다.** `void`는 지금처럼 `Reply::Empty`다. 값을 돌려주는데 장부가
   `todo`인 메서드는 세션이 기록한다(액터, 메서드, 부른 확장). 오류로 답할지는 Q-E8이다.
4. **우선순위는 측정으로 정한다.** `NFR-W1` 회귀 클라이언트(`nfr_w1_extension_regression.rs`)가
   그 기록을 확장별로 `expected.json` 옆에 남긴다. 많이 불리는 `todo`부터 다리를 만든다.
5. **DTO.** 다리를 놓는 부분집합의 DTO만 TypeScript에서 생성한다(`EDITOR-CONNECTION.md` §5의 다음
   단계). 전부를 생성하지 않는다. 쓰지 않는 타입 수백 개가 고정을 올릴 때마다 차이를 만들기
   때문이다.

**다리의 순서(B 단계, 제안).**

| 묶음 | 액터 | 이유 |
| --- | --- | --- |
| B1 | `Storage`, `SecretState`, `Configuration`(쓰기), `Clipboard`, `Dialogs`, `Urls`, `Window`(`$openUri`) | 화면이 거의 없고, 확장이 조용히 망가지는 곳이다 |
| B2 | `TextEditors`, `EditorTabs`, `FileSystemEventService`, `DocumentContentProviders`, `Decorations` | 편집기 상태를 확장이 읽는 경로다 |
| B3 | `StatusBar`, `Progress`, `QuickOpen`, `TreeViews` | 화면이 필요하다. UI는 호출 매크로 이전 뒤다 |
| B4 | `SCM`과 `QuickDiff`, `TerminalService`, `Task`, `DebugService`(#445), `Testing`, `Comments`, `Timeline`, `Authentication` | 각각 설계 문서가 따로 필요하다 |
| B5 | `Notebook*`, `Interactive`, `FileSystem`(확장이 주는 파일 시스템), `TunnelService` | `FR-E8`의 웹뷰 호스트와 `FR-W6`에 묶인다 |
| 밖(제안) | `LanguageModels`, `ChatAgents2`, `ChatSessions` 등 채팅과 AI 액터 약 17개 | Q-E7 |

### 4.4 고정을 올리는 절차

한 PR에서 순서대로 한다. 각 단계가 실패하면 PR이 실패한다.

1. `ember/ose/VERSION`을 올린다.
2. 생성기를 돌려 `protocol.json`, `rpc_ids.rs`, `rpc_methods.rs`를 새로 쓴다.
3. 차이 보고서를 PR 본문에 붙인다: 옮겨진 id, `bridged` 메서드 가운데 인자 수가 바뀐 것, 새로
   생긴 액터와 메서드.
4. 장부를 갱신한다(새 항목은 `todo`나 `out`).
5. `vendor-workbench-css.sh`를 다시 돌린다(D23).
6. 빌더가 `live_ose`, `live_session`, 회귀를 새 OSE 산출물로 돌린다.

**지원 집합.** `PINNED_COMMIT` 하나 대신 `SUPPORTED: &[PinnedBuild]`를 둔다. 새 커밋은 생성기가
"id 순서가 같고 `bridged` 메서드의 서명이 같다"를 보인 경우에만 들어간다. 1.139.1과 1.141.0
사이가 그랬다고 `EDITOR-CONNECTION.md` §2가 적는다. 그러면 표 하나로 이웃 릴리스를 받을 수 있고,
클라이언트와 OSE가 한 판 어긋난 상태가 폴백으로 떨어지지 않는다(Q-E3).

**카나리.** `.github/scripts/checks/pin-canary.sh`가 업스트림의 최신 릴리스 태그에서 생성기를
돌리고 지금 고정과의 차이를 찍는다. 리더가 CI를 띄울 때 함께 돈다. 예약 실행은 "개발 CI는 명시적
실행만"이라는 규칙과 부딪치므로 기본으로 두지 않는다(Q-E5).

### 4.5 폴백의 판정

판정 지점은 둘이다.

1. **열기 전.** 서버(등록된 컴퓨터면 노드)가 OSE의 커밋을 알아내 `GET /api/sessions/{id}/ide`의
   Ember 대상에 `ose_commit`으로 싣는다. 클라이언트는 `SUPPORTED`와 견주어, 맞지 않으면 IDE
   선택기에서 Ember 항목 옆에 이유를 보이고 VS Code를 제안한다. 창 프로세스를 띄운 뒤 실패하는
   것보다 빠르기 때문이다. 커밋을 어디서 읽는지(`product.json`인지 `GET /version`인지)는 구현할
   때 실제 OSE로 확인한다(AGENTS.md 규칙 4).
2. **연결할 때.** 창은 `connect_or_fallback`을 쓴다. `Fallback`이면 연결 실패 화면 대신 한 문장
   (`Fallback::message`의 내용을 한국어 합니다체로)과 `VS Code에서 열기` 단추를 보인다. 서버가
   거짓말을 하거나 OSE가 그 사이 바뀐 경우를 막는 마지막 문이다.

네트워크 오류와 인증 오류는 폴백이 아니다. 지금의 테스트
`network_or_auth_errors_do_not_fall_back`가 그 규칙을 지킨다. 자동으로 여는지 묻고 여는지는
Q-E2다. VS Code 대상의 URL은 `ide.rs`의 기존 스킴 검사를 그대로 거친다.

### 4.6 재연결

1. 세션이 두 연결의 재연결 토큰과 보내고 확인받지 못한 메시지 큐를 가진다(PersistentProtocol의
   규칙, `EDITOR-CONNECTION.md` §3.3, §3.5).
2. 끊기면 `reconnect_loop`가 릴레이를 통해 `reconnection=true`로 다시 핸드셰이크한다. 성공하면
   확장 호스트가 살아 있고, 문서와 제공자 등록이 그대로다.
3. 서버가 영구 오류("Unknown reconnection token" 등)로 답하거나 grace 시간이 지나면 지금의 경로
   (새 세션, 창이 탭과 버퍼를 되돌림, #451)로 내려간다.
4. 릴레이(`/api/computers/{computer}/ide/relay/{launch_id}`)가 같은 OSE로 두 번째 소켓을 열어
   주는지, 토큰 자리 바꿈(`FR-N9.1`)이 재연결 핸드셰이크에도 맞는지는 읽지 않았다. 첫 PR이
   확인한다.

### 4.7 사람 없이 도는 검증

| 층 | 무엇 | 상태 |
| --- | --- | --- |
| L1 | 단위와 흐름 테스트 | 있음, CI |
| L2 | 실제 프레임의 재생: `ember/vectors/editor-conn/<tag>/`에 받은 프레임을 단위 테스트가 다시 돌린다 | 없음 |
| L3 | 실제 OSE의 세션 테스트. 추가할 경우: Hover, CodeLens, 인라인 완성, 완성 목록, 서식, 검색, 웹뷰 열기 | 진단, 저장, 탐색기만 있음 |
| L4 | **창 흐름 테스트.** `WidgetBridge`에 위젯이 낼 이벤트(`on_code_changed`, `on_save_requested`, `on_code_hovered`, `on_decoration_activated`)를 넣고 실제 OSE에서 `SessionUpdate`와 디스크의 파일을 확인한다. 렌더러 없이 `ember/editor/tests`에서 돈다 | 없음 |
| L5 | 화면: 창의 스크린숏 비교와 실제 키보드 | 사람. 한글 조합, 목업 대조 |

L3에는 **탐침 확장**이 필요하다. 공개 `vscode` API만 쓰는 작은 확장(빌드 없는 JS 한 파일)이
CodeLens, Hover, 인라인 완성, 진단을 정해진 값으로 등록한다. `ember/ose/regression/probe/`에 두고
`--extensions-dir`로 넣는다. 실제 확장의 인라인 완성은 계정과 네트워크가 필요해서 CI에서 결정적으로
돌릴 수 없기 때문이다. 기준 3의 "수정하지 않은 확장"은 탐침이 아니라 `regression/extensions.json`의
Open VSX 린터 하나로 따로 증명한다(Q-E6).

L4가 남기는 사람의 몫은 "위젯이 그 이벤트를 실제로 낸다"와 "그려진 모양"뿐이다. 앞의 것은
compose-rust FR-38의 테스트가 맡는 계약이다.

### 4.8 버린 대안

- **Code-OSS를 포크해 프로토콜을 안정시킨다.** E4와 D11이 막는다. 포크는 릴리스마다 병합 비용을
  남긴다.
- **확장 호스트 쪽에 Ember 전용 동반 확장을 넣어 좁은 프로토콜로 말한다.** 수정하지 않은 확장이
  `vscode` API로 부르는 것은 결국 `MainThread*`로 온다. 동반 확장은 그 호출을 대신 받을 수 없다.
- **모든 DTO를 생성한다.** 쓰지 않는 타입의 차이가 고정을 올릴 때마다 쌓인다(4.3의 5).
- **액터마다 손으로 쓴다(지금 방식을 그대로).** 고정을 올릴 때 새 메서드가 빈 답으로 조용히
  들어온다.
- **모르는 버전에도 연결한다.** id가 하나 밀리면 호출이 다른 액터로 간다(`EDITOR-CONNECTION.md`
  §5, 위험 "High").
- **창의 새 세션 재연결만 유지한다.** 끊길 때마다 언어 서버가 다시 색인한다. 큰 프로젝트에서는
  분 단위다.
- **창 전체를 UI 자동화로 검증한다.** 렌더러와 플랫폼 창이 CI에 필요하고, 호출 매크로 이전 중에는
  깨지기 쉽다. L4가 같은 흐름의 대부분을 렌더러 없이 증명한다.

## 5. 위험

| 위험 | 완화 |
| --- | --- |
| 업스트림이 액터를 끼워 넣어 id가 밀린다 | 생성과 `editor-conn-pin`, 연결할 때의 커밋 확인(있음). 지원 집합은 생성기가 같다고 보인 커밋만 받는다 |
| AI 쪽 서명이 빨리 바뀐다(`$registerInlineCompletionsSupport`는 1.100의 8개에서 1.120의 17개) | `rpc_methods.rs`의 인자 수 대조, 탐침 확장의 L3 경우, 고정을 올리는 PR의 차이 보고서 |
| 빈 답이 확장을 조용히 망가뜨린다 | 장부와 호출 기록, 회귀 하네스가 확장별로 미분류 호출을 남긴다 |
| 클라이언트와 OSE의 버전이 어긋난다 | 지원 집합, 열기 전 판정, 창의 폴백 단추 |
| 폴백이 사용자가 고르지 않은 웹뷰 창을 연다 | 묻고 연다(Q-E2). E1은 IDE 창의 웹뷰만 허용하고, VS Code 대상이 그 창이다 |
| 프로토콜 재연결이 릴레이나 토큰 자리 바꿈과 맞지 않는다 | 첫 PR이 실제 OSE로 확인한다. 안 되면 지금의 새 세션 경로가 그대로 남는다 |
| 탐침 확장만 통과하고 실제 확장은 깨진다 | 기준 3은 Open VSX 린터로 따로 증명한다. `NFR-W1` 회귀 목록을 함께 돌린다 |
| live 검증이 풀 리퀘스트에서 돌지 않아 회귀가 늦게 보인다 | L2 재생 벡터가 CI의 `rust` 작업에서 돈다. live는 리더가 띄우는 실행에 묶는다 |
| UI 단계가 빌더 API 위에 쓰여 12월 이전과 부딪친다 | 8절의 UI 단계는 빌더 제거와 호출 매크로 이전 뒤에 호출 매크로로 쓴다. 연결과 세션 단계만 먼저 간다 |
| 확장 저장소(`Storage`, `SecretState`)를 기기에 두면 비밀이 평문으로 남는다 | `SecretState`는 플랫폼 보안 저장소가 있을 때만 켠다. 없으면 `todo`로 둔다. D45의 비밀 파일 규칙을 따른다 |
| 생성기에 Node가 필요해진다(ts-morph) | CI의 `editor-conn-pin` 작업에서만 돈다. 클라이언트와 서버의 빌드에는 들어가지 않는다(Q-E4) |

## 6. 소유자 결정 필요

모두 리더에게 올리는 질문이다. 추천은 제안일 뿐이다.

- **Q-E1. #32를 A 단계에서 닫고 나머지를 좁은 이슈로 추적하는가?** 선택: (a) A 단계가 끝나면
  닫고 B 묶음마다 이슈를 연다, (b) 우산으로 계속 연다, (c) 지금 닫는다. 추천 (a). 기준 2, 3, 4에
  빈 곳이 실제로 남아 있고, 우산은 닫을 조건이 없기 때문이다.
- **Q-E2. 버전이 맞지 않을 때 VS Code를 자동으로 여는가, 묻고 여는가?** 선택: (a) 자동, (b) 한
  문장과 단추, (c) 단추와 "이 컴퓨터는 항상 VS Code로" 기억. 추천 (b). 사용자가 고른 것은 Ember
  에디터이고, VS Code 대상은 웹뷰 창이기 때문이다.
- **Q-E3. 지원하는 서버 버전은 하나인가, 집합인가?** 선택: (a) 고정 하나(지금), (b) 생성기가
  같다고 보인 커밋의 집합, (c) 커밋마다 다른 표와 서명 분기. 추천 (b). 자동 갱신 뒤의 어긋남을
  분기 코드 없이 받기 때문이다.
- **Q-E4. 서명 생성기는 무엇으로 쓰는가?** 선택: (a) 지금의 셸 스크립트를 메서드 표까지 넓힌다,
  (b) CI 작업 안의 ts-morph, (c) Rust의 tree-sitter. 추천: 메서드 표는 (a), 다리 부분집합의 DTO는
  나중에 (b). 메서드 표는 줄 단위로 뽑히고, DTO는 타입 해석이 필요하기 때문이다.
- **Q-E5. 업스트림 카나리를 예약 실행하는가?** 선택: (a) 주 1회 예약, (b) 리더가 띄울 때와
  고정을 올릴 때만. 추천 (b). 2026-11-14의 CI 규칙(명시적 실행만)과 맞기 때문이다.
- **Q-E6. 기준 3의 "수정하지 않은 린터"는 어느 확장인가?** 선택: `regression/extensions.json`의
  한 항목을 지정한다. 추천: Open VSX에 있고 네트워크 없이 진단을 내는 린터 하나를 리더가 고른다.
  목록 자체가 아직 소유자 확인 전이기 때문이다(`NFR-W1` Status).
- **Q-E7. 채팅과 AI 액터(`LanguageModels`, `ChatAgents2` 등)는 범위 밖인가?** 선택: (a) 밖(빈 답),
  (b) 나중에 Ember의 에이전트에 잇는다. 추천 (a). Ember의 에이전트는 하네스이고(D4), 이 서명이
  가장 빨리 바뀌기 때문이다. D38의 "기능은 OSS와 같다"의 예외가 되므로 소유자의 말이 필요하다.
- **Q-E8. 값을 돌려주는 미구현 메서드에 빈 답을 주는가, 오류를 주는가?** 선택: (a) 빈 답(지금),
  (b) 오류, (c) 먼저 회귀 하네스로 세고 메서드별로 정한다. 추천 (c). 어느 쪽이 확장을 덜
  깨뜨리는지 아는 자료가 없기 때문이다.
- **Q-E9. 성능 요구의 숫자.** 선택: (a) 숫자 없이 측정값만 기록한다, (b) 측정 뒤 예산을 정한다.
  추천 (b). 지금은 잴 하네스가 없어 숫자를 정할 근거가 없다.
- **Q-E10. 마이크로소프트 빌드(VSC 런타임)에 에디터 코어를 붙이는가?** 선택: (a) OSE만, (b) 공개
  태그마다 표를 만든다. 추천 (a). D10의 라이선스 해석이 법률 검토 전이고, 검증할 조합이 릴리스
  수만큼 늘기 때문이다. `EDITOR-CONNECTION.md` §7의 열린 질문이다.

## 7. SPEC/INTENT 변경 제안

적용하지 않았다. 아래는 제안 문안이고 모두 [provisional]이다. 번호는 적용할 때 배정한다(다른
1.0 이후 문서가 같은 번호를 쓸 수 있다).

**§E 머리말(교체).**

> §E: Editor core. `ember/editor-conn`, `ember/editor` and the Ember 에디터 window exist. A row is
> done when its acceptance column passes; rows without an acceptance column are drafts.

**표 머리(교체).** `| ID | Requirement | Acceptance |`. `FR-E1`부터 `FR-E6`에 세 번째 칸을 채운다.

**새 행.**

> **FR-E9** *[provisional]* **Protocol coverage is a ledger.** Every `MainThread*` actor of the
> pinned Code-OSS is listed in `ember/editor-conn/coverage.toml` as `bridged`, `acked`, `out` or
> `todo`. Method names and arities are generated from `extHost.protocol.ts` at the pinned tag. A
> call to a `todo` method that returns a value is recorded with the calling extension.
> Acceptance: a unit test fails when the generated protocol has an actor or method the ledger
> lacks; the `NFR-W1` run stores the recorded calls per extension.

> **FR-E10** *[provisional]* **Supported servers and the fallback.** The editor connects only to a
> server whose commit is in the supported set; a commit joins the set only when the generator
> shows the same actor order and the same signatures for every `bridged` method. The server
> reports the OSE commit with the Ember target. On any other commit, or none, the window does not
> connect; it says so in one sentence and offers `VS Code에서 열기`. Network and auth errors never
> switch targets. Acceptance: `connect_and_fallback.rs`; a window-state test for the fallback
> state; a server test for `ose_commit`.

> **FR-E11** *[provisional]* **Reconnection resumes the session.** A lost socket is reopened with
> the reconnection token and the unacknowledged messages are sent again; documents, providers and
> the extension host survive. A permanent error or the end of the grace time starts a new session
> as in `FR-L31.6`. Acceptance: a fake-server test that drops the socket mid-edit; a live test
> that kills the relay socket and checks that the extension host process id is unchanged.

> **FR-E12** *[provisional]* **Verification without a keyboard.** Each bridge (diagnostics,
> CodeLens, hover, inline completion) has a live case against a real OSE with the probe extension
> in `ember/ose/regression/probe/`. A window-flow test feeds the widget bridge the events the
> `CodeEditor` emits and checks the saved file and the diagnostics of one unmodified Open VSX
> extension. Recorded frames in `ember/vectors/editor-conn/<tag>/` are replayed by unit tests.
> Acceptance: the tests named in the row; what still needs a person is listed (IME composition on
> screen, the rendered window).

> **NFR-E1** *[provisional]* **Editor latency is measured.** The live job prints the time from a
> widget change to the acknowledged `$acceptModelChanged`, the first diagnostics after open, and
> the time to open a 10,000-line file, directly and through the relay. Budgets are set after the
> first three runs.

**Status 표의 `FR-E1–E5` 행(문장 추가).**

> The version check runs at connect; the window does not yet offer the VS Code fallback
> (`connect_or_fallback` is not called by `editor_host.rs`). Hover, CodeLens and inline completion
> have no live case; the hover popup is not drawn.

**INTENT(새 결정, 번호 미정).**

> **Dxx: The editor core supports a set of Code-OSS commits, proven equal by generation.**
> [provisional] Rejected: one pin only (a self-updated client meets an older OSE and falls back);
> per-commit signature branches (untestable matrix); connecting to unknown commits (a shifted id
> delivers calls to the wrong actor).

> **Dxx: Chat and AI actors of Code-OSS are outside the Ember 에디터.** [provisional, needs the
> owner: it is an exception to D38] Rejected: bridging them to Ember's agents (the agents are
> harnesses, D4; these signatures change in most releases).

**그 밖의 낡은 문장(문서만 고치는 커밋).** `IMPLEMENTATION.md`의 2026-10-04 상태 문단,
`PROJECT.md`의 M8 행("planned"), `EDITOR-PARITY.md`의 Hover 행(닫힌 #442를 가리킨다),
`EDITOR-CONNECTION.md` §6의 3번("Reconnect is still to do"는 맞지만 #451의 창 재연결을 적지
않는다).

## 8. 작업 분해

추정은 에이전트 세션 하나의 작업일이고 거칠다. 코딩 단계는 빌드하지 않는다. 빌드와 테스트는 빌더
서브 에이전트가 한다(AGENTS.md). `ember/editor-conn`이나 `ember/editor`가 바뀌면 의존하는 크레이트의
`Cargo.lock`을 같은 PR에서 갱신한다. **UI 단계는 12월의 빌더 API 제거와 호출 매크로 이전 뒤에
호출 매크로로 쓴다.** 연결과 세션 단계는 그 이전에 막히지 않는다.

| # | 무엇 | 크레이트 | 증명하는 테스트 | 일 | 사람 |
| --- | --- | --- | --- | --- | --- |
| 1 | 문서 정리: §E 머리말과 표 칸, Status 행, 낡은 문장(7절). SPEC ID를 먼저 넣는다 | 문서 | `check-no-em-dash.sh` | 0.5 | 리더 승인 |
| 2 | 탐침 확장과 L3 경우 셋(Hover, CodeLens, 인라인 완성) | `ember/ose/regression/probe`, `ember/editor/tests/live_session.rs` | `live_session_probe_hover_codelens_inline` (`#[ignore]`, live 작업) | 2 | 없음 |
| 3 | L4 창 흐름 테스트: 열기, 치기, 저장, Open VSX 린터의 진단 | `ember/editor/tests/live_window_flow.rs` | `live_window_flow_type_save_linter` | 1.5 | Q-E6의 확장 지정 |
| 4 | 서버와 노드가 OSE 커밋을 보고한다 | `ember/node`, `ember/server` | `fr_w4_ide_reports_ose_commit` (가짜 바이너리), 실제 OSE 확인은 live 작업 | 1 | 없음 |
| 5 | 세션이 폴백을 상태로 낸다. `editor_host.rs`가 `connect_or_fallback`을 쓰고 `HostState`에 폴백 대상을 둔다(화면 없음) | `ember/editor`, `ember/app`(UI 아님) | `fr_e10_host_state_carries_fallback` | 1 | 없음 |
| 6 | 창의 폴백 문장과 단추, IDE 선택기의 사전 안내 | `ember/app`(UI, 이전 뒤) | 창 상태 테스트, 스크린숏 | 1 | 화면 확인 |
| 7 | Hover 팝업을 창에 그린다 | `ember/app`(UI, 이전 뒤) | 글루 테스트(`editor_workbench_glue_tests.rs`) | 1 | 화면 확인 |
| 8 | 화면 확인 한 번: 한글 조합으로 치기, 저장, 린터의 물결 밑줄, CodeLens, 고스트 텍스트. 결과를 Status 표에 적는다 | 없음 | 기록 | 0.5 | **소유자나 리더의 손과 화면** |
| 9 | #32를 닫는다(Q-E1의 (a)일 때). B 묶음마다 이슈를 연다 | 없음 | 2, 3, 5, 6, 8의 증거 | 0.2 | 리더 |
| 10 | 생성기: `protocol.json`, `rpc_methods.rs`, 파일을 직접 쓰기 | `ember/editor-conn/scripts`, `ember/vectors/editor-conn`, `test.yml` | `editor-conn-pin`이 세 파일을 비교 | 2 | Q-E4 |
| 11 | 범위 장부와 호출 기록 | `ember/editor-conn`, `ember/editor` | `fr_e9_ledger_covers_generated_protocol`, 회귀 클라이언트의 기록 | 1.5 | Q-E8 |
| 12 | 지원 집합과 동등성 검사 | `ember/editor-conn`, `ember/editor/src/fallback.rs` | `fr_e10_supported_set_*`, `connect_and_fallback.rs` 갱신 | 1.5 | Q-E3 |
| 13 | `pin-canary.sh`와 고정 올리기 절차 문서 | `.github/scripts/checks`, `ember/ose/README.md` | 스크립트의 자체 테스트 | 1 | Q-E5 |
| 14 | L2 프레임 받기와 재생 | `ember/editor-conn`, `ember/vectors/editor-conn` | `replay_recorded_frames_<tag>` (CI `rust` 작업) | 1.5 | 없음 |
| 15 | 프로토콜 재연결(세션까지) | `ember/editor-conn`, `ember/editor` | 가짜 서버의 끊김 테스트, live의 확장 호스트 pid 유지 | 3 | 없음 |
| 16 | 재연결 상태를 창에 반영 | `ember/app`(UI, 이전 뒤) | 창 상태 테스트 | 1 | 화면 확인 |
| 17 | B1: `Storage`, `Configuration` 쓰기, `Clipboard`, `Urls`. `SecretState`는 보안 저장소가 있을 때만 | `ember/editor`, `ember/app`(저장 경로, UI 아님) | `fr_e9_storage_round_trip`, 탐침 확장의 live 경우 | 3 | 없음 |
| 18 | B2: `TextEditors`, `EditorTabs`, `FileSystemEventService`, `DocumentContentProviders` | `ember/editor-conn`, `ember/editor` | 흐름 테스트와 탐침 경우 | 4 | 없음 |
| 19 | B3의 세션 모델: `StatusBar`, `Progress`, `QuickOpen`, `TreeViews` | `ember/editor` | 흐름 테스트 | 4 | 없음 |
| 20 | B3의 화면 | `ember/app`(UI, 이전 뒤) | 글루 테스트, 스크린숏 | 4 이상 | 화면 확인, 목업에 없는 화면은 소유자 |
| 21 | 지연 측정 하네스(`NFR-E1`) | `ember/editor/tests` | live 작업의 출력 | 1.5 | Q-E9 |
| 22 | B4, B5: 액터 묶음마다 설계 문서부터 | 문서 | 없음 | 정하지 않음 | 리더 |

의존: 2와 3은 서로 독립이다. 5는 4 없이도 된다(연결할 때의 판정만). 6, 7, 16, 20은 호출 매크로
이전을 기다린다. 9는 2, 3, 5, 6, 8을 기다린다. 11과 12는 10을 기다린다. 17부터 19는 11을 기다린다.

A 단계(1부터 9)의 합은 약 9일이고, 그 가운데 6, 7, 8의 2.5일은 이전과 사람을 기다린다. B 단계의
10부터 21은 약 28일이다. 22는 재지 않았다.

실행 예(빌더만 돌린다):

```
CARGO_BUILD_JOBS=2 cargo test --locked --manifest-path ember/editor/Cargo.toml \
  --test live_window_flow -- --ignored --nocapture --test-threads 1
```

## 확인하지 못한 것

- 어떤 테스트도 돌리지 않았다. CI 실행 번호와 "첫 초록"은 Status 표의 말이다.
- 화면을 보지 않았다. "Hover 팝업이 없다"는 `ember/app/src`에서 `doc.hover`를 읽는 코드를 찾지
  못했다는 뜻이고, 다른 이름으로 그리는 코드가 있을 수 있다.
- 릴레이가 재연결 소켓을 받는지, OSE가 커밋을 어디에 내놓는지, 서버의 grace 시간.
- compose-rust에 창을 사람 없이 모는 테스트 도구가 있는지(FR-38과 FR-53의 머리 부분만 읽었다).
- `ember/ose/regression/extensions.json`의 내용.
- 71개 액터 가운데 `acked`가 맞는 것이 몇 개인지. 장부를 만들어야 알 수 있다.
