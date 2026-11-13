// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire and storage types shared by `xdebbied`, `xdctl`, and `capx`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Label key for marking VMs as protected from deletion.
pub const PROTECTED_LABEL: &str = "xdebbie.io/protected";

/// A physical (or nested) Linux box running libvirtd + QEMU/KVM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Host {
    /// Unique short name, e.g. `node-a`.
    pub name: String,
    /// libvirt connection URI, e.g. `qemu+ssh://xdebbie@10.0.0.11/system`.
    pub uri: String,
    /// Failure domain (rack, PDU, room). CAPI spreads control planes across these.
    #[serde(alias = "failure_domain", default = "default_fd")]
    pub failure_domain: String,
    /// Whether this failure domain may host control-plane machines.
    #[serde(alias = "control_plane_eligible", default = "yes")]
    pub control_plane_eligible: bool,
    /// Free-form labels used by `Placement::required_labels`.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// libvirt storage pool used for VM disks.
    #[serde(alias = "storage_pool", default = "default_pool")]
    pub storage_pool: String,
    /// Linux bridge VMs attach to (must exist on the host).
    #[serde(default = "default_bridge")]
    pub bridge: String,
    /// CPU overcommit ratio (vCPU : pCPU). 1.0 = no overcommit.
    #[serde(alias = "cpu_overcommit", default = "default_cpu_ratio")]
    pub cpu_overcommit: f64,
    /// ABE (NVMe-oF block exporter) API on this host, e.g. `http://10.22.0.1:80`.
    /// VMs with `storage = abe` get their disks from the ABE of the host they
    /// are scheduled on; any host can attach them later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abe: Option<String>,
    /// What this host's ABE reported last (`GET /utilization`); filled by
    /// the inventory loop, never set from config.
    #[serde(
        alias = "abe_capacity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub abe_capacity: Option<AbeCapacity>,
    /// Thin pools: the sum of volume sizes may reach `bytes × overcommit`.
    /// Ignored for thick pools.
    #[serde(alias = "abe_overcommit", default = "one")]
    pub abe_overcommit: f64,
    /// Refuse new ABE allocations at or above this pool data fill (%).
    #[serde(alias = "abe_max_data_percent", default = "default_abe_max_data")]
    pub abe_max_data_percent: f64,
    /// The same for thin metadata (%); a full metadata LV breaks the pool.
    #[serde(
        alias = "abe_max_metadata_percent",
        default = "default_abe_max_metadata"
    )]
    pub abe_max_metadata_percent: f64,
    /// Memory overcommit ratio. Keep at 1.0 unless you know why not.
    #[serde(alias = "mem_overcommit", default = "one")]
    pub mem_overcommit: f64,
    /// Memory (MiB) held back for the host OS / dom0-equivalent.
    #[serde(alias = "reserved_memory_mib", default = "default_reserved_mem")]
    pub reserved_memory_mib: u64,
    /// pCPUs held back for the host OS.
    #[serde(alias = "reserved_cpus", default = "default_reserved_cpu")]
    pub reserved_cpus: u32,
    /// Cordoned hosts accept no new VMs.
    #[serde(default)]
    pub cordoned: bool,
    /// Discovered capacity; filled by xdebbied's inventory loop.
    #[serde(default)]
    pub capacity: Option<Capacity>,
    /// Last successful contact with libvirtd.
    #[serde(alias = "last_seen", default)]
    pub last_seen: Option<DateTime<Utc>>,
}

fn default_fd() -> String {
    "default".into()
}
fn default_pool() -> String {
    "default".into()
}
fn default_bridge() -> String {
    "br0".into()
}
fn default_cpu_ratio() -> f64 {
    4.0
}
fn default_reserved_mem() -> u64 {
    4096
}
fn default_reserved_cpu() -> u32 {
    2
}
fn one() -> f64 {
    1.0
}
fn default_abe_max_data() -> f64 {
    85.0
}
fn default_abe_max_metadata() -> f64 {
    80.0
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Capacity {
    pub cpus: u32,
    pub memory_mib: u64,
    pub disk_free_gib: u64,
}

/// What an ABE's `GET /utilization` said about its pool (thin) or VG
/// (thick), as seen by the inventory loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AbeCapacity {
    /// `thin_pool != null` in the reply: sizes are promises, not blocks.
    pub thin: bool,
    /// Pool (or VG) size.
    pub bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    /// Sum of the virtual sizes of every volume (thin), or `used_bytes`
    /// (thick).
    pub provisioned_bytes: u64,
    pub data_percent: f64,
    #[serde(default)]
    pub metadata_percent: Option<f64>,
    /// The ABE's API is behind mutual TLS (`/version` says `"tls": true`,
    /// abe >= 0.5.0). `false` for older ABEs and until `/version` has
    /// been asked.
    #[serde(default)]
    pub tls: bool,
    pub seen: DateTime<Utc>,
}

