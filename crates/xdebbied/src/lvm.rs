// SPDX-License-Identifier: GPL-3.0-or-later
//! LVM-based disk management for VM root and data disks.
//!
//! This module provides fast, efficient disk provisioning using LVM thin
//! provisioning. Root disks are created as snapshots of base images, and
//! data disks are thin logical volumes.

use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

/// Smallest root disk we will provision, in GiB, for K8s workloads.
///
/// A thin snapshot inherits the size of its origin, so without an explicit
/// extend a VM created from a 10 GiB base image gets a 10 GiB root -- which
/// fills up fast once Kubernetes images land on it. Requests below this are
/// rounded up; the guest's cloud-init growpart/resizefs picks up the extra
/// space on first boot.
///
/// NOTE: This minimum is only enforced for images with "k8s" in the name.
/// Plain base images (like base-debian-13) respect the requested size.
pub const MIN_ROOT_DISK_GIB: u64 = 25;

/// LVM manager for creating and managing VM disks
#[derive(Clone)]
pub struct LvmManager {
    vg_name: String,
    thin_pool: Option<String>,
}

impl LvmManager {
    /// Create a new LVM manager
    ///
    /// # Arguments
    /// * `vg_name` - Volume group name (e.g., "xdebbie-vg")
    /// * `thin_pool` - Optional thin pool name for thin provisioning
    pub fn new(vg_name: String, thin_pool: Option<String>) -> Self {
        Self { vg_name, thin_pool }
    }

    /// Get the volume group name
    pub fn vg_name(&self) -> &str {
        &self.vg_name
    }

    /// Check if volume group exists
    pub fn vg_exists(&self) -> Result<bool> {
        let output = Command::new("/usr/sbin/vgs")
            .arg(&self.vg_name)
            .output()
            .context("checking if volume group exists")?;
        Ok(output.status.success())
    }

