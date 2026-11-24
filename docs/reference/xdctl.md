# xdctl command reference

Every `xdctl` command, its arguments and its defaults.

`xdctl --help` and `xdctl COMMAND --help` print the same from the binary.
Placeholders are in capitals: replace `VM` with a VM's name or id.

## Global options

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--url URL[,URL...]` | `XDEBBIE_URL` | `https://127.0.0.1:23443` | Any xdebbied in the cluster. Several, comma-separated, are tried in order. |
| `--token TOKEN` | `XDEBBIE_TOKEN` | none | The API bearer token. |
| `--ca PATHS` | `XDEBBIE_CA` | `/etc/xdebbie/api-ca.pem,/etc/xdebbie/peers` | CA certificates to trust: comma-separated files, or directories of `*.pem`. |
| `--insecure` | `XDEBBIE_INSECURE` | off | Skip TLS verification. Lab use only. |
| `-j`, `--json` | | off | Print raw JSON. |

`source /etc/xdebbie/xdebbie_env` sets the three environment variables.

**Which box answers.** `xdctl` asks the box at `--url` who the leader is
(`GET /v1/leader`) and sends the command there. It waits up to 20 seconds
for an election to finish. `mesh`, `version` and `image` are the
exceptions: `mesh` and `version` talk to the box at `--url` itself, and
`image` talks to no daemon at all.

## Hosts

| Command | What it does |
|---|---|
| `xdctl host ls` | List hosts. Aliases: `list`, `show`, `get`. |
| `xdctl host describe NAME` | One host's full record (JSON). |
| `xdctl host add NAME --uri URI [options]` | Register or update a host by hand. Options and defaults: `--failure-domain default`, `--pool default`, `--bridge br0`, `--no-control-plane`, `--cpu-overcommit 4`, `--abe URL`, `--reserved-memory-mib 4096`, `--reserved-cpus 2`, `--label KEY=VALUE` (repeatable). Joined boxes register themselves; you rarely need this. |
| `xdctl host rm NAME` | Remove a host. Refused while VMs remain on it. |
| `xdctl host cordon NAME` | No new VMs on the host. |
| `xdctl host uncordon NAME` | Accept new VMs again. |
| `xdctl host drain NAME` | Cordon, then live-migrate every running VM off the host. |
| `xdctl fd` | List failure domains. |

## VMs

| Command | What it does |
|---|---|
| `xdctl vm ls [-l KEY=VALUE,...]` | List VMs, optionally by label selector. Aliases: `list`, `show`, `get`. |
| `xdctl vm describe VM` | One VM's full record (JSON). Takes a name or id. |
| `xdctl vm create NAME --image IMAGE [options]` | Create a VM. See below. |
| `xdctl vm rm VM [--force]` | Delete a VM. `--force` is needed for a VM labelled `xdebbie.io/protected=true`. |
| `xdctl vm start VM` | Power on. |
| `xdctl vm stop VM` | ACPI shutdown. |
| `xdctl vm reboot VM` | Reboot a running VM. |
| `xdctl vm migrate VM [--to HOST]` | Live-migrate, to `HOST` or to the scheduler's choice. |

`xdctl vm create` options:

| Option | Default | Meaning |
|---|---|---|
| `--image IMAGE` | required | Base image name, as it exists on every host (`base-IMAGE` LV or `IMAGE` pool volume). |
| `--cpus N` | `2` | vCPUs. |
| `--memory-mib N` | `4096` | Memory, MiB (at least 256). |
| `--disk-gib N` | `40` | Root disk, GiB. |
| `--data-disk-gib N` | none | A second, blank disk. |
| `--storage local\|abe` | `local` | Where the disks live. See [VM storage](../storage.md). |
| `--ip ADDR` | allocated | Pin an overlay address. Needs `[network]`. |
| `--user-data FILE` | none | cloud-init user-data. |
| `--failure-domain FD` | any | Only hosts in this failure domain. |
| `--group GROUP` | none | Anti-affinity group: spread VMs of a group across hosts. |
| `--strict` | off | Make `--group` a hard rule: never two on one host. |
| `--host HOST` | any | Pin to a host. |
| `--label KEY=VALUE` | none | Owner labels; repeatable. |
| `--dry-run` | off | Show the ranked hosts; create nothing. |

## Joining boxes and the CA