impl AbeCapacity {
    /// Space the pool may still promise (thin: `bytes × overcommit`;
    /// thick: `bytes`).
    pub fn limit(&self, overcommit: f64) -> u64 {
        if self.thin {
            (self.bytes as f64 * overcommit.max(0.0)) as u64
        } else {
            self.bytes
        }
    }
    /// What is already promised: provisioned (thin) or used (thick).
    pub fn committed(&self) -> u64 {
        if self.thin {
            self.provisioned_bytes
        } else {
            self.bytes.saturating_sub(self.free_bytes)
        }
    }
    /// Is the reading older than `stale_after_secs`?
    pub fn is_stale(&self, now: DateTime<Utc>, stale_after_secs: i64) -> bool {
        (now - self.seen).num_seconds() > stale_after_secs
    }
}

impl Host {
    /// Would `request` more bytes fit on this host's ABE, with `pending`
    /// bytes already promised but not yet visible in its utilization?
    /// `Ok(headroom)` is what would be left afterwards; `Err` is the
    /// scheduler's reject reason.
    pub fn abe_fit(&self, pending: u64, request: u64) -> Result<u64, &'static str> {
        if self.abe.is_none() {
            return Err("no abe");
        }
        let Some(c) = &self.abe_capacity else {
            return Err("abe capacity unknown");
        };
        if c.thin {
            if c.data_percent >= self.abe_max_data_percent
                || c.metadata_percent
                    .is_some_and(|m| m >= self.abe_max_metadata_percent)
            {
                return Err("abe pool full");
            }
            let limit = c.limit(self.abe_overcommit);
            let need = c
                .provisioned_bytes
                .saturating_add(pending)
                .saturating_add(request);
            if need > limit {
                return Err("abe capacity");
            }
            Ok(limit - need)
        } else {
            let need = pending.saturating_add(request);
            if need > c.free_bytes {
                return Err("abe capacity");
            }
            Ok(c.free_bytes - need)
        }
    }

    /// Fraction of the ABE pool in use after `pending + request` more
    /// bytes (0..=1), for the spread/pack score. `None` without a fresh
    /// reading.
    pub fn abe_used_fraction(&self, pending: u64, request: u64) -> Option<f64> {
        let c = self.abe_capacity.as_ref()?;
        let need = pending.saturating_add(request);
        if c.thin {
            let limit = c.limit(self.abe_overcommit).max(1) as f64;
            Some(((c.provisioned_bytes.saturating_add(need)) as f64 / limit).min(1.0))
        } else {
            let bytes = c.bytes.max(1) as f64;
            Some((1.0 - (c.free_bytes.saturating_sub(need)) as f64 / bytes).clamp(0.0, 1.0))
        }
    }

    /// Schedulable capacity after reservations and overcommit.
    pub fn allocatable(&self) -> Option<Capacity> {
        let c = self.capacity?;
        let cpus =
            (c.cpus.saturating_sub(self.reserved_cpus) as f64 * self.cpu_overcommit).floor() as u32;
        let mem = (c.memory_mib.saturating_sub(self.reserved_memory_mib) as f64
            * self.mem_overcommit) as u64;
        Some(Capacity {
            cpus,
            memory_mib: mem,
            disk_free_gib: c.disk_free_gib,
        })
    }
    pub fn is_healthy(&self, now: DateTime<Utc>, stale_after_secs: i64) -> bool {
        self.last_seen
            .is_some_and(|t| (now - t).num_seconds() <= stale_after_secs)
    }
}

