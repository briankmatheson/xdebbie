#!/usr/bin/env python3
"""Stand-in for abe 0.4.0's HTTP API (see abe/README.md): volumes are plain
files under $FAKE_ABE_DIR so the test can inspect what xdebbied did.

  GET /version                         -> {version, features} (404 if FAKE_ABE_LEGACY=1)
  GET /configure?bytes=N&raw=true      -> Message {message,id,port,traddr,nqn,bytes,existing}
      &name=<n>                        -> same volume again with existing:true
      ?base=<image>&bytes=N            -> Message + base, sealed (port "" once sealed)
  GET /id/<id>                         -> Message (404 unknown, 409 sealed base)
  DELETE /id/<id>                      -> 200 / 404
  GET /seal/<id>                       -> {id, base, sealed, bytes}
  GET /clone/<id>?bytes=N&name=<n>     -> Message + bytes, origin, layers
  GET /volumes                         -> [{id,bytes,port,traddr,nqn,origin,layers,base,sealed,name}]
  GET /utilization                     -> like abe (thin pool of FAKE_ABE_POOL_BYTES, default 1 TiB)
  GET /_calls                          -> [[method, path], ...]    (test hook)
  GET /_move/<id>                      -> re-export <id> on a new port (test hook)

Knobs: FAKE_ABE_FAIL_FIRST=1 fails the first /configure with 500;
FAKE_ABE_LEGACY=1 behaves like abe < 0.4 (no /version, name/base ignored);
FAKE_ABE_PORT_MAX=N returns 503 once ports 44001..N are all taken;
FAKE_ABE_DIE_AFTER=seal exits the process right after the first /seal reply;
FAKE_ABE_DELETE_FAIL_ONCE=1 fails the first DELETE with 500."""
import json, os, shutil, sys, threading, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

DIR = os.environ.get("FAKE_ABE_DIR", "/tmp/fake-abe")
TRADDR = os.environ.get("FAKE_ABE_TRADDR", "10.22.0.1")
LEGACY = os.environ.get("FAKE_ABE_LEGACY") == "1"
PORT_MIN = 44001
PORT_MAX = int(os.environ.get("FAKE_ABE_PORT_MAX", "44999"))
POOL_BYTES = int(os.environ.get("FAKE_ABE_POOL_BYTES", str(1 << 40)))
DIE_AFTER = os.environ.get("FAKE_ABE_DIE_AFTER", "")
state = {
    "fail_first": os.environ.get("FAKE_ABE_FAIL_FIRST") == "1",
    "delete_fail_once": os.environ.get("FAKE_ABE_DELETE_FAIL_ONCE") == "1",
    "calls": [],
}
LOCK = threading.Lock()
os.makedirs(DIR, exist_ok=True)

def meta_path(i): return os.path.join(DIR, i + ".json")
def img_path(i): return os.path.join(DIR, i + ".img")
def load(i):
    try: return json.load(open(meta_path(i)))
    except (FileNotFoundError, ValueError): return None
def save(m): json.dump(m, open(meta_path(m["id"]), "w"))
def volumes(): return [m for m in (load(f[:-5]) for f in os.listdir(DIR) if f.endswith(".json")) if m]
def find(key, val):
    for m in volumes():
        if m.get(key) == val: return m
    return None
def free_port():
    taken = {int(m["port"]) for m in volumes() if m.get("port")}
    for p in range(PORT_MIN, PORT_MAX + 1):
        if p not in taken: return str(p)
    return None
def message(m):
    port = m.get("port", "")
    msg = f"sudo nvme connect -a {TRADDR} -t tcp -s {port} -n {m['id']}" if port else f"{m['id']} is a sealed base; clone it"
    out = {"message": msg, "id": m["id"], "port": port, "traddr": TRADDR, "nqn": m["id"],
           "bytes": m["bytes"], "existing": m.get("existing", False)}
    if m.get("base"):
        out["base"] = m["base"]; out["sealed"] = m.get("sealed", False)
    return out
