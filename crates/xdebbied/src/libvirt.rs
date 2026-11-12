// SPDX-License-Identifier: GPL-3.0-or-later
//! libvirt driver. Talks to each host with `virsh -c qemu+ssh://...`, so the
//! only thing a host needs is libvirtd, QEMU/KVM, an SSH key for the xdebbie
//! user, a storage pool holding base images, and a bridge.

use std::{collections::{BTreeMap, HashMap}, future::Future, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use xdebbie_core::{Address, Attachment, Capacity, Disk, DiskRole, Host, Storage, Vm, Volume};
use tokio::process::Command;

use crate::{abe, lvm::LvmManager};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomState {
    Running,
    Off,
    Paused,
    Other,
}

/// Everything the reconciler needs from a hypervisor. A trait so tests can
/// drive the reconciler without real hosts.
pub trait Hypervisor: Send + Sync + 'static {
    fn inventory(&self, host: &Host) -> impl Future<Output = Result<Capacity>> + Send;
    fn state(&self, host: &Host, vm: &Vm) -> impl Future<Output = Result<Option<DomState>>> + Send;
    /// Allocate and attach the VM's network disks (`storage = abe`) before
    /// `provision`. Appends to `disks` as it goes, so the reconciler can
    /// persist partial progress and a retry never allocates twice.
    fn prepare_disks(
        &self,
        host: &Host,
        vm: &Vm,
        disks: &mut Vec<Disk>,
    ) -> impl Future<Output = Result<()>> + Send {
        let _ = (host, vm, disks);
        async { Ok(()) }
    }
    fn provision(&self, host: &Host, vm: &Vm) -> impl Future<Output = Result<()>> + Send;
    fn start(&self, host: &Host, vm: &Vm) -> impl Future<Output = Result<()>> + Send;
    fn shutdown(
        &self,
        host: &Host,
        vm: &Vm,
        force: bool,
    ) -> impl Future<Output = Result<()>> + Send;
    fn reboot(&self, host: &Host, vm: &Vm) -> impl Future<Output = Result<()>> + Send;
    fn destroy(&self, host: &Host, vm: &Vm) -> impl Future<Output = Result<()>> + Send;
    fn addresses(&self, host: &Host, vm: &Vm) -> impl Future<Output = Result<Vec<Address>>> + Send;
    /// Host-side tap device name → domain name for every running domain.
    /// Used by the mesh to name VMs whose NIC MTU is stale.
    fn tap_owners(&self, host: &Host) -> impl Future<Output = Result<HashMap<String, String>>> + Send;
    fn migrate(&self, src: &Host, dst: &Host, vm: &Vm) -> impl Future<Output = Result<()>> + Send;
    /// Make `vol` a virtio disk of `vm` (host-side NVMe connect + libvirt
    /// hot-plug). Idempotent: called every tick while the attachment is
    /// wanted, so a host reboot or a recreated domain heals itself.
    /// Returns the libvirt target (`vdc`, ...).
    fn attach_volume(&self, host: &Host, vm: &Vm, vol: &Volume) -> impl Future<Output = Result<String>> + Send {
        let _ = (host, vm, vol);
        async { Ok("vdc".to_string()) }
    }
    /// Unplug `vol` from the domain (which may already be gone) and drop
    /// the host-side NVMe connection.
    fn detach_volume(&self, host: &Host, vol: &Volume, att: &Attachment) -> impl Future<Output = Result<()>> + Send {
        let _ = (host, vol, att);
        async { Ok(()) }
    }
}

#[derive(Clone)]
pub struct Virsh {
    pub bin: String,
    pub seed_command: Vec<String>,
    pub lvm: Option<LvmManager>,
    /// Geneve mesh view: MTU for NICs on the overlay bridge, if managed.
    pub overlay: Arc<crate::mesh::Overlay>,
    /// ABE API client and host-side NVMe ops for `storage = abe` VMs.
    pub abe: abe::Client,
    pub nvme: abe::Nvme,
}

