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

impl Host {
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
                &id[0..8], &id[8..12], &id[12..16], &id[16..20], &id[20..32]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<VolumePerf>,
    /// Desired: the VM this volume should be plugged into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_to: Option<Uuid>,
    /// Observed: where it is plugged in right now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
            created: now,
            updated: now,
        };
        assert_eq!(v.serial(), "0123456789abcdef0123");
        assert_eq!(v.guest_dev_path(), "/dev/disk/by-id/virtio-0123456789abcdef0123");
        assert_eq!(v.disk().role, DiskRole::Volume);
        assert_eq!(v.disk().dev_path(), Disk { role: DiskRole::Root, ..v.disk() }.dev_path());
        let j = serde_json::to_value(&v).unwrap();
        assert!(j.get("attachTo").is_none(), "unset fields are omitted");
    }

    #[test]
    fn storage_defaults_to_local_and_round_trips() {
        let v: serde_json::Value = serde_json::from_str(r#"{"storage":"abe"}"#).unwrap();
        assert_eq!(serde_json::from_value::<Storage>(v["storage"].clone()).unwrap(), Storage::Abe);
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
