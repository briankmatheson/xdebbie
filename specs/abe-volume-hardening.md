# ABE + XDebbie Volume Hardening — Implementation Spec

Version 1.0 · November 13, 2026 · Owner: Brian Matheson · Implementer: Goose (Claude Sonnet)
Targets: `abe` 0.4.0 (from 0.3.7) · `xdebbied`/`xdctl` 0.15.0 (from 0.14.0) · `abe-csi` 0.3.5 (from 0.3.4; one small change)
Repos: <https://github.com/briankmatheson/abe>, <https://github.com/briankmatheson/xdebbie>, <https://github.com/briankmatheson/abe-csi>
Local checkouts: `/home/bmath/src/abe`, `/home/bmath/src/xdebbie`, `/home/bmath/src/abe-csi`

This spec is for an implementing agent. It says what to change, where in the code, the API contract between ABE and xdebbied, and the tests that define "done." It was written against these commits: abe `892a98f`, xdebbie `85993ce`, abe-csi `d3228ca`. If the code has moved since then, follow what the spec intends, not its line numbers, and note any conflict in the milestone log.

---

## 0. Read this first

### The eight changes

| # | Change | Repo(s) | Section |
|---|---|---|---|
| 1 | Re-export every volume when ABE starts, on the port it had before | abe (+ xdebbie safety net) | 2.3, 3.2 |
| 2 | After the fio check, trim the region it wrote so new volumes don't use thin-pool space | abe | 2.4 |
| 3 | When `raw` is off, run fio inside the partition so the GPT survives | abe | 2.4 |
| 4 | Make root disks thin snapshots of a per-image base volume instead of full copies | abe + xdebbie | 2.5, 3.3 |
| 5 | Schedule `storage = abe` VMs and standalone volumes by ABE capacity, not libvirt pool space | xdebbie (+ abe-csi error mapping) | 3.4, 4 |
| 6 | Serialize create/delete so concurrent requests can't collide or orphan volumes | abe + xdebbie | 2.1, 2.6, 3.5 |
| 7 | Use the lowest free nvmet port, and return an error when the range is full | abe | 2.2 |
| 8 | Fix the `github.com/brianmatheson/...` links (missing the "k") | abe + xdebbie | 5 |

### Order of work

Do the work in this order. Each step is a separate commit series and leaves both repos building, tested and installable.

1. **abe 0.4.0-pre:** steps 2.1 (lock and LV tags), 2.2 (ports), 2.3 (re-export), 2.4 (fio), 2.6 (name idempotency) and 5 (links). Run `tests/e2e.sh` on a real host.
2. **abe 0.4.0:** step 2.5 (base volumes, seal, `/version`). Run e2e again.
3. **xdebbie 0.15.0:** sections 3.1 through 3.6, plus updates to the fake ABE and to `tests/abe/run.sh`.
4. **abe-csi 0.3.5:** section 4.
5. **Docs and milestone log:** section 6.

### Working rules (same as earlier milestones)

- Bump the package version on every rebuild that gets installed: abe starts at `0.4.0-1` and xdebbied at `0.15.0-1`.
- Ship host-level changes (systemd units and drop-ins, defaults) in the `.deb`. Never make them as manual edits on the host.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must be clean in every crate. ABE's last commit was "handle every Result; clippy clean", so keep it that way: no new `unwrap()`/`expect()` outside tests and `main` startup.
- **State compatibility:** every new field in xdebbie's state file (`Disk`, `Volume`, `Host`) uses `#[serde(default)]`. A v0.14 state file must load unchanged. Add a test that deserializes a v0.14-shaped JSON document.
- **API compatibility:** ABE stays compatible for existing callers (`/configure`, `/id`, `/clone`, `/volumes`, `/utilization`). Add fields, but never remove or rename them. xdebbie must keep working against an ABE that predates 0.4.0 (section 3.1, feature probe).
- Commit subjects follow the repo style, e.g. `0.4.0: re-export every volume at startup`. End each commit message with the trailer `Co-authored-by: Perplexity <noreply@perplexity.ai>`.
- Record each fix in a new milestone file, `milestones/2026-11-<dd>-abe-volume-hardening.md`: what broke, what changed, which version.

### Out of scope (don't build)

- Running `nvme`/`qemu-img` on a host other than the one running xdebbied, i.e. the elected leader. Multi-host attach and ABE live migration need this, but it is a separate spec. Keep the existing "not supported on remote host" errors and don't add new code paths that silently run NVMe work on the wrong host.
- Base-image refresh or garbage collection. A base stays as it is until someone deletes it by hand.
- Authentication on the ABE API. It stays bound to the underlay address.

---

## 1. Background: what exists today

### ABE (`src/main.rs`, a single file)

- **Volumes:** an LV in `ABE_VG` (thin when `ABE_THIN_POOL` is set) whose name is a 32-hex id (`is_volume_id`). The id is the NQN, and `device_uuid` is set from it so initiators get `/dev/disk/by-id/nvme-uuid.<dashed id>`.
- **Exports:** live only in configfs (`/sys/kernel/config/nvmet`), so a reboot or a fresh module load erases them. Nothing re-creates them.
- **Ports:** `do_attach` takes `last_port_number() + 1` as the port directory index `i` and uses `44{i:03}` as the trsvcid. The number only goes up, and from i = 1000 the formatting produces 6-digit strings. Nothing holds a lock, so two concurrent creates can get the same `i`.
- **fio check:** `configure` calls `create_lv` (with GPT plus one partition at 1 MiB when `raw` is not true), exports the volume, then runs `characterize()`. That does random writes over the first `min(1 GiB, bytes)` of the whole LV, which on the non-raw path overwrites the GPT, and the region is never trimmed.
- **Clones:** `/clone/<id>` makes a thin snapshot of any thin LV in ABE's pool. Nothing calls it today.