| Command | What it does |
|---|---|
| `xdctl enroll ls [--all]` | Pending enrolments. `--all` adds approved and denied ones. |
| `xdctl enroll approve HOST` | Sign the box's certificate. Takes a hostname or enrolment id. |
| `xdctl approve HOST` | Same as `enroll approve`. |
| `xdctl enroll deny HOST` | Refuse. The box can ask again with `bootstrap --join --force`. |
| `xdctl ca [--chain]` | The cluster CA and its `sha256:` fingerprint. `--chain` prints the chain the box at `--url` serves, leaf first. |
| `xdctl leader` | Which box you asked, and which box leads. |

## Token

| Command | What it does |
|---|---|
| `xdctl token status` | Which tokens the leader accepts and where each box stands. |
| `xdctl token rotate` | Mint a new token and distribute it. See [Rotate the API token](../how-to/rotate-token.md). |

## Mesh

These talk to the box at `--url`, not the leader: the mesh is per-box state.

| Command | What it does |
|---|---|
| `xdctl mesh ls` | Peers, clients and conditions. Aliases: `list`, `show`, `get`. |
| `xdctl mesh describe` | The full `{spec, status}` document (JSON). |
| `xdctl mesh apply -f FILE` | `PUT /v1/mesh` from a JSON file (`-` for stdin). |
| `xdctl mesh peer add BOX [REMOTE] [--pubkey KEY] [--endpoint HOST:PORT] [--probe ADDR]` | Add or replace a peer. With managed WireGuard, omit `REMOTE` and give `--pubkey`. |
| `xdctl mesh peer rm BOX` | Remove a peer. |
| `xdctl mesh client add --pubkey KEY [--name NAME] [--box BOX] [--endpoint HOST:PORT] [--hub-endpoint HOST[:PORT]]` | Register an xdj client and print the `xdj join` line to run on it. |
| `xdctl mesh client rm BOX\|NAME\|PUBKEY` | Remove an xdj client. |
| `xdctl mesh client-config [show]` | What this box tells its xdj clients. |
| `xdctl mesh client-config set [--zone ZONE=SERVER]... [--search A,B] [--welcome-file FILE \| --welcome TEXT \| --clear-welcome]` | Change it. Each option replaces that part. `--zone none` and `--search none` clear. |
| `xdctl mesh wg` | This box's WireGuard public key, port and address, and per-peer handshakes. |
| `xdctl mesh reconcile` | Reconcile now. |
| `xdctl mesh delete [--yes]` | Tear down and forget the API document; `[mesh]` in the config applies again. |

## Network

| Command | What it does |
|---|---|
| `xdctl net ls` | `[network]` settings, DHCP responder state, counters, DNS reservations. Aliases: `list`, `show`, `get`. |
| `xdctl net seen` | MACs heard on the bridge and what each was told. |

## Volumes

Standalone ABE volumes. See [VM storage](../storage.md).

| Command | What it does |
|---|---|
| `xdctl volume ls` | List volumes. Aliases: `list`, `show`, `get`. |
| `xdctl volume describe ID` | One volume (JSON). |
| `xdctl volume create NAME --gib N [--host HOST]` | Create a blank volume. |
| `xdctl volume attach ID VM [--force]` | Plug it into a VM. `--force` takes it from a running VM that still has it. |
| `xdctl volume detach ID` | Unplug it. |
| `xdctl volume rm ID` | Delete it. Refused while attached. |

## Images

Runs on the host itself, as root. Never contacts xdebbied.

| Command | What it does |
|---|---|
| `xdctl image import FILE [options]` | Copy a disk image into the thin pool as `base-NAME` and trim its free space. |

Options: `--name NAME` (default: the file name without its extension),
`--format raw|qcow2` (default `raw`; never guessed), `--vg VG` and
`--pool POOL` (default: `volume_group` and `thin_pool` from the config),
`--config PATH` (default `/etc/xdebbie/xdebbied.toml`), `--force`
(replace an existing base), `--no-sparsify` (skip `virt-sparsify`).

## Other

| Command | What it does |
|---|---|
| `xdctl version` | xdctl's version, and the daemon's on the box asked and on the leader. |
| `xdctl completion SHELL` | Shell completion script for `bash`, `zsh`, `fish`, `elvish` or `powershell`. |