    /// Make sure the volume group's logical volumes are active.
    ///
    /// After a host reboot the thin pool (and every thin LV inside it) can be
    /// left inactive -- most commonly because `thin-provisioning-tools` is
    /// missing and LVM refuses to activate the pool without `thin_check`.
    /// When that happens, libvirt autostart fails for every VM whose disks
    /// live in the pool. We run `vgchange -ay` (with `-K` so snapshots that
    /// still carry the activation-skip flag come up too) and surface a clear
    /// error if the thin pool is still inactive afterwards.
    pub fn ensure_activated(&self) -> Result<()> {
        info!("Activating logical volumes in {}", self.vg_name);
        let output = Command::new("/usr/sbin/vgchange")
            .args(["-ay", "-K", &self.vg_name])
            .output()
            .context("executing vgchange -ay")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "vgchange -ay {} reported errors: {}",
                self.vg_name,
                stderr.trim()
            );
        }

        if let Some(ref pool) = self.thin_pool {
            let attr = Command::new("/usr/sbin/lvs")
                .args([
                    "--noheadings",
                    "-o",
                    "lv_attr",
                    &format!("{}/{}", self.vg_name, pool),
                ])
                .output()
                .context("checking thin pool state")?;

            let attr = String::from_utf8_lossy(&attr.stdout).trim().to_string();
            // lv_attr field 5 is the state: 'a' = active
            let active = attr.chars().nth(4) == Some('a');
            if !active {
                let has_thin_check = std::path::Path::new("/usr/sbin/thin_check").exists();
                let hint = if has_thin_check {
                    "Check `journalctl -b | grep -i thin` for the reason."
                } else {
                    "thin_check is missing -- install it with:\n  \
                     sudo apt-get install thin-provisioning-tools\n  \
                     sudo vgchange -ay"
                };
                anyhow::bail!(
                    "Thin pool {}/{} is not active (attr '{}'); VM disks are unavailable. {}",
                    self.vg_name,
                    pool,
                    attr,
                    hint
                );
            }
            info!("Thin pool {}/{} is active", self.vg_name, pool);
        }

        Ok(())
    }

    /// Create a root disk as a snapshot of a base image
    ///
    /// # Arguments
    /// * `vm_name` - VM domain name (e.g., "w1-cp-2ptmn")
    /// * `base_image` - Base image name (e.g., "debian-12")
    /// * `size_gib` - Requested root disk size; rounded up to
    ///   [`MIN_ROOT_DISK_GIB`] and never smaller than the base image.
    ///
    /// # Returns
    /// Path to the block device (e.g., "/dev/xdebbie-vg/w1-cp-2ptmn-root")
    pub fn create_root_disk(
        &self,
        vm_name: &str,
        base_image: &str,
        size_gib: u64,
    ) -> Result<String> {
        let lv_name = format!("{}-root", vm_name);

        // Only enforce minimum for K8s images (they need space for container images)
        // Plain base images (like base-debian-13) respect the requested size
        let size_gib = if base_image.contains("k8s") {
            size_gib.max(MIN_ROOT_DISK_GIB)
        } else {
            size_gib
        };

        // Handle both "debian-12" and "base-debian-12" image names
        let base_lv = if base_image.starts_with("base-") {
            base_image.to_string()
        } else {
            format!("base-{}", base_image)
        };

        info!(
            "create_root_disk called: vm_name={}, base_image={}, lv_name={}, base_lv={}, size_gib={}",
            vm_name, base_image, lv_name, base_lv, size_gib
        );

        // Check if already exists
        if self.lv_exists(&lv_name)? {
            info!("Root disk {}/{} already exists", self.vg_name, lv_name);
            // Grow in place if the spec asks for more than it currently has.
            // Thin volumes can be extended while in use; the guest picks the
            // extra space up via growpart/resizefs on its next boot.
            self.extend_to(&lv_name, size_gib)?;
            return Ok(self.lv_path(&lv_name));
        }

        info!(
            "Root disk {}/{} does not exist, checking base image {}",
            self.vg_name, lv_name, base_lv
        );

        // Check if base image exists
        let base_exists = self.lv_exists(&base_lv)?;
        info!(
            "Base image {}/{} exists: {}",
            self.vg_name, base_lv, base_exists
        );

        if !base_exists {
            let err_msg = format!(
                "Base image '{}/{}' not found. Import it with:\n  \
                 sudo lvcreate -L 10G -n {} {}\n  \
                 sudo qemu-img convert -f qcow2 -O raw <image>.qcow2 /dev/{}/{}",
                self.vg_name, base_lv, base_lv, self.vg_name, self.vg_name, base_lv
            );
            tracing::error!("{}", err_msg);
            anyhow::bail!("{}", err_msg);
        }

        // Create snapshot
        info!(
            "Creating root disk snapshot: {}/{} from {}",
            self.vg_name, lv_name, base_lv
        );

        let output = Command::new("/usr/sbin/lvcreate")
            .args([
                "-s", // snapshot
                "-n",
                &lv_name,                                 // name of new snapshot
                &format!("{}/{}", self.vg_name, base_lv), // source LV (positional)
            ])
            .output()
            .context("executing lvcreate command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "Failed to create root disk snapshot.\nstdout: {}\nstderr: {}",
                stdout,
                stderr
            );
        }

        // Clear activation skip flag and activate (thin snapshots have skip flag set by default)
        info!(
            "Clearing activation skip flag and activating {}/{}",
            self.vg_name, lv_name
        );

        let skip_output = Command::new("/usr/sbin/lvchange")
            .args([
                "-kn", // clear activation skip flag
                &format!("{}/{}", self.vg_name, lv_name),
            ])
            .output()
            .context("clearing activation skip flag")?;

        if !skip_output.status.success() {
            let stderr = String::from_utf8_lossy(&skip_output.stderr);
            tracing::warn!("Failed to clear activation skip flag: {}", stderr);
        }

        let activate_output = Command::new("/usr/sbin/lvchange")
            .args(["-ay", &format!("{}/{}", self.vg_name, lv_name)])
            .output()
            .context("activating logical volume")?;

        if !activate_output.status.success() {
            let stderr = String::from_utf8_lossy(&activate_output.stderr);
            anyhow::bail!("Failed to activate logical volume: {}", stderr);
        }

        // Snapshot inherits the origin's size; grow it to what was asked for.
        self.extend_to(&lv_name, size_gib)?;

        info!(
            "Created and activated root disk: {}/{}",
            self.vg_name, lv_name
        );

        Ok(self.lv_path(&lv_name))
    }

    /// Current size of a logical volume in GiB (rounded down).
    fn lv_size_gib(&self, lv_name: &str) -> Result<u64> {
        let output = Command::new("/usr/sbin/lvs")
            .args([
                "--noheadings",
                "--nosuffix",
                "--units",
                "b",
                "-o",
                "lv_size",
                &format!("{}/{}", self.vg_name, lv_name),
            ])
            .output()
            .context("reading logical volume size")?;
        if !output.status.success() {
            anyhow::bail!(
                "lvs failed for {}/{}: {}",
                self.vg_name,
                lv_name,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let bytes: u64 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .context("parsing lv_size")?;
        Ok(bytes / (1024 * 1024 * 1024))
    }

    /// Extend `lv_name` to `size_gib` if it is currently smaller. No-op
    /// (with an info log) if it is already at least that big -- LVM will
    /// not shrink a thin volume for us and we never want to.
    fn extend_to(&self, lv_name: &str, size_gib: u64) -> Result<()> {
        let current = self.lv_size_gib(lv_name)?;
        if current >= size_gib {
            if current > size_gib {
                info!(
                    "{}/{} is {} GiB, larger than requested {} GiB; leaving as-is",
                    self.vg_name, lv_name, current, size_gib
                );
            }
            return Ok(());
        }

        info!(
            "Extending {}/{} from {} GiB to {} GiB",
            self.vg_name, lv_name, current, size_gib
        );
        let output = Command::new("/usr/sbin/lvextend")
            .args([
                "-L",
                &format!("{}G", size_gib),
                &format!("{}/{}", self.vg_name, lv_name),
            ])
            .output()
            .context("executing lvextend")?;
        if !output.status.success() {
            anyhow::bail!(
                "Failed to extend {}/{} to {} GiB: {}",
                self.vg_name,
                lv_name,
                size_gib,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// Create a data disk as a thin logical volume
    ///
    /// # Arguments
    /// * `vm_name` - VM domain name
    /// * `size_gib` - Size in GiB
    ///
    /// # Returns
    /// Path to the block device (e.g., "/dev/xdebbie-vg/abe-01-data")
    pub fn create_data_disk(&self, vm_name: &str, size_gib: u64) -> Result<String> {
        let lv_name = format!("{}-data", vm_name);

        // Check if already exists
        if self.lv_exists(&lv_name)? {
            info!("Data disk {}/{} already exists", self.vg_name, lv_name);
            return Ok(self.lv_path(&lv_name));
        }

        let size = format!("{}G", size_gib);

        info!(
            "Creating data disk: {}/{} ({})",
            self.vg_name, lv_name, size
        );

        // Create thin or thick LV depending on thin_pool availability
        let status = if let Some(ref pool) = self.thin_pool {
            // Thin provisioned
            Command::new("/usr/sbin/lvcreate")
                .args([
                    "-V",
                    &size, // virtual size
                    "-T",
                    &format!("{}/{}", self.vg_name, pool), // thin pool
                    "-n",
                    &lv_name, // name
                ])
                .status()
                .context("creating thin data disk")?
        } else {
            // Thick provisioned (fallback)
            warn!("No thin pool configured, creating thick LV (slow)");
            Command::new("/usr/sbin/lvcreate")
                .args([
                    "-L",
                    &size, // size
                    "-n",
                    &lv_name,      // name
                    &self.vg_name, // vg
                ])
                .status()
                .context("creating thick data disk")?
        };

        if !status.success() {
            anyhow::bail!("Failed to create data disk");
        }

        Ok(self.lv_path(&lv_name))
    }

    /// Remove all disks for a VM (root and data)
    ///
    /// # Arguments
    /// * `vm_name` - VM domain name
    pub fn remove_disks(&self, vm_name: &str) -> Result<()> {
        for suffix in &["root", "data"] {
            let lv_name = format!("{}-{}", vm_name, suffix);
            if self.lv_exists(&lv_name)? {
                info!("Removing LV: {}/{}", self.vg_name, lv_name);
                let _ = Command::new("/usr/sbin/lvremove")
                    .args(["-f", &self.lv_path(&lv_name)])
                    .status();
            }
        }
        Ok(())
    }

    /// Check if a logical volume exists
    fn lv_exists(&self, lv_name: &str) -> Result<bool> {
        let output = Command::new("/usr/sbin/lvs")
            .arg(format!("{}/{}", self.vg_name, lv_name))
            .output()
            .context("checking if logical volume exists")?;
        Ok(output.status.success())
    }

    /// Get the device path for a logical volume
    fn lv_path(&self, lv_name: &str) -> String {
        format!("/dev/{}/{}", self.vg_name, lv_name)
    }

    /// Free space, in GiB, available for new VM disks: the thin pool's
    /// unallocated data space if thin provisioning is configured, otherwise
    /// the VG's free extents. This is what the scheduler should use for
    /// `disk_free_gib` when disks come from LVM rather than a libvirt pool.
    pub fn free_gib(&self) -> Result<u64> {
        if let Some(ref pool) = self.thin_pool {
            let output = Command::new("/usr/sbin/lvs")
                .args([
                    "--noheadings",
                    "--units",
                    "b",
                    "--nosuffix",
                    "-o",
                    "lv_size,data_percent",
                    &format!("{}/{}", self.vg_name, pool),
                ])
                .output()
                .context("querying thin pool usage")?;
            if !output.status.success() {
                anyhow::bail!(
                    "lvs {}/{}: {}",
                    self.vg_name,
                    pool,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let text = String::from_utf8_lossy(&output.stdout);
            return parse_thin_free_gib(&text);
        }
        let stats = self.get_stats()?;
        Ok(stats.free_gib.floor() as u64)
    }

    /// Get LVM statistics for monitoring
    pub fn get_stats(&self) -> Result<LvmStats> {
        let output = Command::new("/usr/sbin/vgs")
            .args([
                "--noheadings",
                "--units",
                "g",
                "-o",
                "vg_size,vg_free",
                &self.vg_name,
            ])
            .output()
            .context("getting VG stats")?;

        if !output.status.success() {
            anyhow::bail!("Failed to get VG stats");
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = text.split_whitespace().collect();

        if parts.len() < 2 {
            anyhow::bail!("Unexpected vgs output format");
        }

        let free_gib = parts[1]
            .trim_end_matches('g')
            .parse::<f64>()
            .context("parsing free size")?;

        Ok(LvmStats { free_gib })
    }
}

/// Parse `lvs -o lv_size,data_percent --units b --nosuffix` output for a
/// thin pool into free GiB. data_percent is "12.34" (locale-independent in
/// lvs); an empty data_percent means the pool is inactive -- treat as 0 free
/// rather than guessing.
pub fn parse_thin_free_gib(text: &str) -> Result<u64> {
    let mut parts = text.split_whitespace();
    let size: f64 = parts
        .next()
        .context("lvs: missing lv_size")?
        .parse()
        .context("lvs: bad lv_size")?;
    let pct: f64 = match parts.next() {
        Some(p) => p.parse().context("lvs: bad data_percent")?,
        None => return Ok(0),
    };
    let free = size * (1.0 - pct / 100.0);
    Ok((free / (1024.0 * 1024.0 * 1024.0)).floor().max(0.0) as u64)
}

/// LVM statistics
#[derive(Debug, Clone)]
pub struct LvmStats {
    pub free_gib: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thin_free_from_lvs_output() {
        // 200 GiB pool, 5% used -> 190 GiB free
        let gib = 1024f64 * 1024.0 * 1024.0;
        let text = format!("  {} 5.00\n", (200.0 * gib) as u64);
        assert_eq!(parse_thin_free_gib(&text).unwrap(), 190);
        // inactive pool reports no data_percent
        assert_eq!(parse_thin_free_gib("  214748364800\n").unwrap(), 0);
        assert!(parse_thin_free_gib("").is_err());
    }

    #[test]
    fn test_lv_path() {
        let lvm = LvmManager::new("test-vg".to_string(), None);
        assert_eq!(lvm.lv_path("test-lv"), "/dev/test-vg/test-lv");
    }

    // Integration tests require actual LVM setup
    // Run manually with: cargo test --package xdebbied lvm::tests -- --ignored
    #[test]
    #[ignore]
    fn test_vg_exists() {
        let lvm = LvmManager::new("xdebbie-vg".to_string(), None);
        // This will pass if VG exists, fail otherwise
        assert!(lvm.vg_exists().unwrap_or(false));
    }
}
