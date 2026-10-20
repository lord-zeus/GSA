#!/usr/bin/env python3
"""F#29 vision oracle — the free side of the reCAPTCHA v2 image-grid solver.

A tiny local HTTP service (stdlib only, no deps) that turns the "who picks
the tiles?" question into a 30-second human job on ANY device — or a machine
job if you point the solver at a model service speaking the same protocol.
It is the local equivalent of GSA's "Manual Input" captcha mode, but it
answers over HTTP so the Rust runner can call it mid-flight.

Protocol (matches crates/engine/src/captcha_cdp.rs :: oracle_ask)
-----------------------------------------------------------------
  POST /solve
       request  {"image": "<base64 png>", "question": "...", "page_url": "..."}
       response {"points": [{"x": .., "y": ..}, ...], "next": {"x":..,"y":..} | null}
                x/y are screenshot pixels (the solver converts to CSS px)
       504 {"error":"timeout"}  |  502 {"error":"cancelled"}

  POST /answer/SID    {"points": [...], "next": {...} | null}   (from the UI)
  POST /cancel/SID                                             (from the UI)

Views
-----
  GET /             status page: active sessions + open links
  GET /view/SID     click-pick UI (image 1:1 at 0,0, page px == image px)

Run
---
  python tools/captcha_oracle.py [--port 8090] [--no-browser]
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import threading
import time
import uuid
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST = "127.0.0.1"
SOLVE_TIMEOUT = 600  # longest human wait the solver allows
sessions: dict[str, dict] = {}
sessions_lock = threading.Lock()

TEMPLATE_HEAD = """<!doctype html>
<html><head><meta charset="utf-8"><title>Captcha oracle</title>
<style>
  body { margin:0; background:#111; overflow:auto; }
  #bar { position:fixed; top:0; right:0; z-index:9; background:rgba(0,0,0,.78);
         color:#eee; font:14px/1.5 system-ui, sans-serif; padding:8px 12px;
         border-bottom-left-radius:10px; max-width:420px; }
  #bar b { color:#8bd4ff; }
  #btns button { margin:4px 4px 0 0; padding:6px 10px; font:14px system-ui;
                  border:0; border-radius:6px; cursor:pointer; background:#333;color:#fff;}
  #btns .go { background:#1a7f37; }
  #btns .no { background:#7a2b2b; }
  #list { color:#9fd; font:12px/1.6 ui-monospace, monospace; white-space:nowrap; }
  #done { position:fixed; inset:0; z-index:99; background:rgba(0,40,0,.8);
           color:#fff; font:20px/1.6 system-ui; display:none; }
  #done div { position:absolute; top:45%; left:0; right:0; text-align:center; }
  #marker { position:absolute; border:2px solid #ff4d4d; border-radius:50%;
            pointer-events:none; box-shadow:0 0 6px #ff4d4d; }
  #marker span { position:absolute; left:-8px; top:-16px; color:#ff9c9c; font:12px system-ui; }
</style></head>
<body>
<div id="bar">
  <div><b>reCAPTCHA oracle</b> — session __SID__</div>
  <div id="question"></div>
  <div style="margin-top:2px">Click the tiles that match the on-screen
    instruction, then <b>Submit</b>. Use <b>Mark last = Next</b> if one of
    your clicks is the Next/Verify button.</div>
  <div id="btns">
    <button onclick="undo()">Undo</button>
    <button onclick="clearAll()">Clear</button>
    <button onclick="markNext()">Mark last = Next</button>
    <button class="go" onclick="submitPoints()">Submit</button>
    <button class="no" onclick="cancelAll()">Cancel</button>
  </div>
  <div id="list">no clicks yet</div>
