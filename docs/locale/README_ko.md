[English](https://github.com/DarkPyonix/darkpyonix-ember/blob/develop/README.md) | 한국어

# darkpyonix-ember

**하나의 AI, 여러 대의 컴퓨터.** Ember 는 코딩 에이전트를 위한 개발 환경이자 원격 IDE 입니다.
Claude Code, Codex, Antigravity, OMP 가 모든 대화를 보관하는 메인 서버 한 대에서 실행됩니다.
에이전트의 도구는 작업에 필요한 컴퓨터에서 동작합니다. 대화 도중에 다른 컴퓨터로 옮겨 갈 수도
있습니다.

Ember 는 클라이언트이자 에이전트 환경입니다. 그 아래의 DarkPyonix 는 파이썬 커널과 노트북
런타임입니다([darkpyonix-core](https://github.com/DarkPyonix/darkpyonix)).

**사용자 가이드**(영어, 한국어): <https://darkpyonix.dev/darkpyonix-ember/>

---

## 🎯 왜 만드는가

에이전트로 개발하다 보면 매일 세 가지 문제를 만납니다.

- **대화가 그 대화를 시작한 컴퓨터에 묶입니다.** CLI 에이전트는 대화 기록을 로컬 디스크에
  씁니다. 작업을 다른 컴퓨터로 옮기면 대화는 두고 가야 합니다. Ember 는 Mac mini 나 Raspberry Pi
  같은 메인 서버 한 대에 모든 대화를 보관합니다. 그래서 대화가 더 이상 한 컴퓨터에 속하지 않습니다.
- **다른 회사의 에이전트끼리 대화하지 못합니다.** Claude 와 Codex 는 공유 파일로만 협업합니다.
  다른 컴퓨터나 다른 계정의 Claude 세션 둘은 아예 대화할 수 없습니다. Ember 는 회사, 컴퓨터,
  계정에 상관없이 세션 사이에 직접 통하는 채널을 줍니다.
- **원격 컴퓨터에서 보이는 네트워크를 빌려 쓰기 어렵습니다.** Ember 는 고른 컴퓨터에서, 그
  컴퓨터의 IP 로 트래픽이 나가는 브라우저를 엽니다. 쿠키와 로그인은 메인 서버에 남습니다. 그래서
  여러 위치에서 하나의 브라우저 신원을 그대로 씁니다.

---

## 🧭 동작 방식

1. 메인 화면에 **프로젝트** 목록이 나옵니다. 각 프로젝트는 **세션**과 그 상태(실행 중, 승인 대기,
   완료, 실패)를 보여 줍니다. 그 아래에 **컴퓨터** 목록이 온라인 여부와 함께 나옵니다.
2. 대화를 열어 에이전트를 조종합니다. 에이전트는 메인 서버에서 실행되고, 도구는 세션의 현재
   컴퓨터에서 동작합니다. 메시지를 보내고, 승인 요청에 답하고, 턴을 중단하고, 세션을 다른
   컴퓨터로 옮길 수 있습니다.
3. 코드를 읽으려면 **Open IDE** 로 그 프로젝트와 컴퓨터의 VS Code 나 JetBrains Gateway 를 엽니다.
4. 클라이언트를 닫아도 세션은 메인 서버에서 계속 실행됩니다. 나중에 같은 컴퓨터나 다른 컴퓨터에서
   클라이언트를 열면 대화가 두고 간 그 자리에 있습니다.

클라이언트는 JetBrains Gateway 를 따릅니다. 앞문은 가볍고, 무거운 창은 필요할 때 엽니다. 에이전트
작업은 대부분 에디터에 타이핑하는 일이 아니라 에이전트를 지켜보고 조종하는 일이기 때문입니다.

---

## 🏗 구조

Ember 는 세 개의 프로그램입니다.

| 프로그램 | 실행 위치 | 보관하는 것 |
| -------- | --------- | ----------- |
| `ember-server` | 메인 서버 | 모든 에이전트 프로세스, 대화 기록, 계정, A2A 큐, 일정, 브라우저 프로필 |
| `ember-node` | 그 밖의 모든 컴퓨터 | 프로젝트 파일, 그리고 도구가 띄우는 프로세스(빌드, 테스트, 서버, 터미널) |
| `ember-app` | 사용자가 앉은 곳 | 메인 서버가 마지막으로 보낸 내용의 캐시. 그래서 바로 시작됩니다 |

클라이언트는 HTTP 로 `ember-server` 와 통신합니다. `ember-server` 는 각 컴퓨터에 HTTP 로, 또는
[iroh](https://github.com/n0-computer/iroh) 기반 P2P 전송(QUIC, 홀 펀칭과 릴레이)으로 연결합니다.
darkpyonix.dev 허브를 쓰면 일회용 코드로 컴퓨터를 GitHub 계정에 연결합니다. 그래서 주소를
입력하거나 포트를 열 필요가 없습니다.

---

## 🚦 현황

Ember 는 아직 릴리스가 없습니다. 아래 표는
[가이드 개요](https://darkpyonix.dev/darkpyonix-ember/ko/index.html)에 기록된 대로 2026-10-03 기준
`develop` 의 코드와 맞습니다.

| 영역 | 상태 | 비고 |
| ---- | ---- | ---- |
| 세션, 대화 기록, 푸시, 검색, 내보내기 | 구현 | 세션 포크는 어느 에이전트에도 아직 없습니다. |
| Claude Code 와 Codex | 구현 | 각 CLI 의 헤드리스 프로토콜로 구동합니다. |
| Antigravity | 부분 | agy 1.2.16 에서 Ember 의 승인 훅으로 통제합니다. `--sandbox` 에서는 셸 쓰기가 실패합니다([#65](https://github.com/DarkPyonix/darkpyonix-ember/issues/65)). |
| OMP 와 그 밖의 ACP 에이전트 | 부분 | 가짜 ACP 에이전트로 테스트했고, 실제 OMP 실행은 남아 있습니다([#53](https://github.com/DarkPyonix/darkpyonix-ember/issues/53)). |
| 컴퓨터와 전환 | 부분 | 같은 Mac 의 두 번째 노드로 검증했고, 물리적으로 다른 두 컴퓨터 사이는 아직입니다([#6](https://github.com/DarkPyonix/darkpyonix-ember/issues/6)). |
| 프로젝트 마운트 | 부분 | 작성했지만 실제 Mac mini 나 Raspberry Pi 에서 돌려 보지 않았습니다([#6](https://github.com/DarkPyonix/darkpyonix-ember/issues/6)). |
| P2P 전송 | 부분 | 메모리 내 네트워크에서 테스트했고, 실제 네트워크 측정은 남아 있습니다([#10](https://github.com/DarkPyonix/darkpyonix-ember/issues/10)). |
| darkpyonix.dev 허브 | 부분 | 가짜 허브로 테스트했습니다. 허브 주소는 아직 정하는 중입니다([#62](https://github.com/DarkPyonix/darkpyonix-ember/issues/62)). |
| 네이티브 클라이언트 | 부분 | HTTP 로 `ember-server` 에 붙어 동작합니다. 실제 화면에서 레이아웃을 확인하지 않았고, 휴대폰 클라이언트는 없습니다([#9](https://github.com/DarkPyonix/darkpyonix-ember/issues/9)). |
| Open IDE | 부분 | 대화에서 VS Code 와 JetBrains Gateway 를 엽니다. Ember 자체 에디터는 계획입니다([#32](https://github.com/DarkPyonix/darkpyonix-ember/issues/32)). |
| 영구 터미널 | 부분 | [#26](https://github.com/DarkPyonix/darkpyonix-ember/issues/26) |
| 원격 브라우저 | 부분 | [#12](https://github.com/DarkPyonix/darkpyonix-ember/issues/12) |
| 릴리스와 설치 프로그램 | 계획 | 지금은 모든 프로그램을 cargo 로 빌드합니다([#72](https://github.com/DarkPyonix/darkpyonix-ember/issues/72)). |

---

## 🚀 빌드와 실행

`crates/` 아래의 각 크레이트는 자체 `Cargo.lock` 을 가진 별도 cargo 프로젝트입니다. 그래서 크레이트
폴더 안에서 빌드합니다. 작업은 `develop` 에 들어갑니다.

```bash
git clone https://github.com/DarkPyonix/darkpyonix-ember
cd darkpyonix-ember
git checkout develop

# 메인 서버
cd crates/server
cargo build --release --locked
./target/release/ember-server

# 클라이언트, 다른 터미널에서
cd crates/app
EMBER_SERVER_URL=http://127.0.0.1:8740 cargo run --release --locked
```

HTTP 리스너에는 로그인이 없습니다. 그래서 루프백(기본값)이나 믿을 수 있는 네트워크에만 둡니다.
`ember-node`, 설정, 각 프로그램이 저장하는 내용은
[설치](https://darkpyonix.dev/darkpyonix-ember/ko/install.html)에서 다룹니다.

---

## ⚖️ 양보하지 않는 원칙

| ID | 규칙 | 이유 |
| -- | ---- | ---- |
| **E1** | 런처와 대화 화면에는 웹뷰가 절대 들어가지 않습니다. | 클라이언트는 `dioxus-compose` 로 네이티브하게 만들고, 웹뷰가 있을 수 있는 곳은 IDE 창뿐입니다. |
| **E2** | 감싼 에이전트는 본래 동작을 유지합니다. Ember 는 그 바깥에 기능을 더할 뿐 패치하지 않습니다. | 에이전트 설정, 모델, 세션 ID 가 원래대로 동작합니다. |
| **E3** | 대화, 에이전트 프로세스, 계정 자격 증명은 메인 서버에 있습니다. | 컴퓨터는 도구가 실행되는 곳이므로, 대화가 한 컴퓨터에 묶이지 않습니다. |
| **E4** | VS Code 는 감쌀 뿐 수정하지 않고, Extension Host 를 다시 구현하지 않습니다. | 그래야 확장이 VS Code 에서와 똑같이 동작합니다. |
| **E5** | IDE 창 안에서 웹뷰는 필요한 가장 작은 영역으로 한정합니다. | Ember 자체 에디터가 들어온 뒤에 적용되며, 그 전까지는 IDE 창 전체가 예외입니다. |
| **E6** | Compose 네이티브 에디터 코어는 VS Code 의 동작과 달라지지 않습니다. | 둘이 다르면 정의상 VS Code 가 맞습니다. |

---

## 📦 구성 요소

| 구성 요소 | 설명 |
| --------- | ---- |
| **ember** | 이 저장소입니다. `ember-server`, `ember-node`, 클라이언트, IDE 창으로 이루어집니다. IDE 창은 `web/proxy/` 로 VS Code Web 을 감쌉니다. |
| **vscode-darkpyonix** | DarkPyonix 노트북을 렌더링하는 VS Code 확장입니다. 가짜 매니저로 테스트했고, 실제 매니저로는 아직입니다. Ember 의 VS Code 런타임에 기본으로 설치됩니다. |
| **vscode-darkpyonix-theme** | DarkPyonix(phoenix) 디자인 언어의 VS Code 테마입니다. 진행 중입니다. |
| **intellij-darkpyonix** | 같은 노트북 기능을 제공하는 IntelliJ, PyCharm 플러그인입니다. 진행 중이며 CI 에서 빌드하고 테스트합니다. |

---

## 🔗 관련 저장소

- **[dioxus-compose](https://github.com/DarkPyonix/dioxus-compose)**: 클라이언트가 올라가는 네이티브
  GUI 스택입니다. 그 규칙이 Ember 클라이언트에도 그대로 적용됩니다.
- **[darkpyonix-core](https://github.com/DarkPyonix/darkpyonix)**: DarkPyonix 커널, 매니저, 허브와
  그 API 계약입니다. Ember 는 이를 다시 정의하지 않고 링크합니다.

---

## 🗂 프로젝트 구조

```
darkpyonix-ember/
├─ build/
│  └─ ose/               OSE: DarkPyonix 가 빌드한 Code-OSS, IDE 창의 기본 런타임
├─ crates/               크레이트마다 별도 cargo 프로젝트
│  ├─ server/            ember-server: 에이전트 CLI 를 감싸고, 세션을 저장하고, 업데이트를 푸시
│  ├─ node/              ember-node: 각 컴퓨터에서 도구 동작을 수행
│  ├─ app/               ember-app: dioxus-compose 위의 런처와 대화 UI
│  ├─ client/            클라이언트 코어: UI 아래의 연결, 동기화, 상태
│  ├─ transport/         P2P 연결 (iroh 백엔드와 메모리 내 가짜)
│  ├─ hub/               darkpyonix.dev 허브 클라이언트: 기기 등록과 디렉터리
│  ├─ bridge/            IDE 창 브리지: 버전이 붙은 웹뷰와 네이티브 사이 메시지
│  ├─ editor-conn/       Code-OSS 서버용 Rust 클라이언트
│  └─ editor/            에디터 코어 세션 계층 (계획된 에디터, #32)
├─ docs/
│  └─ guide/             사용자 가이드, darkpyonix.dev/darkpyonix-ember 에서 제공
├─ extensions/           vscode-darkpyonix, vscode-darkpyonix-theme, intellij-darkpyonix
├─ scripts/              저장소 검사와 설정 도우미
├─ tests/
│  └─ vectors/           기록한 대화 기록과 브리지 테스트 벡터
├─ web/
│  └─ proxy/             VS Code Web 래핑 계층 (Python, FastAPI)
└─ LICENSE
```

---

## 📄 라이선스

[Apache License 2.0](https://github.com/DarkPyonix/darkpyonix-ember/blob/develop/LICENSE).
