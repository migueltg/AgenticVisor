"""Runs the real interface (static/index.html) in Chrome, with the desktop app's
command bridge replaced by the `agenticvisor bridge` process, which answers from
a fake home folder. Nothing listens on a port and no real data is read.
"""
import json
import mimetypes
import os
import pathlib
import subprocess
import threading

ROOT = pathlib.Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "debug" / "agenticvisor"
STATIC = ROOT / "static"
BASE = "http://app.test"


class Bridge:
    def __init__(self, home, data):
        env = dict(os.environ, HOME=home, AGENTICVISOR_DATA=data)
        self.proc = subprocess.Popen([str(BIN), "bridge"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env)
        self.lock = threading.Lock()
        self.next_id = 0

    def call(self, method, target, body=None):
        with self.lock:
            self.next_id += 1
            self.proc.stdin.write(json.dumps({"id": self.next_id, "method": method, "target": target, "body": body}) + "\n")
            self.proc.stdin.flush()
            answer = json.loads(self.proc.stdout.readline())
        return {"status": answer["status"], "body": answer["body"]}

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=10)


def install(page, bridge):
    """Serves the interface and routes the app's invoke("api", ...) to the bridge."""
    def serve(route):
        path = route.request.url.replace(BASE, "").split("#")[0].split("?")[0]
        if path in ("/", "/index.html"):
            file = STATIC / "index.html"
        elif path.startswith("/vendor/") and (STATIC / path.lstrip("/")).is_file():
            file = STATIC / path.lstrip("/")
        else:
            return route.fulfill(status=404, body="not found")
        route.fulfill(status=200, body=file.read_bytes(), content_type=mimetypes.guess_type(str(file))[0] or "text/plain")

    page.route(BASE + "/**", serve)
    page.expose_function("agvInvoke", lambda cmd, args: bridge.call(args["method"], args["target"], args.get("body")))
    page.add_init_script("window.__TAURI__ = { core: { invoke: (cmd, args) => window.agvInvoke(cmd, args) } };")
