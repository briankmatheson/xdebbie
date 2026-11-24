#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Every key xdebbied.toml accepts must appear, in backticks, in
# docs/reference/configuration.md. The keys are read from the serde structs
# the config deserializes into, so a new field without docs fails here.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
import re, sys

DOC = open("docs/reference/configuration.md", encoding="utf-8").read()
STRUCTS = [
    ("crates/xdebbied/src/config.rs", "Config"),
    ("crates/xdebbied/src/dhcp/model.rs", "NetworkConfig"),
    ("crates/xdebbied/src/dhcp/dnsres.rs", "DnsSource"),
    ("crates/xdebbied/src/mesh/model.rs", "MeshConfig"),
    ("crates/xdebbied/src/mesh/model.rs", "WireguardSpec"),
    ("crates/xdebbied/src/mesh/model.rs", "PeerSpec"),
    ("crates/xdebbied/src/mesh/model.rs", "ClientSpec"),
    ("crates/xdebbied/src/client_config.rs", "ClientConfig"),
    ("crates/xdebbied/src/client_config.rs", "Zone"),
    ("crates/xdebbie-core/src/model.rs", "Host"),
]

def keys(path, name):
    src = open(path, encoding="utf-8").read()
    m = re.search(r"pub struct %s \{\n(.*?)\n\}" % re.escape(name), src, re.S)
    if not m:
        sys.exit(f"lint-config-doc: struct {name} not found in {path}")
    out, attrs = [], ""
    for line in m.group(1).splitlines():
        s = line.strip()
        if not s or s.startswith("//"):
            continue
        f = re.match(r"pub (\w+):", s)
        if not f:
            attrs += s  # an attribute, possibly over several lines
            continue
        if not re.search(r"\bskip\b(?!_)", attrs):
            r = re.search(r'rename = "(\w+)"', attrs)
            out.append(r.group(1) if r else f.group(1))
        attrs = ""
    return out

missing = []
for path, name in STRUCTS:
    for k in keys(path, name):
        # a scalar as `key`; a table as `[key]`, `[[key]]` or `[mesh.key]`
        if not any(f in DOC for f in (f"`{k}`", f"[{k}]", f".{k}]")):
            missing.append(f"{name}.{k} ({path})")
if missing:
    print("keys missing from docs/reference/configuration.md:", file=sys.stderr)
    for m in missing:
        print("  " + m, file=sys.stderr)
    sys.exit(1)
print("lint-config-doc: ok")
PY
