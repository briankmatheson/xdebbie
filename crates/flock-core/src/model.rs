// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire and storage types shared by `flockd`, `flockctl`, and `capf`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A physical (or nested) Linux box running libvirtd + QEMU/KVM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Host {
    /// Unique short name, e.g. `node-a`.
    pub name: String,
    /// libvirt connection URI, e.g. `qemu+ssh://flock@10.0.0.11/system`.
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
    /// Discovered capacity; filled by flockd's inventory loop.
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
        format!(
            "flock-{}-{}",
            self.spec.name,
            &self.id.simple().to_string()[..8]
        )
    }
    /// Cluster API providerID.
    pub fn provider_id(&self) -> String {
        format!("flock://{}", self.id)
    }
    /// Locally-administered, deterministic MAC derived from the VM id.
    pub fn mac(&self) -> String {
        let b = self.id.as_bytes();
        format!("52:54:00:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2])
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