/// What a caller asks for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VmSpec {
    pub name: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub disk_gib: u64,
    /// Name of a base volume (qcow2 cloud image) present in each host's pool.
    pub image: String,
    /// Optional: Size of additional data disk (in GiB) for ABE or other storage workloads.
    /// Creates a raw volume attached as /dev/vdb.
    #[serde(default)]
    pub data_disk_gib: Option<u64>,
    #[serde(default)]
    pub placement: Placement,
    /// cloud-init user-data (for CAPI: the bootstrap secret's `value`).
    #[serde(default)]
    pub user_data: Option<String>,
    /// Extra cloud-init meta-data keys.
    #[serde(default)]
    pub meta_data: BTreeMap<String, String>,
    /// Arbitrary owner labels (cluster name, machine name...).
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Requested overlay IPv4 address. When unset the scheduler allocates one
    /// from `[network]` at creation; either way the result lands in `Vm::ip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<std::net::Ipv4Addr>,
    /// Where the disks live: the host's local pool/LVM (default) or ABE
    /// volumes exported over NVMe/TCP, which any host can attach.
    #[serde(default, skip_serializing_if = "Storage::is_local")]
    pub storage: Storage,
}

impl VmSpec {
    /// Returns true if this VM is protected from deletion (label xdebbie.io/protected=true).
    pub fn is_protected(&self) -> bool {
        self.labels
            .get(PROTECTED_LABEL)
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    #[default]
    Local,
    Abe,
}

impl Storage {
    pub fn is_local(&self) -> bool {
        *self == Storage::Local
    }
}

impl std::str::FromStr for Storage {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "local" => Ok(Storage::Local),
            "abe" => Ok(Storage::Abe),
            other => Err(format!("unknown storage {other:?} (local|abe)")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiskRole {
    Root,
    Data,
    /// A standalone [`Volume`] hot-plugged into the VM; owned by its own
    /// record, never deleted with the VM.
    Volume,
}

/// A network block volume backing one of the VM's disks. Allocated once at
/// provisioning time and kept for the VM's life; the id is the NVMe
/// subsystem NQN on the exporting host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub role: DiskRole,
    /// ABE API that owns the volume (for re-export / delete).
    pub server: String,
    /// NVMe/TCP transport address and service id the exporter advertises.
    pub traddr: String,
    pub port: String,
    /// Volume id == NQN (32 hex chars).
    pub id: String,
    pub bytes: u64,
    /// Root disks are filled from the base image once; this records that it
    /// happened so an interrupted provision never boots a blank disk.
    #[serde(default)]
    pub populated: bool,
    /// Root disks that are thin clones: the sealed base volume (on the same
    /// ABE) this one was snapshotted from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl Disk {
    pub fn nqn(&self) -> &str {
        &self.id
    }
    /// Stable host path udev creates from the namespace's device_uuid.
    pub fn dev_path(&self) -> String {
        let id = &self.id;
        if id.len() == 32 {
            format!(
                "/dev/disk/by-id/nvme-uuid.{}-{}-{}-{}-{}",
                &id[0..8],
                &id[8..12],
                &id[12..16],
                &id[16..20],
                &id[20..32]
            )
        } else {
            format!("/dev/disk/by-id/nvme-uuid.{id}")
        }
    }
}

/// Length of the virtio-blk serial a guest can see (`VIRTIO_BLK_ID_BYTES`).
pub const VIRTIO_SERIAL_LEN: usize = 20;

/// fio numbers ABE measured when it made the volume (`perf` in its reply,
/// abe >= 0.3.5): random direct writes at `bs` with `iodepth` in flight
/// for `seconds`; `write_lat_us` is the mean completion latency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VolumePerf {
    pub bs: String,
    pub iodepth: u32,
    pub seconds: f64,
    pub write_mib_s: f64,
    pub write_iops: f64,
    pub write_lat_us: f64,
}

/// Deserialize `Option<T>`, yielding `None` for anything that is not a `T`.
fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

/// A standalone ABE volume with its own lifecycle: created and deleted
/// through the API, hot-plugged into a VM as a virtio disk on request
/// (`attachTo` is the wish, `attached` is what the reconciler did). This is
/// how guests get block storage without a route to ABE: the host does the
/// NVMe/TCP, the guest sees `/dev/disk/by-id/virtio-<serial>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    /// ABE volume id == NQN (32 hex chars).
    pub id: String,
    /// Caller's name (e.g. a PV name); unique among live volumes.
    #[serde(default)]
    pub name: String,
    /// ABE API that owns the volume.
    pub server: String,
    pub traddr: String,
    pub port: String,
    pub bytes: u64,
    /// Lenient on read: a `perf` written by another abe/xdebbie version
    /// becomes `None` rather than making the whole state file unreadable.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub perf: Option<VolumePerf>,
    /// Desired: the VM this volume should be plugged into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_to: Option<Uuid>,
    /// Observed: where it is plugged in right now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// A `DELETE` has started: ABE has been (or is being) asked to remove
    /// it. Never attached again; a retried `DELETE` finishes the job.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleting: bool,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub vm: Uuid,
    pub host: String,
    /// libvirt target (`vdc`, ...).
    pub target: String,
}