### xdebbie

- `crates/xdebbied/src/abe.rs` holds `Client` (only `create` and `delete`) and `Nvme` (`connect`, `disconnect`, `wait_dev`, `populate`).
- `crates/xdebbied/src/libvirt.rs`:
  - `prepare_disks` allocates root and data disks with `create` and fills the root disk with `qemu-img convert` from `base_image()`, so every root is a full copy.
  - `ensure_connected` reconnects disks before `start`/`provision`.
  - `release_disks` deletes them.
  - `attach_volume`/`detach_volume` only work for a local URI.
- `crates/xdebbied/src/api/mod.rs`:
  - `create_volume` picks the server with `.find_map(|h| h.abe.clone())`, i.e. the first healthy host by name. It checks name idempotency, drops the store snapshot, calls ABE, then inserts, so two concurrent creates with the same name can both reach ABE.
  - `delete_volume` checks for attachment on a snapshot and then deletes without holding anything.
- `crates/xdebbie-core/src/scheduler.rs`: `place()` checks `spec.disk_gib` against `capacity.disk_free_gib`, which is libvirt pool space. It has no notion of ABE, so a `storage = abe` VM can land on a host with no ABE and then sits in Provisioning with "host X has no ABE".
- `tests/abe/`: `fake-abe.py`, `run.sh`, and stubs for `nvme`, `qemu-img` and `virsh`.

---

## 2. ABE changes

### 2.1 Foundations: one lock for mutations, LV tags as durable metadata

**One process-wide lock.** Every operation that changes LVM or configfs runs while holding it:

```rust
static OPS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_ops<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _g = OPS.lock().unwrap_or_else(|e| e.into_inner()); // survive poisoning
    f()
}
```

- These operations are blocking (`Command`, `fs`), so run each handler's mutating part as `tokio::task::spawn_blocking(move || with_ops(|| ...))`. Don't hold a `tokio::sync::Mutex` across blocking calls.
- Inside the lock: LV create/partition/tag, export (subsystem, namespace, port, link), unexport, `lvremove`, clone, seal, and the startup re-export pass.
- Outside the lock: fio and the discard that follows it (2.4). Only the creating request knows the new id at that point, so nobody else can touch the volume. This keeps one slow fio run from stalling every other create.
- Read-only handlers (`/volumes`, `/utilization`, `/version`) don't take the lock.

**LV tags.** These are durable per-volume metadata. They live in the VG metadata and survive reboots without a separate state file.

| Tag | Meaning |
|---|---|
| `abe_port=<n>` | trsvcid this volume is exported on; re-export uses it |
| `abe_name=<name>` | caller's name for idempotent create (2.6) |
| `abe_base=<image>` | this volume is the base for `<image>` (2.5) |
| `abe_sealed` | base is filled and frozen: never exported, clone-only (2.5) |

- LVM tags allow `[A-Za-z0-9_+.\-/=!:&#]`. Validate any caller-supplied value (`name`, `image`) against `^[A-Za-z0-9._+:-]{1,128}$` and return 400 otherwise.
- Write tags with `lvchange --addtag` and `--deltag`. When a port changes, delete the old `abe_port=` tag and add the new one in the same `lvchange` call.
- Append `lv_tags,lv_active` to the end of `LVS_FIELDS` so the existing indices don't shift. `lvs` prints tags comma-separated. Extend `Lv` with `tags: Vec<String>` and `active: bool`, and update `parse_lvs`, its fixture constant `LVS` and the tests.
- Add helpers: `fn tag_value<'a>(tags: &'a [String], key: &str) -> Option<&'a str>` and `fn has_tag(tags, key) -> bool`.

**Make the configfs root injectable for tests.** Replace `NVMET_PATH` with `fn nvmet_path() -> String`, reading `ABE_NVMET_PATH` with `/sys/kernel/config/nvmet` as the default. Pure functions such as port scanning can then be unit-tested against a tempdir.

**Clean up half-made exports.** Refactor `do_attach` into `fn export(id: &str, port: u16) -> Result<Message>`. If any step after the subsystem directory exists fails, call `remove_export(id)` before returning the error. Also give `configure` and `clone` rollback: if export fails, `lvremove` the LV they just created. A failed create must leave no LV and no configfs junk behind.

### 2.2 Lowest free nvmet port, with a range error (change 7)

- **Configuration:** `ABE_PORT_MIN` defaults to `44001` and `ABE_PORT_MAX` to `44999`. Validate at startup (`1 <= min <= max <= 65535`) and exit non-zero with a clear message if they're invalid. Add both to `debian/abe.default`, commented out.
- **Used set:** read `addr_trsvcid` from every `nvmet_path()/ports/*` and parse each one as `u16`. Ignore unparseable entries but log them.
- **Pure function, unit-tested:**
  ```rust
  fn lowest_free(used: &BTreeSet<u16>, min: u16, max: u16) -> Option<u16>
  ```
