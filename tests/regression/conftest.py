"""Live regression checks for an xdebbie cluster, run from a machine on the
mesh (an xdj client such as gram, or a box). Read-only: nothing here changes
the cluster. See README.md.

Fixtures give each test the cluster as xdebbie reports it:

- `vms`: running VMs, from `xdctl vm ls`;
- `hosts`: the boxes, each with its `[network]` read over ssh;
- `here`: this machine's mesh address (10.22.<box>.1 on the WireGuard link).
"""
import json
import tomllib

import pytest

from mesh import UNDERLAY, Host, Vm, run, ssh, xdctl


@pytest.fixture(scope="session")
def vms():
    out = [Vm(v["spec"].get("name") or v["id"], v["host"], v["ip"])
           for v in xdctl("vm", "ls") if v.get("phase") == "Running" and v.get("ip")]
    assert out, "xdctl vm ls: no running VMs"
    return sorted(out, key=lambda v: v.name)


@pytest.fixture(scope="session")
def hosts():
    out = []
    for h in xdctl("host", "ls"):
        host = Host(h["name"], h["name"].split(".")[0])
        cfg = tomllib.loads(ssh(host.ssh, "cat /etc/xdebbie/xdebbied.toml"))
        host.network = cfg.get("network", {})
        out.append(host)
    return sorted(out, key=lambda h: h.name)


@pytest.fixture(scope="session")
def here():
    out = run(["ip", "-4", "-j", "addr", "show", "dev", UNDERLAY])
    addrs = [a["local"] for link in json.loads(out) for a in link.get("addr_info", [])]
    if not addrs:
        pytest.skip(f"no address on {UNDERLAY}: not on the mesh")
    return addrs[0]
