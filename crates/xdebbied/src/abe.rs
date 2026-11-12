//! ABE-backed VM disks: volumes exported over NVMe/TCP by the `abe` daemon
//! on each hypervisor (`[[hosts]].abe`), attached on the host that runs the
//! VM. Two halves:
//!
//! * [`Client`] — the tiny ABE HTTP API (`/configure`, `/id/<id>`, `/volumes`).
//! * [`Nvme`]  — the host side: `nvme connect`/`disconnect`, waiting for the
//!   stable `/dev/disk/by-id/nvme-uuid.<id>` link, filling a root disk from
//!   the base image with `qemu-img`.
//!
//! Everything the reconciler needs to find a disk again later lives in the
//! VM record ([`xdebbie_core::Disk`]): server, traddr, port, id. The host
//! that attaches a volume need not be the host whose ABE owns it, which is
//! what makes these disks migratable.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::{path::Path, time::Duration};
use tokio::process::Command;
use xdebbie_core::Disk;

/// Reply of `GET /configure` and `GET /id/<id>`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Export {
    pub id: String,
    pub port: String,
    #[serde(default)]
    pub traddr: String,
    #[serde(default)]
    pub nqn: String,
    /// Size ABE actually made (abe >= 0.3.2).
    #[serde(default)]
    pub bytes: Option<u64>,
    /// fio characterization, when ABE ran one (abe >= 0.3.1).
    #[serde(default)]
    pub perf: Option<xdebbie_core::VolumePerf>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("reqwest client"),
        }
    }

    fn url(base: &str, path: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), path)
    }

    /// Create a blank volume of `bytes` and export it.
    pub async fn create(&self, base: &str, bytes: u64) -> Result<Export> {
        let url = Self::url(base, &format!("/configure?bytes={bytes}&raw=true"));
        let r = self.http.get(&url).send().await.with_context(|| format!("GET {url}"))?;
        let st = r.status();
        let body = r.text().await.unwrap_or_default();
        if !st.is_success() {
            bail!("abe {base}: configure returned {st}: {body}");
        }
        let e: Export = serde_json::from_str(&body).with_context(|| format!("abe reply: {body}"))?;
        if e.traddr.is_empty() || e.traddr == "0.0.0.0" {
            bail!(
                "abe {base} advertises traddr {:?}; set ABE_ADVERTISE in /etc/default/abe",
                e.traddr
            );
        }
        Ok(e)
    }

    /// Unexport and remove. A volume that is already gone is not an error.
    pub async fn delete(&self, base: &str, id: &str) -> Result<()> {
        let url = Self::url(base, &format!("/id/{id}"));
        let r = self.http.delete(&url).send().await.with_context(|| format!("DELETE {url}"))?;
        let st = r.status();
        if st.is_success() || st == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let body = r.text().await.unwrap_or_default();
        bail!("abe {base}: delete {id} returned {st}: {body}")
    }
}

/// Host-side NVMe/TCP initiator operations. Paths are parameters so tests
/// can point at a tempdir instead of /sys and /dev.
#[derive(Clone)]
pub struct Nvme {
    pub nvme_bin: String,
    pub qemu_img_bin: String,
    /// `/sys/class/nvme`
    pub sys_class: String,
}

impl Default for Nvme {
    fn default() -> Self {
        Self {
            nvme_bin: "nvme".into(),
            qemu_img_bin: "qemu-img".into(),
            sys_class: "/sys/class/nvme".into(),
        }
    }
}

impl Nvme {
    /// Is a controller for `nqn` present on this host?
    pub fn connected(&self, nqn: &str) -> bool {
        connected_in(Path::new(&self.sys_class), nqn)
    }

