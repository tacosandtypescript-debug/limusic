#!/usr/bin/env python3
"""A development server for overlay/page.html.

Why this exists: the overlay page is embedded in the LiMusic binary with `include_str!`, so the only
way to see a change through the app's own server is to rebuild it. That is a 30-second loop for the
debug profile and fourteen minutes for the release one, and neither is a loop anyone wants while
adjusting an easing curve.

This serves the same file straight off disk instead, so an edit is visible on a refresh. It also
mirrors two things the real server does, so what is being reviewed behaves like what ships:

  * `/cover?<url>` — the artwork proxy, with the same allowlist the Rust side uses. That matters
    beyond convenience: the artwork has to be same-origin for the accent extraction to be able to
    read its pixels, and a cover fetched straight from Google taints the canvas.
  * `/` — the overlay, with a small reload script appended. The appended script is *not* written to
    the file, so the page on disk stays exactly what the binary will embed.

Everything is loopback-only. This is a development harness and has none of the token gate or the
route assertions the real server has — do not expose it.
"""

import http.server
import os
import socketserver
import sys
import urllib.parse
import urllib.request

PORT = 8788
HERE = os.path.dirname(os.path.abspath(__file__))
# `tools/` sits inside the repo, so the repo is one level up — not a sibling directory,
# which is where this pointed when it lived in a scratch folder beside the checkout.
REPO = os.path.dirname(HERE)
PAGE = os.path.join(REPO, "src-tauri", "src", "overlay", "page.html")
OVERLAY_DIR = os.path.dirname(PAGE)
SWITCHER = os.path.join(HERE, "overlay-switch.html")

# The page is a shell plus these. Served by name from an allowlist rather than from the requested
# path, so a request cannot walk out of the directory — the same reasoning as the token on the real
# server, one order of magnitude simpler.
ASSETS = {
    "base.css": "text/css; charset=utf-8",
    "designs/sleeve-wide.css": "text/css; charset=utf-8",
    "designs/sleeve-tall.css": "text/css; charset=utf-8",
    "designs/playout-wide.css": "text/css; charset=utf-8",
    "designs/playout-tall.css": "text/css; charset=utf-8",
    "designs/vinyl-wide.css": "text/css; charset=utf-8",
    "designs/vinyl-tall.css": "text/css; charset=utf-8",
    "overlay.js": "text/javascript; charset=utf-8",
}

# The same hosts the Rust proxy allows, and the same rules: https only.
ALLOWED_SUFFIXES = ("googleusercontent.com", "ggpht.com", "ytimg.com")


def cover_allowed(url: str) -> bool:
    try:
        parts = urllib.parse.urlsplit(url)
    except ValueError:
        return False
    if parts.scheme != "https" or not parts.hostname:
        return False
    host = parts.hostname.lower()
    # A literal host match or a real subdomain — never a suffix, or `evil-ytimg.com` would pass.
    return any(host == s or host.endswith("." + s) for s in ALLOWED_SUFFIXES)


RELOAD = """
<script>
/* Appended by the development server, never written to the file on disk. */
(() => {
  let seen = null;
  setInterval(async () => {
    try {
      const t = await (await fetch("/version", { cache: "no-store" })).text();
      if (seen === null) { seen = t; return; }
      if (t !== seen) location.reload();
    } catch (e) { /* the server went away; leave the page as it is */ }
  }, 600);
})();
</script>
"""


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass                                    # quiet: it would drown the useful output

    def _send(self, code, body, ctype="text/html; charset=utf-8", extra=None):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        # Nothing here should ever be cached, or a "reload" shows the previous edit.
        self.send_header("Cache-Control", "no-store, must-revalidate")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        parsed = urllib.parse.urlsplit(self.path)
        route = parsed.path
        try:
            if route in ("/", "/index.html"):
                with open(PAGE, encoding="utf-8") as fh:
                    html = fh.read()
                # Inject before the last closing tag so the script runs after the page's own.
                idx = html.rfind("</body>")
                html = (html[:idx] + RELOAD + html[idx:]) if idx != -1 else html + RELOAD
                self._send(200, html)

            elif route.lstrip("/") in ASSETS:
                name = route.lstrip("/")
                with open(os.path.join(OVERLAY_DIR, name), encoding="utf-8") as fh:
                    self._send(200, fh.read(), ASSETS[name])

            elif route in ("/switch", "/switch.html"):
                if not os.path.exists(SWITCHER):
                    self._send(404, "switcher not generated yet")
                    return
                with open(SWITCHER, encoding="utf-8") as fh:
                    self._send(200, fh.read())

            elif route == "/version":
                try:
                    stamp = f"{os.path.getmtime(PAGE):.3f}"
                except OSError:
                    stamp = "missing"
                self._send(200, stamp, "text/plain; charset=utf-8")

            elif route == "/cover":
                # The whole query *is* the URL — no parameter name — because that is what the page
                # builds (`COVER_URL + encodeURIComponent(url)`) and what the Rust route reads
                # (`req.uri().query()`). Parsing it as `?u=` would silently 400 every cover.
                target = urllib.parse.unquote(parsed.query).strip()
                if target.startswith("//"):
                    target = "https:" + target      # protocol-relative, as `normalize_cover` does
                if not target:
                    self._send(400, "missing url", "text/plain; charset=utf-8")
                elif not cover_allowed(target):
                    self._send(403, "host not allowed", "text/plain; charset=utf-8")
                else:
                    req = urllib.request.Request(target, headers={"User-Agent": "Mozilla/5.0"})
                    with urllib.request.urlopen(req, timeout=20) as resp:
                        data = resp.read()
                        ctype = resp.headers.get("Content-Type", "image/jpeg")
                    self._send(200, data, ctype)

            else:
                self._send(404, "not found", "text/plain; charset=utf-8")

        except BrokenPipeError:
            pass                                # the browser navigated away mid-response
        except Exception as exc:                # noqa: BLE001 - a dev server should say what broke
            self._send(500, f"{type(exc).__name__}: {exc}", "text/plain; charset=utf-8")


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    if not os.path.exists(PAGE):
        print(f"  page not found: {PAGE}")
        return 1
    with Server(("127.0.0.1", PORT), Handler) as httpd:
        print(f"  overlay dev server on http://127.0.0.1:{PORT}/switch")
        print(f"  serving {PAGE}")
        print("  edits to that file reload the page by themselves; Ctrl+C to stop")
        sys.stdout.flush()
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print("\n  stopped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
