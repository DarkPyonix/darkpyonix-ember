# Client mockup

The browser mockup of the Ember client (#151, v8, 2026-10-25). Each page behaves like the app
with demo data kept in browser storage; `index.html` lists the screens. It is the reference the
client screens are compared with: `.github/scripts/preview/side-by-side.sh` renders its pages at
the capture sizes and joins them with the app captures (FR-L37, #222), and the CI job "Screen
previews" (`.github/workflows/test-previews.yml`) does the same on every dispatched run.

Kept in this public repository by the owner's approval (2026-11-21, through the leader: "commit
the mockup to darkpyonix-ember docs/design/mockup/ (public OK)"). Only the files the pages load
are here: the pages, their CSS and JS, the phoenix-fold logo and the DarkPyonix logo image. The
review notes, reference screenshots of other products, the icon-pack comparison and the logo
review stayed out, because the comparison does not need them and the screenshots are not ours to
publish.

Open it from a local server (`uv run python -m http.server 4000` in this folder), because the pages
load each other's scripts.

`icons.js` holds icon paths from Heroicons (MIT, Copyright (c) Tailwind Labs, Inc.,
https://github.com/tailwindlabs/heroicons).

## Tools, dashboard reveal and the app bar mark (2026-11-27, INTENT D58)

The owner asked for these on 2026-11-27: "도구 섹션에서 프로젝트별로 에이전트 별로 접근 권한 설정 따로
할 수 있게 해줘야지. 지금 목업 기준으로 메인 페이지에 있는 대시보드 같은거에 있는 여러 연동 관련 부분들을
도구 섹션으로 분리해야 하고 대시보드는 아래쪽 스크롤이 아니라 메인 페이지 위쪽 스크롤을 하면 나오는걸로
수정해야 해. 스크롤을 끝까지 당기면 쏟아져 나오는걸로 하고 다크피닉스 로고 눌러도 나오는걸로 하고. 그리고
목업 앱바에 있는 다크피닉스 로고는 좀 심플리파이드 한 버전으로 수정이 필요할거 같아. 실제 다크피닉스 로고를
그대로 쓰는 케이스는 Ember editor의 왼쪽 상단 로고, 그리고 앱 작업 표시줄 로고 자체. 이렇게 두군데에서만
사용되도록 해야지."

- **도구 (Tools)**, `tools.html` (`tools.js`, `tools.css`, data in `EMBER_DEMO.tools`): ember-server's Tools section,
  in the sidebar after 에이전트 and in the phone's 더보기 sheet. It has three sections: 연동 (the integration cards
  that were on the Home dashboard), MCP 서버, and 서버 CLI (CLIs registered on the server, with how each node finds
  them through its terminal environment). Every agent CLI runs on ember-server. Each tool opens an access sheet
  with a default, a project list and a team member list, each set to 허용, 요청 시 확인 or 차단. A member's own
  setting comes before its project's, and a project set to 차단 blocks its members. Captures: `tools.html#mcp`
  filters, `tools.html#cli:gh` or `tools.html#int:github` opens the sheet, `tools.html#int:calendar/rules` opens
  the rules tab.
- **Home dashboard**: no longer below the composer. Pulling Home down from the top (wheel or touch) shows a
  progress hint; a full pull pours the dashboard out over the page. The app bar mark, the phone's top-left mark
  and the "오늘의 팀" pill also open it. The close button, Escape, the bottom pill and pushing past the bottom edge
  close it. Reduced motion swaps the pour for a short fade. Captures: `home.html#dash` (open), `home.html#pull`
  (partial pull), or `Ember.dash.open()`, `Ember.dash.pull(px)`, `Ember.dash.close()`.
- **App bar mark**: `logo/phoenix-fold/mark-simple.svg`, a flat single-colour trace of the phoenix silhouette
  (inlined by `core.js` as `BRAND.mark`, colour `--brand-mark`). The full logo (`mark-sm.svg`, `BRAND.full`) stays
  only at the Ember editor's top left and as the taskbar/dock icon (the favicon in the mockup). The sidebar
  toggle is its own button next to the brand again, because the mark now opens the dashboard.