    /// Attach the volume; a no-op if already attached. `ctrl-loss-tmo -1`
    /// keeps retrying forever when the exporter is away, so the guest
    /// stalls instead of taking I/O errors.
    pub async fn connect(&self, d: &Disk) -> Result<()> {
        if self.connected(d.nqn()) {
            return Ok(());
        }
        let out = Command::new(&self.nvme_bin)
            .args([
                "connect", "-t", "tcp", "-a", &d.traddr, "-s", &d.port, "-n", d.nqn(),
                "--ctrl-loss-tmo", "-1",
            ])
            .output()
            .await
            .with_context(|| format!("running {}", self.nvme_bin))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // Raced with ourselves or a previous boot; the sysfs check wins.
            if !(err.contains("already connected") && self.connected(d.nqn())) {
                bail!(
                    "nvme connect {}:{} {}: {} {}",
                    d.traddr,
                    d.port,
                    d.nqn(),
                    out.status,
                    err
                );
            }
        }
        Ok(())
    }

    /// Detach; missing is fine.
    pub async fn disconnect(&self, nqn: &str) -> Result<()> {
        if !self.connected(nqn) {
            return Ok(());
        }
        let out = Command::new(&self.nvme_bin)
            .args(["disconnect", "-n", nqn])
            .output()
            .await
            .with_context(|| format!("running {}", self.nvme_bin))?;
        if !out.status.success() && self.connected(nqn) {
            bail!(
                "nvme disconnect {nqn}: {} {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Wait for udev to create the by-id link after a connect.
    pub async fn wait_dev(&self, d: &Disk, timeout: Duration) -> Result<String> {
        let path = d.dev_path();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if Path::new(&path).exists() {
                return Ok(path);
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "{path} did not appear within {:?} (is abe >= 0.2.0 setting device_uuid?)",
                    timeout
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Fill `dev` from `src` (qcow2 or raw). `-n` writes into the existing
    /// block device instead of trying to create it.
    pub async fn populate(&self, src: &str, src_format: &str, dev: &str) -> Result<()> {
        let out = Command::new(&self.qemu_img_bin)
            .args([
                "convert", "-n", "-f", src_format, "-O", "raw", "-t", "none", "-T", "none", src,
                dev,
            ])
            .output()
            .await
            .with_context(|| format!("running {}", self.qemu_img_bin))?;
        if !out.status.success() {
            bail!(
                "qemu-img convert {src} -> {dev}: {} {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

/// Scan `<sys_class>/nvme*/subsysnqn` for `nqn`.
fn connected_in(sys_class: &Path, nqn: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(sys_class) else {
        return false;
    };
    rd.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("subsysnqn"))
            .map(|s| s.trim() == nqn)
            .unwrap_or(false)
    })
}

/// Sum of a VM's requested disk sizes in bytes, for capacity accounting.
pub fn gib(n: u64) -> u64 {
    n << 30
}

#[allow(dead_code)]
fn _assert_send() {
    fn f<T: Send + Sync>() {}
    f::<Client>();
    f::<Nvme>();
    let _ = anyhow!("");
}

#[cfg(test)]
mod tests {
    use super::*;
    use xdebbie_core::DiskRole;

    fn disk(id: &str) -> Disk {
        Disk {
            role: DiskRole::Root,
            server: "http://127.0.0.1:1".into(),
            traddr: "127.0.0.1".into(),
            port: "44001".into(),
            id: id.into(),
            bytes: 1,
            populated: false,
        }
    }

    #[test]
    fn connected_scans_sysfs_for_the_nqn() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("nvme0")).unwrap();
        std::fs::write(t.path().join("nvme0/subsysnqn"), "aaaa\n").unwrap();
        std::fs::create_dir_all(t.path().join("nvme3")).unwrap();
        std::fs::write(t.path().join("nvme3/subsysnqn"), "0123456789abcdef0123456789abcdef\n").unwrap();
        let n = Nvme { sys_class: t.path().to_string_lossy().into(), ..Default::default() };
        assert!(n.connected("0123456789abcdef0123456789abcdef"));
        assert!(n.connected("aaaa"));
        assert!(!n.connected("bbbb"));
        let empty = Nvme { sys_class: "/nonexistent".into(), ..Default::default() };
        assert!(!empty.connected("aaaa"));
    }

    #[tokio::test]
    async fn connect_is_a_noop_when_already_attached_and_runs_nvme_otherwise() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path().join("sys");
        std::fs::create_dir_all(sys.join("nvme1")).unwrap();
        std::fs::write(sys.join("nvme1/subsysnqn"), "present\n").unwrap();
        // stub nvme: record argv, succeed
        let log = t.path().join("argv");
        let stub = t.path().join("nvme");
        std::fs::write(
            &stub,
            format!("#!/bin/sh\necho \"$@\" >> {}\nexit 0\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let n = Nvme {
            nvme_bin: stub.to_string_lossy().into(),
            sys_class: sys.to_string_lossy().into(),
            ..Default::default()
        };
        n.connect(&disk("present")).await.unwrap();
        assert!(!log.exists(), "no nvme call for an attached volume");
        n.connect(&disk("absent")).await.unwrap();
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            argv.trim(),
            "connect -t tcp -a 127.0.0.1 -s 44001 -n absent --ctrl-loss-tmo -1"
        );
        n.disconnect("absent").await.unwrap(); // not in sysfs → no call
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
        n.disconnect("present").await.unwrap();
        assert!(std::fs::read_to_string(&log).unwrap().contains("disconnect -n present"));
    }

    #[tokio::test]
    async fn wait_dev_times_out_with_a_hint() {
        let n = Nvme::default();
        let e = n
            .wait_dev(&disk("0123456789abcdef0123456789abcdef"), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("nvme-uuid.01234567-89ab-cdef-0123-456789abcdef"));
        assert!(e.to_string().contains("device_uuid"));
    }

    #[test]
    fn export_parses_abe_reply() {
        let e: Export = serde_json::from_str(
            r#"{"message":"sudo nvme ...","id":"abc","port":"44001","traddr":"10.22.0.1","nqn":"abc"}"#,
        )
        .unwrap();
        assert_eq!(e.port, "44001");
        assert_eq!(e.traddr, "10.22.0.1");
        // pre-0.2.0 abe without traddr still parses (create() rejects it later)
        let old: Export = serde_json::from_str(r#"{"message":"","id":"abc","port":"44001"}"#).unwrap();
        assert_eq!(old.traddr, "");
    }
}
