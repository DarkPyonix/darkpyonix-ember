# 탭 떼어 내기와 네이티브 브리지: 1.0 이후 설계 (#30, FR-B1~B4)

상태: 제안. 이 문서는 설계 노트이고 SPEC, INTENT에는 아직 반영하지 않았다. 소유자 결정이 필요한
항목은 6절에 질문으로 모았다. 기준 커밋은 `develop` 8ea03cb9 (2026-11-20)이다.

## 1. 범위

다루는 것:

- IDE 탭을 창 밖으로 끌어 새 창으로 여는 동작과, 다시 끌어 넣어 합치는 동작 (FR-B1~B3).
- 창과 호스트 사이의 메시지 통로 (FR-B2, FR-B4)와 그 측정 (NFR-B1, NFR-B2).
- 세 가지 IDE 대상(D32) 가운데 두 가지: Ember 에디터(네이티브 창)와 VS Code(VS Code Web).

다루지 않는 것:

- JetBrains Gateway. Gateway는 자기 창을 스스로 관리하므로 Ember가 떼어 낼 탭이 없다.
- 터미널 탭, 대화 화면의 탭. 이 문서는 에디터 탭만 다룬다. 같은 구조를 뒤에 재사용할 수 있다.
- 확장 웹뷰 호스트 자체 (FR-E8, `docs/design/EDITOR-WEBVIEWS.md`). 여기서는 의존 관계만 적는다.

## 2. 현재 있는 것

모두 코드에서 확인했다.

| 조각 | 위치 | 상태 |
| --- | --- | --- |
| 떼어 내기 스크립트 | `ember/proxy/static/detach.js` (604줄), `ember/proxy/dpx/vscode/html_rewrite.py`가 주입 | CI. `node --test ember/proxy/tests/js/detach.test.js`, `ember/proxy/tests/test_inject.py` |
| 메시지 타입 | `ember/bridge/src/messages.rs`: `TabDetach`, `OpenWindow`, `SiblingWindowClosed`, `Inbound::UnknownKind` | CI, 공유 벡터 `ember/vectors/bridge/detach_vectors.json` |
| 플랫폼 추상화 | `ember/bridge/src/bridge.rs`: `WebviewBridge`, `LoopbackBridge` | CI. 플랫폼 구현은 없다 |
| 호스트 로직 | `ember/bridge/src/host.rs`: `BridgeHost`, `WindowOpener`, `notify_sibling_closed` | CI |
| 여러 창 바닥 | compose-rust FR-53 (`Window` 노드, `DragFromPointer`, `WindowDrag`, `WindowDragOver`, 마지막 창 정책). compose-rust#355는 닫혔다 | Ember의 핀은 `v0.0.11` |

확인한 사실 가운데 설계를 좌우하는 것은 넷이다.

1. **`ember-bridge`를 쓰는 크레이트가 없다.** `ember/` 아래 어느 `Cargo.toml`도 이 크레이트에
   의존하지 않는다. 호스트 로직은 테스트 안에서만 돈다.
2. **VS Code 대상은 OS 기본 브라우저에서 열린다.** `ember/app/src/ide.rs`의 `plan`이
   `IdeAction::OpenUrl`을 내고 `open_external`이 `open`, `rundll32`, `xdg-open`에 넘긴다. Ember가
   가진 웹뷰 창은 없다. 그래서 `detach.js`의 네이티브 경로(`webkit.messageHandlers.emberBridge`,
   `chrome.webview`)를 받을 상대가 없고, 실제로 도는 길은 브라우저 폴백(새 창을
   `payload=[["openFile",...]]` URL로 여는 것)뿐이다.
3. **Ember 에디터는 창마다 프로세스가 하나다.** "Open IDE → Ember"는 같은 바이너리를
   `--editor <project> <folder> <endpoint>`로 다시 띄운다 (`ember/app/src/ide.rs`의 `EDITOR_ARG`,
   `ember/app/src/editor_host.rs`). 창을 HTML 모드로 통째로 띄우는 것은 dioxus-compose#161
   (열림: Compose 루트 아래의 `HtmlView`가 실제 렌더러에서 죽는다)의 우회다. FR-53은 한 프로세스
   안의 여러 창을 전제로 하므로, 지금 구조에서는 `WindowDragOver`로 합칠 상대 창이 같은 프로세스에
   없다.
