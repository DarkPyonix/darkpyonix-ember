# 1.0 이후 설계: Ember의 브라우저는 Rivet이다 (#12, M6, SPEC §R)

상태: **제안, 리더 검토(2026-11-20)를 반영해 다시 썼다.** 이 문서의 첫 판은 메인 서버의 headless
Chrome을 클라이언트로 스트리밍하는 뷰어를 설계했다. 그것은 소유자 원칙과 어긋난다:

> Rivet(LogitAI/rivet-browser)이 Ember의 브라우저다. 사용자가 설치한 Chromium 위의 네이티브
> 프레임과 에이전트 CLI다. Ember는 설정, 작업 공간, egress를 맞춘다. Ember는 서버의 Chrome을
> 스트리밍하지 않는다. [리더가 전한 소유자 원칙, 2026-11-20]

그래서 이 판은 Rivet을 기준으로 다시 짠다. 코드, SPEC, INTENT는 바꾸지 않았다. 기준: Ember
`develop` 8ea03cb9, Rivet `develop`의 `PROJECT.md`, `docs/INTENT.md`, `docs/SPEC.md`,
`rivet/protocol/control.proto` (2026-11-20에 읽음). 빌드와 테스트는 돌리지 않았다.

## 1. 범위

안에 있는 것:

- Ember가 Rivet을 위해 하는 세 가지: 사용자별 설정과 논리 작업 공간의 보관과 전달(Rivet `FR-6`,
  `PR-2`), 컴퓨터 egress(Rivet `FR-7`, `PR-3`), 에이전트가 `rivet` CLI에 닿는 길(Rivet `FR-4`,
  `FR-5`).
- 지금 Ember에 있는 서버 Chrome 스트리밍 코드와 SPEC §R 가운데 무엇이 남고 무엇이 바뀌는지.
- #12의 완료 기준 네 가지를 Rivet 기준으로 다시 읽은 것.

밖에 있는 것:

- Rivet 자체(프레임, 런타임 주입, 제어 프로토콜, 설치 도구). Rivet 저장소의 일이다. 이 문서는
  Ember 쪽 절반과 두 저장소 사이의 계약만 다룬다.
- 서버 Chrome의 프레임을 받는 뷰어, 프레임 표면 위젯, 입력 전달, 공유 뷰포트. 원칙에 따라 만들지
  않는다. compose-rust에 `FrameSurface`를 요청하지 않는다.
- 쿠키, 비밀번호, 패스키, 브라우저 프로필 파일의 복사나 동기화. Rivet `C7`, `NFR-2`가 금지한다.
- 에디터 창의 확장 웹뷰(`FR-E8`)와 에이전트 API 트래픽 egress(`FR-X6`, D19). 같은 노드 `/egress`를
  쓰지만 별개 기능이다.

## 2. 현재 있는 것

### 2.1 Rivet (문서와 README에서 확인, 코드는 읽지 않음)

| 항목 | 내용 |
| --- | --- |
| 정체 | 설치된 Chromium 계열 브라우저의 프로세스 안에 런타임을 넣어 프레임을 바꾼다. 포크도 웹뷰도 아니다 (`C1`, `C2`) |
| 플랫폼 | Windows가 첫 호스트다 (`C4`). macOS와 Linux 호스트는 현재 범위 밖이다 (`PROJECT.md`) |
| 프로필 | Rivet 전용 프로필에만 들어간다. 사용자의 기존 브라우저 프로세스와 기본 프로필에는 들어가지 않는다 (`FR-1`) |
| 자격증명 | 브라우저와 OS의 보안 저장소에 남는다. Rivet과 Ember는 복사하지 않는다 (`C7`, D14, `NFR-2`) |
| 제어 | `rivet` CLI와 프레임이 같은 로컬 제어 채널을 쓴다 (`PR-1`, `control.proto`: `PageOpen`, `Navigate`, `InputText`, `InputClick`, `InputKey`, `ReadPage`, `ReadFrame`, `WorkspaceList`, `WorkspaceSwitch`, `SetVisibility`) |
| 표시 정책 | `visible`, `hidden`, `auto`. `hidden`은 보이는 창과 프레임 캡처를 만들지 않는다 (`FR-5`) |
| Ember 쪽 요구 | 설정과 작업 공간 `FR-6`, `PR-2` (Draft). 컴퓨터 egress `FR-7`, `PR-3` (Draft) |
| 상태 | README: "Early implementation". `rivet session start`, 생성된 제어 프로토콜, Windows 런타임 주입이 있다. 직접 프레임, 브라우저 제어 명령, Ember 통합, 설치 도구는 끝나지 않았다 |
| 열린 질문 | Q2: 설정 파일의 인증, 암호화, 충돌 해결을 Ember API와 Rivet 중 어디에 둘지. Q3: 원격 컴퓨터의 제어 연결을 기존 CDP relay와 병행할지, Rivet 전용 relay로 둘지 |