impl Virsh {
    async fn run(&self, uri: &str, args: &[&str]) -> Result<String> {
        let out = Command::new(&self.bin)
            .arg("-q")
            .arg("-c")
            .arg(uri)
            .args(args)
            .env("LC_ALL", "C")
            .kill_on_drop(true)
            .output()
            .await
            .with_context(|| format!("spawning {}", self.bin))?;
        if !out.status.success() {
            bail!(
                "virsh {} on {uri}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Start any domains that are marked autostart but are shut off.
    ///
    /// libvirt's own autostart runs when the daemon starts, which can be
    /// before LVM thin volumes are active (e.g. after a host reboot). Those
    /// domains fail to start and stay off. We get a second chance here, after
    /// `LvmManager::ensure_activated` has run.
    pub async fn start_autostart_domains(&self, host: &Host) -> Result<usize> {
        let out = self
            .run(&host.uri, &["list", "--name", "--autostart", "--inactive"])
            .await?;
        let mut started = 0;
        for name in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match self.run(&host.uri, &["start", name]).await {
                Ok(_) => {
                    tracing::info!("started autostart domain {} on {}", name, host.name);
                    started += 1;
                }
                Err(e) => {
                    tracing::warn!("could not start autostart domain {} on {}: {:#}", name, host.name, e)
                }
            }
        }
        Ok(started)
    }

    async fn vol_exists(&self, host: &Host, name: &str) -> bool {
        self.run(&host.uri, &["vol-info", "--pool", &host.storage_pool, name])
            .await
            .is_ok()
    }

    fn disk_vol(vm: &Vm) -> String {
        format!("{}.qcow2", vm.domain_name())
    }
    fn seed_vol(vm: &Vm) -> String {
        format!("{}-seed.iso", vm.domain_name())
    }
    fn data_vol(vm: &Vm) -> String {
        format!("{}-data.raw", vm.domain_name())
    }

    /// Thin qcow2 overlay on the host-local base image, or LVM snapshot if configured.
    async fn ensure_disk(&self, host: &Host, vm: &Vm) -> Result<()> {
        if vm.spec.storage == Storage::Abe {
            return Ok(()); // prepare_disks did it
        }
        // Use LVM if configured
        if let Some(ref lvm) = self.lvm {
            let _device = lvm.create_root_disk(&vm.domain_name(), &vm.spec.image, vm.spec.disk_gib)
                .map_err(|e| anyhow::anyhow!("creating LVM root disk: {:#}", e))?;
            return Ok(());
        }
        
        // Fallback to qcow2
        let vol = Self::disk_vol(vm);
        if self.vol_exists(host, &vol).await {
            return Ok(());
        }
        let size = format!("{}G", vm.spec.disk_gib);
        let backing_vol = format!("{}.qcow2", vm.spec.image);
        self.run(
            &host.uri,
            &[
                "vol-create-as",
                &host.storage_pool,
                &vol,
                &size,
                "--format",
                "qcow2",
                "--backing-vol",
                &backing_vol,
                "--backing-vol-format",
                "qcow2",
            ],
        )
        .await
        .context("creating root disk (is the base image in the pool?)")?;
        Ok(())
    }

    /// Where the base image lives on this host and in what format, for
    /// filling an ABE root disk.
    async fn base_image(&self, host: &Host, image: &str) -> Result<(String, &'static str)> {
        if let Some(ref lvm) = self.lvm {
            let lv = if image.starts_with("base-") { image.to_string() } else { format!("base-{image}") };
            let path = format!("/dev/{}/{lv}", lvm.vg_name());
            if !Path::new(&path).exists() {
                bail!("base image {path} not found (import it with lvm-import-base)");
            }
            return Ok((path, "raw"));
        }
        let path = self
            .run(&host.uri, &["vol-path", "--pool", &host.storage_pool, &format!("{image}.qcow2")])
            .await
            .context("locating base image in the pool")?;
        Ok((path.trim().to_string(), "qcow2"))
    }

    /// Attach every disk of an ABE-backed VM on this host (after a host
    /// reboot the domain still exists but its devices do not).
    async fn ensure_connected(&self, vm: &Vm) -> Result<()> {
        for d in &vm.disks {
            self.nvme.connect(d).await?;
            self.nvme.wait_dev(d, std::time::Duration::from_secs(15)).await?;
        }
        Ok(())
    }

    /// Detach and delete every disk. All of them are attempted; the first
    /// error is returned so the reconciler retries.
    async fn release_disks(&self, vm: &Vm) -> Result<()> {
        let mut first_err = None;
        for d in &vm.disks {
            let r = async {
                self.nvme.disconnect(d.nqn()).await?;
                self.abe.delete(&d.server, &d.id).await
            }
            .await;
            if let Err(e) = r {
                tracing::warn!(vm = %vm.spec.name, disk = ?d.role, id = %d.id, "releasing disk: {e:#}");
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Build a NoCloud seed locally and upload it through libvirt.
    async fn ensure_seed(&self, host: &Host, vm: &Vm) -> Result<()> {
        let vol = Self::seed_vol(vm);
        if self.vol_exists(host, &vol).await {
            return Ok(());
        }
        let dir = tempfile::tempdir()?;
        let (user, meta, net, vendor, out) = (
            dir.path().join("user-data"),
            dir.path().join("meta-data"),
            dir.path().join("network-config"),
            dir.path().join("vendor-data"),
            dir.path().join("seed.iso"),
        );
        tokio::fs::write(
            &user,
            vm.spec
                .user_data
                .as_deref()
                .unwrap_or("#cloud-config\n{}\n"),
        )
        .await?;
        tokio::fs::write(&meta, meta_data(vm)).await?;
        tokio::fs::write(&net, network_config(vm, self.overlay.mtu_for(&host.bridge))).await?;
        tokio::fs::write(&vendor, vendor_data()).await?;
        let sub = |s: &str| {
            s.replace("{out}", p(&out))
                .replace("{user}", p(&user))
                .replace("{meta}", p(&meta))
                .replace("{net}", p(&net))
                .replace("{vendor}", p(&vendor))
        };
        let mut argv: Vec<String> = self.seed_command.iter().map(|a| sub(a)).collect();
        // Older configs predate `{vendor}`. If the builder is cloud-localds,
        // inject the flag so those installs still get vendor-data.
        let has_vendor = self.seed_command.iter().any(|a| a.contains("{vendor}"));
        let is_localds = argv
            .first()
            .map(|b| b.ends_with("cloud-localds"))
            .unwrap_or(false);
        if !has_vendor && is_localds {
            argv.insert(1, format!("--vendor-data={}", p(&vendor)));
        }
        let st = Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .await
            .with_context(|| format!("running seed builder {}", argv[0]))?;
        if !st.success() {
            bail!("seed builder failed: {st}");
        }
        let len = tokio::fs::metadata(&out).await?.len().to_string();
        self.run(
            &host.uri,
            &[
                "vol-create-as",
                &host.storage_pool,
                &vol,
                &len,
                "--format",
                "raw",
            ],
        )
        .await?;
        self.run(
            &host.uri,
            &["vol-upload", "--pool", &host.storage_pool, &vol, p(&out)],
        )
        .await?;
        Ok(())
    }

    /// Create a raw data disk if vm.spec.data_disk_gib is set.
    /// This is used for ABE and other storage workloads.
    /// Uses LVM thin provisioning if configured, otherwise creates a volume in the storage pool.
    async fn ensure_data_disk(&self, host: &Host, vm: &Vm) -> Result<()> {
        let Some(size_gib) = vm.spec.data_disk_gib else {
            return Ok(()); // No data disk requested
        };
        if vm.spec.storage == Storage::Abe {
            return Ok(()); // prepare_disks did it
        }
        
        // Use LVM if configured
        if let Some(ref lvm) = self.lvm {
            let _device = lvm.create_data_disk(&vm.domain_name(), size_gib)
                .context("creating LVM data disk")?;
            return Ok(());
        }
        
        // Fallback to storage pool volume
        let vol = Self::data_vol(vm);
        if self.vol_exists(host, &vol).await {
            return Ok(());
        }
        let size = format!("{}G", size_gib);
        self.run(
            &host.uri,
            &[
                "vol-create-as",
                &host.storage_pool,
                &vol,
                &size,
                "--format",
                "raw",
            ],
        )
        .await
        .context("creating data disk")?;
        Ok(())
    }
}

fn p(x: &Path) -> &str {
    x.to_str().expect("utf-8 temp path")
}

/// NoCloud meta-data. `provider-id` lets kubelet set `--provider-id` via
/// `{{ ds.meta_data.provider_id }}` in jinja-templated user-data.
pub fn meta_data(vm: &Vm) -> String {
    let mut m: BTreeMap<String, String> = vm.spec.meta_data.clone();
    m.insert("instance-id".into(), vm.id.to_string());
    m.insert("local-hostname".into(), vm.spec.name.clone());
    m.insert("provider_id".into(), vm.provider_id());
    m.iter()
        .map(|(k, v)| format!("{k}: {}\n", serde_json::to_string(v).unwrap()))
        .collect()
}

/// NoCloud vendor-data: platform-side guest setup that must not collide
/// with the caller's user-data.
///
/// Shipped as a shell script rather than cloud-config so cloud-init runs it
/// via `scripts-vendor` without any YAML merging against user-data (list
/// keys such as `runcmd`/`write_files` would otherwise replace each other).
/// Runs once per instance; everything it installs persists.
///
/// Currently installs `xdebbie-fstrim.service`, a oneshot that runs
/// `fstrim -av` on every boot so freed blocks are returned to the LVM thin
/// pool (the virtio disks are attached with `discard='unmap'`), and enables
/// the distro's weekly `fstrim.timer` as a backstop.
pub fn vendor_data() -> &'static str {
    r#"#!/bin/sh
# xdebbie vendor-data -- runs once on first boot via cloud-init scripts-vendor.
set -eu

cat >/etc/systemd/system/xdebbie-fstrim.service <<'EOF'
[Unit]
Description=xdebbie: return freed blocks to the thin pool (fstrim -av)
Documentation=man:fstrim(8)
After=local-fs.target
ConditionVirtualization=vm

[Service]
Type=oneshot
ExecStart=/sbin/fstrim -av
Nice=19
IOSchedulingClass=idle

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now xdebbie-fstrim.service || true
systemctl enable --now fstrim.timer 2>/dev/null || true
"#
}

/// DHCP on whichever NIC carries our MAC. `mtu` is set when the NIC sits
/// on a mesh-managed overlay bridge (section 2.5 of the mesh spec).
pub fn network_config(vm: &Vm, mtu: Option<u16>) -> String {
    let mtu_line = mtu.map(|m| format!("    mtu: {m}\n")).unwrap_or_default();
    format!(
        "version: 2\nethernets:\n  primary:\n    match:\n      macaddress: \"{}\"\n{mtu_line}    dhcp4: true\n    dhcp6: false\n",
        vm.mac()
    )
}

/// `overlay_mtu`: when set, `<mtu size=.../>` goes on the interface so
/// virtio-net hands the mesh MTU to the guest.
pub fn domain_xml(host: &Host, vm: &Vm, volume_group: Option<&str>, overlay_mtu: Option<u16>) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\'', "&apos;")
    };
    
    let block_disk = |dev: &str, target: &str| {
        format!(
            r#"    <disk type='block' device='disk'>
      <driver name='qemu' type='raw' cache='none' io='native' discard='unmap'/>
      <source dev='{dev}'/>
      <target dev='{target}' bus='virtio'/>
    </disk>
"#,
            dev = esc(dev)
        )
    };
    let abe_disk = |role: DiskRole| vm.disks.iter().find(|d| d.role == role).map(|d| d.dev_path());

    // Root disk: ABE volume, LVM block device or qcow2 volume
    let root_disk_xml = if let (Storage::Abe, Some(dev)) = (vm.spec.storage, abe_disk(DiskRole::Root)) {
        block_disk(&dev, "vda")
    } else if let Some(vg) = volume_group {
        format!(
            r#"    <disk type='block' device='disk'>
      <driver name='qemu' type='raw' cache='none' io='native' discard='unmap'/>
      <source dev='/dev/{vg}/{vm}-root'/>
      <target dev='vda' bus='virtio'/>
    </disk>
"#,
            vg = esc(vg),
            vm = esc(&vm.domain_name())
        )
    } else {
        format!(
            r#"    <disk type='volume' device='disk'>
      <driver name='qemu' type='qcow2' discard='unmap'/>
      <source pool='{pool}' volume='{disk}'/>
      <target dev='vda' bus='virtio'/>
    </disk>
"#,
            pool = esc(&host.storage_pool),
            disk = esc(&Virsh::disk_vol(vm))
        )
    };
    
    // Optional data disk
    let data_disk_xml = if vm.spec.data_disk_gib.is_some() {
        if let (Storage::Abe, Some(dev)) = (vm.spec.storage, abe_disk(DiskRole::Data)) {
            block_disk(&dev, "vdb")
        } else if let Some(vg) = volume_group {
            format!(
                r#"    <disk type='block' device='disk'>
      <driver name='qemu' type='raw' cache='none' io='native' discard='unmap'/>
      <source dev='/dev/{vg}/{vm}-data'/>
      <target dev='vdb' bus='virtio'/>
    </disk>
"#,
                vg = esc(vg),
                vm = esc(&vm.domain_name())
            )
        } else {
            format!(
                r#"    <disk type='volume' device='disk'>
      <driver name='qemu' type='raw' discard='unmap'/>
      <source pool='{pool}' volume='{data}'/>
      <target dev='vdb' bus='virtio'/>
    </disk>
"#,
                pool = esc(&host.storage_pool),
                data = esc(&Virsh::data_vol(vm))
            )
        }
    } else {
        String::new()
    };
    
    format!(
        r#"<domain type='kvm'>
  <name>{name}</name>
  <uuid>{uuid}</uuid>
  <metadata>
    <xdebbie:vm xmlns:xdebbie='https://xdebbie.dev/xmlns/vm/1'><xdebbie:name>{vmname}</xdebbie:name></xdebbie:vm>
  </metadata>
  <memory unit='MiB'>{mem}</memory>
  <vcpu placement='static'>{cpu}</vcpu>
  <os><type arch='x86_64' machine='q35'>hvm</type><boot dev='hd'/></os>
  <features><acpi/><apic/></features>
  <cpu mode='host-passthrough' check='none' migratable='on'/>
  <clock offset='utc'/>
  <on_poweroff>destroy</on_poweroff><on_reboot>restart</on_reboot><on_crash>restart</on_crash>
  <devices>
{root_disk}{data_disk}<disk type='volume' device='cdrom'>
      <driver name='qemu' type='raw'/>
      <source pool='{pool}' volume='{seed}'/>
      <target dev='sda' bus='sata'/><readonly/>
    </disk>
    <interface type='bridge'>
      <mac address='{mac}'/>
      <source bridge='{bridge}'/>
      <model type='virtio'/>{mtu_xml}
    </interface>
    <channel type='unix'><target type='virtio' name='org.qemu.guest_agent.0'/></channel>
    <serial type='pty'><target port='0'/></serial>
    <console type='pty'><target type='serial' port='0'/></console>
    <graphics type='vnc' port='-1' autoport='yes' listen='127.0.0.1'/>
    <video><model type='virtio' vram='16384'/></video>
    <rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>
    <memballoon model='virtio'/>
  </devices>
</domain>
"#,
        name = vm.domain_name(),
        uuid = vm.id,
        vmname = esc(&vm.spec.name),
        mem = vm.spec.memory_mib,
        cpu = vm.spec.vcpus,
        pool = esc(&host.storage_pool),
        root_disk = root_disk_xml,
        data_disk = data_disk_xml,
        seed = esc(&Virsh::seed_vol(vm)),
        mac = vm.mac(),
        bridge = esc(&host.bridge),
        mtu_xml = overlay_mtu
            .map(|m| format!("\n      <mtu size='{m}'/>"))
            .unwrap_or_default()
    )
}

/// `virsh domblklist` → (target, source) pairs.
pub fn parse_domblklist(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let t = it.next()?;
            let s = it.next().unwrap_or("-");
            (t != "Target" && !t.starts_with('-')).then(|| (t.to_string(), s.to_string()))
        })
        .collect()
}

