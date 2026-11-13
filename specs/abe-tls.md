# ABE 0.5.0 / xdebbie 0.16.0: mTLS on the ABE API, nvmet ports fenced to the caller

Status: ready to build. Baseline: abe `7ade21e`+ (0.4.0, e2e green on gram), xdebbie main `7152883`+ (0.15.0-2), both installed on gram and timesink.

## 0. Read this first

### The two changes

1. **ABE's HTTP API moves to HTTPS with mutual TLS.** ABE serves with the box's cert and only accepts clients whose cert chains to the cluster CA. On these boxes that CA is xdebbie's (`/etc/xdebbie/api-ca.pem`; every box's `api-cert.pem` already carries serverAuth + clientAuth EKU). ABE itself stays generic: three PEM paths in `/etc/default/abe`.
2. **Each nvmet port is fenced to the IP that asked for it.** Linux NVMe/TCP TLS is PSK-only (no x.509 at either end), and the wg underlay already encrypts gram↔timesink, so instead ABE keeps an nftables set of `(client ip, port)` pairs and rejects everything else in its port range. The client IP is taken from the TLS connection that created/attached the volume — never from a parameter — so opening a port to an address requires holding a cert *and* connecting from that address. This is sound because xdebbied's `nvme connect` is always local to the box that called ABE (`attach_volume` refuses remote hosts), and both go out over the same wg address.

### Order of work

ABE first (it is self-contained and testable with curl), then xdebbie. The xdebbie change is small: build the ABE client from `Config::http_client()`, accept `https://` in host records, and bump the fake/test harness.

### Working rules (same as before)

- Keep the single-file ABE layout. New code goes in `src/main.rs` next to what it touches; a `tls.rs`/`fence.rs` split is fine if it stays under ~300 lines each and `main.rs` just `mod`s them.
- rustfmt-clean on touched files; `cargo test` green; `tests/lint-links.sh` green.
- `make e2e` on gram is the acceptance test for ABE; `tests/abe/run.sh` (unprivileged) for xdebbie. Both must pass before packaging.
- Don't touch the 0.4.0 features (ports, reexport, trim, bases, name). This is additive.
- Conventional commits, one concern per commit, CHANGELOG entries.

### Out of scope (don't build)

- Kernel NVMe/TCP TLS, DH-HMAC-CHAP, PSKs, tlshd. Not now.
- nvmet `allowed_hosts` by hostnqn (unverifiable without auth; the IP fence is the control).
- IPv6 for the nvmet fence (traddr is v4; v6 to the port range is rejected outright).
- A client-nominated address (`?client=`). If remote attach ever lands in xdebbie it can be added then, gated on the cert.
- Cert generation inside ABE. Standalone installs use openssl (README shows the five commands); on xdebbie boxes the certs already exist.

## 1. Background

### ABE today (0.4.0)

- `axum::serve(TcpListener, app)` on `ABE_LISTEN` (default `0.0.0.0:80`), plain HTTP, no auth. `main()` at the bottom of `src/main.rs`.
- Exports live in `/sys/kernel/config/nvmet`; `export(id, port)` creates subsystem + port and links them; `remove_export(id)` unlinks and drops an empty port. Both run under the `OPS` mutex via `with_ops`.
- Durable per-volume metadata is LV tags: `abe_port=`, `abe_name=`, `abe_base=`, `abe_sealed`. Helpers `tag_value`, `has_tag`, `retag`, `set_port_tag`, `valid_tag_value`.
- `reexport_locked()` walks LVs with `abe_port` tags at startup and on `GET /reexport`, re-creating missing exports; summary in `/utilization.reexport`.
- `/version` lists features: `reexport, ports, name, trim, base, clone`.
- `tests/e2e.sh` (root, real nvmet, 9 scenarios) reads `ABE_LISTEN` from `/etc/default/abe` to build the URL; uses curl + jq + nvme-cli + lvs.

### xdebbie today (0.15.0)