### 2.2 Ember (코드에서 확인)

| 무엇 | 어디 | Rivet 기준에서 |
| --- | --- | --- |
| 노드의 SOCKS5 출구 (`CONNECT`만, `EMBER_NODE_EGRESS_DENY`) | `ember/node/src/egress.rs` | **남는다.** Rivet `FR-7`의 바닥이다 |
| 컴퓨터별 loopback SOCKS5 리스너 | `ember/server/src/computers/egress.rs`, `computers/mod.rs` (`egress_proxy`) | **남는다, 자리가 바뀐다.** 지금은 서버에서 듣는다. 브라우저가 도는 기기에서 들어야 한다 (4.3) |
| egress 선택과 영속화 (`direct`, `proxy`, `computer`) | `ember/server/src/browser/egress.rs`, `store.rs` | **남는다.** 프로젝트가 아니라 작업 공간이나 세션에 붙는다 |
| owner 전용 egress 변경 | `ember/client/src/owner.rs` | 남는다 |
| headless Chromium 실행, 프로젝트별 프로필 | `ember/server/src/browser/chrome.rs`, `instance.rs`, `mod.rs` | **대체 예정** |
| 뷰 스트림 WebSocket, JPEG 프레임, 입력, takeover | `browser/api.rs`, `instance.rs` | **대체 예정** |
| CDP 릴레이, 에이전트용 MCP 설정 (`EMBER_BROWSER_MCP`) | `browser/cdp.rs`, `browser/agent.rs`, `auth.rs` (`agent_cdp_nonce`) | **대체 예정.** 에이전트는 `rivet` CLI를 쓴다 |
| 테스트 | `ember/server/tests/browser.rs` (Chrome이 없으면 건너뛰고 통과), `browser_egress.rs`, `ember/node/tests/egress.rs` | 뒤의 둘은 남는다 |

"대체 예정"은 지금 지우라는 뜻이 아니다. Rivet 경로가 수용 기준을 통과할 때까지 코드는 그대로
두고, 새 작업을 그 위에 얹지 않는다 (작업 분해 R9).

Ember의 문서, 코드 어디에도 Rivet이라는 말이 없다 (`docs/`, `ember/*/src` 검색). SPEC §R, INTENT
D6, `docs/design/REMOTE-BROWSER.md`, `PROJECT.md`의 M6는 모두 서버 Chrome 모델을 적는다.

## 3. 빈 곳

| ID | 빈 곳 |
| --- | --- |
| G1 | **문서가 원칙과 다르다.** `FR-R2`(프로필 데이터를 메인 서버에 둔다), `FR-R3`(DevTools 프로토콜 기반 도구, takeover), `FR-R5`(모든 클라이언트에서 원격 브라우저), INTENT D6("data on the main server")가 Rivet의 `C7`, D4와 맞지 않는다. Rivet D14는 "Ember는 원래 브라우저 데이터를 메인 서버에 둔다. 이 결정은 Rivet의 브라우저에만 해당한다"고 적어 두 모델이 나란히 있는 것으로 읽힌다 (Q-R15) |
| G2 | Ember 서버에 Rivet 설정 문서와 작업 공간을 보관하는 API가 없다. 계약(`PR-2`)도 Draft이고 스키마가 없다 |
| G3 | egress 리스너가 서버에서만 뜬다. 사용자의 데스크톱에서 도는 브라우저가 쓸 수 없다 |
| G4 | egress의 모양이 정해지지 않았다. Rivet D5는 "그 컴퓨터의 Rivet 호스트에서 실제 요청을 실행한다"고 적고 결합 방식은 `PR-3`의 실험으로 미룬다 (Rivet Q3) |
| G5 | 에이전트가 `rivet` CLI에 닿는 길이 없다. 노드의 `/env` 탐침은 `rivet`을 보지 않고, 에이전트 안내문에 브라우저 항목이 없다 |
| G6 | **플랫폼.** Rivet은 Windows뿐이다. Ember의 메인 서버와 컴퓨터는 주로 macOS와 Linux다 (`PROJECT.md`: Raspberry Pi 또는 Mac mini). 그 컴퓨터의 세션은 Rivet이 올 때까지 브라우저가 없다 (Q-R16) |
| G7 | 사람과 에이전트가 같은 Rivet 창을 동시에 만질 때의 규칙이 없다. 서버 Chrome의 takeover는 없어진다 (Q-R17) |
| G8 | 수용 실행이 한 번도 없었다. 그리고 그 기준의 뜻이 바뀐다 (4.6) |
| G9 | Rivet 자체가 초기 구현이다. Ember 쪽 작업의 대부분은 Rivet의 M3(CLI 표시 정책)과 M4, M5를 기다린다 |