/// First `vdX` not in `used`, after the slots provisioning may use (vda root,
/// vdb data).
pub fn free_target(used: &[String]) -> Option<String> {
    ('c'..='z')
        .map(|c| format!("vd{c}"))
        .find(|t| !used.contains(t))
}

/// Hot-plug XML for a volume; same driver settings as the provisioned disks.
pub fn volume_disk_xml(vol: &Volume, target: &str) -> String {
    format!(
        r#"<disk type='block' device='disk'>
  <driver name='qemu' type='raw' cache='none' io='native' discard='unmap'/>
  <source dev='{dev}'/>
  <target dev='{target}' bus='virtio'/>
  <serial>{serial}</serial>
</disk>
"#,
        dev = vol.disk().dev_path(),
        serial = vol.serial()
    )
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .map(|v| v.trim_start_matches(':').trim())
}

pub fn parse_nodeinfo(text: &str) -> Result<(u32, u64)> {
    let cpus = field(text, "CPU(s)").context("CPU(s) missing")?.parse()?;
    let kib: u64 = field(text, "Memory size")
        .context("Memory size missing")?
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .parse()?;
    Ok((cpus, kib / 1024))
}

pub fn parse_pool_available(text: &str) -> Result<u64> {
    let bytes: u64 = field(text, "Available")
        .context("Available missing")?
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .parse()?;
    Ok(bytes / (1 << 30))
}

