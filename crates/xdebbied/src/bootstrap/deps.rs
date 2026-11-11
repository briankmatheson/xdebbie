// SPDX-License-Identifier: GPL-3.0-or-later
//! Dependency checking and installation for xdebbied bootstrap.

use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

/// Required packages for xdebbied operation
const REQUIRED_PACKAGES: &[&str] = &[
    "libvirt-daemon-system",
    "libvirt-clients",
    "qemu-kvm",
    "qemu-utils",
    "cloud-image-utils",
    // LVM thin provisioning. thin-provisioning-tools provides thin_check,
    // without which LVM refuses to activate thin pools at boot -- every VM
    // disk in the pool silently stays inactive and libvirt autostart fails.
    "lvm2",
    "thin-provisioning-tools",
    // The host firewall; bootstrap writes its rules (bootstrap/firewall.rs).
    "ufw",
];

/// Check if a command exists in PATH
fn command_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if a package is installed via dpkg
fn package_installed(pkg: &str) -> bool {
    Command::new("dpkg")
        .args(["-l", pkg])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if running as root
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Check all dependencies and return missing ones
pub fn check_dependencies() -> Result<Vec<String>> {
    let mut missing = Vec::new();

    // Check for essential commands
    if !command_exists("virsh") {
        missing.push("libvirt-clients".to_string());
    }
    if !command_exists("qemu-system-x86_64") {
        missing.push("qemu-kvm".to_string());
    }
    if !command_exists("qemu-img") {
        missing.push("qemu-utils".to_string());
    }
    if !command_exists("cloud-localds") {
        missing.push("cloud-image-utils".to_string());
    }

    // Check for packages
    for pkg in REQUIRED_PACKAGES {
        if !package_installed(pkg) && !missing.iter().any(|p| p == pkg) {
            missing.push(pkg.to_string());
        }
    }

    Ok(missing)
}

/// Install missing packages using apt-get
pub fn install_dependencies(packages: &[String]) -> Result<()> {
    if packages.is_empty() {
        info!("All dependencies satisfied");
        return Ok(());
    }

    if !is_root() {
        anyhow::bail!(
            "Missing packages but not running as root. Please install: {}",
            packages.join(", ")
        );
    }

    info!("Installing missing packages: {}", packages.join(", "));

    // Update package cache
    let update = Command::new("apt-get")
        .args(["update", "-qq"])
        .status()
        .context("running apt-get update")?;

    if !update.success() {
        warn!("apt-get update failed, continuing anyway");
    }

    // Install packages
    let mut args = vec!["install", "-y", "-qq"];
    for pkg in packages {
        args.push(pkg);
    }

    let status = Command::new("apt-get")
        .args(&args)
        .status()
        .context("running apt-get install")?;

    if !status.success() {
        anyhow::bail!("Failed to install packages: {}", packages.join(", "));
    }

    info!("Successfully installed dependencies");
    Ok(())
}

/// Ensure libvirtd service is running
pub fn ensure_libvirtd_running() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["is-active", "libvirtd"])
        .output()
        .context("checking libvirtd status")?;

    if status.status.success() {
        info!("libvirtd is already running");
        return Ok(());
    }

    if !is_root() {
        anyhow::bail!("libvirtd not running and not root to start it");
    }

    info!("Starting libvirtd service");
    let start = Command::new("systemctl")
        .args(["start", "libvirtd"])
        .status()
        .context("starting libvirtd")?;

    if !start.success() {
        anyhow::bail!("Failed to start libvirtd service");
    }

    // Enable for auto-start
    let _ = Command::new("systemctl")
        .args(["enable", "libvirtd"])
        .status();

    info!("libvirtd started and enabled");
    Ok(())
}

/// `systemctl status libvirtd` (head) for error messages.
pub fn libvirtd_status() -> String {
    Command::new("systemctl")
        .args(["--no-pager", "-n", "8", "status", "libvirtd"])
        .output()
        .map(|o| {
            let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
            t.push_str(&String::from_utf8_lossy(&o.stderr));
            t.lines().take(20).collect::<Vec<_>>().join("\n")
        })
        .unwrap_or_else(|e| format!("(systemctl status failed: {e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_exists() {
        assert!(command_exists("sh"));
        assert!(!command_exists("nonexistent-command-xyz"));
    }
}
