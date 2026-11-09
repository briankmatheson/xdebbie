// SPDX-License-Identifier: GPL-3.0-or-later
//! LVM-based disk management for VM root and data disks.
//!
//! This module provides fast, efficient disk provisioning using LVM thin
//! provisioning. Root disks are created as snapshots of base images, and
//! data disks are thin logical volumes.

use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

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

    /// Create a root disk as a snapshot of a base image
    ///
    /// # Arguments
    /// * `vm_name` - VM domain name (e.g., "w1-cp-2ptmn")
    /// * `base_image` - Base image name (e.g., "debian-12")
    ///
    /// # Returns
    /// Path to the block device (e.g., "/dev/xdebbie-vg/w1-cp-2ptmn-root")
    pub fn create_root_disk(&self, vm_name: &str, base_image: &str) -> Result<String> {
        let lv_name = format!("{}-root", vm_name);
        // Handle both "debian-12" and "base-debian-12" image names
        let base_lv = if base_image.starts_with("base-") {
            base_image.to_string()
        } else {
            format!("base-{}", base_image)
        };
        
        info!("create_root_disk called: vm_name={}, base_image={}, lv_name={}, base_lv={}", 
              vm_name, base_image, lv_name, base_lv);
        
        // Check if already exists
        if self.lv_exists(&lv_name)? {
            info!("Root disk {}/{} already exists", self.vg_name, lv_name);
            return Ok(self.lv_path(&lv_name));
        }

        info!("Root disk {}/{} does not exist, checking base image {}", self.vg_name, lv_name, base_lv);
        
        // Check if base image exists
        let base_exists = self.lv_exists(&base_lv)?;
        info!("Base image {}/{} exists: {}", self.vg_name, base_lv, base_exists);
        
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
        info!("Creating root disk snapshot: {}/{} from {}", 
              self.vg_name, lv_name, base_lv);
        
        let output = Command::new("/usr/sbin/lvcreate")
            .args(&[
                "-s",                                          // snapshot
                "-n", &lv_name,                                // name of new snapshot
                &format!("{}/{}", self.vg_name, base_lv),     // source LV (positional)
            ])
            .output()
            .context("executing lvcreate command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "Failed to create root disk snapshot.\nstdout: {}\nstderr: {}",
                stdout, stderr
            );
        }

        // Clear activation skip flag and activate (thin snapshots have skip flag set by default)
        info!("Clearing activation skip flag and activating {}/{}", self.vg_name, lv_name);
        
        let skip_output = Command::new("/usr/sbin/lvchange")
            .args(&[
                "-kn",  // clear activation skip flag
                &format!("{}/{}", self.vg_name, lv_name),
            ])
            .output()
            .context("clearing activation skip flag")?;

        if !skip_output.status.success() {
            let stderr = String::from_utf8_lossy(&skip_output.stderr);
            tracing::warn!("Failed to clear activation skip flag: {}", stderr);
        }

        let activate_output = Command::new("/usr/sbin/lvchange")
            .args(&[
                "-ay",
                &format!("{}/{}", self.vg_name, lv_name),
            ])
            .output()
            .context("activating logical volume")?;

        if !activate_output.status.success() {
            let stderr = String::from_utf8_lossy(&activate_output.stderr);
            anyhow::bail!("Failed to activate logical volume: {}", stderr);
        }

        info!("Created and activated root disk: {}/{}", self.vg_name, lv_name);

        Ok(self.lv_path(&lv_name))
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
        
        info!("Creating data disk: {}/{} ({})", 
              self.vg_name, lv_name, size);

        // Create thin or thick LV depending on thin_pool availability
        let status = if let Some(ref pool) = self.thin_pool {
            // Thin provisioned
            Command::new("/usr/sbin/lvcreate")
                .args(&[
                    "-V", &size,                              // virtual size
                    "-T", &format!("{}/{}", self.vg_name, pool), // thin pool
                    "-n", &lv_name,                           // name
                ])
                .status()
                .context("creating thin data disk")?
        } else {
            // Thick provisioned (fallback)
            warn!("No thin pool configured, creating thick LV (slow)");
            Command::new("/usr/sbin/lvcreate")
                .args(&[
                    "-L", &size,                              // size
                    "-n", &lv_name,                           // name
                    &self.vg_name,                            // vg
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
                    .args(&["-f", &self.lv_path(&lv_name)])
                    .status();
            }
        }
        Ok(())
    }

    /// Check if a logical volume exists
    fn lv_exists(&self, lv_name: &str) -> Result<bool> {
        let output = Command::new("/usr/sbin/lvs")
            .arg(&format!("{}/{}", self.vg_name, lv_name))
            .output()
            .context("checking if logical volume exists")?;
        Ok(output.status.success())
    }

    /// Get the device path for a logical volume
    fn lv_path(&self, lv_name: &str) -> String {
        format!("/dev/{}/{}", self.vg_name, lv_name)
    }

    /// Get LVM statistics for monitoring
    pub fn get_stats(&self) -> Result<LvmStats> {
        let output = Command::new("/usr/sbin/vgs")
            .args(&[
                "--noheadings",
                "--units", "g",
                "-o", "vg_size,vg_free",
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

        let total_gib = parts[0].trim_end_matches('g').parse::<f64>()
            .context("parsing total size")?;
        let free_gib = parts[1].trim_end_matches('g').parse::<f64>()
            .context("parsing free size")?;

        Ok(LvmStats {
            vg_name: self.vg_name.clone(),
            total_gib,
            free_gib,
            used_gib: total_gib - free_gib,
        })
    }
}

/// LVM statistics
#[derive(Debug, Clone)]
pub struct LvmStats {
    pub vg_name: String,
    pub total_gib: f64,
    pub free_gib: f64,
    pub used_gib: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

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
