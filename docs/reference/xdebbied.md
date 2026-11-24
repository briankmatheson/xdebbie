# xdebbied command reference

The daemon's subcommands and options.

```text
xdebbied [-c PATH] [COMMAND]
```

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `-c`, `--config PATH` | `XDEBBIED_CONFIG` | `/etc/xdebbie/xdebbied.toml` | The [configuration file](configuration.md). It goes before the subcommand: `xdebbied -c PATH serve`. |
| `-V`, `--version` | | | Print the version. |

Log verbosity follows `RUST_LOG` (default `info`), for example
`RUST_LOG=debug`.

With no subcommand, `xdebbied` runs `serve`.

## `xdebbied serve`

Run the API, discovery, the mesh, the DHCP responder and, on the leader,
the scheduler and reconciler. This is what `xdebbied.service` runs.

| Option | Meaning |
|---|---|
| `--bootstrap` | Run `bootstrap` first. The service does this with `ExecStartPre` instead. |
| `--mesh-only` | Run only the API and the mesh: no libvirt, no LVM, no discovery. Used by `tests/mesh/`. |

A configuration, state or token file that does not load makes `serve` exit
with status 78. The unit does not restart on 78, so the error stays in
`systemctl status xdebbied`.

## `xdebbied bootstrap`

Prepare this box and write its configuration. Safe to run again: it creates
only what is missing. The package runs it on first install and the service
runs it before every start. See [Bootstrap](../bootstrap.md) for each step.

| Option | Default | Meaning |
|---|---|---|
| `--join URL` | | Join the cluster whose CA box is at `URL`, for example `https://box0.example.org:23443`. Plain `http://` is refused. |
| `--ca-fingerprint sha256:HEX` | | The CA fingerprint you expect (from `xdctl ca` on the cluster). Without it you are asked to confirm. Needs `--join`. |
| `--standalone` | | Start a new cluster without listening for one. Conflicts with `--join`. |
| `--discover-secs SECS` | `20` | Without `--join` or `--standalone`: how long to listen for a running cluster. `0` does not listen. |
| `--force` | | Regenerate the configuration, backing the old one up. Your settings are carried over (see [Configuration](configuration.md)). With `--join`, start a new enrolment with a new key. |
| `--box N` | assigned | Mesh box id to ask for, 0-253. The CA box assigns one at approval; with `--join` this is only a wish. |
| `--subnet CIDR` | `10.23.0.0/16` | The overlay subnet. Box N's bridge is `.N.1` of it (`10.23.N.1` by default). |
| `--network NAME` | `xdebbie` | libvirt network to create. |
| `--pool NAME` | `xdebbie` | libvirt storage pool to create. |
| `--pool-path PATH` | `/var/lib/libvirt/xdebbie` | Directory for that pool. |
| `--uri URI` | `qemu:///system` | libvirt URI. |

## `xdebbied check`

Try to reach every host in the configuration over libvirt and print `ok` or
the error for each.