- **New port directory names:** name the directory after the port number itself (`ports/44003`), since nvmet port ids are u16 and these fit. Existing exports made by older versions with small directory names (`ports/1`, `ports/2`) keep working. Treat directory names as opaque and always go by `addr_trsvcid`.
- **Exhausted range:** return `503 Service Unavailable` with body `{"error": "no free nvmet port in 44001-44999 (N exported)"}`, and roll back the LV per 2.1.
- **Re-attach of an already-exported volume** (`GET /id/<id>`): report the existing port unchanged, even if it's outside the current range.
- Drop the `iteration` field from `Port`. Keep `addr_adrfam ipv4` and `addr_traddr = bind_traddr()` as they are.

### 2.3 Re-export everything at startup (change 1)

Add `fn reexport_all() -> ReexportSummary`. Call it from `main` after `ensure_configfs_mounted()`/`ensure_nvmet_present()` and before binding the HTTP listener, so no request ever sees a half-restored target.

```
take OPS lock
lvs (with tags, active)
for each LV with is_volume_id(name), sorted by name:
    if has_tag(abe_sealed):
        if exported: remove_export (sealed bases are never exported); continue
    if !active: lvchange -ay -K <vg>/<id>       # -K: ignore activation-skip (clones)
    if let Some(p) = existing_export(id):
        ensure the abe_port tag equals p; count "already"; continue
    want = tag abe_port if it parses, is in [min,max] and is not used
           else lowest_free(...)                 # warn: "re-exporting <id> on <new> (was <old>)"
    export(id, want); set abe_port tag; count "exported"
    on error: log the id + error, count "failed", continue with the next LV
log one summary line: exported=N already=M failed=K moved=J
```

- **Underlay not up yet:** the docs' drop-in waits for the underlay address, but nvmet still fails to enable a port with `EADDRNOTAVAIL` if it isn't there. Retry the whole pass every 2 s for up to `ABE_REEXPORT_WAIT` seconds (default 90). Only failures with `EADDRNOTAVAIL` (os error 99) count toward this. After that, start serving anyway and keep retrying the failed ids in a background task every 10 s until they succeed.
- **Thin pool inactive:** if the pool itself is inactive and `lvchange -ay` fails, log the same hint xdebbie's `lvm.rs` gives: install `thin-provisioning-tools` and check `journalctl -b | grep -i thin`.
- **Every export records its port:** `/configure`, `/clone` and `GET /id` all set the `abe_port` tag on success, so the next restart reuses the same port.
- **Expose the result:** add `GET /reexport` (it runs the pass again, holding the lock) and include the last summary in `/utilization` as `"reexport": {"exported":..,"already":..,"failed":[ids],"at":"<rfc3339>"}`.
- **systemd:** add `After=lvm2-monitor.service local-fs.target` to `debian/abe.service`, keeping the existing lines. Keep `Restart=always`.

### 2.4 fio check: run inside the partition, then trim (changes 2 and 3)

Replace `characterize(dev, bytes)` with `characterize(dev, region: Region, thin: bool)`, where:

```rust
struct Region { offset: u64, len: u64 }

/// Pure, unit-tested. MIB = 1 << 20, GIB = 1 << 30.
fn fio_region(bytes: u64, raw: bool) -> Option<Region> {
    let (start, end) = if raw {
        (0, bytes)
    } else {
        // parted mkpart primary 1MiB 100%: partition 1 starts at 1 MiB; the
        // backup GPT occupies the last 33 sectors. Stay 1 MiB clear of both.
        (MIB, bytes.saturating_sub(MIB))
    };
    let len = (end.saturating_sub(start)).min(GIB) & !(MIB - 1); // whole MiBs
    (len >= 16 * MIB).then_some(Region { offset: start, len })
}
```

**fio on non-raw volumes runs inside the partition.** I'm reading "run fio in the fs" as "inside partition 1, never over the partition table", because ABE leaves the partition blank and the client picks the filesystem and encryption. Run fio on the LV device with `--offset=<region.offset> --size=<region.len>`, which keeps every write inside partition 1. Don't run it on the dm partition device (`abe-<id>p1`): it holds the LV open and is what `remove_holders` exists to clean up. If `fio_region` returns `None` (volume under ~18 MiB), skip fio and return `perf: None`.

**Then trim the same region**, outside the lock:

- **Thin volume:** `blkdiscard --offset <o> --length <l> <dev>`. Thin LVs give fully-covered chunks back to the pool, and the region is MiB-aligned so any chunk size up to 1 MiB is freed completely. Reads afterwards return zeros, so raw VM disks also no longer start with fio garbage.
- **Thick volume:** `blkdiscard --zeroout --offset <o> --length <l> <dev>`. This frees nothing, but the caller still gets a blank region.
- **If the trim fails:** log `warn!`, set `perf.trimmed = false`, and still return the volume. Otherwise set `perf.trimmed = true`. Add `trimmed: bool` (serde default `false`) to `Perf`; xdebbie's `VolumePerf` ignores unknown fields.