4. **에디터 탭 줄에 끌기가 없다.** `ember/app/src/ui/editor_workbench.rs`의 탭 모델은
   `EditorTabs` 하나이고, 탭 순서는 "Move Editor Left / Right" 명령으로만 바뀐다 (주석: "the reorder
   drag cannot do yet"). 떼어 내기는 순서 바꾸기 끌기 위에 얹히는 동작이므로 이것이 먼저다.

## 3. 빈 곳

| 항목 | 빈 곳 |
| --- | --- |
| FR-B1 | VS Code Web 쪽은 구현됐다. Ember 에디터에는 끌기 자체가 없다. 터치는 범위 밖으로 적혀 있다 |
| FR-B2 | 메시지는 정의됐으나 받는 호스트가 없다. Ember 에디터가 같은 스키마를 쓴다는 것은 `lib.rs` 주석에만 있다 |
| FR-B3 | 브라우저 폴백은 커서만 옮긴다. 선택 범위와 스크롤은 넘어가지 않는다. "한 프레임 안에 창이 뜬다"는 기준은 페이지를 새로 읽는 경로에서는 지킬 수 없다 |
| FR-B4 | `sibling_window_closed`를 보낼 실제 창 목록이 없다 |
| NFR-B1, NFR-B2 | 측정한 적이 없다. `TabDetach::latency_ms`는 있으나 부르는 곳이 없다 |
| 합치기 | SPEC §B에 행이 없다. compose-rust 53.7에는 있다 |
| 검증 | 실제 브라우저, 실제 네이티브 창에서 확인한 적이 없다 (#30 다섯째 기준) |
| 문서 불일치 | #30 본문의 "Versioned message ... version mismatch logged"는 D15(스키마에 버전 없음)와 FR-B2("No version field")에 맞지 않는다 |

## 4. 제안 구조

### 4.1 큰 그림: 대상마다 길이 다르다

```
                    ┌──────────────────────────── ember-app (에디터 프로세스 하나) ───────────────┐
 Ember 에디터       │  EditorSession (확장 호스트 연결 1개, 문서 모델 공유)                        │
 (경로 A, 주 경로)  │     ├─ Window 노드 1: 탭 그룹 ──┐  DragFromPointer / WindowDragOver          │
                    │     └─ Window 노드 2: 탭 그룹 ──┘  (compose-rust FR-53, 직렬화 없음)         │
                    └──────────────────────────────────────────────────────────────────────────────┘

 VS Code, 브라우저   detach.js ── window.open(payload URL) ──▶ 새 브라우저 창        (경로 B, 지금 동작)
 VS Code, Ember 창   detach.js ── tab_detach ──▶ WebviewBridge 구현 ──▶ BridgeHost ──▶ WindowOpener
                     (경로 C, 플랫폼 웹뷰 노드가 생긴 뒤. 6절 Q-B3)
```

**경로 A를 주 경로로 삼는다.** 이유는 셋이다. 소유자가 FR-53을 지시하며 든 예가 "electron처럼 IDE 탭
떼서 새 창"이고 Ember 자신의 IDE는 Ember 에디터다 (D9a). 경로 A만 문서 모델을 나눠 가지므로 저장하지
않은 탭도 옮길 수 있다. 경로 C는 아직 없는 플랫폼 웹뷰 노드에 막혀 있다.

### 4.2 경로 A: Ember 에디터, 한 프로세스의 여러 창

**프로세스 모델.** 에디터 프로세스 하나가 `(project, computer, folder)` 하나를 맡고, 그 안에 `Window`
노드를 여러 개 둔다. `EditorSession`과 확장 호스트 연결은 프로세스에 하나다. 창을 떼어도 확장 호스트가
하나 더 뜨지 않는다. VS Code의 보조 창(auxiliary window)과 같은 모양이다.

**모델.** 지금의 `tabs: Signal<EditorTabs>` 하나를 창 그룹으로 넓힌다.

```
EditorWindows {
    groups: Vec<WindowGroup>,        // 순서는 연 순서, 첫 그룹이 주 창
}
WindowGroup {
    key: RestoreKey,                 // "<project>/<folder>#<n>", FR-53.3
    tabs: EditorTabs,                // 지금 타입 그대로
    chrome: Main | Auxiliary,        // 보조 창은 탭 줄과 에디터 영역, 상태 줄만 그린다
}
```

- 문서(버퍼, 실행 취소 기록, 진단)는 그룹이 아니라 세션에 속한다. 탭은 문서를 가리키는 보기다. 그래서
  떼어 내기는 "보기 하나를 다른 그룹으로 옮기기"이고, 저장하지 않은 문서도 그대로 옮겨진다.
- 보기 상태(커서, 선택, 스크롤, 접기)는 `ember_bridge::OpenWindow`의 필드와 같은 이름으로 들고 다닌다.
  프로세스 안에서는 직렬화하지 않고 구조체로 넘긴다. 같은 구조체를 창 복원(D43의 핫 엑시트 백업)과
  아래의 프로세스 폴백이 직렬화해서 쓴다. `editor` 필드는 `"ember"`다.
- 보조 창이 닫히면 그 그룹의 탭은 닫힌다. 저장하지 않은 문서가 있으면 지금의 닫기 확인
  (`close_confirm`)을 그대로 쓴다. 주 창이 닫히면 프로세스가 끝나므로 보조 창도 같이 닫힌다
  (FR-53.6의 마지막 창 정책을 "주 창이 닫히면 종료"로 둔다. 6절 Q-B5).

**몸짓.** FR-B1의 규칙을 Ember 에디터에도 같은 숫자로 적용한다.

1. 탭이 FR-46의 `Down`, `Move`를 구독한다. 탭 줄 안에서의 움직임은 순서 바꾸기다.
2. 포인터가 탭 줄에서 48 dp 넘게 벗어나면(`detach.js`의 48 px와 같은 값) 떼어 내기로 판정한다.
   판정은 한 번만 하고, 판정 뒤에는 순서 바꾸기로 돌아가지 않는다. "둘 다는 절대 아니다"가 지켜진다.
3. 같은 배치에서 원래 그룹의 탭을 지우고 새 `Window` 노드를 `DragFromPointer`와 함께 넣는다
   (FR-53.7). 창은 포인터를 따라온다.
4. 다른 그룹의 탭 줄이 `OnWindowDragOver`를 구독한다. `Enter`, `Move`에서 놓일 자리를 그리고
   `Drop`에서 탭을 그 그룹으로 옮긴 뒤 끌던 `Window` 노드를 지운다.
5. 탭이 하나뿐인 보조 창의 탭을 끌면 창 전체가 끌린다. 주 창의 마지막 탭은 떼어 내지 않는다
   (빈 주 창을 남기지 않기 위해서다).

**끌 수 없는 플랫폼.** FR-53.7.1에 따라 iPhone과 Android 12L 미만에서는 Renderer가 끌기를 시작하지
않는다. 탭 메뉴(`tab_menu`)에 "새 창에서 열기"와 "주 창으로 합치기"를 두고, 두 키맵(D32)에 단축키를
건다. 데스크톱에서도 같은 메뉴가 보인다. 끌기가 안 되는 상황(트랙패드 접근성 설정 등)의 길이기도 하다.

**선행 조건과 확인할 것.**

- dioxus-compose#161. 지금 에디터 창은 HTML 모드로 통째로 뜬다. HTML 모드 프로세스 안에서 `Window`
  요소(dioxus-compose#152, 핀에 들어 있다)를 여러 개 둘 수 있는지는 **확인하지 못했다.** 작업 분해의
  A2가 이 확인이다. 안 되면 dioxus-compose에 이슈를 내고 그 뒤 단계는 기다린다.
- Ember의 compose-rust 빌더 API 제거(12월, call 매크로로 이전)가 먼저다. 아래 UI 단계는 그 이전이
  끝난 뒤에 시작하고 빌더 API로는 쓰지 않는다. 순수 모델 단계(A1)는 UI API에 닿지 않으므로 먼저 할 수
  있다.

**버린 대안: 창마다 프로세스.** 지금 구조를 그대로 써서 `--editor`에 `OpenWindow`를 넘겨 프로세스를
하나 더 띄우는 길이다. 툴킷 의존이 없어 가장 빠르다. 버린 이유: 창마다 확장 호스트 연결이 하나씩 늘고,
문서 모델이 갈라져 저장하지 않은 탭을 옮길 수 없고, `WindowDragOver`가 프로세스를 넘지 못해 합치기가
안 된다. 다만 A2의 확인이 실패해 여러 창이 오래 막히면 "새 창에서 열기" 메뉴만 이 방식으로 먼저 내는
것은 가능하다. 그때 상태는 `OpenWindow` JSON을 환경 변수로 넘긴다 (토큰과 같은 이유로 argv에는
싣지 않는다).

### 4.3 경로 B: VS Code Web, 브라우저

이미 동작하는 길이다. 남은 것은 검증과 측정이다.

- **자동 검증.** 실제 OSE `serve-web`에 헤드리스 Chromium을 붙여 CDP의 `Input.dispatchDragEvent`로
  탭을 끈다. 확인 항목: 짧은 끌기는 순서만 바뀐다, 긴 끌기는 새 창이 열리고 URL의 `payload`가 그
  파일과 커서를 가리킨다, 저장하지 않은 탭은 거절되고 토스트가 뜬다. 자리는 `test-ose.yml`의 실제
  서버 검사 옆이다. 사람 손이 필요 없다.
- **NFR-B1 측정.** 같은 하네스에서 새 창의 `navigationStart`부터 에디터가 포커스를 받을 때까지를
  재고, `payload` 없는 평범한 페이지 로드와 비교한다. 숫자는 SPEC에 적는다.
- 브라우저 창끼리는 합칠 수 없다. 브라우저가 다른 창의 탭 줄 위에 있다는 것을 페이지에 알려 주지
  않기 때문이다. 경로 B에서 합치기는 범위 밖으로 명시한다.
- 선택 범위와 스크롤은 VS Code Web의 `payload`가 받지 않는다. 새 창이 뜬 뒤 `detach.js`가
  `BroadcastChannel`로 원래 창에서 상태를 받아 적용하는 방법이 있으나, VS Code 내부 API에 닿아야 해서
  E4(소스 비패치)의 경계에 가깝다. 하지 않는 것을 권한다 (6절 Q-B6).

### 4.4 경로 C: VS Code Web, Ember 창 안의 웹뷰

`WebviewBridge`의 플랫폼 구현이 사는 곳이다. 전제는 Ember가 VS Code Web을 자기 창에 담는 것이고,
그것은 지금 없는 결정이자 없는 부품이다.

- 부품: compose-rust의 플랫폼 웹뷰 노드. FR-E8(확장 웹뷰)을 위해 계획된 것과 같은 노드다
  (`EDITOR-WEBVIEWS.md`, 미구현). 그 노드의 메시지 통로 위에 `WebviewBridge`를 구현하면 플랫폼별
  코드를 Ember가 따로 쓰지 않는다: `send_to_webview`는 노드의 `postMessage`, `send_to_native`는 노드의
  수신 콜백이다. `detach.js`가 찾는 전역(`webkit.messageHandlers.emberBridge`, `chrome.webview`)은
  노드의 "페이지 스크립트 전에 넣는 스크립트"로 맞춘다.
- 호스트: `BridgeHost<EmberWindowOpener>`. `WindowOpener::open`은 같은 프로세스에 `Window` 노드를 하나
  더 넣고 그 안의 웹뷰를 `OpenWindow::vscode_web_path`로 연다. `sibling_window_closed`는 실제 창
  목록에서 나온다.
- 규칙: `check-no-webview.sh`(NFR-L2)의 예외는 FR-E8의 호스트 모듈 하나로 계획돼 있다. 경로 C는
  예외를 하나 더 요구한다. 런처와 대화 화면은 여전히 웹뷰를 만들지 않는다 (E1).

경로 C는 소유자 결정(Q-B3)과 웹뷰 노드가 모두 있어야 시작한다. 그 전까지 VS Code 대상의 떼어 내기는
경로 B다.

## 5. 위험

| 위험 | 완화 |
| --- | --- |
| HTML 모드 프로세스에서 `Window` 요소 여러 개가 안 될 수 있다 (dioxus-compose#161 계열) | A2에서 가장 먼저 확인한다. 실패하면 메뉴 동작만 프로세스 폴백으로 내고 끌기는 기다린다 |
| 끄는 동안 창이 포인터를 못 따라온다 (FR-53.7의 1 dp 요구) | Renderer의 책임이다. Ember는 `fr53_*` 검사가 도는 핀만 쓴다. 플랫폼별 눈 확인은 소유자 항목이다 |
| 보기 하나가 두 그룹에 동시에 속하는 버그 | 모델 단계(A1)에서 "문서당 보기 목록, 보기당 그룹 하나"를 불변식으로 두고 속성 테스트로 지킨다 |
| 보조 창의 단축키와 포커스가 주 창으로 샌다 | 명령 라우팅을 "포커스를 가진 그룹" 기준으로 바꾼다. `WindowFocusChanged`로 추적한다 |
| 창이 늘면 렌더 비용이 는다 | 보조 창은 탭 줄, 에디터, 상태 줄만 그린다. 탐색기와 하단 패널은 주 창에만 둔다 |
| 경로 C가 E1 위반으로 읽힌다 | 예외를 SPEC에 모듈 이름으로 못 박고 검사 스크립트가 그 목록만 허용한다 |
| Wayland 직접 창에서는 `DragFromPointer`가 다르게 동작한다 (FR-53.9) | 데스크톱 렌더러는 X11/XWayland다. 그 범위 밖은 지원하지 않는다고 적는다 |

## 6. 소유자 결정 필요

각 항목은 리더를 거쳐 소유자에게 묻는다. 권고는 제안일 뿐이다.

- **Q-B1. 떼어 내기를 어느 대상부터 만드는가?** (a) Ember 에디터 먼저, (b) VS Code Web의 Ember 창
  먼저, (c) 둘 다 같이. **권고 (a).** Ember 에디터가 Ember 자신의 IDE이고, (b)는 아직 없는 웹뷰 노드에
  막혀 있다.
- **Q-B2. Ember 에디터의 창 모델은?** (a) 프로세스 하나에 창 여러 개(FR-53), (b) 창마다 프로세스.
  **권고 (a).** 합치기와 저장하지 않은 탭 옮기기가 (a)에서만 된다. (b)는 A2가 실패할 때 메뉴 동작의
  임시 길로만 둔다.
- **Q-B3. VS Code 대상을 Ember 창(플랫폼 웹뷰)에 담는가, 계속 기본 브라우저로 여는가?** D32는
  "Ember's IDE window에 감싼다"고 적지만 지금 구현은 브라우저다. (a) 브라우저 유지, (b) 웹뷰 노드가
  생기면 Ember 창으로 옮기고 NFR-L2 예외를 하나 더 둔다. **권고: 지금은 (a), 웹뷰 노드(FR-E8)가
  들어온 뒤 (b)를 다시 묻는다.** 그 전에는 만들 수 없기 때문이다.
- **Q-B4. 저장하지 않은 탭을 떼어 낼 수 있는가?** Ember 에디터에서는 문서 모델을 나누므로 가능하다.
  VS Code Web(경로 B, C)에서는 새 창이 다른 문서라 지금처럼 거절한다. **권고: 그대로 (Ember 에디터
  허용, VS Code Web 거절).**
- **Q-B5. 주 창을 닫으면 보조 창은?** (a) 같이 닫히고 프로세스가 끝난다, (b) 보조 창 하나가 주 창으로
  승격된다. **권고 (a).** VS Code의 보조 창과 같고 구현이 단순하다.
- **Q-B6. 브라우저 폴백에서 선택 범위와 스크롤까지 옮기는가?** **권고: 옮기지 않는다.** VS Code
  내부에 닿아야 하고(E4), 커서가 맞으면 쓰는 데 지장이 없다. SPEC에 한계로 적는다.
- **Q-B7. FR-B3의 "한 프레임 안" 기준을 고치는가?** **권고: 고친다.** 프로세스 안의 창(경로 A)은
  "판정 다음 프레임에 창이 뜬다", 페이지를 읽는 창(경로 B, C)은 NFR-B1로 잰다 (7절).
- **Q-B8. #30 본문의 "Versioned message" 기준을 D15에 맞게 고치는가?** **권고: 고친다.** FR-B2와
  D15가 이미 "버전 없음, 모르는 kind는 기록하고 보존"으로 정했다.
- **Q-B9. 모바일 범위는?** **권고: 1차는 데스크톱 세 플랫폼의 끌기와 모든 플랫폼의 메뉴 동작.**
  iPadOS와 Android 12L 이상의 끌기는 FR-53.7.1의 Renderer 단계가 들어온 뒤 따로 잡는다.

## 7. SPEC/INTENT 변경 제안

적용하지 않은 제안문이다. 모두 [provisional]로 넣는다.

**§B 제목과 머리말.** "Native bridge (IDE window webview ↔ native shell)"를 "IDE 창 떼어 내기와
브리지"로 넓히고, 대상별 길(4.1)을 한 문단으로 적는다.

**FR-B1 (수정).**
> Tab drag-out is detected without interfering with tab reorder, in VS Code Web (injected script)
> and in the Ember 에디터 (pointer events on the tab strip). The threshold is 48 px (dp) from the
> tab strip in both. | A short drag reorders; a long drag detaches; never both. The decision is
> made once per gesture.

**FR-B2 (수정).**
> On detach the view state (file URI, cursor, selection, scroll, source window) travels to the new
> window. In VS Code Web it is a `tab_detach` message through the platform webview message API, or
> the payload URL in a plain browser. In the Ember 에디터 the same fields move in process as an
> `OpenWindow` value, without serialization. | No version field; an unknown kind is logged, not
> dropped. In the Ember 에디터 an unsaved document moves with its edits and undo history.

**FR-B3 (수정).**
> The new window opens at the same project and computer with that state. | Ember 에디터: the window
> appears on the frame after the detach decision and follows the pointer until release
> (compose-rust FR-53.7). VS Code Web: bounded by `NFR-B1`. Plain browser: the cursor survives; the
> selection range and scroll do not.

**FR-B5 (신규).**
> *[provisional]* A torn-off Ember 에디터 window dropped on another window's tab strip merges into
> it (compose-rust FR-53.7 `WindowDragOver`). | The tab lands at the indicated position with its
> state; the emptied window closes. Browser windows do not merge.

**FR-B6 (신규).**
> *[provisional]* "새 창에서 열기" and "주 창으로 합치기" are tab menu actions with keymap entries in
> both keymaps, on every platform. | On platforms where the system offers no window drag (iPhone,
> Android below 12L) they are the only path (compose-rust FR-53.7.1).

**FR-B7 (신규, Q-B3이 (b)일 때만).**
> *[provisional]* The VS Code target opens in an Ember window hosting VS Code Web in a platform
> webview; `WebviewBridge` is implemented over that webview's message channel. | `NFR-L2`'s check
> allows exactly this module and the `FR-E8` host module.

**INTENT.** 새 결정 하나를 제안한다: "Ember 에디터의 창은 한 프로세스가 가진다. 창을 떼어도 확장 호스트
연결과 문서 모델은 하나다. 버린 것: 창마다 프로세스(합치기와 저장 전 탭 이동이 안 된다)." D32의 "wrapped
in Ember's IDE window"는 Q-B3의 답에 맞춰 고친다.

**Status 표.** FR-B 행에 "어느 크레이트도 `ember-bridge`에 의존하지 않는다, VS Code 대상은 기본
브라우저에서 열린다"를 적는다. 지금 문장("No platform webview host")보다 정확하다.

## 8. 작업 분해

추정은 에이전트 세션 하나의 작업일 기준이고 거칠다. 빌드와 테스트는 빌더가 돌린다.

| # | 내용 | 닿는 곳 | 증명하는 테스트 | 추정 | 비고 |
| --- | --- | --- | --- | --- | --- |
| D1 | SPEC §B와 Status 표 고치기, FR-53 연결, #30 본문 정정 | `docs/` | 문서 검사 | 0.5일 | Q-B7, Q-B8의 답 뒤 |
| A1 | `EditorWindows` 순수 모델: 그룹, 보기 이동, 합치기, 마지막 창 규칙 | `ember/app` (모델만) | `fr_b5_*` 단위와 속성 테스트 | 2일 | UI API에 닿지 않는다. 지금 시작할 수 있다 |
| A2 | 확인: HTML 모드 에디터 프로세스에서 `Window` 요소 두 개 | `.scratch/` 프로브 | 프로브가 창 두 개를 띄운다 | 1일 | 빌더와 화면이 필요하다. 실패하면 dioxus-compose 이슈 |
| A3 | 탭 줄의 포인터 끌기로 순서 바꾸기 | `ember/app` | `fr_l31_5_*`에 끌기 경우 추가 | 2일 | 빌더 제거 이전 뒤 |
| A4 | 보조 창 그리기, 그룹별 명령 라우팅과 포커스 | `ember/app` | 글루 테스트 (`editor_workbench_glue_tests.rs` 방식) | 3일 | A1, A2 뒤 |
| A5 | 떼어 내기 몸짓: 48 dp 판정, `DragFromPointer` | `ember/app` | `fr_b1_*` (판정은 순수 함수로 뽑아 테스트) | 2일 | A3, A4 뒤 |
| A6 | 합치기: `OnWindowDragOver`, 놓일 자리 표시, `Drop` | `ember/app` | `fr_b5_*` | 2일 | A5 뒤 |
| A7 | 메뉴 동작과 키맵 두 벌 | `ember/app` | `fr_b6_*`, 키맵 표 테스트 | 1일 | A4 뒤. 끌기 없이도 낼 수 있다 |
| A8 | 창 복원: `RestoreKey`, 핫 엑시트(D43)에 그룹 기록 | `ember/app`, `ember/editor` | 다시 시작 뒤 그룹이 돌아오는 테스트 | 2일 | A4 뒤 |
| A9 | 화면 확인: macOS, Windows, Linux에서 떼기와 합치기 | 없음 | 손 확인 기록 | 소유자 0.5일 | 소유자 항목 |
| B1 | CDP 하네스: 실제 OSE에서 순서 바꾸기, 떼어 내기, 거절 | `ember/proxy/tests`, `test-ose.yml` | `fr_b1_live_*`, `fr_b3_live_*` | 2일 | CI는 리더가 돌린다 |
| B2 | NFR-B1 측정과 SPEC 기록 | 같은 하네스 | 측정값이 SPEC에 있다 | 1일 | B1 뒤 |
| C1 | `WebviewBridge`를 웹뷰 노드 위에 구현, `BridgeHost` 연결 | `ember/app`, `ember/bridge` | 가짜 노드로 `fr_b2_*`, `fr_b4_*` | 3일 | Q-B3이 (b)이고 웹뷰 노드가 있을 때 |
| C2 | VS Code 대상을 Ember 창에서 열기, NFR-L2 예외 | `ember/app`, `.github/scripts/checks` | 검사 스크립트 테스트 | 3일 | C1 뒤 |
| C3 | NFR-B2 측정 (`latency_ms`) | `ember/app` | 측정값이 SPEC에 있다 | 1일 | C2 뒤 |

경로 A는 합쳐서 15일 안팎, 경로 B는 3일, 경로 C는 7일 안팎이다. A1, B1, B2, D1은 서로 닿는 파일이
달라 나란히 할 수 있다.