## 4. 제안 구조

### 4.1 전체 그림

```text
 사용자의 데스크톱 (Windows)                    ember server                  컴퓨터 X (ember node)
 ┌────────────────────────────────┐          ┌──────────────────────┐       ┌──────────────────┐
 │ 설치된 Chromium + Rivet 프레임  │          │ Rivet 설정 문서       │       │ SOCKS5 출구       │─▶ 대상
 │   (Rivet 전용 프로필, 로컬)     │          │ 논리 작업 공간        │       │ (있는 코드)       │
 │      ▲ 로컬 제어 채널 (PR-1)    │          │ egress 선택           │       │                  │
 │      │                         │          │ (쿠키, 비밀번호 없음)  │       │ [Windows일 때]    │
 │ rivet CLI ◀── ember-app ───────┼─ sync ──▶│ /api/rivet/*         │       │ rivet CLI (hidden)│
 │              (등록된 기기, D37) │          │                      │       │   ▲               │
 │ loopback SOCKS5 리스너 ────────┼──────────┼── transport (FR-N5) ─┼──────▶│   │ ember-exec    │
 └────────────────────────────────┘          └──────────────────────┘       └───┼──────────────┘
                                              agent CLI (메인 서버) ── 도구 호출 ─┘
```

프레임은 어느 선으로도 흐르지 않는다. 사람이 보는 화면은 자기 기기의 Rivet 창이다.

### 4.2 설정과 작업 공간 동기화

- **보관.** 서버는 사용자별 Rivet 설정 문서 하나와 논리 작업 공간 목록을 가진다. 내용은 Rivet이
  정한 스키마의 불투명한 문서로 다루고, Ember는 스키마 버전, 수정 시각, 쓴 기기만 읽는다. 이유:
  Rivet의 사이드바 모델(Rivet D10)이 바뀔 때마다 Ember 서버를 고치지 않기 위해서다.
- **허용 목록.** 서버는 문서에 쿠키, 비밀번호, 패스키, 암호화 키로 보이는 필드가 있으면 거절한다.
  Rivet `FR-6`의 셋째 수용 기준을 Ember 쪽에서도 지키는 검사다. 키 이름 목록은 계약에 적는다.
- **누가 말하는가.** `ember-app`이 등록된 기기(D37의 기기 키)로 서버와 말하고, 받은 문서를 로컬
  `rivet` CLI로 적용한다. Rivet은 Ember의 transport를 직접 말하지 않는다. 이유: transport 타입은
  transport 크레이트 밖에서 금지돼 있고(`check-transport-isolation.sh`), Rivet이 기기 등록을 따로
  하면 기기 키가 둘이 된다. Rivet Q2("인증, 암호화, 충돌 해결을 어디에 둘지")에 대한 Ember 쪽
  제안은 "인증과 전달은 Ember, 충돌을 보여 주고 고르는 것은 Rivet"이다.
- **적용.** Rivet `FR-6`대로 다른 기기의 변경은 사용자 확인 뒤 적용한다. 서버가 안 닿으면 마지막
  문서를 쓴다. 충돌은 문서 전체의 수정 시각과 기기로 알리고 병합은 하지 않는다.
- **프로젝트와의 연결.** 논리 작업 공간에 Ember 프로젝트 id를 선택 필드로 둔다. 대화 화면의
  "브라우저" 버튼은 그 프로젝트의 작업 공간으로 Rivet을 연다. 지금의 "프로젝트마다 브라우저
  하나"(서버 프로필)를 대신한다.

### 4.3 egress

두 경우를 나눈다.

