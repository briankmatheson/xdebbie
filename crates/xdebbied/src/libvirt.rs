// SPDX-License-Identifier: GPL-3.0-or-later
//! libvirt driver. Talks to each host with `virsh -c qemu+ssh://...`, so the
//! only thing a host needs is libvirtd, QEMU/KVM, an SSH key for the xdebbie
//! user, a storage pool holding base images, and a bridge.

use std::{collections::BTreeMap, future::Future, path::Path};

use anyhow::{Context, Result, bail};
use xdebbie_core::{Address, Capacity, Host, Vm};
use tokio::process::Command;

use crate::lvm::LvmManager;

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
    fn migrate(&self, src: &Host, dst: &Host, vm: &Vm) -> impl Future<Output = Result<()>> + Send;
}

#[derive(Clone)]
pub struct Virsh {
    pub bin: String,
    pub seed_command: Vec<String>,
    pub lvm: Option<LvmManager>,
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

    /// Build a NoCloud seed locally and upload it through libvirt.
    async fn ensure_seed(&self, host: &Host, vm: &Vm) -> Result<()> {
        let vol = Self::seed_vol(vm);
        if self.vol_exists(host, &vol).await {
            return Ok(());
        }
        let dir = tempfile::tempdir()?;
        let (user, meta, net, out) = (
            dir.path().join("user-data"),
            dir.path().join("meta-data"),
            dir.path().join("network-config"),
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
        tokio::fs::write(&net, network_config(vm)).await?;
        let sub = |s: &str| {
            s.replace("{out}", p(&out))
                .replace("{user}", p(&user))
                .replace("{meta}", p(&meta))
                .replace("{net}", p(&net))
        };
        let argv: Vec<String> = self.seed_command.iter().map(|a| sub(a)).collect();
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

/// DHCP on whichever NIC carries our MAC.
pub fn network_config(vm: &Vm) -> String {
    format!(
        "version: 2\nethernets:\n  primary:\n    match:\n      macaddress: \"{}\"\n    dhcp4: true\n    dhcp6: false\n",
        vm.mac()
    )
}

pub fn domain_xml(host: &Host, vm: &Vm, volume_group: Option<&str>) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\'', "&apos;")
    };
    
    // Root disk: LVM block device or qcow2 volume
    let root_disk_xml = if let Some(vg) = volume_group {
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
    
    // Optional data disk (for ABE storage)
    let data_disk_xml = if vm.spec.data_disk_gib.is_some() {
        if let Some(vg) = volume_group {
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
      <model type='virtio'/>
    </interface>
    <channel type='unix'><target type='virtio' name='org.qemu.guest_agent.0'/></channel>
    <serial type='pty'><target port='0'/></serial>
    <console type='pty'><target type='serial' port='0'/></console>
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
        bridge = esc(&host.bridge)
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

impl Hypervisor for Virsh {
    async fn inventory(&self, host: &Host) -> Result<Capacity> {
        let (cpus, memory_mib) = parse_nodeinfo(&self.run(&host.uri, &["nodeinfo"]).await?)?;
        let _ = self
            .run(&host.uri, &["pool-refresh", &host.storage_pool])
            .await;
        let disk_free_gib = parse_pool_available(
            &self
                .run(&host.uri, &["pool-info", "--bytes", &host.storage_pool])
                .await?,
        )?;
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

    async fn provision(&self, host: &Host, vm: &Vm) -> Result<()> {
        tracing::info!("provision called for VM: {} on host: {}", vm.spec.name, host.name);
        self.ensure_disk(host, vm).await?;
        self.ensure_seed(host, vm).await?;
        self.ensure_data_disk(host, vm).await?;
        if self.state(host, vm).await?.is_none() {
            let f = tempfile::Builder::new().suffix(".xml").tempfile()?;
            let vg = self.lvm.as_ref().map(|l| l.vg_name());
            tokio::fs::write(f.path(), domain_xml(host, vm, vg)).await?;
            self.run(&host.uri, &["define", p(f.path())]).await?;
            self.run(&host.uri, &["autostart", &vm.domain_name()])
                .await?;
        }
        Ok(())
    }

    async fn start(&self, host: &Host, vm: &Vm) -> Result<()> {
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
        Ok(())
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
}
