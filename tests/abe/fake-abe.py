#!/usr/bin/env python3
"""Minimal stand-in for the abe daemon's HTTP API (see abe/README.md): volumes
are plain files under $FAKE_ABE_DIR so the test can inspect what xdebbied did.
  GET /configure?bytes=N&raw=true  -> {id, port, traddr, nqn}
  GET /id/<id>                     -> same (404 if unknown)
  DELETE /id/<id>                  -> 200 / 404
  GET /volumes                     -> [{id, bytes, port, traddr, nqn}]
Optionally fails the first /configure (FAKE_ABE_FAIL_FIRST=1) to exercise retry."""
import json, os, sys, uuid
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

DIR = os.environ.get("FAKE_ABE_DIR", "/tmp/fake-abe")
TRADDR = os.environ.get("FAKE_ABE_TRADDR", "10.22.0.1")
state = {"next_port": 44001, "fail_first": os.environ.get("FAKE_ABE_FAIL_FIRST") == "1", "calls": []}
os.makedirs(DIR, exist_ok=True)

def meta_path(i): return os.path.join(DIR, i + ".json")
def load(i):
    try: return json.load(open(meta_path(i)))
    except FileNotFoundError: return None

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        u = urlparse(self.path); q = parse_qs(u.query)
        state["calls"].append(("GET", u.path))
        if u.path == "/configure":
            if state["fail_first"]:
                state["fail_first"] = False
                return self.send(500, {"error": "lvcreate failed: simulated"})
            i = uuid.uuid4().hex
            size = int(q.get("bytes", ["0"])[0]); raw = q.get("raw", ["false"])[0] == "true"
            port = str(state["next_port"]); state["next_port"] += 1
            m = {"id": i, "bytes": size, "port": port, "traddr": TRADDR, "nqn": i, "raw": raw}
            json.dump(m, open(meta_path(i), "w"))
            open(os.path.join(DIR, i + ".img"), "wb").truncate(size)
            return self.send(200, {"message": f"sudo nvme connect -a {TRADDR} -t tcp -s {port} -n {i}", **{k: m[k] for k in ("id","port","traddr","nqn")}})
        if u.path.startswith("/id/"):
            m = load(u.path[4:])
            if not m: return self.send(404, {"error": "no volume"})
            return self.send(200, {"message": "", **{k: m[k] for k in ("id","port","traddr","nqn")}})
        if u.path == "/volumes":
            vols = [load(f[:-5]) for f in os.listdir(DIR) if f.endswith(".json")]
            return self.send(200, [{k: m[k] for k in ("id","bytes","port","traddr","nqn")} for m in vols])
        self.send(404, {"error": "no route"})
    def do_DELETE(self):
        u = urlparse(self.path); state["calls"].append(("DELETE", u.path))
        if u.path.startswith("/id/"):
            i = u.path[4:]
            if not load(i): return self.send(404, {"error": "no volume"})
            os.remove(meta_path(i)); os.remove(os.path.join(DIR, i + ".img"))
            return self.send(200, {"message": f"deleted {i}", "id": i, "port": "", "traddr": TRADDR, "nqn": i})
        self.send(404, {"error": "no route"})

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    HTTPServer(("127.0.0.1", port), H).serve_forever()