**Check the partition table survived** (non-raw only), after fio: run `parted -s -m <dev> unit B print` and confirm partition 1 exists, starts at 1048576 and is named `<id>`. If not, delete the volume (it's new) and return 500 `"fio damaged the partition table of <id>"`. This should never fire, but it turns a silent corruption into a loud one.

**Warn at startup** if the thin pool's discard mode makes trimming useless. Read `lvs -o discards <vg>/<pool>`, and if it is `ignore`, warn: `thin pool discards=ignore: the fio region cannot be returned to the pool; run lvchange --discards passdown <vg>/<pool>`.

Update the `README.md` table and paragraph on fio to say: region inside partition 1 for non-raw volumes, trimmed afterwards, `trimmed` in `perf`.

### 2.5 Base volumes, seal and `/version` (ABE half of change 4)

A **base** is an ordinary ABE volume (32-hex id, so `device_uuid` and the by-id path still work) tagged `abe_base=<image>`. It is filled once by xdebbied over NVMe/TCP and then **sealed**: unexported, never exported again, and clone-only.

| Request | Behaviour |
|---|---|
| `GET /configure?base=<image>&bytes=N` | Implies `raw=true` and skips fio. Idempotent under the lock: if an LV tagged `abe_base=<image>` exists, return it (200). Return 409 if its `bytes < N`. Otherwise create, export and tag it. The reply adds `"base": "<image>"`, `"sealed": false\|true`. A sealed base is returned **without** exporting it (`port: ""`). |
| `GET /seal/<id>` | 404 if no such LV, 400 if it isn't a base. Under the lock: `remove_export(id)`, add `abe_sealed`, delete `abe_port`. Idempotent: sealing a sealed base returns 200. Reply: `{"id","base","sealed":true,"bytes"}`. |
| `GET /id/<id>` on a sealed base | 409 `"<id> is a sealed base; clone it"` |
| `GET /clone/<id>?bytes=N[&name=..]` | Unchanged for ordinary thin volumes. If the origin is a base, it must be sealed (409 `"base <id> is not sealed yet"` otherwise). After `lvcreate -s`, remove any `abe_base`, `abe_sealed`, `abe_name` and `abe_port` tags the snapshot may have inherited, then apply the clone's own tags. |
| `DELETE /id/<id>` on a base | Allowed. Existing thin clones stay valid (thin snapshots don't depend on the origin LV). Log `warn!("deleting base <image> (<id>); <n> clones keep their data")`. |
| `GET /volumes` | Each entry adds `"base": "<image>"\|null`, `"sealed": bool`, `"name": "<name>"\|null`. |
| `GET /version` | `{"version": env!("CARGO_PKG_VERSION"), "features": [...]}`. Features: `"reexport"`, `"ports"`, `"name"`, `"trim"`, plus `"base"` and `"clone"` **only when `ABE_THIN_POOL` is set** (bases need thin clones). |

Don't make bases read-only with `lvchange -p r`: the permission can carry over to snapshots. Protect them with the tag and the API refusals above.

### 2.6 Name idempotency (ABE half of change 6)

- `GET /configure?name=<name>` and `GET /clone/<id>?name=<name>`: under the lock, if an LV tagged `abe_name=<name>` exists, export it if needed and return it with `"existing": true` (200, no fio, `perf: null`). Return 409 if its bytes are below the request. Otherwise create it and add the tag.
- Without `name`, behaviour is as before (always a new volume).
- This makes creates crash-safe and leader-change-safe: a retry with the same name after xdebbied died mid-request gets the same volume back instead of a new one.

### 2.7 ABE tests

**Unit tests** (in `src/main.rs`):
- `lowest_free`: empty set, gaps, full range → `None`, `min == max`.
- `fio_region`: raw/non-raw, tiny volumes (`None`), > 1 GiB (capped), alignment.
- Tag parsing: `tag_value`/`has_tag`; `parse_lvs` with the new `lv_tags,lv_active` columns, including empty tags and multiple comma-separated tags.
- Name/image validation (accept and reject cases).
- Used-port scan against a tempdir laid out like `ports/*/addr_trsvcid` (set `ABE_NVMET_PATH`).

**`tests/e2e.sh`** (runs as root on a host with a thin pool and fio). Add:
1. **Port reuse:** create A, B, C; delete B; create D, and D's port must equal B's.
2. **Range exhausted:** restart abe with `ABE_PORT_MIN=44001 ABE_PORT_MAX=44002` and nothing exported. Two creates succeed; the third returns 503, and `lvs` shows no third LV.
3. **Concurrency:** 8 creates in parallel (`curl ... &` then `wait`) give 8 distinct ports and 8 LVs, and each is connectable.
4. **Name idempotency:** two parallel `?name=e2e-x` creates return the same id, and only one LV is tagged `abe_name=e2e-x`.
5. **Re-export:**
   - Create two volumes and write a marker to each.
   - `systemctl stop abe`, then tear down every ABE export in configfs (add a helper `tests/nvmet-wipe.sh` that unlinks ports, disables and removes namespaces and subsystems).
   - `systemctl start abe`. Both volumes are exported again on **the same ports** and the markers read back.
6. **Trim:** a new raw 2 GiB thin volume reports `perf.trimmed == true`, its `used_bytes` in `/volumes` is under 64 MiB, and the first 64 MiB read back as zeros (`cmp -n $((64<<20)) /dev/zero <dev>`).
7. **GPT intact:** a non-raw volume connected over NVMe shows `/dev/disk/by-partlabel/<id>`, and `parted -s <dev> print` lists partition 1.
8. **Bases:**
   - `configure?base=e2e-img&bytes=64M`, connect it, `dd` a marker in, disconnect, `seal`.
   - `GET /id/<base>` → 409.
   - Two clones each read the marker, and each clone's `used_bytes` is under 8 MiB.
   - `DELETE` the base; both clones still read the marker.
9. Update the existing `perf` assertion: the default `bs` is 4k now, not 4M (the current check says `"4M"`, which is stale), and also require `.perf.trimmed == true`.

---

## 3. xdebbie changes

### 3.1 ABE client additions (`crates/xdebbied/src/abe.rs`)

Add to `Client`, each returning typed replies and treating 404/409/503/507 distinctly in errors:

```rust
pub struct AbeFeatures { pub version: String, pub features: BTreeSet<String> }
pub async fn version(&self, base: &str) -> Result<Option<AbeFeatures>>;   // 404 -> Ok(None): pre-0.4 abe
pub async fn create_named(&self, base: &str, bytes: u64, name: &str) -> Result<Export>; // ?bytes&raw=true&name=
pub async fn export(&self, base: &str, id: &str) -> Result<Export>;      // GET /id/<id>
pub async fn base(&self, base: &str, image: &str, bytes: u64) -> Result<BaseVol>; // ?base=&bytes=
pub async fn seal(&self, base: &str, id: &str) -> Result<()>;
pub async fn clone(&self, base: &str, origin: &str, bytes: u64, name: &str) -> Result<Export>;
pub async fn utilization(&self, base: &str) -> Result<AbeUtilization>;
```

- `Export` gains `#[serde(default)] existing: bool`. `BaseVol` is `{ export: Export (flattened), base: String, sealed: bool, bytes: u64 }`.
- **Feature cache:** add `features: Arc<Mutex<HashMap<String, (Instant, Option<AbeFeatures>)>>>` with a 60 s TTL, and a helper `has(base, "clone")`.
- Raise the `reqwest` timeout from 60 s to 120 s, since a create waits for fio plus the trim.
- **Names sent to ABE:**
  - Standalone volumes: `vol-<name>` if it passes the charset check, else `vol-<sha256(name)[..32]>`.
  - VM disks: `vm-<vm uuid>-root` and `vm-<vm uuid>-data`.
  - These names are why a retry after a crash finds the volume instead of making another.
- **Old ABE:** when `version()` returns `None`, use the old `create` (no name) and the full-copy root path. Log once per server: `abe at <url> predates 0.4.0: no thin root clones, no name idempotency`.

### 3.2 Endpoint refresh: safety net for change 1

ABE now re-exports on the recorded port, but if a port was taken while it was down, it moves the volume and logs it. xdebbie must notice.

- Add to `Hypervisor`, with a default no-op so the test fake keeps compiling:
  ```rust
  fn refresh_endpoints(&self, host: &Host, disks: &mut Vec<Disk>) -> impl Future<Output = Result<bool>> + Send
  ```
  `LibvirtDriver` implements it this way: for each disk whose NQN is **not** connected (`Nvme::connected`), call `abe.export(server, id)`. If `port` or `traddr` differ from the record:
  - `nvme disconnect -n <nqn>`, which drops a controller stuck retrying the old port forever under `ctrl-loss-tmo -1`;
  - update the record;
  - return `true`.
- **Reconciler `step`:** for `storage = abe` VMs in Provisioning and on every start path, call `refresh_endpoints` on a clone of `vm.disks` before `prepare_disks`/`start`, and put changes in `Patch.disks`.
- **`volume_step`:** before `attach_volume`, do the same for `vol.disk()`. Add `VolPatch.endpoint: Option<(String traddr, String port)>` and apply it to the `Volume`.
- Log at `warn!`: `abe moved <id> from <old> to <new>; record updated`.

### 3.3 Root disks as thin clones of a base volume (change 4)

Change `LibvirtDriver::prepare_disks` (in `libvirt.rs`) for the root role only. Data disks stay blank (now created with `create_named`).

```
if root not in disks:
    if abe.has(server, "base") && abe.has(server, "clone"):
        (src, fmt) = base_image(host, image)
        image_bytes = qemu_img_virtual_size(src, fmt)   # `qemu-img info -f <fmt> --output=json` -> "virtual-size", round up to MiB
        root_bytes  = gib(vm.spec.disk_gib)
        if root_bytes < image_bytes: bail!("disk_gib {} is smaller than image {} ({} bytes)")
        let _g = self.base_locks.lock((server, image)).await;   // KeyedLock, 3.5
        b = abe.base(server, image, image_bytes)
        if !b.sealed:
            d = Disk{role: Root /*temporary*/, id: b.id, traddr, port, ...}
            nvme.connect(d); dev = nvme.wait_dev(d, 15s)
            nvme.populate(src, fmt, dev)          # existing qemu-img convert -n
            sync: `blockdev --flushbufs <dev>`
            nvme.disconnect(b.id)
            abe.seal(server, b.id)
        drop(_g)
        e = abe.clone(server, b.id, root_bytes, "vm-<uuid>-root")
        disks.push(Disk{role: Root, ..from e, bytes: root_bytes, populated: true, origin: Some(b.id)})
    else:
        legacy: create + populate (current code, unchanged)
```

- Add `Disk.origin: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`) in `xdebbie-core/src/model.rs`. `xdctl vm describe` shows it.
- **Progress is still saved step by step:** a root disk is pushed into `disks` only after the clone exists. A crash between `seal` and `clone` is harmless: next tick `base()` returns the sealed base and `clone(..., name)` is idempotent on name.
- **Unsealed base after a crash:** a crash during `populate` leaves an unsealed base. The next attempt fills it again from the start (`qemu-img convert -n` overwrites) and seals it. That's correct, because nothing can clone an unsealed base.
- The guest grows its root partition at first boot with cloud-init `growpart`, as today. Don't add anything for that.
- Base-filling, like everything else in `prepare_disks`, runs on the xdebbied host. Keep that limitation and leave the remote-host error paths as they are.
- **Delete:** `release_disks` deletes the clone only (it is in `vm.disks`), and the base stays.
- **Docs:** update `docs/storage.md`. Remove "thin clones" from the "What is *not* done" list and add a "Base volumes" subsection: one per image per ABE, filled once, sealed, cloned per VM; to refresh an image, delete its base on the ABE host (`curl -X DELETE http://<abe>/id/<base id>`) and the next VM makes a new one.

### 3.4 Capacity accounting for ABE (change 5)

**Model** (`xdebbie-core/src/model.rs`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AbeCapacity {
    pub thin: bool,                 // abe /utilization thin_pool != null
    pub bytes: u64,                 // pool (or VG) size
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub provisioned_bytes: u64,     // sum of virtual sizes (thin) / used (thick)
    pub data_percent: f64,
    #[serde(default)] pub metadata_percent: Option<f64>,
    pub seen: DateTime<Utc>,
}
```

New `Host` fields, all `#[serde(default...)]`:

| Field | Default | Meaning |
|---|---|---|
| `abe_capacity: Option<AbeCapacity>` | `None` | filled by inventory; never set from config |
| `abe_overcommit: f64` | `1.0` | thin pools: provisioned may reach `bytes × overcommit`. Ignored for thick pools |
| `abe_max_data_percent: f64` | `85.0` | refuse new allocations at or above this pool fill |
| `abe_max_metadata_percent: f64` | `80.0` | the same for thin metadata (a full metadata LV breaks the whole pool) |

Add a pure method:

```rust
impl Host {
    /// Ok(headroom bytes after the request) or Err(reason) for scheduler messages.
    pub fn abe_fit(&self, pending: u64, request: u64) -> Result<u64, &'static str>;
}
```

- No `abe` URL → `Err("no abe")`. No `abe_capacity` → `Err("abe capacity unknown")`.
- **Thin:** reject `"abe pool full"` when `data_percent >= max` or `metadata_percent >= max`. Then `limit = bytes × overcommit`, `need = provisioned_bytes + pending + request`, and reject `"abe capacity"` when `need > limit`. Headroom is `limit - need`.
- **Thick:** `need = pending + request` against `free_bytes`.
- Unit-test every branch.

**Inventory** (`reconcile.rs::inventory`):
- Add `fn abe_utilization(&self, host: &Host) -> impl Future<Output = Result<Option<AbeCapacity>>> + Send` to `Hypervisor` (default `Ok(None)`). `LibvirtDriver` implements it with `abe.utilization()` when `host.abe` is set.
- Run it in the same `join_all` as the libvirt inventory.
- **On success:** store it with `seen = now`. **On error:** keep the old value and `warn!`. Don't update `seen`.
- **Staleness:** before calling `place()`, the reconciler clones the hosts and sets `abe_capacity = None` where `now - seen > host_stale_secs`. The scheduler itself stays pure and deterministic.

**Pending bytes:** space promised but not yet visible in `/utilization`. Compute it in `place()` from `vms`. For each VM with `storage = Abe`, a host, and `consumes_capacity()`, add the requested bytes of every role (root: `disk_gib`; data: `data_disk_gib`) **not yet present** in `vm.disks`. Disks that already exist are in ABE's `provisioned_bytes`, so counting them again would double-count. For standalone volumes being created, see the ledger in 3.5. The reconciler passes ledger totals into `place()` as a new `abe_inflight: &BTreeMap<String, u64>` (host name → bytes) argument and adds them to pending.

**Scheduler** (`scheduler.rs::place`):
- **`storage = Abe`:** call `h.abe_fit(pending, request)` with `request = gib(disk_gib) + gib(data_disk_gib.unwrap_or(0))`, and use the `Err` reason with `reject()`. **Skip** the libvirt `disk_free_gib` check, since only the small seed ISO is local. These VMs no longer land on a host without ABE.
- **`storage = Local`:** unchanged.
- **Spread/Pack score:** for ABE VMs, the utilization fed into the score is `max(cpu, mem, abe_used_fraction)`, where `abe_used_fraction = need / limit` (thin) or `1 - (free - need) / bytes` (thick).
- Update the module doc comment at the top of `scheduler.rs` to list the new filter.

**Standalone volume placement** (`api/mod.rs::create_volume`):
- **Unpinned:** candidates are healthy hosts with `abe` and fresh `abe_capacity` where `abe_fit(pending_for(h), req.bytes)` is `Ok`. Choose the **largest headroom**, ties broken by host name. Pending comes from VM roles plus the ledger, the same as the scheduler, so factor one helper out of `place()`'s loop: `pub fn abe_pending(hosts, vms) -> BTreeMap<String, u64>`.
- **Pinned (`host` set):** the same check. If it fails, return the error below.
- **No fit:** `507 Insufficient Storage` with `"no ABE host fits <bytes> bytes: <host>: <reason>, ..."`.
- Volumes on an ABE that predates 0.4.0 (no fresh capacity) are refused the same way, with reason `"abe capacity unknown"`. That's the correct behaviour: don't guess.

**xdctl:**
- `xdctl host ls`: add an `ABE` column showing `free/total GiB` (thin: `limit - provisioned` / `limit`), or `-` when there's none.
- `xdctl host describe`: prints `abeCapacity`.
- Keep the JSON output unchanged apart from the added fields.

### 3.5 Serialize create and delete (change 6)

**KeyedLock helper:** add `crates/xdebbied/src/keyed_lock.rs`.

```rust
pub struct KeyedLock<K> { inner: std::sync::Mutex<HashMap<K, Arc<tokio::sync::Mutex<()>>>> }
impl<K: Eq + Hash + Clone> KeyedLock<K> {
    pub async fn lock(&self, k: K) -> KeyedGuard<K>;   // removes the map entry on drop when no one else holds/waits (Arc::strong_count == 2 check under the std mutex)
}
```

Unit-test it: two tasks on the same key run one after the other, tasks on different keys run in parallel, and the map is empty after both finish.

**`create_volume`:**
1. Validate.
2. `let _g = app.volume_names.lock(req.name.clone()).await;`
3. **Re-read the store under the guard** and run the existing name-idempotency check.
4. Choose the server (3.4) and reserve `req.bytes` in the **ABE ledger** (`app.abe_ledger: Arc<Mutex<BTreeMap<String host, u64>>>`; the reconciler gets the same `Arc`). Release the reservation with a drop guard so an early return or a panic can't leak it.
5. Call `abe.create_named(server, bytes, abe_name)`. If the reply has `existing: true` (a retry after an earlier crash), use it as is.
6. Insert into the store, then drop the guard.

Concurrent POSTs with the same name now produce one ABE call. The second one gets 200 with the same record. Across a leader change, ABE's name tag (2.6) gives the same guarantee.

**`delete_volume`:**
- Add `Volume.deleting: bool` (`#[serde(default, skip_serializing_if = "std::ops::Not::not")]`).
- In **one** `store.update` closure: look the volume up (missing → 204), return 409 if `attach_to` or `attached` is set, otherwise set `deleting = true`.
- Then `abe.delete`. **On success:** remove the record. **On failure:** return 502 and leave `deleting = true`. A retried DELETE goes on from there.
- `attach_volume` returns 409 `"volume is being deleted"` for a deleting volume.
- `volume_step` never attaches a deleting volume. It only makes sure the volume is detached.

**VM disks:**
- `prepare_disks` is already serialized per VM (one reconciler, one step per VM per tick). Name-idempotent ABE calls make it crash-safe as well.
- Bases are serialized per `(server, image)` with `LibvirtDriver.base_locks: KeyedLock<(String, String)>`, and across processes by ABE's base idempotency (2.5).

### 3.6 xdebbie tests

**`tests/abe/fake-abe.py`:** extend it to match ABE 0.4.0.
- **`/version`:** returns the features list. If `FAKE_ABE_LEGACY=1`, return 404 for it and ignore `name`/`base`.
- **Creates:** `?name=` dedup (store the name in the `.json` metadata). `?base=` with base dedup. `/seal/<id>`. `/clone/<id>?bytes=&name=`: copy the origin `.img` with `shutil.copyfile` and record `origin`.
- **Ports:** lowest free port instead of `next_port`. `FAKE_ABE_PORT_MAX` makes it return 503.
- **`/utilization`:** pool size from `FAKE_ABE_POOL_BYTES` (default 1 TiB). `provisioned_bytes` is the sum of volume sizes, and `used_bytes` the sum of actual file sizes.
- **Call counter:** `GET /_calls` returns `state["calls"]` so `run.sh` can count calls.

**`tests/abe/stubs/qemu-img`:** answer `info --output=json` with `{"virtual-size": <size of the file or $FAKE_IMAGE_BYTES>}`, and count `convert` calls in a log file.

**`tests/abe/run.sh`:** add these scenarios.
1. **Two VMs, one image:** two `storage = abe` VMs on the same image give exactly one `?base=` create, one `qemu-img convert`, one `/seal`, and two `/clone` calls. Both root disks have `origin` set and `populated: true`.
2. **Crash safety:** kill xdebbied right after `/seal` (use `FAKE_ABE_DIE_AFTER=seal`, which makes the fake exit, and restart it). After restart there is still one base and one clone per VM.
3. **Legacy ABE:** with `FAKE_ABE_LEGACY=1`, the behaviour is the same as v0.14: one full `convert` per VM.
4. **Capacity:** with `FAKE_ABE_POOL_BYTES` set to 15 GiB, the first 10 GiB VM is placed and the second stays `Pending` with `abe capacity` in its message. `POST /v1/volumes` with 10 GiB returns 507.
5. **No ABE on the host:** a `storage = abe` VM against a host without `abe` stays `Pending` with `no abe`. It is never placed and left stuck in Provisioning.
6. **Concurrent creates:** 5 parallel `POST /v1/volumes` with the same name give one `/configure` call in `/_calls`, one 201, and four 200s with the same id.
7. **Delete race:** `DELETE` with the fake ABE answering 500 once gives 502 and `deleting: true`. `attach` then returns 409. A second DELETE gives 204 and the record is gone.
8. **Endpoint refresh:** after the fake re-exports a volume on another port (`/_move/<id>` test hook) and the stub `nvme` reports it disconnected, the record's port updates, and the stub log shows `disconnect` then `connect` on the new port.

**Unit tests:**
- `Host::abe_fit` (every branch); `place()` with ABE hosts (thin, thick, pending, stale → unknown, no abe, Spread prefers the emptier pool).
- `abe_pending` excludes disks that already exist.
- `KeyedLock`.
- Compatibility: a v0.14 state file (VMs with `disks` lacking `origin`, volumes lacking `deleting`, hosts lacking `abe*`) loads, and saving it again round-trips.
- The fake `Hypervisor` in `reconcile.rs` tests implements `abe_utilization` and `refresh_endpoints`.

---

## 4. abe-csi 0.3.5

There's no functional change, only correct error codes for the new capacity behaviour.

- **`scripts/abe-create-volume`:** when xdebbied answers 507, exit with code `28` and print the xdebbied message to stderr. Other failures keep exiting 1.
- **`src/exec.rs` / `src/controller.rs`:** map exit code 28 from `abe-create-volume` to `Status::resource_exhausted(msg)`, so the external-provisioner reports a capacity problem on the PVC instead of a generic internal error.
- **Test:** add a unit test for the mapping. In the README, mention that PVCs on a full ABE now report `ResourceExhausted`.

---

## 5. Wrong repo links (change 8)

The repo is `github.com/briankmatheson/...`; the bad form drops the "k". Known occurrences:

- `xdebbie/docs/storage.md`, line 13: `https://github.com/brianmatheson/abe` → `https://github.com/briankmatheson/abe`
- `xdebbie/CHANGELOG.md`, line 242: the same fix
- `abe/debian/abe.service`, line 3: `Documentation=https://github.com/brianmatheson/abe` → `https://github.com/briankmatheson/abe`

Then sweep all three repos for the bad form and fix anything else it finds, without touching email addresses:

```sh
rg -n 'github\.com/brianmatheson/' --glob '!target' --glob '!Cargo.lock'
```

Add the same `rg` line as a check in each repo's test script (`abe/tests/lint-links.sh` called from CI or `make check`, and the xdebbie equivalent). It fails if it finds a match.

---

## 6. Docs and milestone log

- **abe `README.md`:**
  - Port range and the lowest-free rule; the 503 when it's full.
  - Re-export at startup and `GET /reexport`.
  - The tags table from 2.1.
  - fio region and trim. Also fix the env table, which still says the `ABE_FIO_BS` default is `4M`; the code default is `4k`.
  - Bases, `/seal`, `/version`, `?name=`.
  - New environment variables: `ABE_PORT_MIN`, `ABE_PORT_MAX`, `ABE_REEXPORT_WAIT`, `ABE_NVMET_PATH`.
- **xdebbie `docs/storage.md`:**
  - Base volumes (3.3); capacity accounting with the new `[[hosts]]` keys (3.4); endpoint refresh (3.2).
  - The new `What is not done` list: remote-host NVMe, migration, base garbage collection.
  - Update the configuration reference block.
- **xdebbie `docs/api.md`:**
  - `POST /v1/volumes` can return 507.
  - `DELETE` can return 502 and leave the volume with `deleting: true`.
  - `attach` returns 409 for a volume being deleted.
- **`CHANGELOG.md`:** entries in both repos.
- **Milestone file:** `milestones/2026-11-<dd>-abe-volume-hardening.md` with one row per change: issue, fix, version, test that proves it.

---

## 7. Definition of done

On a real host (`timesink`, thin pool `xdebbie-pool` in `timesink-vg`, abe and xdebbied installed from the new `.deb`s):

- [ ] `sudo tests/e2e.sh` in abe passes every scenario in 2.7, and `cargo test` / `clippy -D warnings` are clean in all three repos.
- [ ] `tests/abe/run.sh` in xdebbie passes every scenario in 3.6.
- [ ] **Reboot test:**
  - Before: two `storage = abe` VMs running, plus one standalone volume attached to a VM with a mounted filesystem and a marker file.
  - `sudo reboot`.
  - After: abe logs `exported=N failed=0 moved=0`, both VMs come back running, the volume is re-attached, and the marker file is readable. Nothing was done by hand.
- [ ] **Space test:**
  - Create a 20 GiB standalone volume. `curl http://10.22.0.1/volumes` shows `used_bytes` under 64 MiB.
  - Create a third `storage = abe` VM from an image whose base already exists. `/volumes` shows its root disk as a clone of the base (`origin` set) with `used_bytes` under 64 MiB.
  - The VM's provision step takes seconds, not a full image copy (compare `journalctl -u xdebbied` timestamps).
- [ ] **GPT test:** `curl 'http://10.22.0.1/configure?bytes=1073741824'` (non-raw), connect, and `/dev/disk/by-partlabel/<id>` exists.
- [ ] **Capacity test:**
  - Set `abe_overcommit = 1.0` and request a VM bigger than the pool's remaining headroom. It stays `Pending` with `abe capacity`.
  - A PVC that doesn't fit shows `ResourceExhausted` in `kubectl describe pvc`.
- [ ] **Port test:** after creating and deleting volumes in a mixed order, `ls /sys/kernel/config/nvmet/ports` shows no gaps below the highest port in use, except ports from before the upgrade.
- [ ] `rg 'github\.com/brianmatheson/'` finds nothing in any of the three repos.
- [ ] Milestone file written; versions bumped; `.deb`s built with `scripts/build-deb.sh` (xdebbie) and `cargo deb` (abe).
