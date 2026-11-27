"""Helpers the checks share: the cluster as xdctl reports it, ssh, probes."""
import ipaddress
import json
import os
import socket
import subprocess
from dataclasses import dataclass, field

# xdctl tries each in turn: the environment's first, then the hub's bridge
# (a stale XDEBBIE_URL, e.g. a box that left, still works).
URL = ",".join(u for u in (os.environ.get("XDEBBIE_URL"), "https://10.23.0.1:23443") if u)
HUB = os.environ.get("XDEBBIE_HUB", "timesink")        # the box xdj clients peer with
UNDERLAY = os.environ.get("XDEBBIE_UNDERLAY", "wg-flock")


def run(argv, timeout=20):
    """stdout of a command; a failure raises with its stderr."""
    r = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    if r.returncode:
        raise RuntimeError(f"{' '.join(argv)}: {r.stderr.strip() or r.returncode}")
    return r.stdout


def xdctl(*args):
    return json.loads(run(["xdctl", "--url", URL, "-j", *args]))


def ssh(host, command, timeout=15):
    """Run `command` on `host` as the current user (no sudo, no prompts)."""
    return run(["ssh", "-n", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", host, command], timeout)


def tcp_reaches(ip, port, timeout=3.0):
    """True when the address answers at all: a connection or a refusal
    (RST) both prove the path works both ways; only silence fails."""
    try:
        socket.create_connection((ip, port), timeout).close()
        return True
    except ConnectionRefusedError:
        return True
    except OSError:
        return False


@dataclass
class Host:
    name: str                    # as xdebbie knows it (FQDN)
    ssh: str                     # what `ssh` is given
    network: dict = field(default_factory=dict)

    @property
    def short(self):
        return self.name.split(".")[0]

    @property
    def gateway(self):
        return ipaddress.ip_address(self.network["gateway"])


@dataclass
class Vm:
    name: str
    host: str
    ip: str