- `crates/xdebbied/src/abe.rs`: `Client::new()` builds a bare `reqwest::Client` with a 120 s timeout. Constructed in `main.rs:233,446` and `api/mod.rs:1320,1362`.
- `Config::http_client(timeout)` (`config.rs:283`) already builds a reqwest client with `ca_file` + every PEM in `peer_ca_dir` as roots and the box's `tls_cert`/`tls_key` as identity. Used for leader forwarding.
- `LibvirtDriver::refresh_endpoints` (`libvirt.rs:913`) calls `abe.export(server, id)` (`GET /id/<id>`) for every disk not currently connected, before `nvme connect`. The reconciler runs it on every start/attach path. **This is what sets the fence to the right box after any restart; no new call sites are needed.**
- Host records carry `abe = "http://10.22.x.1:80"` (`[[hosts]]` in `xdebbied.toml`, `PUT /v1/hosts/<name>`).
- `tests/abe/fake-abe.py` is a plain-HTTP fake ABE used by `tests/abe/run.sh`.

### The boxes

- gram: kernel 7.3.3, nft 1.1.7, ABE on `10.22.1.1` (wg-flock), xdebbie cert SANs include `10.22.1.1`.
- timesink: kernel 6.12, nft 1.1.3, ABE on `10.22.0.1`, leader, node0 running with an attached ABE volume (`pvc-9e8b…`). **Verify its cert SAN includes `10.22.0.1` before rollout** (`openssl x509 -in /etc/xdebbie/api-cert.pem -noout -ext subjectAltName`).
- xdebbie already owns an nft table for wg-flock (`mesh/firewall.rs`). ABE's table is separate; base chains on the same hook all run, so neither bypasses the other.

## 2. ABE 0.5.0

### 2.1 API over mTLS

Config (`/etc/default/abe`, all read via `var()` like the rest):