/// Interface names from `virsh domiflist` (skips the header and `-`).
pub fn parse_domiflist(text: &str) -> Vec<String> {
    text.lines()
        .skip(2)
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| *n != "-" && !n.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn parse_domifaddr(text: &str) -> Vec<Address> {
    text.lines()
        .filter_map(|l| {
            let cols: Vec<_> = l.split_whitespace().collect();
            let i = cols.iter().position(|c| *c == "ipv4")?;
            let ip = cols.get(i + 1)?.split('/').next()?;
            (!ip.starts_with("127.")).then(|| Address {
                kind: "InternalIP".into(),
                address: ip.into(),
            })
        })
        .collect()
}

/// Does this libvirt URI point at the machine xdebbied is running on?
/// `qemu:///system`, localhost, or a host that is one of our own names or
/// addresses. Used to decide whether host-local resources (the LVM thin
/// pool) describe this host.
pub fn uri_is_local(uri: &str) -> bool {
    let Some(rest) = uri.split("://").nth(1) else { return true };
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or("")
        .trim_start_matches('[')
        .split(']')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if host.is_empty() || host == "localhost" || host.starts_with("127.") || host == "::1" {
        return true;
    }
    let mut names: Vec<String> = Vec::new();
    if let Ok(h) = hostname::get() {
        let h = h.to_string_lossy().to_string();
        names.push(h.split('.').next().unwrap_or(&h).to_string());
        names.push(h);
    }
    if let Ok(out) = std::process::Command::new("hostname").arg("-f").output() {
        names.push(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    if let Ok(out) = std::process::Command::new("ip").args(["-o", "addr", "show"]).output() {
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            let mut f = l.split_whitespace();
            if f.any(|w| w == "inet" || w == "inet6")
                && let Some(a) = f.next().and_then(|a| a.split('/').next()) {
                    names.push(a.to_string());
                }
        }
    }
    names.iter().any(|n| n == host)
}

impl Hypervisor for Virsh {
    async fn inventory(&self, host: &Host) -> Result<Capacity> {
        let (cpus, memory_mib) = parse_nodeinfo(&self.run(&host.uri, &["nodeinfo"]).await?)?;
        let _ = self
            .run(&host.uri, &["pool-refresh", &host.storage_pool])
            .await;
        let pool_free_gib = parse_pool_available(
            &self
                .run(&host.uri, &["pool-info", "--bytes", &host.storage_pool])
                .await?,
        )?;
        // With LVM configured, root disks are carved from the local thin pool
        // (see ensure_disk), not the libvirt pool -- so for *this* host the
        // thin pool's free space is the number the scheduler must see. Other
        // hosts keep the libvirt-pool figure; their LVM is not visible here.
        let disk_free_gib = match (&self.lvm, uri_is_local(&host.uri)) {
            (Some(lvm), true) => match lvm.free_gib() {
                Ok(g) => {
                    tracing::debug!(host = %host.name, lvm_free_gib = g, pool_free_gib, "using LVM free space for capacity");
                    g
                }
                Err(e) => {
                    tracing::warn!(host = %host.name, error = %e, "LVM free space unavailable; falling back to libvirt pool");
                    pool_free_gib
                }
            },
            _ => pool_free_gib,
        };
        Ok(Capacity {
            cpus,
            memory_mib,
            disk_free_gib,
        })
    }

    async fn state(&self, host: &Host, vm: &Vm) -> Result<Option<DomState>> {
        match self.run(&host.uri, &["domstate", &vm.domain_name()]).await {
            Ok(s) => Ok(Some(match s.trim() {
                "running" => DomState::Running,
                "shut off" => DomState::Off,
                "paused" => DomState::Paused,
                _ => DomState::Other,
            })),
            Err(e)
                if e.to_string().contains("ailed to get domain")
                    || e.to_string().contains("no domain") =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn prepare_disks(&self, host: &Host, vm: &Vm, disks: &mut Vec<Disk>) -> Result<()> {
        if vm.spec.storage != Storage::Abe {
            return Ok(());
        }
        let server = host
            .abe
            .as_deref()
            .ok_or_else(|| anyhow!("host {} has no ABE ([[hosts]].abe) for storage = abe", host.name))?;
        let mut wanted = vec![(DiskRole::Root, abe::gib(vm.spec.disk_gib))];
        if let Some(g) = vm.spec.data_disk_gib {
            wanted.push((DiskRole::Data, abe::gib(g)));
        }
        for (role, bytes) in wanted {
            if disks.iter().any(|d| d.role == role) {
                continue;
            }
            let e = self
                .abe
                .create(server, bytes)
                .await
                .with_context(|| format!("allocating {role:?} disk for {} on {server}", vm.spec.name))?;
            tracing::info!(vm = %vm.spec.name, ?role, id = %e.id, traddr = %e.traddr, port = %e.port, "abe: allocated volume");
            disks.push(Disk {
                role,
                server: server.to_string(),
                traddr: e.traddr,
                port: e.port,
                id: e.id,
                bytes,
                // data disks stay blank
                populated: role == DiskRole::Data,
            });
        }
        for d in disks.iter_mut() {
            self.nvme.connect(d).await?;
            let dev = self.nvme.wait_dev(d, std::time::Duration::from_secs(15)).await?;
            if !d.populated {
                let (src, fmt) = self.base_image(host, &vm.spec.image).await?;
                tracing::info!(vm = %vm.spec.name, %src, %dev, "abe: filling root disk from base image");
                self.nvme.populate(&src, fmt, &dev).await?;
                d.populated = true;
            }
        }
        Ok(())
    }

    async fn provision(&self, host: &Host, vm: &Vm) -> Result<()> {
        tracing::info!("provision called for VM: {} on host: {}", vm.spec.name, host.name);
        if vm.spec.storage == Storage::Abe {
            if !vm.disks.iter().any(|d| d.role == DiskRole::Root && d.populated) {
                bail!("abe root disk not prepared yet");
            }
            self.ensure_connected(vm).await?;
        }
        self.ensure_disk(host, vm).await?;
        self.ensure_seed(host, vm).await?;
        self.ensure_data_disk(host, vm).await?;
        if self.state(host, vm).await?.is_none() {
            let f = tempfile::Builder::new().suffix(".xml").tempfile()?;
            let vg = self.lvm.as_ref().map(|l| l.vg_name());
            let mtu = self.overlay.mtu_for(&host.bridge);
            tokio::fs::write(f.path(), domain_xml(host, vm, vg, mtu)).await?;
            self.run(&host.uri, &["define", p(f.path())]).await?;
            self.run(&host.uri, &["autostart", &vm.domain_name()])
                .await?;
        }
        Ok(())
    }

    async fn start(&self, host: &Host, vm: &Vm) -> Result<()> {
        if vm.spec.storage == Storage::Abe {
            self.ensure_connected(vm).await?;
        }
        self.run(&host.uri, &["start", &vm.domain_name()])
            .await
            .map(drop)
    }
    async fn shutdown(&self, host: &Host, vm: &Vm, force: bool) -> Result<()> {
        let cmd = if force { "destroy" } else { "shutdown" };
        self.run(&host.uri, &[cmd, &vm.domain_name()])
            .await
            .map(drop)
    }
    async fn reboot(&self, host: &Host, vm: &Vm) -> Result<()> {
        self.run(&host.uri, &["reboot", &vm.domain_name()])
            .await
            .map(drop)
    }

    async fn destroy(&self, host: &Host, vm: &Vm) -> Result<()> {
        let d = vm.domain_name();
        if self.state(host, vm).await?.is_some() {
            let _ = self.run(&host.uri, &["destroy", &d]).await;
            self.run(
                &host.uri,
                &[
                    "undefine",
                    &d,
                    "--nvram",
                    "--managed-save",
                    "--snapshots-metadata",
                ],
            )
            .await?;
        }
        
        // Clean up LVM volumes if using LVM
        if let Some(ref lvm) = self.lvm {
            let _ = lvm.remove_disks(&vm.domain_name());
        }
        for vol in [Self::disk_vol(vm), Self::seed_vol(vm)] {
            if self.vol_exists(host, &vol).await {
                self.run(
                    &host.uri,
                    &["vol-delete", "--pool", &host.storage_pool, &vol],
                )
                .await?;
            }
        }
        self.release_disks(vm).await?;
        Ok(())
    }

    async fn attach_volume(&self, host: &Host, vm: &Vm, vol: &Volume) -> Result<String> {
        if !uri_is_local(&host.uri) {
            bail!("attaching volumes on remote host {} is not supported yet (needs nvme connect there)", host.name);
        }
        let d = vol.disk();
        self.nvme.connect(&d).await?;
        let dev = self.nvme.wait_dev(&d, std::time::Duration::from_secs(15)).await?;
        let dom = vm.domain_name();
        let blk = parse_domblklist(&self.run(&host.uri, &["domblklist", &dom]).await?);
        if let Some((t, _)) = blk.iter().find(|(_, src)| *src == dev) {
            return Ok(t.clone());
        }
        let used: Vec<String> = blk.into_iter().map(|(t, _)| t).collect();
        let target = free_target(&used).ok_or_else(|| anyhow!("no free virtio slot on {dom}"))?;
        let f = tempfile::Builder::new().suffix(".xml").tempfile()?;
        tokio::fs::write(f.path(), volume_disk_xml(vol, &target)).await?;
        self.run(&host.uri, &["attach-device", &dom, p(f.path()), "--persistent"])
            .await
            .with_context(|| format!("hot-plugging {} into {dom}", vol.id))?;
        tracing::info!(vm = %vm.spec.name, volume = %vol.id, %target, "attached volume");
        Ok(target)
    }

    async fn detach_volume(&self, host: &Host, vol: &Volume, att: &Attachment) -> Result<()> {
        if !uri_is_local(&host.uri) {
            bail!("detaching volumes on remote host {} is not supported yet", host.name);
        }
        let dev = vol.disk().dev_path();
        // The domain may have been deleted already; then there is nothing to unplug.
        let doms = self.run(&host.uri, &["list", "--all", "--name"]).await?;
        for dom in doms.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let blk = parse_domblklist(&self.run(&host.uri, &["domblklist", dom]).await?);
            if let Some((t, _)) = blk.iter().find(|(_, src)| *src == dev) {
                self.run(&host.uri, &["detach-disk", dom, t, "--persistent"])
                    .await
                    .with_context(|| format!("unplugging {} ({t}) from {dom}", vol.id))?;
                tracing::info!(vm = %att.vm, volume = %vol.id, target = %t, "detached volume");
            }
        }
        self.nvme.disconnect(vol.disk().nqn()).await
    }

    async fn tap_owners(&self, host: &Host) -> Result<HashMap<String, String>> {
        let names = self.run(&host.uri, &["list", "--name"]).await?;
        let mut map = HashMap::new();
        for dom in names.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let out = match self.run(&host.uri, &["domiflist", dom]).await {
                Ok(o) => o,
                Err(_) => continue,
            };
            for tap in parse_domiflist(&out) {
                map.insert(tap, dom.to_string());
            }
        }
        Ok(map)
    }

    async fn addresses(&self, host: &Host, vm: &Vm) -> Result<Vec<Address>> {
        for src in ["agent", "arp", "lease"] {
            if let Ok(out) = self
                .run(
                    &host.uri,
                    &["domifaddr", &vm.domain_name(), "--source", src],
                )
                .await
            {
                let a = parse_domifaddr(&out);
                if !a.is_empty() {
                    return Ok(a);
                }
            }
        }
        Ok(vec![])
    }

    /// Live migration with incremental storage copy. Base images exist on
    /// every host, so only the overlay and seed travel over the wire.
    async fn migrate(&self, src: &Host, dst: &Host, vm: &Vm) -> Result<()> {
        if vm.spec.storage == Storage::Abe {
            // The volumes are reachable from any host, but attaching them
            // there means running nvme on the destination, which this
            // driver cannot do for a remote host yet.
            bail!("live migration of abe-backed VMs is not supported yet (needs nvme connect on {})", dst.name);
        }
        if src.storage_pool != dst.storage_pool || src.bridge != dst.bridge {
            bail!(
                "migration requires matching pool and bridge names on {} and {}",
                src.name,
                dst.name
            );
        }
        self.ensure_disk(dst, vm).await?;
        self.ensure_seed(dst, vm).await?;
        self.ensure_data_disk(dst, vm).await?;
        self.run(
            &src.uri,
            &[
                "migrate",
                "--live",
                "--persistent",
                "--undefinesource",
                "--copy-storage-inc",
                "--verbose",
                &vm.domain_name(),
                &dst.uri,
            ],
        )
        .await?;
        for vol in [Self::disk_vol(vm), Self::seed_vol(vm)] {
            let _ = self
                .run(&src.uri, &["vol-delete", "--pool", &src.storage_pool, &vol])
                .await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod locality_tests {
    use super::uri_is_local;

    #[test]
    fn local_forms() {
        assert!(uri_is_local("qemu:///system"));
        assert!(uri_is_local("qemu+tls://localhost/system"));
        assert!(uri_is_local("qemu+tcp://127.0.0.1:16509/system"));
        let short = hostname::get().unwrap().to_string_lossy().to_string();
        assert!(uri_is_local(&format!("qemu+ssh://root@{}/system", short)));
    }

    #[test]
    fn remote_forms() {
        assert!(!uri_is_local("qemu+tls://203.0.113.9/system"));
        assert!(!uri_is_local("qemu+ssh://root@not-this-host.invalid/system"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nodeinfo() {
        let t = "CPU model:           x86_64\nCPU(s):              32\nMemory size:         131072000 KiB\n";
        assert_eq!(parse_nodeinfo(t).unwrap(), (32, 128000));
    }
    #[test]
    fn parses_pool() {
        assert_eq!(
            parse_pool_available("Name: default\nAvailable:      1099511627776\n").unwrap(),
            1024
        );
    }
    #[test]
    fn parses_domblklist_and_picks_a_free_slot() {
        let t = " Target   Source\n---------------------------------------------\n vda      /dev/timesink-vg/node0-root\n vdc      /dev/disk/by-id/nvme-uuid.0123\n sda      -\n";
        let blk = parse_domblklist(t);
        assert_eq!(
            blk,
            vec![
                ("vda".to_string(), "/dev/timesink-vg/node0-root".to_string()),
                ("vdc".to_string(), "/dev/disk/by-id/nvme-uuid.0123".to_string()),
                ("sda".to_string(), "-".to_string()),
            ]
        );
        let used: Vec<String> = blk.into_iter().map(|(t, _)| t).collect();
        assert_eq!(free_target(&used).as_deref(), Some("vdd"));
        assert_eq!(free_target(&[]).as_deref(), Some("vdc"));
    }
    #[test]
    fn volume_hotplug_xml_carries_the_serial() {
        let now = chrono::Utc::now();
        let v = Volume {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "pv".into(),
            server: "http://10.22.0.1".into(),
            traddr: "10.22.0.1".into(),
            port: "44002".into(),
            bytes: 1,
            perf: None,
            attach_to: None,
            attached: None,
            message: None,
            created: now,
            updated: now,
        };
        let x = volume_disk_xml(&v, "vdc");
        assert!(x.contains("<source dev='/dev/disk/by-id/nvme-uuid.01234567-89ab-cdef-0123-456789abcdef'/>"));
        assert!(x.contains("<target dev='vdc' bus='virtio'/>"));
        assert!(x.contains("<serial>0123456789abcdef0123</serial>"));
        assert!(x.contains("discard='unmap'"));
    }
    #[test]
    fn parses_ifaddr() {
        let t = " Name       MAC address          Protocol     Address\n-----\n lo  00:00:00:00:00:00 ipv4 127.0.0.1/8\n enp1s0 52:54:00:aa:bb:cc ipv4 10.0.0.50/24\n - - ipv6 fe80::1/64\n";
        assert_eq!(
            parse_domifaddr(t),
            vec![Address {
                kind: "InternalIP".into(),
                address: "10.0.0.50".into()
            }]
        );
    }

    #[test]
    fn domain_xml_uses_by_id_block_devices_for_abe_disks() {
        let host: Host = serde_json::from_value(serde_json::json!({"name": "a", "uri": "test:///"})).unwrap();
        let mut vm: Vm = serde_json::from_value(serde_json::json!({
            "id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            "spec": {"name": "x", "vcpus": 1, "memoryMib": 512, "diskGib": 8, "image": "img",
                     "dataDiskGib": 4, "storage": "abe"},
            "phase": "Pending", "power": "On",
            "created": "2026-01-01T00:00:00Z", "updated": "2026-01-01T00:00:00Z"
        })).unwrap();
        let mk = |role: DiskRole, id: &str| Disk {
            role, server: "http://10.22.0.1:80".into(), traddr: "10.22.0.1".into(), port: "44001".into(),
            id: id.into(), bytes: 1, populated: true,
        };
        vm.disks = vec![mk(DiskRole::Root, "0123456789abcdef0123456789abcdef"), mk(DiskRole::Data, "fedcba9876543210fedcba9876543210")];
        let xml = domain_xml(&host, &vm, Some("vg-that-must-be-ignored"), None);
        assert!(xml.contains("<source dev='/dev/disk/by-id/nvme-uuid.01234567-89ab-cdef-0123-456789abcdef'/>"));
        assert!(xml.contains("<source dev='/dev/disk/by-id/nvme-uuid.fedcba98-7654-3210-fedc-ba9876543210'/>"));
        assert!(xml.contains("<target dev='vda'"));
        assert!(xml.contains("<target dev='vdb'"));
        assert!(!xml.contains("vg-that-must-be-ignored"));
    }
}