impl Volume {
    /// The volume as a host-side disk, for the NVMe initiator code.
    pub fn disk(&self) -> Disk {
        Disk {
            role: DiskRole::Volume,
            server: self.server.clone(),
            traddr: self.traddr.clone(),
            port: self.port.clone(),
            id: self.id.clone(),
            bytes: self.bytes,
            populated: true,
            origin: None,
        }
    }
    /// virtio-blk serial: the id truncated to what the guest can read, so
    /// the guest finds the disk at `/dev/disk/by-id/virtio-<serial>`.
    pub fn serial(&self) -> &str {
        &self.id[..self.id.len().min(VIRTIO_SERIAL_LEN)]
    }
    pub fn guest_dev_path(&self) -> String {
        format!("/dev/disk/by-id/virtio-{}", self.serial())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    /// Hard requirement: only hosts in this failure domain.
    #[serde(default)]
    pub failure_domain: Option<String>,
    /// Soft anti-affinity: VMs sharing a group are spread across hosts,
    /// then across failure domains.
    #[serde(default)]
    pub anti_affinity_group: Option<String>,
    /// Hard anti-affinity: never two VMs of the group on one host.
    #[serde(default)]
    pub strict_anti_affinity: bool,
    /// Hard requirement: host must carry all of these labels.
    #[serde(default)]
    pub required_labels: BTreeMap<String, String>,
    /// Require a control-plane-eligible host.
    #[serde(default)]
    pub control_plane: bool,
    /// Pin to a specific host (escape hatch).
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum VmPhase {
    /// Accepted, not yet placed.
    Pending,
    /// Placed; disk/domain being created on the host.
    Provisioning,
    /// Domain running.
    Running,
    /// Domain defined but shut off (user requested stop).
    Stopped,
    /// Live migration in progress.
    Migrating,
    /// Deletion requested; resources being torn down.
    Deleting,
    /// Terminal failure; see `message`.
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum PowerIntent {
    On,
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Vm {
    pub id: Uuid,
    pub spec: VmSpec,
    pub phase: VmPhase,
    pub power: PowerIntent,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub addresses: Vec<Address>,
    /// Overlay IPv4 address bound to this VM's MAC for its whole life. The
    /// built-in DHCP responder answers `mac()` with exactly this address, so
    /// the (MAC, IP) pair is part of the VM record rather than a lease.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<std::net::Ipv4Addr>,
    /// Network block volumes backing this VM (empty for local storage).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<Disk>,
    #[serde(default)]
    pub message: Option<String>,
    /// Set when a migration has been requested.
    #[serde(default)]
    pub migrate_to: Option<String>,
    /// Incremented on every reboot request; the reconciler acts on changes.
    #[serde(default)]
    pub reboot_generation: u64,
    #[serde(default)]
    pub observed_reboot_generation: u64,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

impl Vm {
    /// libvirt domain name: stable, unique, human-readable.
    pub fn domain_name(&self) -> String {
        self.spec.name.clone()
    }
    /// Cluster API providerID.
    pub fn provider_id(&self) -> String {
        format!("xdebbie://{}", self.id)
    }
    /// Locally-administered, deterministic MAC derived from the VM id.
    pub fn mac(&self) -> String {
        let b = self.id.as_bytes();
        format!("52:54:00:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2])
    }
    /// `mac()` as bytes, for comparing against DHCP `chaddr`.
    pub fn mac_bytes(&self) -> [u8; 6] {
        let b = self.id.as_bytes();
        [0x52, 0x54, 0x00, b[0], b[1], b[2]]
    }
    /// Does this VM count against a host's capacity?
    pub fn consumes_capacity(&self) -> bool {
        self.host.is_some() && !matches!(self.phase, VmPhase::Pending | VmPhase::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    /// Mirrors CAPI MachineAddressType: InternalIP, ExternalIP, Hostname, ...
    #[serde(rename = "type")]
    pub kind: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FailureDomain {
    pub name: String,
    pub control_plane: bool,
    pub hosts: Vec<String>,
}

/// Request body for `PUT /v1/vms/{id}`.
pub type CreateVm = VmSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrateRequest {
    /// Destination host; omit to let the scheduler choose.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

/// `GET /v1/leader`: who decides for this cluster. Clients talk to
/// `leader.api_url`; followers forward `/v1` writes and reads there anyway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeaderStatus {
    /// The box that answered.
    pub this: LeaderRef,
    /// `standalone` (no discovery), `follower`, `election`, `leader`.
    pub state: String,
    pub is_leader: bool,
    /// `None` while an election is running.
    pub leader: Option<LeaderRef>,
    /// Peers this box currently sees (excluding itself).
    pub peers: usize,
}

/// `GET /v1/token`: which bearer tokens this box accepts, by fingerprint
/// (first 12 hex of SHA-256; the tokens themselves never leave a box
/// except in `PUT /v1/token`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenStatus {
    /// Fingerprint of the current token.
    pub id: String,
    /// Fingerprint of the previous token, still accepted while a rotation
    /// is being distributed.
    pub previous: Option<String>,
    /// A request has authenticated with the current token on this box.
    pub current_used: bool,
    /// A rotation is in progress (the previous token is still accepted).
    pub rotating: bool,
    /// What the leader last saw on each peer; empty on followers.
    #[serde(default)]
    pub nodes: Vec<TokenNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenNode {
    pub hostname: String,
    pub api_url: String,
    /// `current`: accepts the current token (and nothing older);
    /// `overlap`: accepts current and previous; `installing`: new set
    /// pushed, not yet confirmed; `stale`: rejects every token we hold;
    /// `unreachable: <why>`.
    pub state: String,
}

/// `PUT /v1/token`: the set a box must accept. Sent by the leader.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenSet {
    pub current: String,
    #[serde(default)]
    pub previous: Option<String>,
}

/// `POST /v1/token/rotate` reply. The only time a token is shown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenRotated {
    pub token: String,
    pub id: String,
    /// Fingerprint of the token being retired.
    pub previous: String,
}

/// `POST /v1/enroll` body: a box asking the cluster CA to sign its API
/// certificate. The private key never leaves the joiner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnrollRequest {
    pub hostname: String,
    /// Mesh box id (0-253) the joiner would like; `None` = CA box picks
    /// the lowest free one. Either way the answer is in `Enrollment.box_id`.
    #[serde(rename = "box", default, skip_serializing_if = "Option::is_none")]
    pub box_id: Option<u8>,
    /// Where the joiner's API will be once it starts.
    pub api_url: String,
    /// PEM certificate signing request.
    pub csr: String,
    /// WireGuard public key, so an operator can `mesh peer add` straight
    /// from the enrolment list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wg_pubkey: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EnrollStatus {
    Pending,
    Approved,
    Denied,
}

impl std::fmt::Display for EnrollStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EnrollStatus::Pending => "pending",
            EnrollStatus::Approved => "approved",
            EnrollStatus::Denied => "denied",
        })
    }
}

/// One enrolment as the CA box reports it (`GET /v1/enroll`,
/// `GET /v1/enroll/{id}`). `cert` and `ca` are filled in once approved; the
/// joiner installs them and restarts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Enrollment {
    pub id: String,
    pub hostname: String,
    /// Mesh box id assigned to this box (bridge MAC, underlay address).
    #[serde(rename = "box")]
    pub box_id: u8,
    pub api_url: String,
    /// `sha256:<hex>` of the requested public key; compare it with what
    /// `xdebbied bootstrap --join` printed before approving.
    pub fingerprint: String,
    pub status: EnrollStatus,
    pub requested_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wg_pubkey: Option<String>,
    /// PEM certificate issued by the cluster CA (approved only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    /// PEM cluster CA certificate (approved only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeaderRef {
    /// Election identity; `None` when running standalone.
    pub node_id: Option<Uuid>,
    pub hostname: String,
    pub api_url: String,
}

#[cfg(test)]
mod disk_tests {
    use super::*;

