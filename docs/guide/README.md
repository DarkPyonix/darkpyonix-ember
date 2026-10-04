# docs/guide

Ember 사용자 가이드입니다. 사이트 생성기 없이 손으로 쓴 정적 HTML/CSS이며, 빌드 단계가 없습니다.
이 디렉터리를 그대로 GitHub Pages로 배포합니다(`.github/workflows/pages.yml`, `main` 브랜치에
들어올 때). 배포 주소는 <https://darkpyonix.dev/darkpyonix-ember/> 입니다.

형식은 compose-rust와 dioxus-compose 가이드와 같습니다. 프로젝트 문서(README, PROJECT, INTENT,
SPEC)와 달리 이 가이드는 **영어와 한국어 두 언어를 동등하게** 제공합니다. 영어가 기본 진입
언어입니다.

## 구조

```
docs/guide/
├─ index.html            # 진입점. 기본은 en/, 이전에 한국어를 본 독자는 ko/로 보냅니다
├─ robots.txt, sitemap.xml
├─ assets/
│  ├─ style.css          # 사이트 전체의 유일한 스타일시트
│  └─ guide.js           # 유일한 자바스크립트 (테마, 사이드바, 코드 복사)
├─ en/                   # 영어 페이지
│  ├─ index.html               Overview: 대화 중심, 메인 서버, 컴퓨터 전환, A2A, 현재 상태
│  ├─ install.html             Install: 소스에서 ember server, ember node, 클라이언트 빌드와 실행
│  ├─ first-conversation.html  Your first conversation: 프로젝트, 세션, 에이전트와 계정, 승인
│  ├─ computers.html           Computers and the hub: 컴퓨터 추가(주소, 허브), 전환, 프로젝트 마운트
│  ├─ ide.html                 Opening an IDE
│  ├─ terminals.html           Persistent terminals
│  ├─ browser.html             Remote browser
│  ├─ agents.html              Agents: Claude Code, Codex, Antigravity, OMP
│  └─ troubleshooting.html     Troubleshooting: 실제 오류 메시지와 대처, 로그 위치
└─ ko/                   # 한국어 페이지. 파일 이름은 en/과 1:1로 같습니다
```

**파일 이름은 두 언어에서 반드시 같아야 합니다.** 언어 전환 링크가 같은 이름의 파일을
가리키는 방식으로 동작하기 때문입니다.

사이드바 순서와 제목은 모든 페이지에서 같습니다.

| 순서 | 파일 | 영어 | 한국어 |
| ---- | ---- | ---- | ------ |
| 1 | `index.html` | Overview | 개요 |
| 2 | `install.html` | Install | 설치 |
| 3 | `first-conversation.html` | Your first conversation | 첫 대화 |
| 4 | `computers.html` | Computers and the hub | 컴퓨터와 허브 |
| 5 | `ide.html` | Opening an IDE | IDE 열기 |
| 6 | `terminals.html` | Persistent terminals | 영구 터미널 |
| 7 | `browser.html` | Remote browser | 원격 브라우저 |
| 8 | `agents.html` | Agents | 에이전트 |
| 9 | `troubleshooting.html` | Troubleshooting | 문제 해결 |

## 규칙

- 빌드 도구, Node, 프레임워크, 외부 CDN을 쓰지 않습니다(글꼴만 Google Fonts에서 읽습니다).
  브라우저로 파일을 열면 그게 전부입니다.
- 자바스크립트는 `assets/guide.js` 하나뿐이고, 없어도 모든 페이지가 읽히고 이동할 수 있어야
  합니다. JS가 하는 일은 테마 토글, 좁은 화면의 사이드바 토글, 코드 복사 버튼입니다.
- 문법 강조는 손으로 붙인 `<span>` 클래스입니다(`k` 키워드, `s` 문자열, `n` 숫자, `c` 주석,
  `f` 함수). 하이라이터 라이브러리를 추가하지 않습니다.
- **모든 명령, 환경 변수, 경로, 오류 메시지는 `develop` 의 코드에서 가져오거나 대조해서 확인한
  것이어야 합니다.** 없는 플래그를 지어내지 않습니다. 아직 동작하지 않거나 확인하지 못한 기능은
  상태 배지와 이슈 번호를 붙입니다: `<span class="pill works">`(구현),
  `<span class="pill partial">`(부분), `<span class="pill planned">`(계획).
- 문체는 thisisthepy/pythonx-compose의 `docs/style/writing.md` 를 따릅니다. 현재 시제로 사실만
  쓰고, 한국어는 합니다체로 씁니다. em-dash(U+2014)는 쓰지 않습니다
  (`scripts/check-no-em-dash.sh`). 설치와 실행 예시는 cargo, uv, ppp, tcl만 씁니다.
- 번역은 직역이 아니라 각 언어로 자연스럽게 씁니다. 내용과 순서는 같게 유지합니다.

## 페이지 추가하기

1. `en/` 에서 가장 비슷한 페이지를 복사해 새 이름으로 만듭니다. 머리말, 사이드바, 푸터 구조를
   그대로 유지합니다.
2. `<head>` 를 고칩니다.
   - `<title>`, `<meta name="description">`
   - `<link rel="alternate" hreflang="en" href="새이름.html">`
   - `<link rel="alternate" hreflang="ko" href="../ko/새이름.html">`
   - `<link rel="alternate" hreflang="x-default" href="새이름.html">`
3. 상단 언어 전환 링크를 새 파일 이름으로 맞춥니다. 현재 언어 쪽에 `aria-current="true"` 를 둡니다.
4. 같은 이름으로 `ko/` 페이지를 만듭니다. `<html lang="ko">` 로 바꾸고, `hreflang` 두 줄을
   서로 반대로(`en` → `../en/새이름.html`, `ko` → ` 새이름.html`) 씁니다.
5. **모든 페이지**(en과 ko 전부)의 사이드바 목록에 새 항목을 추가합니다. 현재 페이지에는
   `aria-current="page"` 를 붙입니다. 위 표도 고칩니다.
6. 앞뒤 페이지의 `.pagenav` 링크와 `sitemap.xml` 을 갱신합니다.

## 로컬에서 확인하기

```bash
cd docs/guide
uv run python -m http.server 8000
# http://localhost:8000/  → en/ 으로 이동합니다
```

확인할 것: 진입 페이지, 내용 페이지 하나, 언어 전환(같은 페이지에 머무르는지), 다크 테마
토글, 좁은 창에서의 사이드바.

## 테마 동작

- 기본값은 OS 설정(`prefers-color-scheme`)입니다. 별도 표시가 없습니다.
- 토글을 누르면 `<html data-theme="light|dark">` 가 설정되고 `localStorage` 의 `dxc-theme` 에
  저장됩니다. 이후 방문에는 `<head>` 의 짧은 인라인 스크립트가 이 값을 먼저 적용해서
  화면 깜빡임을 막습니다. 새 페이지를 만들 때 이 인라인 스크립트를 빠뜨리지 마세요.
- 읽던 언어는 `dxc-lang` 에 저장되고, `docs/guide/index.html` 이 그 값을 참고합니다. 두 키는
  darkpyonix.dev의 다른 가이드와 같은 이름이라, 한 곳에서 고른 테마와 언어가 다른 가이드에도
  이어집니다.