</div>
<img id="img" alt="captcha screenshot" src="/img/__SID__">
<div id="done"><div id="donetext">Sending...</div></div>
"""

TEMPLATE_JS = """
<script>
const SID = "__SID__";
let pts = [];
let next = null;
const img = document.getElementById('img');
img.addEventListener('click', (e) => {
  if (pts.length >= 9) { flash('max 9 tile clicks'); return; }
  pts.push({x: Math.round(e.clientX), y: Math.round(e.clientY)});
  render();
});
function flash(m) {
  const d = document.getElementById('done'); d.style.display = 'block';
  document.getElementById('donetext').textContent = m;
  setTimeout(() => d.style.display = 'none', 1500);
}
function render() {
  document.querySelectorAll('#marker').forEach(m => m.remove());
  pts.forEach((p, i) => {
    const d = document.createElement('div');
    d.id = 'marker';
    d.style.left = (p.x - 8) + 'px'; d.style.top = (p.y - 8) + 'px';
    d.style.width = d.style.height = '16px';
    d.innerHTML = '<span>' + (i + 1) + '</span>';
    document.body.appendChild(d);
  });
  document.getElementById('list').textContent =
    pts.length ? pts.map((p, i) => '#' + (i + 1) + ' ' + p.x + ',' + p.y).join('   ')
               : 'no clicks yet';
}
function undo()     { if (next) { pts.push(next); next = null; } else { pts.pop(); } render(); }
function clearAll() { pts = []; next = null; render(); }
function markNext() { next = pts.length ? pts.pop() : null; render(); }
function submitPoints(){ finish({points: pts, next: next}, '/answer/' + SID); }
function cancelAll()  { finish(null, '/cancel/' + SID); }
function finish(payload, url) {
  const d = document.getElementById('done'); d.style.display = 'block';
  document.getElementById('donetext').textContent = 'Sending...';
  fetch(url, {method: 'POST', headers: {'Content-Type': 'application/json'},
    body: payload ? JSON.stringify(payload) : 'null'})
  .then(r => r.json()).catch(() => {})
  .then(() => {
    document.getElementById('donetext').textContent =
      (url.indexOf('cancel') >= 0) ? 'Cancelled' : 'Done — solver will continue.';
    setTimeout(() => d.style.display = 'none', 1200);
  });
}
</script></body></html>
"""

def view_html(sid: str) -> str:
    return (TEMPLATE_HEAD + TEMPLATE_JS).replace("__SID__", sid)

class Handler(BaseHTTPRequestHandler):
    server_version = "CaptchaOracle/1.0"

    def log_message(self, fmt, *args):
        print("[oracle] " + (fmt % args), flush=True)

    def _json(self, code: int, obj: dict) -> None:
        data = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/" or self.path.startswith("/?"):
            with sessions_lock:
                rows = [
                    f'<li><a href="/view/{sid}">session {sid}</a> — '
                    f'{s.get("question", "")[:60] or "(no question)"} — '
                    f'{int(time.time() - s["t"])}s ago'
                    for sid, s in sessions.items()
                ]
            page = (
                "<html><head><title>Captcha oracle</title></head>"
                "<body style='font:16px/2 system-ui;background:#111;color:#eee;'>"
                "<h2>reCAPTCHA vision oracle</h2>"
                "<p>POST /solve {\"image\": b64png} — solver chain target.</p>"
                f"<ul>{''.join(rows) or '<li><i>no active sessions</i></li>'}</ul>"
                "</body></html>"
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)
            return
        m = re.match(r"^/view/([0-9a-f-]+)", self.path)
        if m:
            html = view_html(m.group(1)).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(html)))
            self.end_headers()
            self.wfile.write(html)
            return
        im = re.match(r"^/img/([0-9a-f-]+)", self.path)
        if im:
            with sessions_lock:
                rec = sessions.get(im.group(1))
                b64 = rec["image"] if rec else None
            if not b64:
                self._json(404, {"error": "no image for that session"})
                return
            data = b64.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self._json(404, {"error": "not found"})

    def _body_read(self) -> bytes:
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def _body_json(self):
        try:
            return json.loads(self._body_read().decode() or "{}")
        except Exception:
            return None

    def do_POST(self):
        if self.path == "/solve":
            req = self._body_json() or {}
            sid = uuid.uuid4().hex[:8]
            event = threading.Event()
            rec = {
                "t": time.time(),
                "image": req.get("image"),
                "question": req.get("question") or "",
                "page_url": req.get("page_url") or "",
                "event": event,
                "answer": None,
            }
            with sessions_lock:
                sessions[sid] = rec
                # prune finished sessions older than 10 min
                now = time.time()
                for k in [s for s, r in sessions.items()
                          if (s != sid) and r["event"].is_set() and now - r["t"] > 600]:
                    sessions.pop(k, None)
            sync = "sync" not in self.path and "sync=false" not in self.path
            print(f"[oracle] session {sid} question={rec['question']!r}", flush=True)
            if not args.no_browser:
                try:
                    webbrowser.open(f"http://{HOST}:{args.port}/view/{sid}")
                except Exception:
                    pass
            if not sync:
                self._json(202, {"waiting": True, "session": sid,
                                 "view": f"http://{HOST}:{args.port}/view/{sid}"})
                return
            got = event.wait(SOLVE_TIMEOUT)
            if got and rec["answer"] is not None:
                with sessions_lock:
                    rec["t"] = time.time()
                self._json(200, rec["answer"])
            elif not got:
                self._json(504, {"error": "timeout", "session": sid,
                                 "view": f"http://{HOST}:{args.port}/view/{sid}"})
            else:
                self._json(502, {"error": "cancelled", "session": sid})
            return
        m = re.match(r"^/(answer|cancel)/([0-9a-f-]+)", self.path)
        if m:
            kind, sid = m.group(1), m.group(2)
            with sessions_lock:
                rec = sessions.get(sid)
            if not rec:
                self._json(404, {"error": "no such session"})
                return
            if kind == "cancel":
                rec["event"].set()
                self._json(200, {"ok": True, "cancelled": True})
                return
            payload = self._body_json() or {}
            pts = payload.get("points") or []
            clean = []
            for p in pts:
                try:
                    clean.append({"x": round(float(p["x"])), "y": round(float(p["y"]))})
                except Exception:
                    pass
            nxt = None
            if payload.get("next"):
                try:
                    nxt = {"x": round(float(payload["next"]["x"])),
                           "y": round(float(payload["next"]["y"]))}
                except Exception:
                    pass
            if not clean:
                self._json(400, {"error": "no valid points"})
                return
            rec["answer"] = {"points": clean, "next": nxt}
            rec["event"].set()
            print(f"[oracle] session {sid} answer: {len(clean)} points next={nxt}", flush=True)
            self._json(200, {"ok": True, "points": len(clean)})
            return
        self._json(404, {"error": "not found"})


def main():
    global args
    ap = argparse.ArgumentParser(description="F#29 captcha vision oracle")
    ap.add_argument("--port", type=int, default=8090)
    ap.add_argument("--no-browser", action="store_true",
                    help="do not auto-open the browser tab on /solve")
    args = ap.parse_args()
    httpd = ThreadingHTTPServer((HOST, args.port), Handler)
    print(f"[oracle] listening on http://{HOST}:{args.port}/  (Ctrl-C to stop)", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