def new_volume(size, raw, name=None, base=None):
    port = free_port()
    if port is None: return None
    i = uuid.uuid4().hex
    m = {"id": i, "bytes": size, "port": port, "traddr": TRADDR, "nqn": i, "raw": raw,
         "name": name, "base": base, "sealed": False, "origin": None, "layers": 0}
    save(m)
    open(img_path(i), "wb").truncate(size)
    return m

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        with LOCK: self.get_locked()
    def get_locked(self):
        u = urlparse(self.path); q = parse_qs(u.query)
        state["calls"].append(["GET", self.path])
        if u.path == "/version":
            if LEGACY: return self.send(404, {"error": "no route"})
            return self.send(200, {"version": "0.4.0", "features": ["reexport", "ports", "name", "trim", "base", "clone"]})
        if u.path == "/configure":
            if state["fail_first"]:
                state["fail_first"] = False
                return self.send(500, {"error": "lvcreate failed: simulated"})
            size = int(q.get("bytes", ["0"])[0]); raw = q.get("raw", ["false"])[0] == "true"
            name = None if LEGACY else q.get("name", [None])[0]
            base = None if LEGACY else q.get("base", [None])[0]
            if base:
                m = find("base", base)
                if m:
                    m["existing"] = True
                    return self.send(200, message(m))
                raw = True
            elif name:
                m = find("name", name)
                if m:
                    m["existing"] = True
                    return self.send(200, message(m))
            m = new_volume(size, raw, name=name, base=base)
            if m is None: return self.send(503, {"error": f"no free port in {PORT_MIN}..{PORT_MAX}"})
            return self.send(200, message(m))
        if u.path.startswith("/id/"):
            m = load(u.path[4:])
            if not m: return self.send(404, {"error": "no volume"})
            if m.get("sealed"): return self.send(409, {"error": f"{m['id']} is a sealed base; clone it"})
            if not m.get("port"):
                m["port"] = free_port() or ""; save(m)
            return self.send(200, message(m))
        if u.path.startswith("/seal/"):
            m = load(u.path[6:])
            if not m: return self.send(404, {"error": "no volume"})
            if not m.get("base"): return self.send(400, {"error": f"{m['id']} is not a base volume"})
            m["sealed"] = True; m["port"] = ""; save(m)
            self.send(200, {"id": m["id"], "base": m["base"], "sealed": True, "bytes": m["bytes"]})
            if DIE_AFTER == "seal":
                self.wfile.flush(); os._exit(0)
            return
        if u.path.startswith("/clone/"):
            origin = u.path[7:]
            name = q.get("name", [None])[0]
            if name:
                m = find("name", name)
                if m:
                    m["existing"] = True
                    return self.send(200, {**message(m), "bytes": m["bytes"], "origin": m.get("origin") or origin, "layers": m.get("layers", 1)})
            src = load(origin)
            if not src: return self.send(404, {"error": "no volume"})
            if src.get("base") and not src.get("sealed"):
                return self.send(409, {"error": f"base {origin} is not sealed yet"})
            size = int(q.get("bytes", [str(src["bytes"])])[0])
            if size < src["bytes"]:
                return self.send(400, {"error": f"a clone cannot be smaller than its origin ({size} < {src['bytes']} bytes)"})
            m = new_volume(size, True, name=name)
            if m is None: return self.send(503, {"error": f"no free port in {PORT_MIN}..{PORT_MAX}"})
            shutil.copyfile(img_path(origin), img_path(m["id"]))
            open(img_path(m["id"]), "r+b").truncate(size)
            m["origin"] = origin; m["layers"] = src.get("layers", 0) + 1; save(m)
            return self.send(200, {**message(m), "bytes": size, "origin": origin, "layers": m["layers"]})
        if u.path == "/volumes":
            return self.send(200, [{"id": m["id"], "bytes": m["bytes"], "port": m.get("port") or None,
                                    "traddr": TRADDR, "nqn": m["id"], "origin": m.get("origin"),
                                    "layers": m.get("layers", 0), "base": m.get("base"),
                                    "sealed": m.get("sealed", False), "name": m.get("name")} for m in volumes()])
        if u.path == "/utilization":
            vols = volumes()
            prov = sum(m["bytes"] for m in vols)
            used = sum(os.stat(img_path(m["id"])).st_blocks * 512 for m in vols if os.path.exists(img_path(m["id"])))
            return self.send(200, {"vg": "fake", "thin_pool": "pool0", "bytes": POOL_BYTES, "used_bytes": used,
                                   "free_bytes": max(POOL_BYTES - used, 0), "data_percent": 100.0 * used / POOL_BYTES,
                                   "metadata_percent": 1.0, "metadata_bytes": 1 << 24, "provisioned_bytes": prov,
                                   "vg_bytes": POOL_BYTES + (1 << 30), "vg_free_bytes": 1 << 30,
                                   "volumes": len(vols), "max_layers": max([m.get("layers", 0) for m in vols] + [0]),
                                   "warnings": []})
        if u.path == "/_calls":
            return self.send(200, state["calls"])
        if u.path.startswith("/_move/"):
            m = load(u.path[7:])
            if not m: return self.send(404, {"error": "no volume"})
            taken = [int(v["port"]) for v in volumes() if v.get("port")]
            m["port"] = str(max(taken + [PORT_MIN - 1]) + 1); save(m)
            return self.send(200, message(m))
        self.send(404, {"error": "no route"})
    def do_DELETE(self):
        with LOCK:
            u = urlparse(self.path); state["calls"].append(["DELETE", u.path])
            if u.path.startswith("/id/"):
                i = u.path[4:]
                if not load(i): return self.send(404, {"error": "no volume"})
                if state["delete_fail_once"]:
                    state["delete_fail_once"] = False
                    return self.send(500, {"error": "lvremove failed: simulated"})
                os.remove(meta_path(i)); os.remove(img_path(i))
                return self.send(200, {"message": f"deleted {i}", "id": i, "port": "", "traddr": TRADDR, "nqn": i, "existing": False})
            self.send(404, {"error": "no route"})

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
