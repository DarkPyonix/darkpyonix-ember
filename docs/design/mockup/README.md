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