**사람이 보는 브라우저.** Rivet은 사용자의 기기에서 돌고, 그 트래픽만 고른 컴퓨터로 나간다.
`ember-app`이 자기 기기의 loopback에 SOCKS5 리스너를 띄우고 transport로 그 컴퓨터의 `/egress`에
잇는다. Rivet은 그 리스너를 프록시로 받아 브라우저를 연다. 지금 서버에 있는 `EgressListener`를
클라이언트가 쓸 수 있는 자리로 옮기는 일이다. 이 모양을 권하는 이유는 둘이다. 프로필이 사용자의
기기에 그대로 있으므로 egress를 바꿔도 로그인이 유지된다(#12 기준 2). 화면을 다른 컴퓨터에서
끌어올 필요가 없으므로 스트리밍이 생기지 않는다.

**에이전트의 브라우저.** 에이전트의 도구는 세션의 현재 컴퓨터에서 돈다(D4). 그 컴퓨터에 Rivet이
있으면 에이전트는 거기서 `rivet`을 `hidden`으로 부른다. 트래픽은 그 컴퓨터에서 바로 나가므로
egress가 따로 필요 없다. Rivet D5의 "그 컴퓨터의 Rivet 호스트에서 실제 요청을 실행한다"가 이
경우다. 로그인은 그 컴퓨터의 Rivet 전용 프로필의 것이다.

두 경우 모두 대상 컴퓨터가 안 닿으면 직접 연결로 조용히 바꾸지 않고 실패한다 (Rivet `FR-7`,
Ember의 지금 동작과 같다).

Rivet Q3(원격 제어 연결을 기존 CDP relay와 병행할지)에 대한 Ember 쪽 제안은 "둘 다 필요 없다"다.
에이전트는 그 컴퓨터에서 CLI를 로컬로 부르고, 사람은 자기 기기에서 본다. 다른 컴퓨터의 Rivet을
원격으로 조종하는 연결은 이 설계에 없다. 이것이 Q-R14의 결정이다 [leader, 2026-11-20]. Rivet `PR-3`과 맞춘다.

### 4.4 에이전트 경로

- **도구는 CLI다.** 에이전트는 셸 도구로 `rivet ... --json`을 부른다. `ember-exec`가 그 명령을
  현재 컴퓨터의 노드로 보낸다. MCP 서버를 따로 띄우지 않는다. 이유: Rivet `C5`가 CLI를 일급
  표면으로 정했고, 에이전트를 고치지 않고 설정만 더한다는 E2에 맞는다.
- **발견.** 노드의 `/env` 탐침에 `rivet --version`을 더한다. 없으면 에이전트 안내문에 브라우저
  항목을 넣지 않는다. 브라우저를 추측해 띄우지 않는다 (Rivet `FR-1`, Q-R9).
- **버전 고정.** 지원하는 `rivet` 버전 범위를 SPEC에 적고, 범위 밖이면 안내문에서 뺀다. 올릴 때는
  실제 CLI로 통합 실행을 한다 (Q-R5, AGENTS 규칙 4).
- **스위치.** 프로젝트별로 켜고 기본은 꺼짐을 권한다. 로그인된 프로필은 권한이기 때문이다
  (Q-R4, 소유자 답 대기).
- **표시.** 에이전트의 기본은 `hidden`이다. `control.proto`에는 `FAILURE_KIND_NEEDS_VISIBLE`이
  있다: 명령이 시각 증거를 요구하는데 정책이 창을 금지할 때의 실패다. 사람이 직접 해야 하는
  단계(로그인, 2단계 인증)를 알리는 신호는 프로토콜에 없다. 그 신호를 계약(R1)에 넣자고 Rivet
  쪽에 제안하고, Ember는 두 경우를 세션의 "사람이 필요함" 상태로 올린다(`FR-T8`의 needs input과
  같은 길).

### 4.5 클라이언트 UI

작다. 대화 화면 헤더의 "브라우저" 버튼(그 프로젝트의 작업 공간으로 Rivet을 연다), egress 선택,
Rivet이나 Chromium이 없을 때의 안내, 설정 동기화의 충돌 알림. 웹뷰도 프레임 표면도 없다. Rivet은
별도 프로세스의 브라우저이므로 E1과 `NFR-L2`에 닿지 않는다. 이 UI는 12월의 call 매크로 이전 뒤에
call 매크로로 쓴다.

### 4.6 #12의 완료 기준을 다시 읽는다

| 기준 | Rivet 기준의 뜻 |
| --- | --- |
| 1. IP 확인 페이지가 egress 컴퓨터를 보이고 그 컴퓨터의 localhost, LAN 페이지에 닿는다 | 그대로다. Rivet `FR-7`의 첫 수용 기준과 같다. 사람의 Rivet은 4.3의 리스너로, 에이전트의 Rivet은 그 컴퓨터에서 직접 |
| 2. 로그인이 egress 전환 뒤에도 남는다 | 사람의 Rivet에서는 프로필이 움직이지 않으므로 남는다. 컴퓨터를 옮긴 에이전트 세션은 새 컴퓨터의 프로필을 쓰므로 남지 않는다. 그 경우는 브라우저의 공식 동기화와 사용자의 로그인에 맡긴다 (Rivet D4). 기준 문장을 고쳐야 한다 |
| 3. 에이전트가 폼을 채우는 동안 사용자가 로그인 단계를 넘겨받는다 | `visible` 세션에서 사람이 같은 창을 직접 만진다. 넘겨받기 버튼이 아니라 충돌 규칙이 필요하다 (Q-R17) |
| 4. 런처와 대화 화면에 웹뷰가 없다 | 그대로다. Rivet은 웹뷰가 아니고 다른 프로세스다 |

### 4.7 버린 것

- **서버 Chrome을 스트리밍하는 뷰어 (이 문서의 첫 판).** 소유자 원칙과 어긋난다.
- **프로필을 메인 서버에 두고 컴퓨터마다 내려보낸다.** Rivet `C7`, `NFR-2`가 금지한다.
- **Rivet이 올 때까지 서버 Chrome 코드를 계속 키운다.** 버릴 코드에 일을 쌓는다. 지금 코드는 얼리고
  새 작업은 Rivet 계약 쪽에 둔다.
- **Ember가 CDP로 설치된 Chromium을 직접 조종한다.** Rivet의 제어 표면과 겹치고, Rivet 전용
  프로필과 세션 수명 규칙(`FR-1`)을 Ember가 다시 구현하게 된다.

## 5. 위험

| # | 위험 | 완화 |
| --- | --- | --- |
| 1 | Rivet이 Windows뿐이라 macOS, Linux 컴퓨터의 세션에는 오래 브라우저가 없다 | Q-R16으로 묻는다. 그 사이의 임시 길(얼린 서버 Chrome을 스위치 뒤에 남길지)은 원칙에 닿으므로 리더와 소유자가 정한다 |
| 2 | Rivet의 일정에 Ember 쪽이 묶인다 | Rivet 없이 할 수 있는 것을 먼저 한다: 문서 재정렬(R0), 계약 초안(R1), 서버 보관 API(R2), 리스너 이전(R3). 가짜 `rivet`으로 테스트한다 |
| 3 | 계약이 두 저장소에서 따로 바뀐다 | 계약 벡터를 한 곳에 두고 양쪽 테스트가 같은 파일을 읽는다 (Rivet `rivet/protocol/vectors`의 방식). 스키마 버전이 다르면 손실 없이 호환 여부를 보고한다 (`PR-2`) |
| 4 | 프록시를 거치지 않는 UDP(WebRTC, QUIC)로 진짜 IP가 샌다 | egress가 컴퓨터일 때만 막는 플래그를 Rivet이 브라우저에 넘긴다. 실제 브라우저로 확인한다 (Q-R11, 소유자 답 대기) |
| 5 | 에이전트가 로그인된 Rivet 프로필로 사용자의 권한을 쓴다 | 프로젝트별 스위치, 기본 꺼짐 (Q-R4). 에이전트 기본은 `hidden`이고 사람이 필요한 단계는 올린다 |
| 6 | loopback SOCKS5 리스너에 같은 기기의 다른 프로세스가 닿는다 | Rivet 세션이 끝나면 리스너를 내린다. 리스너는 요청마다 임의 포트를 쓴다. 지금 서버 쪽과 같은 신뢰 경계다 |
| 7 | 동기화 문서에 비밀이 섞인다 | 서버의 허용 목록 검사(4.2)와 Rivet 쪽 검사를 둘 다 둔다 |
| 8 | 서버 Chrome 코드를 지우기 전에 누가 그 위에 기능을 얹는다 | SPEC Status 표에 "대체 예정, 새 작업 금지"를 적는다 (R0) |

## 6. 결정과 열린 질문

첫 판의 Q-R1~R13에 대한 리더 검토 결과다.

| 질문 | 상태 | 내용 |
| --- | --- | --- |
| Q-R1 뷰어 위치 | 무효 [leader, 2026-11-20] | 뷰어 창이 없다. 브라우저는 Rivet 자신의 네이티브 창이다 |
| Q-R2 프레임 표면 | 무효 [leader, 2026-11-20] | 스트리밍하지 않는다. `FrameSurface` 위젯을 요청하지 않는다 |
| Q-R3 `FR-R5`의 "web" | 무효 [leader, 2026-11-20] | `FR-R5`를 Rivet 기준으로 다시 쓴다 (데스크톱 먼저). 웹 뷰어는 없다 |
| Q-R4 에이전트 브라우저 기본값 | **열림, 소유자** | 추천: 기본 꺼짐, 프로젝트별로 사람이 켠다 |
| Q-R5 버전 고정 | 결정 [leader, 2026-11-20] | Rivet 에이전트 CLI(브라우저 제어 도구)의 버전을 SPEC에 고정하고 통합 실행을 거쳐서만 올린다 |
| Q-R6 takeover | 무효 [leader, 2026-11-20] | 사용자가 로컬 Rivet 창을 직접 쓴다. 에이전트와 사람이 부딪칠 수 있으면 Rivet 설계에서 다시 묻는다 (Q-R17) |
| Q-R7 휴대폰 뷰포트 | 무효 [leader, 2026-11-20] | 공유 서버 뷰포트가 없다 |
| Q-R8 프로필 지우기 | 결정 [leader, 2026-11-20] | owner 전용이고 확인을 거친다. 되돌릴 수 없는 동작이다 |
| Q-R9 Chromium 마련 | 결정 [leader, 2026-11-20] | 설치된 Chromium만 쓰고 없으면 안내한다. 내려받기와 번들은 없다 |
| Q-R10 실제 사이트 수용 실행 | **열림, 소유자** | 추천: 테스트 사이트와 소유자가 고른 실제 사이트 하나, 둘 다 |
| Q-R11 WebRTC 누출 | **열림, 소유자** | 추천: 다른 컴퓨터를 거칠 때만 막는다 |
| Q-R12 프로필 암호화 | 결정 [leader, 2026-11-20] | 디렉터리 권한만. Rivet에서 프로필은 설치된 Chromium의 것이고 OS 키체인을 이미 쓴다 |
| Q-R13 지연 예산 | 무효 [leader, 2026-11-20] | 프레임 경로가 없다. Rivet의 지연 숫자는 측정 뒤에만 SPEC에 넣는다 |

다시 쓰면서 생긴 질문과 리더의 분류다 (2026-11-20).

| 질문 | 상태 | 내용 |
| --- | --- | --- |
| Q-R14 egress의 모양 | 결정 [leader, 2026-11-20] | (a) 사람의 Rivet은 자기 기기에서 돌고 loopback 리스너로 나간다. 에이전트의 Rivet은 세션의 컴퓨터에서 돈다. 원격 조종 relay는 없다. Rivet `PR-3`과 맞춘다 |
| Q-R15 브라우저 데이터의 자리 | **열림, 소유자** | 추천 (a): D6와 `FR-R2`를 "로그인은 브라우저와 OS가 가진다, Ember는 설정과 작업 공간만 가진다"로 고친다 |
| Q-R16 Rivet이 없는 플랫폼 | **열림, 소유자** | 추천 (a): 브라우저 없음, 이유를 보여 준다 |
| Q-R17 사람과 에이전트의 충돌 | 결정 [leader, 2026-11-20] | 충돌 규칙은 Rivet이 정한다. Ember는 "사람이 쓰는 중" 실패를 "사람이 필요함"으로 올린다 |

아래는 분류 전의 질문 원문이다. 선택지와 근거를 남기려고 그대로 둔다.

- **Q-R14. egress의 모양.** (a) 사람의 Rivet은 자기 기기에서 돌고 loopback 리스너로 나간다. 에이전트의
  Rivet은 세션의 컴퓨터에서 직접 돈다. 다른 컴퓨터의 Rivet을 원격 조종하는 연결은 없다 (4.3).
  (b) Rivet 전용 relay로 다른 컴퓨터의 Rivet 호스트를 조종한다 (Rivet Q3). 추천 (a). 스트리밍이
  생기지 않고 있는 노드 출구를 그대로 쓴다. Rivet 쪽 `PR-3`과 맞춰야 한다.
- **Q-R15. `FR-R2`와 D6의 "브라우저 데이터는 메인 서버에".** Rivet `C7`과 맞지 않는다. (a) D6와
  `FR-R2`를 "로그인은 브라우저와 OS가 가진다, Ember는 설정과 작업 공간만 가진다"로 고친다.
  (b) 두 모델을 나란히 둔다. 추천 (a). D6가 [user] 결정이므로 소유자의 확인이 필요해 보인다.
- **Q-R16. Rivet이 없는 플랫폼.** macOS, Linux 컴퓨터와 휴대폰의 세션은 Rivet이 올 때까지 무엇을
  쓰는가. (a) 브라우저 없음, 이유를 보여 준다. (b) 얼린 서버 Chrome의 에이전트 CDP 경로만 스위치
  뒤에 남긴다 (화면 스트리밍은 없이). 추천 (a). (b)는 버릴 코드를 살려 두고 원칙의 경계에 닿는다.
- **Q-R17. 사람과 에이전트의 충돌.** `visible` 세션에서 둘이 같은 창을 만질 때. (a) 사람의 입력이
  오면 에이전트의 입력 명령이 "사람이 쓰는 중"으로 실패하고 다시 시도한다. (b) 에이전트는 자기
  탭만 만지고 사람은 다른 탭을 쓴다. (c) 규칙 없음. 추천: Rivet 쪽에서 정하되 Ember는 (a)의 실패
  종류를 "사람이 필요함"으로 올릴 수 있으면 된다.

## 7. SPEC/INTENT 변경 제안

적용하지 않았다. 모두 [provisional]이고, 소유자의 Q-R15, Q-R16 답에 따라 바뀐다.

> **§R (title)** Browser: Rivet on the user's installed Chromium, with Ember's settings,
> workspaces and egress (D6).

> **FR-R1** (changed) A Rivet browser session can egress from a chosen computer. For the user's
> own Rivet the path is a loopback SOCKS5 listener on the user's device carried over a transport
> stream (`FR-N5`) to that computer's ember node; an agent's Rivet runs on the session's current
> computer and egresses there directly. An unreachable computer fails explicitly, never falls back
> to a direct connection. | A what-is-my-IP page shows the chosen computer's public IP; a page on
> that computer's LAN or `localhost` is reachable (Rivet `FR-7`).

> **FR-R2** (replaced) Ember stores each user's Rivet settings document and logical workspaces on
> the main server and delivers them to the user's registered devices. Ember never stores, copies
> or transfers cookies, passwords, passkeys or browser encryption keys (Rivet `C7`, `NFR-2`). |
> A setting saved on one device is offered on another and applied after the user confirms; a
> document carrying a credential-like field is refused; with the server unreachable the last
> document is used.

> **FR-R3** (replaced) Agents drive the browser through the `rivet` CLI on the session's current
> computer, hidden by default; a step that needs a person is raised as "needs input" in the
> session. The supported `rivet` version range is stated here and raised only after an
> integration run with the real CLI. | With `rivet` absent or out of range, the agent's
> instructions carry no browser entry. An agent fills a form; at the login step the session shows
> that a person is needed; after the person signs in, the agent continues.

> **FR-R4** (changed) Clearing a workspace's browser data is owner-only and confirmed. Ember
> removes its own records (workspace, egress choice) and asks Rivet to clear the Rivet profile's
> site data on that device.

> **FR-R5** (replaced) The browser is Rivet, on desktop first. Ember streams no browser frames
> and has no viewer. Platforms without a Rivet host show why the browser is unavailable.

> **Status table, FR-R1 to R5.** "Server Chrome implementation (`ember/server/src/browser/`:
> launch, view stream, input, CDP relay, MCP config) is to be replaced by Rivet; no new work lands
> on it. The node egress and the egress choice stay."