    #[test]
    fn dev_path_is_the_dashed_uuid_by_id_link() {
        let d = Disk {
            role: DiskRole::Root,
            server: "http://10.22.0.1".into(),
            traddr: "10.22.0.1".into(),
            port: "44001".into(),
            id: "0123456789abcdef0123456789abcdef".into(),
            bytes: 1,
            populated: false,
            origin: None,
        };
        assert_eq!(
            d.dev_path(),
            "/dev/disk/by-id/nvme-uuid.01234567-89ab-cdef-0123-456789abcdef"
        );
        assert_eq!(d.nqn(), "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn volume_serial_is_what_virtio_blk_can_carry() {
        let now = Utc::now();
        let v = Volume {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "pvc-1".into(),
            server: "http://10.22.0.1".into(),
            traddr: "10.22.0.1".into(),
            port: "44002".into(),
            bytes: 1 << 30,
            perf: None,
            attach_to: None,
            attached: None,
            message: None,
            deleting: false,
            created: now,
            updated: now,
        };
        assert_eq!(v.serial(), "0123456789abcdef0123");
        assert_eq!(
            v.guest_dev_path(),
            "/dev/disk/by-id/virtio-0123456789abcdef0123"
        );
        assert_eq!(v.disk().role, DiskRole::Volume);
        assert_eq!(
            v.disk().dev_path(),
            Disk {
                role: DiskRole::Root,
                ..v.disk()
            }
            .dev_path()
        );
        let j = serde_json::to_value(&v).unwrap();
        assert!(j.get("attachTo").is_none(), "unset fields are omitted");
    }

    #[test]
    fn storage_defaults_to_local_and_round_trips() {
        let v: serde_json::Value = serde_json::from_str(r#"{"storage":"abe"}"#).unwrap();
        assert_eq!(
            serde_json::from_value::<Storage>(v["storage"].clone()).unwrap(),
            Storage::Abe
        );
        assert_eq!("local".parse::<Storage>().unwrap(), Storage::Local);
        assert!("nfs".parse::<Storage>().is_err());
        assert_eq!(Storage::default(), Storage::Local);
    }

    #[test]
    fn is_protected_label() {
        let mut spec = VmSpec {
            name: "test".into(),
            vcpus: 1,
            memory_mib: 512,
            disk_gib: 10,
            image: "img".into(),
            data_disk_gib: None,
            placement: Default::default(),
            user_data: None,
            meta_data: Default::default(),
            labels: Default::default(),
            ip: None,
            storage: Storage::Local,
        };
        assert!(!spec.is_protected());

        spec.labels.insert(PROTECTED_LABEL.into(), "true".into());
        assert!(spec.is_protected());

        spec.labels.insert(PROTECTED_LABEL.into(), "TRUE".into());
        assert!(spec.is_protected());

        spec.labels.insert(PROTECTED_LABEL.into(), "false".into());
        assert!(!spec.is_protected());

        spec.labels.insert(PROTECTED_LABEL.into(), "".into());
        assert!(!spec.is_protected());
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;

    #[test]
    fn perf_from_another_version_reads_as_none() {
        // Written by xdebbie 0.9.1 (abe 0.3.4): no `iodepth`, `write_lat_ms`.
        let old = serde_json::json!({
            "id": "00a108b720bd4d3ab4113ebbfeee1f64", "name": "pvc-1", "server": "http://10.22.0.1:80",
            "traddr": "10.22.0.1", "port": "44001", "bytes": 5368709120u64,
            "created": "2026-11-12T01:00:00Z", "updated": "2026-11-12T01:00:00Z",
            "perf": {"bs": "4M", "seconds": 5.0, "write_mib_s": 1022.0, "write_iops": 255.0, "write_lat_ms": 31.0, "lat_4k_us": 187.0}
        });
        let v: Volume = serde_json::from_value(old).expect("old perf must not break the volume");
        assert_eq!(v.perf, None);

        let new = serde_json::json!({
            "id": "x", "name": "pvc-2", "server": "s", "traddr": "t", "port": "p", "bytes": 1,
            "created": "2026-11-12T01:00:00Z", "updated": "2026-11-12T01:00:00Z",
            "perf": {"bs": "4k", "iodepth": 1, "seconds": 5.0, "write_mib_s": 19.5, "write_iops": 5000.0, "write_lat_us": 187.5}
        });
        let v: Volume = serde_json::from_value(new).unwrap();
        assert_eq!(v.perf.as_ref().map(|p| p.iodepth), Some(1));

        let none = serde_json::json!({"id": "x", "name": "pvc-3", "server": "s", "traddr": "t", "port": "p", "bytes": 1,
            "created": "2026-11-12T01:00:00Z", "updated": "2026-11-12T01:00:00Z"});
        assert_eq!(serde_json::from_value::<Volume>(none).unwrap().perf, None);
    }
}

#[cfg(test)]
mod abe_capacity_tests {
    use super::*;

    fn cap(thin: bool) -> AbeCapacity {
        AbeCapacity {
            thin,
            bytes: 100 << 30,
            used_bytes: 10 << 30,
            free_bytes: 90 << 30,
            provisioned_bytes: 60 << 30,
            data_percent: 10.0,
            metadata_percent: Some(5.0),
            tls: false,

            seen: Utc::now(),
        }
    }
    fn host(cap: Option<AbeCapacity>) -> Host {
        let mut h: Host =
            serde_json::from_value(serde_json::json!({"name": "a", "uri": "test:///"})).unwrap();
        h.abe = Some("http://10.22.1.1:80".into());
        h.abe_capacity = cap;
        h
    }

    #[test]
    fn no_abe_and_unknown_capacity_are_distinct_reasons() {
        let mut h = host(Some(cap(true)));
        h.abe = None;
        assert_eq!(h.abe_fit(0, 1), Err("no abe"));
        assert_eq!(host(None).abe_fit(0, 1), Err("abe capacity unknown"));
    }

    #[test]
    fn thin_pool_checks_fill_then_provisioned_against_overcommit() {
        let h = host(Some(cap(true)));
        // 60 provisioned + 0 pending + 40 == 100: fits exactly, no headroom
        assert_eq!(h.abe_fit(0, 40 << 30), Ok(0));
        assert_eq!(h.abe_fit(0, 41 << 30), Err("abe capacity"));
        // pending counts
        assert_eq!(h.abe_fit(10 << 30, 30 << 30), Ok(0));
        assert_eq!(h.abe_fit(10 << 30, 31 << 30), Err("abe capacity"));
        // overcommit raises the limit
        let mut o = h.clone();
        o.abe_overcommit = 2.0;
        assert_eq!(o.abe_fit(0, 100 << 30), Ok(40 << 30));
        // a full pool refuses regardless of promises
        let mut full = h.clone();
        full.abe_capacity.as_mut().unwrap().data_percent = 85.0;
        assert_eq!(full.abe_fit(0, 1), Err("abe pool full"));
        let mut meta = h.clone();
        meta.abe_capacity.as_mut().unwrap().metadata_percent = Some(80.0);
        assert_eq!(meta.abe_fit(0, 1), Err("abe pool full"));
        let mut lax = meta.clone();
        lax.abe_max_metadata_percent = 90.0;
        assert!(lax.abe_fit(0, 1).is_ok());
        // thin metadata unknown: only the data threshold applies
        let mut nometa = h.clone();
        nometa.abe_capacity.as_mut().unwrap().metadata_percent = None;
        assert!(nometa.abe_fit(0, 1).is_ok());
    }

    #[test]
    fn thick_pool_uses_free_bytes_and_ignores_overcommit() {
        let mut h = host(Some(cap(false)));
        h.abe_overcommit = 4.0;
        assert_eq!(h.abe_fit(0, 90 << 30), Ok(0));
        assert_eq!(h.abe_fit(1, 90 << 30), Err("abe capacity"));
        assert_eq!(h.abe_fit(40 << 30, 40 << 30), Ok(10 << 30));
        // fill thresholds do not apply to thick pools
        h.abe_capacity.as_mut().unwrap().data_percent = 99.0;
        assert!(h.abe_fit(0, 1).is_ok());
    }

    #[test]
    fn used_fraction_feeds_the_score() {
        let h = host(Some(cap(true)));
        assert!((h.abe_used_fraction(0, 20 << 30).unwrap() - 0.8).abs() < 1e-9);
        let t = host(Some(cap(false)));
        // 90 free - 40 = 50 free of 100 -> 0.5 used
        assert!((t.abe_used_fraction(0, 40 << 30).unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(host(None).abe_used_fraction(0, 1), None);
    }

    #[test]
    fn staleness_is_by_seen() {
        let mut c = cap(true);
        c.seen = Utc::now() - chrono::Duration::seconds(100);
        assert!(c.is_stale(Utc::now(), 60));
        assert!(!c.is_stale(Utc::now(), 120));
    }

    #[test]
    fn config_keys_take_snake_case() {
        let h: Host = toml::from_str(
            "name = \"t\"\nuri = \"qemu:///system\"\nabe = \"http://10.22.1.1:80\"\nabe_overcommit = 2.5\nabe_max_data_percent = 70\nabe_max_metadata_percent = 60\n",
        )
        .unwrap();
        assert_eq!(h.abe_overcommit, 2.5);
        assert_eq!(h.abe_max_data_percent, 70.0);
        assert_eq!(h.abe_max_metadata_percent, 60.0);
        assert_eq!(h.abe_capacity, None);
    }
}

#[cfg(test)]
mod compat_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The state document as written by xdebbie 0.14: hosts without any
    /// `abe*` capacity keys, disks without `origin`, volumes without
    /// `deleting`. It must load unchanged and survive a save/load cycle.
    #[test]
    fn v0_14_state_loads_and_round_trips() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct State {
            hosts: BTreeMap<String, Host>,
            vms: BTreeMap<Uuid, Vm>,
            #[serde(default)]
            volumes: BTreeMap<String, Volume>,
        }
        let doc = serde_json::json!({
          "hosts": {
            "timesink": {
              "name": "timesink", "uri": "qemu:///system", "failureDomain": "default",
              "controlPlaneEligible": true, "labels": {}, "storagePool": "default", "bridge": "virbr-xdebbie",
              "cpuOvercommit": 4.0, "abe": "http://10.22.0.1:80", "memOvercommit": 1.0,
              "reservedMemoryMib": 4096, "reservedCpus": 2, "cordoned": false,
              "capacity": {"cpus": 32, "memoryMib": 128000, "diskFreeGib": 800},
              "lastSeen": "2026-11-12T10:00:00Z"
            }
          },
          "vms": {
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8": {
              "id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
              "spec": {"name": "w0", "vcpus": 2, "memoryMib": 2048, "diskGib": 10, "image": "debian-13",
                       "dataDiskGib": 4, "placement": {}, "storage": "abe"},
              "phase": "Running", "power": "On", "host": "timesink",
              "addresses": [{"type": "InternalIP", "address": "10.23.0.5"}],
              "ip": "10.23.0.5",
              "disks": [
                {"role": "root", "server": "http://10.22.0.1:80", "traddr": "10.22.0.1", "port": "44001",
                 "id": "0123456789abcdef0123456789abcdef", "bytes": 10737418240u64, "populated": true},
                {"role": "data", "server": "http://10.22.0.1:80", "traddr": "10.22.0.1", "port": "44002",
                 "id": "fedcba9876543210fedcba9876543210", "bytes": 4294967296u64, "populated": true}
              ],
              "message": null, "migrateTo": null, "rebootGeneration": 0, "observedRebootGeneration": 0,
              "created": "2026-11-12T10:00:00Z", "updated": "2026-11-12T10:00:00Z"
            }
          },
          "volumes": {
            "00a108b720bd4d3ab4113ebbfeee1f64": {
              "id": "00a108b720bd4d3ab4113ebbfeee1f64", "name": "pvc-1", "server": "http://10.22.0.1:80",
              "traddr": "10.22.0.1", "port": "44003", "bytes": 5368709120u64,
              "perf": {"bs": "4k", "iodepth": 1, "seconds": 5.0, "write_mib_s": 19.5, "write_iops": 5000.0, "write_lat_us": 187.5},
              "attachTo": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
              "attached": {"vm": "6ba7b810-9dad-11d1-80b4-00c04fd430c8", "host": "timesink", "target": "vdc"},
              "created": "2026-11-12T10:00:00Z", "updated": "2026-11-12T10:00:00Z"
            }
          }
        });
        let st: State = serde_json::from_value(doc.clone()).expect("v0.14 state must load");
        let h = &st.hosts["timesink"];
        assert_eq!(h.abe_capacity, None);
        assert_eq!(h.abe_overcommit, 1.0);
        assert_eq!(h.abe_max_data_percent, 85.0);
        assert_eq!(h.abe_max_metadata_percent, 80.0);
        let vm = st.vms.values().next().unwrap();
        assert!(vm.disks.iter().all(|d| d.origin.is_none()));
        let vol = st.volumes.values().next().unwrap();
        assert!(!vol.deleting);
        assert!(vol.perf.is_some());

        // Round trip: what we write reads back identical, and the new
        // fields stay out of the document while unset.
        let again = serde_json::to_value(&st).unwrap();
        let st2: State = serde_json::from_value(again.clone()).unwrap();
        assert_eq!(st, st2);
        let hj = &again["hosts"]["timesink"];
        assert!(hj.get("abeCapacity").is_none());
        assert_eq!(hj["abeOvercommit"], 1.0);
        assert!(
            again["vms"].as_object().unwrap().values().next().unwrap()["disks"][0]
                .get("origin")
                .is_none()
        );
        assert!(again["volumes"]["00a108b720bd4d3ab4113ebbfeee1f64"]
            .get("deleting")
            .is_none());
    }
}
