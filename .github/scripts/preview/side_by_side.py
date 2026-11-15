"""Helper of side-by-side.sh (FR-L37, #222). Not run directly.

  render: draws mockup pages in headless Chromium (Playwright) and saves one PNG per job.
  join:   puts an app capture (left) and a mockup render (right) into one PNG (Pillow).

side-by-side.sh starts it through uv, so nothing is installed into the system Python:
  uv run --no-project --with playwright python3 side_by_side.py render ...
  uv run --no-project --with pillow     python3 side_by_side.py join ...

Job files are tab separated, one job per line.
  render: out.png, width, height, theme, page[#hash], storage ('k=v;k=v' or '-'), action (JS or '-')
  join:   app.png, mockup.png, out.png, width
"""

import argparse
import glob
import os
import pathlib
import signal
import sys

THEME_KEY = "ember-mock-theme"  # the mockup's own theme switch (core.js)
GAP = 24  # px between the two halves of a pair
GREY = (128, 128, 128)


def read_jobs(path, fields):
    jobs = []
    for line in pathlib.Path(path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) != fields:
            sys.exit(f"error: {path}: expected {fields} fields, got {len(parts)}: {line!r}")
        jobs.append(parts)
    return jobs


def find_browser():
    """CHROME_BIN, else the newest Playwright Chromium in the user cache, else Google Chrome."""
    env = os.environ.get("CHROME_BIN")
    if env:
        return env
    cache = os.path.expanduser("~/Library/Caches/ms-playwright")
    found = sorted(
        glob.glob(os.path.join(cache, "chromium-*", "chrome-mac*", "*.app", "Contents", "MacOS", "*")),
        key=lambda p: int("".join(c for c in p.split("chromium-")[1].split(os.sep)[0] if c.isdigit()) or 0),
    )
    if found:
        return found[-1]
    chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    return chrome if os.path.exists(chrome) else None


def js_string(text):
    return "'" + text.replace("\\", "\\\\").replace("'", "\\'") + "'"


def render(args):
    from playwright.sync_api import sync_playwright

    root = pathlib.Path(args.mockup_dir).resolve()
    jobs = read_jobs(args.jobs, 7)
    browser_path = find_browser()
    failed = 0
    with sync_playwright() as p:
        try:
            # The two flags keep Chromium from asking for the login keychain.
            browser = p.chromium.launch(
                executable_path=browser_path,
                headless=True,
                args=["--use-mock-keychain", "--password-store=basic"],
            )
        except Exception as e:  # no browser: say how to give one
            print(f"error: no Chromium to render the mockup ({str(e).splitlines()[0]})", file=sys.stderr)
            print("       set CHROME_BIN to a Chrome or Chromium executable, or run", file=sys.stderr)
            print("       uv run --with playwright playwright install chromium", file=sys.stderr)
            return 2
        try:
            for out, width, height, theme, url, storage, action in jobs:
                name = pathlib.Path(out).name
                page_file, _, frag = url.partition("#")
                target = root / page_file
                if not target.is_file():
                    print(f"FAIL {name} no mockup page {target}")
                    failed += 1
                    continue
                pairs = [(THEME_KEY, theme)]
                if storage != "-":
                    for item in storage.split(";"):
                        key, _, value = item.partition("=")
                        pairs.append((key, value))
                init = "try{" + "".join(
                    f"localStorage.setItem({js_string(k)},{js_string(v)});" for k, v in pairs
                ) + "}catch(e){}"
                # A new context per job: the mockup keeps its state in localStorage, and one
                # job's clicks must not show up in the next.
                ctx = browser.new_context(
                    viewport={"width": int(width), "height": int(height)},
                    device_scale_factor=args.scale,
                    color_scheme=theme,
                )
                try:
                    ctx.add_init_script(init)
                    page = ctx.new_page()
                    page.goto(target.as_uri() + ("#" + frag if frag else ""))
                    page.wait_for_timeout(args.wait_ms)
                    if action != "-":
                        page.evaluate(action)
                        page.wait_for_timeout(args.wait_ms + 200)
                    page.screenshot(path=out)
                    print(f"OK {name}")
                except Exception as e:
                    print(f"FAIL {name} {str(e).splitlines()[0][:160]}")
                    failed += 1
                finally:
                    ctx.close()
                sys.stdout.flush()
        finally:
            browser.close()
    return 1 if failed else 0


def join(args):
    from PIL import Image

    failed = 0
    for app, mock, out, width in read_jobs(args.jobs, 4):
        name = pathlib.Path(out).name
        try:
            target_w = int(width) * args.scale
            halves = []
            sizes = []
            for path in (app, mock):
                im = Image.open(path).convert("RGB")
                sizes.append(f"{im.width}x{im.height}")
                if im.width != target_w:
                    im = im.resize((target_w, max(1, round(im.height * target_w / im.width))), Image.LANCZOS)
                halves.append(im)
            canvas = Image.new("RGB", (target_w * 2 + GAP, max(h.height for h in halves)), GREY)
            canvas.paste(halves[0], (0, 0))  # app on the left
            canvas.paste(halves[1], (target_w + GAP, 0))  # mockup on the right
            canvas.save(out)
            print(f"OK {name} app={sizes[0]} mockup={sizes[1]}")
        except Exception as e:
            print(f"FAIL {name} {str(e).splitlines()[0][:160]}")
            failed += 1
    return 1 if failed else 0


def main():
    # side-by-side.sh stops this helper with SIGTERM: leave through the normal exit path so the
    # browser this process started is closed with it.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("render")
    r.add_argument("--mockup-dir", required=True)
    r.add_argument("--jobs", required=True)
    r.add_argument("--scale", type=int, default=2)
    r.add_argument("--wait-ms", type=int, default=500)
    r.set_defaults(fn=render)
    j = sub.add_parser("join")
    j.add_argument("--jobs", required=True)
    j.add_argument("--scale", type=int, default=2)
    j.set_defaults(fn=join)
    args = ap.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
