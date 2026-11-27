# Regression checks (live)

Read-only checks against a running cluster, each one a bug that happened.
Run from a machine on the mesh (an xdj client such as gram, or a box) with
`xdctl`, `dig` and ssh access (no sudo) to the boxes:

```sh
cd tests/regression
pytest                          # or: pytest test_reach.py -k answers
```

| file | checks | since |
|---|---|---|
| `test_reach.py` | every running VM answers from here, on any box | 2026-11-26, box-5 VMs unreachable from gram |
| `test_routes.py` | boxes other than the hub route clients back via the hub | the same |
| `test_dns.py` | each box's DHCP DNS is not a reserved address, and answers | 2026-11-26, box 5 handed out 10.23.0.11 |
| `test_egress.py` | VMs reach the internet (those we can ssh into) | 2026-11-26, VMs outside their box's /24 not NATed |

Environment: `XDEBBIE_URL` (tried first; then `https://10.23.0.1:23443`),
`XDEBBIE_HUB` (the box xdj clients peer with, default `timesink`),
`XDEBBIE_UNDERLAY` (default `wg-flock`).

**Adding a check:** a new `test_<area>.py`, a docstring naming the date and
what broke, and the shared fixtures (`vms`, `hosts`, `here`, in
`conftest.py`) and helpers (`mesh.py`: `xdctl`, `ssh`, `tcp_reaches`).
Keep it read-only.