| var | default | meaning |
|---|---|---|
| `ABE_TLS` | `on` | `off` = plain HTTP (for the unprivileged test harness and standalone experiments). Logged at `warn!` on every start. |
| `ABE_TLS_CERT` | `/etc/abe/cert.pem` | server cert (PEM, leaf first, chain optional) |
| `ABE_TLS_KEY` | `/etc/abe/key.pem` | private key (PKCS#8 or RSA/EC PEM) |
| `ABE_TLS_CA` | `/etc/abe/ca.pem` | CA bundle client certs must chain to |
| `ABE_LISTEN` | `0.0.0.0:443` (was `:80`) | unchanged meaning |

Implementation:

- Dependencies: `axum-server = { version = "0.7", features = ["tls-rustls"] }`, `rustls = "0.23"` (default provider is fine), `rustls-pemfile = "2"`. Pin to whatever xdebbie uses if the versions clash with axum 0.7; xdebbie is on axum-server 0.8 / rustls 0.23.
- `fn tls_config() -> Result<Arc<rustls::ServerConfig>>`: load CA into a `RootCertStore`; `WebPkiClientVerifier::builder(roots).build()` (no `allow_unauthenticated`); `ServerConfig::builder().with_client_cert_verifier(v).with_single_cert(chain, key)`. Any load error is fatal at startup with a message naming the path (`exit(3)`).
- Serve with `axum_server::bind_rustls(addr, RustlsConfig::from_config(cfg)).serve(app.into_make_service_with_connect_info::<SocketAddr>())`. With `ABE_TLS=off`, `axum_server::bind(addr)` with the same make-service, so handlers see `ConnectInfo` in both modes.
- Print `Listening on https://…` (or `http://` when off) in the existing interface loop.
- `/version` gains features `tls` and `fence`. `Version` also gets `"tls": bool` so a client can tell at a glance.
- Nothing in the API changes shape for TLS. A client without a valid cert never reaches a handler (rustls aborts the handshake), which is the point.

### 2.2 The nvmet fence (nftables)

Owned table, replaced atomically at startup and on `GET /reexport`:

```
destroy table inet abe
table inet abe {
    set allowed {
        type ipv4_addr . inet_service
        comment "client ip . nvmet port opened by abe"
    }
    chain input {
        type filter hook input priority filter - 10; policy accept;
        tcp dport $MIN-$MAX  ip saddr . tcp dport @allowed  accept
        tcp dport $MIN-$MAX  reject with tcp reset
        meta nfproto ipv6  tcp dport $MIN-$MAX  reject with tcp reset
    }
}
```

Fed to `nft -f -` as one transaction. `$MIN-$MAX` is `port_range()`. `destroy` (not `delete`) so a missing table is not an error (nft ≥ 1.0.7; both boxes qualify). Note `accept` here only ends *this* chain; other base chains on `input` (xdebbie's) still run, so this never widens anything.

- `fn fence_install() -> Result<()>`: the script above. Called in `main()` **after** `ensure_nvmet_present()` and **before** `reexport_at_startup()`, so no port is ever open without the fence. Failure is fatal (`exit(4)`) with the nft stderr in the message. `ABE_NFT=/usr/sbin/nft` overrides the binary; search `nft` on PATH then `/usr/sbin/nft`.
- `fn fence_open(ip: Ipv4Addr, port: u16) -> Result<()>`: `fence_close_port(port)` then `nft add element inet abe allowed { <ip> . <port> }`.
- `fn fence_close_port(port: u16) -> Result<()>`: `nft -j list set inet abe allowed`, delete every element whose port matches. (JSON: `.nftables[].set.elem[] | .concat == [ip, port]`.) Tolerates a missing element.
- `fn fence_list() -> Result<BTreeMap<u16, Ipv4Addr>>` for `/volumes` and tests.
- All three run under `OPS` with the export they belong to (they are `Command` calls, so inside the same `with_ops` closure as `export`/`remove_export`).
- Ordering on export: configfs export first, then open. On failure to open: `remove_export` and fail the request (a volume that exists but nobody can reach is worse than a 500). Ordering on remove: close first, then unexport.

### 2.3 The client tag, and adopting 0.4.0 exports

- New tag `abe_client=<ipv4>` (`TAG_CLIENT`). Written by every path that opens a port: `/configure` (new and `existing: true`), `GET /id/<id>`, `/clone`. Replaced, not appended (`retag` with the old value in `del`). Removed by `seal` (base goes unexported) and gone with the LV on delete.
- `reexport_one` reads the tag and opens the port to it after exporting. If the tag is missing (a volume exported by 0.4.0):
  1. look for an **established TCP connection to that port** in `/proc/net/tcp` (st `01`, local port matches; remote address is the client — parse the little-endian hex). If exactly one peer, adopt it: open the fence to it, write the tag, log `adopted <ip> for <id> on <port>`;
  2. if none or several, leave the port closed and count it in `ReexportSummary.closed: Vec<String>` (ids) and `/utilization.warnings`: `"N volume(s) exported with no client; closed until attached again"`. xdebbie's next `refresh_endpoints` will `GET /id` and open it.
- `GET /reexport` re-installs the table (2.2) and re-opens every tagged port from scratch — this is the "fix the firewall" button.
- `Volume` gets `client: Option<String>`; `Message` gets `client` too (the address the port was opened to, i.e. the caller's).
- `valid_tag_value` already accepts `.` and digits; add a unit test that `abe_client=10.22.0.1` round-trips through `retag`/`tag_value`.

### 2.4 Handler changes

- Handlers that export take `ConnectInfo(peer): ConnectInfo<SocketAddr>`. Only `peer.ip()` as `Ipv4Addr` is used; an IPv6 peer (including v4-mapped — unmap it first) is a 400 `"fence needs an IPv4 client address"`.
- Pass the client into the `with_ops` closures: `create_and_export(id, size, raw, tags, client)`, `export_new`, `export_or_existing`, `existing_named`, `existing_base`, `clone_locked`. Keep signatures flat (an extra `Ipv4Addr` arg), no global.
- `detach`/`DELETE` and `seal` do not check the caller against the tag. The cert is the authority for destroy; the fence is only about who can *read the bytes*.
- Log lines: `exporting <id> on <port> for <ip>`, `closing <port>`, `adopted …`.

### 2.5 Packaging and config

- `Cargo.toml` version `0.5.0`. `debian/abe.default` documents the five new vars; `ABE_LISTEN` example becomes `:443`.
- `debian/abe.service`: add `After=nftables.service` and `Wants=` nothing (nft the binary is enough). Add `Requires=`/`After=` for `xdebbied.service` **only** as the already-planned underlay drop-in follow-up, not here.
- `cargo-deb` `depends` gains `nftables`. Keep `conflicts = "abe"`.
- README: a "TLS" section (vars, the openssl five-liner for a standalone CA + server cert + client cert, `curl --cert --key --cacert` example) and a "Port fence" section (the table, what gets adopted, `nft list table inet abe` to inspect, `GET /reexport` to rebuild). SECURITY_AUDIT.md: strike the "no authentication" finding, add the fence, and note what the fence does *not* do (spoofed source IPs on an untrusted L2 — fine behind wg, not on a flat LAN). The postinst "installed without authentication" banner goes.
- `Makefile`: `deploy`/`install` unchanged (they curl `/version`); the curl needs `--cacert`/`--cert`/`--key`, read from `/etc/default/abe` on the target. Add `CURL_TLS` derivation to `tests/e2e.sh` (below) and reuse it in the Makefile's verification line.

### 2.6 ABE tests

Unit (no root):

- `fence_script_for_range(44001, 44999)` golden string.
- `parse_nft_set_json` on a captured `nft -j list set` output with two elements → map.
- `peers_on_port` on a canned `/proc/net/tcp` body (one established, one TIME_WAIT, one other port) → exactly one peer, correctly byte-swapped.
- `tls_config` on `rcgen`-free fixtures: check in a tiny test CA/server/client PEM under `tests/certs/` (generated once with openssl, 10-year validity, committed) and assert load succeeds and that a wrong CA fails. Keep the keys obviously test-only.
- `Version` JSON includes `tls` and `fence`.

`tests/e2e.sh` (root):

- URL/scheme: read `ABE_TLS`, `ABE_TLS_CERT/KEY/CA`, `ABE_LISTEN` from `/etc/default/abe`; when TLS is on, `CURL="curl --cacert $CA --cert $CERT --key $KEY"` and `https://`. Every existing `curl -sf` goes through `$CURL`. The script is also the model for anyone scripting ABE.
- New scenarios, after the existing nine:
  10. **no cert → refused**: `curl --cacert $CA https://…/version` (no client cert) exits non-zero, no HTTP status; `curl -k` without a cert also fails. With `ABE_TLS=off` these are `skip`.
  11. **fence basic**: after `configure`, `nft list set inet abe allowed` contains `<my ip> . <port>`; after `DELETE`, it does not.
  12. **fence negative (fully local)**: pick a second local IPv4 that is not the ABE address (first non-loopback, non-ABE address from `ip -4 -o addr`; on gram that is the overlay `10.23.0.2`; `skip` if none). `configure` via `$CURL --interface $ALT` → the set contains `$ALT . port`. `nvme connect` from the default source (ABE's own address) → **fails** within `--ctrl-loss-tmo 2`; `nvme connect --host-traddr $ALT` → succeeds, device appears, write/read ok. Then `GET /id/<id>` via the default source → set now has `<abe ip> . port`, and the existing controller from `$ALT` keeps working only until it reconnects (don't assert that; just disconnect and check the new source can connect).
  13. **adopt on re-export**: create via default source, connect; strip the tag (`lvchange --deltag abe_client=… abe/<id>`); `systemctl restart abe`; `/utilization.reexport.closed` is empty, `/volumes[].client` is our IP again, the connection still works (write/read). Then: create a second volume, strip its tag, **don't** connect, restart → it is in `reexport.closed`, port rejects, `GET /id` reopens it.
  14. **reexport rebuilds the fence**: `nft destroy table inet abe` by hand; `GET /reexport`; table is back with every exported port's element.
- `tests/nvmet-wipe.sh` additionally `nft destroy table inet abe` under `--all`.

## 3. xdebbie 0.16.0

### 3.1 ABE client uses the cluster identity

- `abe::Client::new()` → `abe::Client::with_http(reqwest::Client)`; the four construction sites pass `cfg.http_client(Duration::from_secs(120))`. Keep `new()` for tests (bare client), marked `#[cfg(test)]` or `pub(crate)`.
- `Config::http_client` is already right (roots = `ca_file` + `peer_ca_dir`, identity = this box's cert/key). One addition: `.use_rustls_tls()` is there; make sure `http://` still works through it (it does — reqwest only applies TLS to https URLs), so `fake-abe.py` needs no change.
- Error mapping: a TLS handshake failure (`reqwest::Error::is_connect()` with a rustls source) surfaces as `AbeHttpError` with message `tls: <err>` so `xdctl host ls` shows something better than a timeout. No new variant.
- `Disk`/`Volume` learn nothing new. `Message.client` is ignored (serde default).

### 3.2 Host records and config

- `[[hosts]].abe` accepts `https://`; docs and `bootstrap --join` examples switch to `https://10.22.x.1:443`. No scheme-rewriting: an operator who still runs `http://` gets http.
- `xdctl host ls` ABE column: unchanged. `xdctl host show <name>` prints the abe URL with `tls` when the probed `/version` says `"tls": true`.
- Follow-up from the previous milestone stays a follow-up: `bootstrap --join` carrying `abe`.

### 3.3 xdebbie tests

- `tests/abe/run.sh`: unchanged behaviour (fake-abe over http). Add one case: the daemon started with `tls_cert`/`tls_key`/`ca_file` pointing at the test-generated localhost certs (`bootstrap::api_tls::generate_localhost` already exists for the API tests) still talks to the http fake — proves the identity-bearing client doesn't break plain URLs.
- Unit: `Client::with_http` is used by the real constructors (a test that builds `App` from a `Config` with cert paths and asserts the abe client is the identity one is overkill — instead, grep-level: no `abe::Client::new()` outside `#[cfg(test)]`, enforced by a `tests/lint-abe-client.sh` one-liner wired into `make test`).

### 3.4 Docs

- `CHANGELOG.md` for both repos. `docs/abe.md` (or wherever ABE wiring is described): the host record is `https`, ABE's cert vars point at `/etc/xdebbie/api-*.pem`, and the fence paragraph: "ABE opens an nvmet port only to the box that asked for it; after moving a VM or restoring ABE, the next reconcile re-asks."
- `milestones/<date>-abe-tls.md`: what shipped, the kernel-TLS-is-PSK-only reasoning, the adoption behaviour, follow-ups.

## 4. Rollout on gram and timesink

Order matters because timesink's ABE carries node0's live disk.

1. Build `abe-block-exporter_0.5.0-1` and `xdebbied_0.16.0-1`. Run `tests/abe/run.sh` (xdebbie, unprivileged) on gram.
2. gram: `/etc/default/abe` gets `ABE_TLS_CERT=/etc/xdebbie/api-cert.pem`, `ABE_TLS_KEY=/etc/xdebbie/api-key.pem`, `ABE_TLS_CA=/etc/xdebbie/api-ca.pem`, `ABE_LISTEN=10.22.1.1:443`. `make install` (answer **N** at the conffile prompt). `make e2e` → 14 scenarios green.
3. gram: install xdebbied 0.16.0; set `abe = "https://10.22.1.1:443"` in `[[hosts]]`; `PUT /v1/hosts/gram…` on the leader with the https URL. `xdctl host ls` shows gram's ABE GiB again.
4. timesink: check the cert SAN (see §1). Same `/etc/default/abe` edits with `10.22.0.1`. Install ABE 0.5.0: on restart it re-exports node0's volume and **adopts** the established connection from `10.22.0.1` (local). Confirm: `nft list set inet abe allowed` has `10.22.0.1 . <port>`, `/utilization.reexport.closed == []`, node0's disk still reads/writes (marker file).
5. timesink: install xdebbied 0.16.0, `abe = "https://10.22.0.1:443"` locally and via PUT. Both hosts Ready.
6. Then the pending 0.4.0/0.15.0 §7 checklist (space, GPT, capacity, port, reboot) runs on the 0.5.0/0.16.0 pair instead — it is a superset and we avoid testing twice.

## 5. Definition of done

- `make e2e` on gram: all 14 scenarios `ok` (none skipped, since gram has `10.23.0.2` as the alternate address).
- `make e2e` on timesink: scenarios 1–11, 13, 14 `ok`; 9 (range) and possibly 12 may `skip`; node0 unaffected (marker survives, `nvme list` on timesink still shows its controller, no `reconnecting` in dmesg).
- From gram, `curl --cacert /etc/xdebbie/api-ca.pem https://10.22.0.1/version` **fails** (no client cert); with `--cert/--key` it returns `"tls": true`.
- From gram, `nvme connect -a 10.22.0.1 -s <node0's port> -n <id>` is **refused** (RST), and `nft list table inet abe` on timesink shows exactly the ports in `/volumes`, each with one client.
- `xdctl host ls` on both boxes: both Ready with ABE GiB; `xdctl host show` says `tls`.
- A fresh ABE VM on gram provisions in seconds (thin clone of the sealed base), boots, and its port is in the set with `10.22.1.1`.
- Reboot gram: abe-loop → abe (fence installed before re-export) → xdebbied; `/utilization.reexport` has `failed: []`, `closed: []`; VMs come back.
- Both repos: `cargo test`, `tests/lint-links.sh`, rustfmt on touched files green; CHANGELOGs and milestone written; debs built with the right names; pushed.