INTENT 제안:

> **D6 amendment** [user principle relayed by the leader, 2026-11-20; wording provisional] Rivet
> (LogitAI/rivet-browser) is Ember's browser: a native frame on the user's installed Chromium and
> an agent CLI. Ember syncs settings and workspaces and provides the computer egress. Ember never
> streams a server Chrome. Sign-in state stays with the browser and the operating system.
> Rejected: a headless Chrome on the main server streamed to a viewer; a browser profile held on
> the main server.

그 밖에 `PROJECT.md`의 M6 행("viewing depends on dioxus-compose's frame-surface widget")과
`docs/design/REMOTE-BROWSER.md`(머리에 "대체됨, 이 문서를 보라")를 고친다.

## 8. 작업 분해

추정은 에이전트 세션 하나의 작업일이고 거칠다. Rivet 쪽 작업은 세지 않았다. 빌드와 테스트는
빌더가 돌린다. **이 표는 재계획의 초안이다. 소유자가 Q-R15, Q-R16에 답하고 Rivet 쪽과 계약을 맞춘
뒤에 일정에 넣는다.**

| # | 무엇 | 닿는 곳 | 증명하는 테스트 | 일 | 기다리는 것 |
| --- | --- | --- | --- | --- | --- |
| R0 | 문서 재정렬: SPEC §R, Status 표, INTENT D6, `PROJECT.md` M6, `REMOTE-BROWSER.md`, #12 본문 | `docs/` | 문서 검사 | 1 | Q-R15 |
| R1 | 계약 초안 (`PR-2`, `PR-3`의 Ember 쪽): 설정 문서의 봉투, 금지 필드 목록, egress 인자, CLI 실패 종류의 대응. 공유 벡터 | `docs/design/`, `ember/vectors/rivet/` | 벡터를 읽는 양쪽 테스트 | 2 | Rivet 저장소와 합의 |
| R2 | 서버 보관 API: `/api/rivet/settings`, `/api/rivet/workspaces`, 기기 범위, 허용 목록 검사 | `ember/server` | `fr_r2_settings_round_trip_between_devices`, `fr_r2_credential_like_field_is_refused`, `fr_r2_scope_is_user_and_device` | 2.5 | R1 |
| R3 | egress 리스너를 클라이언트에서 쓸 수 있게 옮긴다 (transport 위) | `ember/client`, `ember/server` (공유 코드) | `fr_r1_client_listener_egresses_through_a_node`, `fr_r1_unreachable_node_fails_explicitly` | 2 | 없음. 지금 할 수 있다 |
| R4 | `ember-app`의 동기화 글루: 받기, 로컬 `rivet` CLI로 적용, 충돌 알림 (UI 없는 부분) | `ember/app`, `ember/client` | 가짜 `rivet` 실행 파일로 `fr_r2_app_applies_after_confirmation` | 2 | R2, Rivet M4 |
| R5 | 에이전트 경로: `/env` 탐침, 안내문, 버전 범위, 프로젝트별 스위치, 사람이 필요한 단계 올리기 | `ember/node`, `ember/server` | `fr_r3_no_rivet_no_browser_entry`, `fr_r3_person_needed_becomes_needs_input`, 실제 CLI 통합 테스트 | 3 | Rivet M3, Q-R4 |
| R6 | 프로필 데이터 지우기를 owner 전용으로 (Q-R8) | `ember/server`, `ember/client` (`owner.rs`) | `fr_r4_clear_is_owner_only` | 0.5 | 없음 |
| R7 | 클라이언트 UI: 브라우저 버튼, egress 선택, 안내, 충돌 알림. call 매크로로 쓴다 | `ember/app` | UI 테스트, `no_webview.rs` | 2.5 | 12월 call 매크로 이전, R4 |
| R8 | 수용 실행 (4.6의 네 기준): Windows 기기의 Rivet, 다른 공인 IP의 컴퓨터, 실제 사이트 | 없음 (실행과 기록) | SPEC 수용 기준, 실제 출력 | 1 | Rivet M5, Q-R10, Q-R11, 소유자의 Windows 기기와 두 번째 컴퓨터 |
| R9 | 서버 Chrome 코드와 테스트를 지운다 (`browser/chrome.rs`, `instance.rs`, `cdp.rs`, `agent.rs`, 뷰와 입력 경로) | `ember/server` | 남은 egress 테스트가 통과한다 | 1 | R8 통과, Q-R16 |

Rivet을 기다리지 않는 것은 R0, R3, R6이고 R1과 R2는 계약 합의만 있으면 된다. 합쳐서 17일 안팎이다.

## 확인하지 못한 것

- Rivet의 코드는 읽지 않았다. 문서와 `control.proto`의 메시지 이름만 봤다. CLI의 실제 명령 이름과
  JSON 출력은 모른다.
- Rivet이 브라우저에 프록시 인자를 넘기는 길이 있는지 모른다. 4.3의 첫 경우는 그것을 전제한다.
- Rivet의 Android 계획(Rivet D14가 언급한다)의 상태는 모른다.
- 소유자 원칙의 원문을 보지 못했다. 리더가 전한 문장으로 적었다.
- 첫 판에서 찾은 서버 Chrome 쪽 사실(Chrome이 없으면 테스트가 건너뛰고 통과한다, takeover가 풀리지
  않는다, 프로필 디렉터리 권한을 좁히지 않는다)은 대체될 코드의 것이라 이 판에서는 뺐다.
