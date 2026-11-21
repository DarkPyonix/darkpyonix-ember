# Previews: app next to mockup (SPEC FR-L37)

Status: **written, not run** (#222). The mockup-pair script exists. The `#[Preview]` entries are
not built: they wait for the compose-rust preview API (compose-rust FR-54, compose-rust issue
#357).

## English

### What it does

`.github/scripts/preview/side-by-side.sh` captures every client screen next to its mockup render.
For each row of `.github/scripts/preview/screens.tsv`, at each size and theme, it:

1. renders the mockup page in headless Chromium at the same size and theme
   (`side_by_side.py render`, Playwright through `uv run --with playwright`);
2. starts the debug client with `EMBER_DEMO_DATA=1`, a fresh `EMBER_DATA_DIR`, `EMBER_CR_SHELL`,
   `EMBER_CR_SIZE`, `EMBER_CR_ROUTE` and the row's other `EMBER_CR_*` hooks, waits for the first
   frame, and captures the window by its own window id (`window-id.js`);
3. converts the capture to sRGB with `sips`, because a capture carries the display profile and
   token colours match only on the converted copy;
4. joins the two images, app on the left and mockup on the right
   (`side_by_side.py join`, Pillow through `uv run --with pillow`).

Sizes: 1440x900, 1024x800, 768x900, 390x844. Themes: light and dark.

It runs on macOS only for now, because it uses `screencapture`, CoreGraphics window ids and
`sips`. It needs a display and the Screen Recording permission for the terminal that runs it. The
CI job "Screen previews" (`.github/workflows/test-previews.yml`, #222) runs it on a hosted
`macos-15` runner and uploads the images as the artifact `ember-previews-macos`; it has not run
yet, so its capture step does not fail the run until the first green run. The mockup is not in this
public repository, so the job writes app captures only until the repository variable
`EMBER_PREVIEW_MOCKUP_DIR` names a mockup folder in the checkout (an owner decision).

### How to run

```sh
MOCKUP_DIR=/path/to/design-mockup .github/scripts/preview/side-by-side.sh
MOCKUP_DIR=/path/to/design-mockup .github/scripts/preview/side-by-side.sh --only home --size 390 --theme dark
.github/scripts/preview/side-by-side.sh --list
```

| Variable | Meaning | Default |
|---|---|---|
| `MOCKUP_DIR` | The mockup folder. It is not in git. Without it, only the app is captured (no pairs, no `uv` needed). | none |
| `EMBER_APP_BIN` | The client binary. A debug build, because the review hooks exist in debug builds only. | `ember/app/target/debug/ember-app` |
| `OUT_DIR` | Output folder. | `.scratch/preview/` |
| `CHROME_BIN` | The browser that renders the mockup. | Playwright's cached Chromium, then Google Chrome |

Output under `OUT_DIR`: `pairs/<screen>-<size>-<theme>-pair.png`, `app/` (sRGB), `raw/` (as
captured), `mockup/`, `logs/` and `index.md`, which lists every pair and every missing one with
its reason.

Exit status: 0 when every pair is written, 1 when a mockup render or a join failed, 2 for wrong
usage or a missing tool, 3 when an app window was not found or not captured.

### Rules the script keeps

- The window is found by the window id of the app's own pid, never by front-most, so the owner's
  windows are never captured.
- No key or click is sent to any application. Every state comes from an environment hook (app)
  or from the page's hash, `localStorage` and one script call (mockup).
- Only the processes the script started are stopped, by pid, also when it is interrupted.
- The app never reaches a real server: `EMBER_SERVER_URL` is removed from its environment.

### The screen table

`screens.tsv` has one row per screen: name, app route, extra app env, mockup page with its hash,
mockup `localStorage`, mockup action. A new screen or state is one new row. A row whose mockup
column is `-` is captured in the app only and gets no pair.

Known limits:

- The app shows demo data and the mockup shows its own sample data, so the pair compares layout
  and tokens, not content.
- `monitor`: the mockup's values move every second, so its render is not the same between runs.
- `search` and `new-session` have no mockup state with a deterministic entry: app only.
- The Ember editor window has no `EMBER_CR_ROUTE` value and is not in the table.
- A sheet that is opened by a click in the mockup (`project-sheet-*`, `files-share`,
  `vcs-pr-detail`) depends on that control being present at every width.

### `#[Preview]` entries: not built

compose-rust v0.0.11 has no preview API: no `#[Preview]` macro and no preview binary. FR-54
exists only as a draft (compose-rust issue #357). Until it lands, there is no preview function
per screen state and no fixed clock. When it lands, each row of the table becomes
a `#[Preview]` entry and the script takes the app image from the preview render instead of from
a window capture (#222).

## 한국어

### 하는 일

`.github/scripts/preview/side-by-side.sh`는 클라이언트의 모든 화면을 목업 렌더 옆에 놓고
캡처한다. `.github/scripts/preview/screens.tsv`의 각 행을 크기와 테마마다 다음 순서로 처리한다.

1. 목업 페이지를 헤드리스 Chromium에서 같은 크기와 테마로 그린다
   (`side_by_side.py render`, `uv run --with playwright`로 Playwright를 쓴다).
2. 디버그 클라이언트를 `EMBER_DEMO_DATA=1`, 새 `EMBER_DATA_DIR`, `EMBER_CR_SHELL`,
   `EMBER_CR_SIZE`, `EMBER_CR_ROUTE`, 그리고 그 행의 다른 `EMBER_CR_*` 훅과 함께 띄우고, 첫
   프레임을 기다린 뒤 그 창을 자기 창 id로 캡처한다 (`window-id.js`).
3. 캡처를 `sips`로 sRGB로 바꾼다. 캡처에는 디스플레이 프로필이 붙어 있어서, 바꾼 사본에서만
   토큰 색이 맞기 때문이다.
4. 두 이미지를 붙인다. 왼쪽이 앱, 오른쪽이 목업이다
   (`side_by_side.py join`, `uv run --with pillow`로 Pillow를 쓴다).

크기는 1440x900, 1024x800, 768x900, 390x844이고 테마는 라이트와 다크다.

지금은 macOS에서만 돈다. `screencapture`, CoreGraphics 창 id, `sips`를 쓰기 때문이다. 디스플레이와
실행하는 터미널의 화면 기록 권한이 필요하다. CI 작업 "Screen previews"(`.github/workflows/test-previews.yml`,
#222)가 호스팅 `macos-15` 러너에서 이 스크립트를 돌리고 이미지를 아티팩트 `ember-previews-macos`로 올린다.
아직 돌린 적이 없어서 첫 성공 전까지 캡처 단계는 실행을 실패시키지 않는다. 목업은 이 공개 저장소에 없으므로,
저장소 변수 `EMBER_PREVIEW_MOCKUP_DIR`가 체크아웃 안의 목업 폴더를 가리키기 전까지(소유자 결정) 앱 캡처만 쓴다.

### 실행 방법

```sh
MOCKUP_DIR=/path/to/design-mockup .github/scripts/preview/side-by-side.sh
MOCKUP_DIR=/path/to/design-mockup .github/scripts/preview/side-by-side.sh --only home --size 390 --theme dark
.github/scripts/preview/side-by-side.sh --list
```

| 변수 | 뜻 | 기본값 |
|---|---|---|
| `MOCKUP_DIR` | 목업 폴더. git에 없다. 주지 않으면 앱만 캡처한다(짝 이미지 없음, `uv` 불필요). | 없음 |
| `EMBER_APP_BIN` | 클라이언트 바이너리. 리뷰 훅이 디버그 빌드에만 있으므로 디버그 빌드를 쓴다. | `ember/app/target/debug/ember-app` |
| `OUT_DIR` | 출력 폴더. | `.scratch/preview/` |
| `CHROME_BIN` | 목업을 그리는 브라우저. | Playwright가 받아 둔 Chromium, 없으면 Google Chrome |

`OUT_DIR` 아래에 `pairs/<screen>-<size>-<theme>-pair.png`, `app/` (sRGB), `raw/` (캡처 원본),
`mockup/`, `logs/`, `index.md`가 생긴다. `index.md`는 모든 쌍과, 빠진 쌍의 이유를 적는다.

종료 코드: 모든 쌍을 썼으면 0, 목업 렌더나 붙이기가 실패하면 1, 사용법이 틀렸거나 도구가 없으면
2, 앱 창을 찾지 못했거나 캡처하지 못하면 3이다.

### 스크립트가 지키는 규칙

- 창은 앱 자신의 pid가 가진 창 id로 찾는다. 맨 앞 창으로 찾지 않으므로 오너의 창은 캡처되지
  않는다.
- 어떤 앱에도 키나 클릭을 보내지 않는다. 상태는 모두 환경 변수 훅(앱)이나 페이지의 해시,
  `localStorage`, 스크립트 호출 한 번(목업)으로 만든다.
- 스크립트가 띄운 프로세스만 pid로 끝낸다. 중간에 멈춰도 같다.
- 앱은 실제 서버에 닿지 않는다. 앱의 환경에서 `EMBER_SERVER_URL`을 지운다.

### 화면 표

`screens.tsv`는 화면마다 한 행이다: 이름, 앱 라우트, 추가 앱 환경 변수, 해시를 포함한 목업
페이지, 목업 `localStorage`, 목업 동작. 화면이나 상태를 더하려면 행 하나를 더한다. 목업 칸이
`-`인 행은 앱만 캡처하고 쌍을 만들지 않는다.

알려진 한계:

- 앱은 데모 데이터를, 목업은 자체 샘플 데이터를 보여 준다. 그래서 쌍은 배치와 토큰을 비교하는
  것이고 내용을 비교하는 것이 아니다.
- `monitor`: 목업의 값이 1초마다 움직이므로 렌더가 실행마다 같지 않다.
- `search`와 `new-session`은 목업에 결정적으로 들어갈 수 있는 상태가 없다. 앱만 캡처한다.
- Ember 에디터 창은 `EMBER_CR_ROUTE` 값이 없어서 표에 없다.
- 목업에서 클릭으로 여는 시트(`project-sheet-*`, `files-share`, `vcs-pr-detail`)는 그 컨트롤이
  모든 폭에 있어야 열린다.

### `#[Preview]` 항목: 만들지 않았다

compose-rust v0.0.11에는 미리보기 API가 없다. `#[Preview]` 매크로도, 미리보기 바이너리도 없다.
FR-54는 초안으로만 있다 (compose-rust 이슈 #357). 그것이 들어오기 전에는 화면 상태마다의 미리보기
함수와 고정된 시계가 없다. 들어오면 표의 각 행이 `#[Preview]` 항목이 되고, 스크립트는
앱 이미지를 창 캡처 대신 미리보기 렌더에서 가져온다 (#222).
