//! ABE-backed VM disks: volumes exported over NVMe/TCP by the `abe` daemon
//! on each hypervisor (`[[hosts]].abe`), attached on the host that runs the
//! VM. Two halves:
//!
//! * [`Client`] — the ABE HTTP API: `/configure` (plain, `?name=` for
//!   idempotent creates, `?base=` for per-image base volumes), `/id/<id>`,
//!   `/seal/<id>`, `/clone/<id>`, `/utilization` and `/version` (which
//!   tells us whether the ABE predates 0.4.0 and has none of the extras).
//! * [`Nvme`]  — the host side: `nvme connect`/`disconnect`, waiting for the
//!   stable `/dev/disk/by-id/nvme-uuid.<id>` link, filling a root disk from
//!   the base image with `qemu-img`.
//!
//! Everything the reconciler needs to find a disk again later lives in the
//! VM record ([`xdebbie_core::Disk`]): server, traddr, port, id. The host
//! that attaches a volume need not be the host whose ABE owns it, which is
//! what makes these disks migratable.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::process::Command;
use xdebbie_core::{AbeCapacity, Disk, DiskRole};

/// How long a `/version` answer is trusted before asking again.
const FEATURE_TTL: Duration = Duration::from_secs(60);

/// A non-2xx reply from ABE, kept in the error chain so callers can map
/// 404 / 409 / 503 / 507 to their own status codes. Status 0 means no
/// reply at all because the TLS handshake failed; `body` is then
/// `tls: <reason>` (see [`Client::send_error`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbeHttpError {
    pub status: u16,
    pub body: String,
}

impl std::fmt::Display for AbeHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.status == 0 {
            return f.write_str(self.body.trim());
        }
        let msg = serde_json::from_str::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
            .unwrap_or_else(|| self.body.clone());
        write!(f, "{} {}", self.status, msg.trim())
    }
}

impl std::error::Error for AbeHttpError {}

/// The HTTP status behind an error from [`Client`], if it was one. A TLS
/// failure (status 0) is not an HTTP status and reads as `None`.
pub fn http_status(e: &anyhow::Error) -> Option<u16> {
    e.chain()
        .find_map(|c| c.downcast_ref::<AbeHttpError>())
        .map(|h| h.status)
        .filter(|s| *s != 0)
}

/// Was the request refused by TLS? reqwest wraps the rustls error
/// several layers deep and the kinds (`connect`, `request`) overlap with
/// a plain refused connection, so the source chain is read for what the
/// handshake said: `certificate`, `handshake`, `rustls`, `tls`. Returns
/// the innermost message, which is the one naming the cause
/// (`invalid peer certificate: UnknownIssuer`, `received fatal alert:
/// CertificateRequired`, …).
fn tls_failure(e: &reqwest::Error) -> Option<String> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    let mut hit = None;
    while let Some(s) = cur {
        let m = s.to_string();
        let l = m.to_ascii_lowercase();
        if l.contains("certificate")
            || l.contains("handshake")
            || l.contains("rustls")
            || l.contains("tls")
        {
            hit = Some(m);
        }
        cur = s.source();
    }
    hit
}

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
    /// fio characterization, when ABE ran one (abe >= 0.3.1). Lenient:
    /// a `perf` from another abe version reads as `None`.
    #[serde(default, deserialize_with = "lenient_perf")]
    pub perf: Option<xdebbie_core::VolumePerf>,
    /// `?name=`/`?base=` matched a volume that already existed (abe >= 0.4.0).
    #[serde(default)]
    pub existing: bool,
}

fn lenient_perf<'de, D>(d: D) -> Result<Option<xdebbie_core::VolumePerf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

/// Reply of `GET /configure?base=<image>`: the export plus what makes it a
/// base. A sealed base comes back with an empty `port`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BaseVol {
    #[serde(flatten)]
    pub export: Export,
    pub base: String,
    #[serde(default)]
    pub sealed: bool,
    /// Size of the base (`export.bytes` is filled from this too).
    #[serde(default)]
    pub bytes: u64,
}

impl BaseVol {
    #[cfg(test)]
    pub fn id(&self) -> &str {
        &self.export.id
    }
}

/// `GET /version`: what an ABE can do.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AbeFeatures {
    pub version: String,
    #[serde(default)]
    pub features: BTreeSet<String>,
    /// The API is served over mutual TLS (abe >= 0.5.0; older ones say
    /// nothing, which reads as `false`).
    #[serde(default)]
    pub tls: bool,
}

/// `GET /utilization`, the parts capacity accounting needs. Unknown fields
/// are ignored, so newer ABEs keep parsing.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AbeUtilization {
    #[serde(default)]
    pub vg: String,
    #[serde(default)]
    pub thin_pool: Option<String>,
    pub bytes: u64,
    #[serde(default)]
    pub used_bytes: u64,
    #[serde(default)]
    pub free_bytes: u64,
    #[serde(default)]
    pub data_percent: f64,
    #[serde(default)]
    pub metadata_percent: Option<f64>,
    #[serde(default)]
    pub provisioned_bytes: u64,
    #[serde(default)]
    pub volumes: usize,
}

impl AbeUtilization {
    pub fn capacity(&self, seen: chrono::DateTime<chrono::Utc>) -> AbeCapacity {
        AbeCapacity {
            thin: self.thin_pool.is_some(),
            bytes: self.bytes,
            used_bytes: self.used_bytes,
            free_bytes: self.free_bytes,
            provisioned_bytes: self.provisioned_bytes,
            data_percent: self.data_percent,
            metadata_percent: self.metadata_percent,
            tls: false,
            seen,
        }
    }
}

/// Does `s` satisfy ABE's tag-value charset (`[A-Za-z0-9._+:-]{1,128}`)?
pub fn valid_abe_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+:-".contains(&b))
}

/// The idempotency name sent to ABE for a standalone volume: `vol-<name>`
/// when that is a valid tag value, else `vol-<sha256(name)[..32]>`.
pub fn volume_abe_name(name: &str) -> String {
    let plain = format!("vol-{name}");
    if valid_abe_name(&plain) {
        return plain;
    }
    use sha2::Digest;
    let h = hex::encode(sha2::Sha256::digest(name.as_bytes()));
    format!("vol-{}", &h[..32])
}

/// The idempotency name for a VM disk: `vm-<uuid>-root` / `vm-<uuid>-data`.
pub fn vm_disk_abe_name(vm: uuid::Uuid, role: DiskRole) -> String {
    let r = match role {
        DiskRole::Root => "root",
        DiskRole::Data => "data",
        DiskRole::Volume => "volume",
    };
    format!("vm-{vm}-{r}")
}

/// `/version` per ABE url, with when it was asked. `None`: a pre-0.4.0
/// ABE (404).
type FeatureCache = HashMap<String, (Instant, Option<AbeFeatures>)>;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    features: Arc<Mutex<FeatureCache>>,
    /// ABEs we already warned about being old, so the log says it once.
    legacy_logged: Arc<Mutex<HashSet<String>>>,
}

/// How long one ABE call may take: a create waits for fio plus the trim.
pub const TIMEOUT: Duration = Duration::from_secs(120);

impl Client {
    /// The client the daemon uses: `http` comes from
    /// [`crate::config::Config::http_client`] (the cluster CA as roots,
    /// this box's cert as identity), so an ABE behind mutual TLS lets us
    /// in and a plain `http://` ABE works as before (reqwest only applies
    /// TLS to `https://` URLs).
    pub fn with_http(http: reqwest::Client) -> Self {
        Self {
            http,
            features: Arc::default(),
            legacy_logged: Arc::default(),
        }
    }

    /// A bare client (no roots beyond the system's, no identity) for
    /// tests against a plain-HTTP fake. The daemon never builds one;
    /// `tests/lint-abe-client.sh` keeps it that way.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_http(
            reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("reqwest client"),
        )
    }

    fn url(base: &str, path: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), path)
    }

    /// A failed `send()`: a TLS failure (handshake refused, certificate
    /// not trusted, no client certificate accepted) becomes an
    /// [`AbeHttpError`] with status 0 and a `tls: …` message, so the
    /// host list and the log say so instead of looking like a timeout.
    /// Anything else is the reqwest error as it was.
    fn send_error(e: reqwest::Error) -> anyhow::Error {
        if let Some(msg) = tls_failure(&e) {
            anyhow::Error::new(AbeHttpError {
                status: 0,
                body: format!("tls: {msg}"),
            })
        } else {
            e.into()
        }
    }

    /// GET and parse a JSON reply; a non-2xx status becomes an
    /// [`AbeHttpError`] in the chain.
    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        base: &str,
        path: &str,
        what: &str,
    ) -> Result<T> {
        let url = Self::url(base, path);
        let r = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(Self::send_error)
            .with_context(|| format!("GET {url}"))?;
        let st = r.status();
        let body = r.text().await.unwrap_or_default();
        if !st.is_success() {
            return Err(AbeHttpError {
                status: st.as_u16(),
                body,
            })
            .with_context(|| format!("abe {base}: {what}"));
        }
        serde_json::from_str(&body).with_context(|| format!("abe {base}: {what} reply: {body}"))
    }

    fn check_traddr(base: &str, e: &Export) -> Result<()> {
        if e.traddr.is_empty() || e.traddr == "0.0.0.0" {
            bail!(
                "abe {base} advertises traddr {:?}; set ABE_ADVERTISE in /etc/default/abe",
                e.traddr
            );
        }
        Ok(())
    }

    /// Create a blank volume of `bytes` and export it (any abe version).
    pub async fn create(&self, base: &str, bytes: u64) -> Result<Export> {
        let e: Export = self
            .get_json(
                base,
                &format!("/configure?bytes={bytes}&raw=true"),
                "configure",
            )
            .await?;
        Self::check_traddr(base, &e)?;
        Ok(e)
    }

    /// Idempotent create (abe >= 0.4.0): the same `name` returns the same
    /// volume with `existing: true` instead of making another.
    pub async fn create_named(&self, base: &str, bytes: u64, name: &str) -> Result<Export> {
        let e: Export = self
            .get_json(
                base,
                &format!("/configure?bytes={bytes}&raw=true&name={name}"),
                "configure",
            )
            .await?;
        Self::check_traddr(base, &e)?;
        Ok(e)
    }

    /// (Re-)export an existing volume: the port and address it is on now.
    pub async fn export(&self, base: &str, id: &str) -> Result<Export> {
        let e: Export = self.get_json(base, &format!("/id/{id}"), "export").await?;
        Self::check_traddr(base, &e)?;
        Ok(e)
    }

    /// The base volume for `image` (made raw, no fio, `bytes` big), the
    /// same one every time. Exported while unsealed; `port` empty once
    /// sealed.
    pub async fn base(&self, base: &str, image: &str, bytes: u64) -> Result<BaseVol> {
        let mut b: BaseVol = self
            .get_json(
                base,
                &format!("/configure?base={image}&bytes={bytes}"),
                "base",
            )
            .await?;
        if b.export.bytes.is_none() && b.bytes > 0 {
            b.export.bytes = Some(b.bytes);
        }
        if let Some(n) = b.export.bytes
            && b.bytes == 0
        {
            b.bytes = n;
        }
        if !b.sealed {
            Self::check_traddr(base, &b.export)?;
        }
        Ok(b)
    }

    /// Freeze a filled base: unexported, clone-only. Idempotent.
    pub async fn seal(&self, base: &str, id: &str) -> Result<()> {
        let _: serde_json::Value = self.get_json(base, &format!("/seal/{id}"), "seal").await?;
        Ok(())
    }

    /// A thin snapshot of `origin`, `bytes` big, idempotent on `name`.
    pub async fn clone(&self, base: &str, origin: &str, bytes: u64, name: &str) -> Result<Export> {
        let e: Export = self
            .get_json(
                base,
                &format!("/clone/{origin}?bytes={bytes}&name={name}"),
                "clone",
            )
            .await?;
        Self::check_traddr(base, &e)?;
        Ok(e)
    }

    /// Pool / VG usage.
    pub async fn utilization(&self, base: &str) -> Result<AbeUtilization> {
        self.get_json(base, "/utilization", "utilization").await
    }

    /// `GET /version`; `Ok(None)` for an ABE before 0.4.0 (404).
    pub async fn version(&self, base: &str) -> Result<Option<AbeFeatures>> {
        match self
            .get_json::<AbeFeatures>(base, "/version", "version")
            .await
        {
            Ok(f) => Ok(Some(f)),
            Err(e) if http_status(&e) == Some(404) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `version()` through a 60 s cache. Logs once per ABE that predates
    /// 0.4.0.
    pub async fn features(&self, base: &str) -> Result<Option<AbeFeatures>> {
        if let Some((at, f)) = self
            .features
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(base)
            && at.elapsed() < FEATURE_TTL
        {
            return Ok(f.clone());
        }
        let f = self.version(base).await?;
        if f.is_none()
            && self
                .legacy_logged
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(base.to_string())
        {
            tracing::warn!(
                "abe at {base} predates 0.4.0: no thin root clones, no name idempotency"
            );
        }
        self.features
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(base.to_string(), (Instant::now(), f.clone()));
        Ok(f)
    }

    /// Does the ABE at `base` advertise `feature`? `false` for a pre-0.4.0
    /// ABE; an error only if it cannot be asked at all.
    pub async fn has(&self, base: &str, feature: &str) -> Result<bool> {
        Ok(self
            .features(base)
            .await?
            .is_some_and(|f| f.features.contains(feature)))
    }

    /// Unexport and remove. A volume that is already gone is not an error.
    pub async fn delete(&self, base: &str, id: &str) -> Result<()> {
        let url = Self::url(base, &format!("/id/{id}"));
        let r = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(Self::send_error)
            .with_context(|| format!("DELETE {url}"))?;
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
                "connect",
                "-t",
                "tcp",
                "-a",
                &d.traddr,
                "-s",
                &d.port,
                "-n",
                d.nqn(),
                "--ctrl-loss-tmo",
                "-1",
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

    /// `nvme disconnect` whether or not sysfs shows a controller: drops
    /// one stuck reconnecting to a port the exporter no longer serves,
    /// which `ctrl-loss-tmo -1` would otherwise keep retrying for ever.
    pub async fn disconnect_force(&self, nqn: &str) -> Result<()> {
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

impl Nvme {
    /// The guest-visible size of an image (`qemu-img info --output=json`
    /// `virtual-size`), rounded up to a whole MiB.
    pub async fn virtual_size(&self, src: &str, src_format: &str) -> Result<u64> {
        let out = Command::new(&self.qemu_img_bin)
            .args(["info", "-f", src_format, "--output=json", src])
            .output()
            .await
            .with_context(|| format!("running {}", self.qemu_img_bin))?;
        if !out.status.success() {
            bail!(
                "qemu-img info {src}: {} {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .with_context(|| format!("qemu-img info {src}: not JSON"))?;
        let n = v
            .get("virtual-size")
            .and_then(|n| n.as_u64())
            .ok_or_else(|| anyhow!("qemu-img info {src}: no virtual-size"))?;
        Ok(round_up_mib(n))
    }

    /// Push everything written to `dev` out of the page cache
    /// (`blockdev --flushbufs`) before the exporter stops serving it.
    /// Best effort: `populate` already writes with `-t none`.
    pub async fn flush(&self, dev: &str) {
        match Command::new("blockdev")
            .args(["--flushbufs", dev])
            .output()
            .await
        {
            Ok(o) if o.status.success() => {}
            Ok(o) => tracing::warn!(
                "blockdev --flushbufs {dev}: {} {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => tracing::warn!("blockdev --flushbufs {dev}: {e}"),
        }
    }
}

/// Round up to a whole MiB (what ABE does with sizes too).
pub fn round_up_mib(n: u64) -> u64 {
    const MIB: u64 = 1 << 20;
    n.div_ceil(MIB) * MIB
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

    fn disk(id: &str) -> Disk {
        Disk {
            role: DiskRole::Root,
            server: "http://127.0.0.1:1".into(),
            traddr: "127.0.0.1".into(),
            port: "44001".into(),
            id: id.into(),
            bytes: 1,
            populated: false,
            origin: None,
        }
    }

    #[test]
    fn connected_scans_sysfs_for_the_nqn() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("nvme0")).unwrap();
        std::fs::write(t.path().join("nvme0/subsysnqn"), "aaaa\n").unwrap();
        std::fs::create_dir_all(t.path().join("nvme3")).unwrap();
        std::fs::write(
            t.path().join("nvme3/subsysnqn"),
            "0123456789abcdef0123456789abcdef\n",
        )
        .unwrap();
        let n = Nvme {
            sys_class: t.path().to_string_lossy().into(),
            ..Default::default()
        };
        assert!(n.connected("0123456789abcdef0123456789abcdef"));
        assert!(n.connected("aaaa"));
        assert!(!n.connected("bbbb"));
        let empty = Nvme {
            sys_class: "/nonexistent".into(),
            ..Default::default()
        };
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
        std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
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
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("disconnect -n present")
        );
    }

    #[tokio::test]
    async fn wait_dev_times_out_with_a_hint() {
        let n = Nvme::default();
        let e = n
            .wait_dev(
                &disk("0123456789abcdef0123456789abcdef"),
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("nvme-uuid.01234567-89ab-cdef-0123-456789abcdef")
        );
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
        let old: Export =
            serde_json::from_str(r#"{"message":"","id":"abc","port":"44001"}"#).unwrap();
        assert_eq!(old.traddr, "");
    }

    #[test]
    fn export_reply_carries_existing_and_tolerates_a_foreign_perf() {
        let e: Export = serde_json::from_str(
            r#"{"id":"abc","port":"44001","traddr":"10.22.1.1","nqn":"abc","bytes":4194304,"existing":true,"perf":{"bs":"4M","seconds":5.0}}"#,
        )
        .unwrap();
        assert!(e.existing);
        assert_eq!(e.perf, None);
        let b: BaseVol = serde_json::from_str(
            r#"{"message":"","id":"b1","port":"","traddr":"10.22.1.1","nqn":"b1","bytes":1073741824,"existing":true,"base":"debian-13","sealed":true}"#,
        )
        .unwrap();
        assert_eq!(
            (b.id(), b.base.as_str(), b.sealed, b.bytes),
            ("b1", "debian-13", true, 1 << 30)
        );
    }

    #[test]
    fn abe_names_follow_the_tag_charset() {
        assert_eq!(volume_abe_name("pvc-1b2c"), "vol-pvc-1b2c");
        let odd = volume_abe_name("has space/and#hash");
        assert!(
            odd.starts_with("vol-") && odd.len() == 4 + 32 && valid_abe_name(&odd),
            "{odd}"
        );
        assert_eq!(volume_abe_name("has space/and#hash"), odd, "deterministic");
        let long = "x".repeat(200);
        assert!(valid_abe_name(&volume_abe_name(&long)));
        let id = uuid::Uuid::nil();
        assert_eq!(
            vm_disk_abe_name(id, DiskRole::Root),
            "vm-00000000-0000-0000-0000-000000000000-root"
        );
        assert!(valid_abe_name(&vm_disk_abe_name(id, DiskRole::Data)));
        assert!(!valid_abe_name(""));
        assert!(!valid_abe_name("a b"));
        assert_eq!(round_up_mib(1), 1 << 20);
        assert_eq!(round_up_mib(2 << 20), 2 << 20);
        assert_eq!(round_up_mib((2 << 20) + 1), 3 << 20);
    }

    /// A few routes of abe 0.4.0 and of a pre-0.4.0 abe, served by axum.
    async fn fake_abe(legacy: bool) -> String {
        use axum::{Router, extract::Path, extract::Query, routing::get};
        use std::collections::HashMap;
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let c = calls.clone();
        let configure = move |Query(q): Query<HashMap<String, String>>| {
            let c = c.clone();
            async move {
                c.lock().unwrap().push(format!("configure {q:?}"));
                let existing = q.get("name").is_some_and(|n| n == "vol-again");
                if let Some(img) = q.get("base") {
                    return axum::Json(serde_json::json!({
                        "message": "", "id": "b1", "port": if img == "sealed" { "" } else { "44009" },
                        "traddr": "10.22.1.1", "nqn": "b1", "bytes": 1073741824u64,
                        "existing": true, "base": img, "sealed": img == "sealed"
                    }));
                }
                axum::Json(serde_json::json!({
                    "message": "", "id": "v1", "port": "44001", "traddr": "10.22.1.1", "nqn": "v1",
                    "bytes": q.get("bytes").and_then(|b| b.parse::<u64>().ok()).unwrap_or(0), "existing": existing
                }))
            }
        };
        let mut r = Router::new()
            .route("/configure", get(configure))
            .route(
                "/id/{id}",
                get(|Path(id): Path<String>| async move {
                    if id == "gone" {
                        return (axum::http::StatusCode::NOT_FOUND, axum::Json(serde_json::json!({"error": "no volume gone"})));
                    }
                    if id == "sealed" {
                        return (axum::http::StatusCode::CONFLICT, axum::Json(serde_json::json!({"error": "sealed is a sealed base; clone it"})));
                    }
                    (axum::http::StatusCode::OK, axum::Json(serde_json::json!({"message": "", "id": id, "port": "44002", "traddr": "10.22.1.1", "nqn": "x"})))
                }),
            )
            .route(
                "/utilization",
                get(|| async {
                    axum::Json(serde_json::json!({
                        "vg": "vg", "thin_pool": "pool", "bytes": 1000u64, "used_bytes": 100u64, "free_bytes": 900u64,
                        "data_percent": 10.0, "metadata_percent": 2.5, "metadata_bytes": 1u64, "provisioned_bytes": 600u64,
                        "vg_bytes": 2000u64, "vg_free_bytes": 1000u64, "volumes": 3, "max_layers": 1, "warnings": [],
                        "reexport": {"exported": 1, "already": 0, "failed": [], "moved": 0, "at": "x"}
                    }))
                }),
            );
        if !legacy {
            r = r
                .route(
                    "/version",
                    get(|| async {
                        axum::Json(serde_json::json!({"version": "0.4.0", "features": ["reexport", "ports", "name", "trim", "base", "clone"]}))
                    }),
                )
                .route("/seal/{id}", get(|| async { axum::Json(serde_json::json!({"id": "b1", "base": "img", "sealed": true, "bytes": 1})) }))
                .route(
                    "/clone/{id}",
                    get(|Path(id): Path<String>, Query(q): Query<HashMap<String, String>>| async move {
                        if id == "unsealed" {
                            return (axum::http::StatusCode::CONFLICT, axum::Json(serde_json::json!({"error": "base unsealed is not sealed yet"})));
                        }
                        (axum::http::StatusCode::OK, axum::Json(serde_json::json!({
                            "message": "", "id": "c1", "port": "44003", "traddr": "10.22.1.1", "nqn": "c1",
                            "bytes": q.get("bytes").and_then(|b| b.parse::<u64>().ok()).unwrap_or(0),
                            "origin": id, "layers": 1, "existing": q.get("name").is_some_and(|n| n.ends_with("again"))
                        })))
                    }),
                );
        }
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn client_speaks_abe_0_4_and_maps_statuses() {
        let url = fake_abe(false).await;
        let c = Client::new();
        let f = c.version(&url).await.unwrap().unwrap();
        assert_eq!(f.version, "0.4.0");
        assert!(c.has(&url, "clone").await.unwrap());
        assert!(!c.has(&url, "teleport").await.unwrap());
        let e = c.create_named(&url, 5 << 20, "vol-new").await.unwrap();
        assert_eq!(
            (e.id.as_str(), e.existing, e.bytes),
            ("v1", false, Some(5 << 20))
        );
        assert!(c.create_named(&url, 1, "vol-again").await.unwrap().existing);
        assert_eq!(c.export(&url, "v1").await.unwrap().port, "44002");
        let e = c.export(&url, "gone").await.unwrap_err();
        assert_eq!(http_status(&e), Some(404));
        assert!(e.to_string().contains("export"), "{e:#}");
        assert_eq!(
            http_status(&c.export(&url, "sealed").await.unwrap_err()),
            Some(409)
        );
        let b = c.base(&url, "img", 1 << 30).await.unwrap();
        assert_eq!(
            (b.id(), b.sealed, b.export.port.as_str(), b.bytes),
            ("b1", false, "44009", 1 << 30)
        );
        let b = c.base(&url, "sealed", 1 << 30).await.unwrap();
        assert!(b.sealed && b.export.port.is_empty());
        c.seal(&url, "b1").await.unwrap();
        let cl = c.clone(&url, "b1", 10 << 30, "vm-x-root").await.unwrap();
        assert_eq!(
            (cl.id.as_str(), cl.port.as_str(), cl.bytes),
            ("c1", "44003", Some(10 << 30))
        );
        assert_eq!(
            http_status(&c.clone(&url, "unsealed", 1, "n").await.unwrap_err()),
            Some(409)
        );
        let u = c.utilization(&url).await.unwrap();
        assert_eq!(
            (
                u.thin_pool.as_deref(),
                u.provisioned_bytes,
                u.metadata_percent
            ),
            (Some("pool"), 600, Some(2.5))
        );
        let cap = u.capacity(chrono::Utc::now());
        assert!(cap.thin && cap.bytes == 1000 && cap.free_bytes == 900);
    }

    /// An abe 0.5.0: `/version` over mutual TLS with the test CA, client
    /// certificates required. Returns the URL and the cert paths.
    async fn mtls_abe(dir: &std::path::Path) -> (String, crate::bootstrap::api_tls::ApiTlsPaths) {
        use axum::{Router, routing::get};
        use rustls::pki_types::pem::PemObject;
        let paths = crate::bootstrap::api_tls::generate_localhost(dir);
        let roots = crate::tls::client_roots(&paths.ca_cert, std::path::Path::new("/nonexistent"));
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
            rustls::pki_types::CertificateDer::pem_file_iter(&paths.server_cert)
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
        let key = rustls::pki_types::PrivateKeyDer::from_pem_file(&paths.server_key).unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .unwrap();
        let sc = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .unwrap();
        let r = Router::new().route(
            "/version",
            get(|| async {
                axum::Json(serde_json::json!({
                    "version": "0.5.0",
                    "features": ["reexport", "ports", "name", "trim", "tls", "fence", "base", "clone"],
                    "tls": true
                }))
            }),
        );
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let url = format!("https://{}", l.local_addr().unwrap());
        let cfg = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(sc));
        tokio::spawn(async move {
            axum_server::from_tcp_rustls(l, cfg)
                .unwrap()
                .serve(r.into_make_service())
                .await
                .unwrap()
        });
        (url, paths)
    }

    /// The daemon's client (`Config::http_client`: cluster CA as root,
    /// box cert as identity) gets through an abe behind mutual TLS and
    /// reads `tls: true`; a bare client is refused with a `tls: …` error
    /// that carries no HTTP status.
    #[tokio::test]
    async fn mtls_abe_admits_the_identity_client_and_names_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let (url, paths) = mtls_abe(dir.path()).await;
        let e = Client::new().version(&url).await.unwrap_err();
        assert_eq!(http_status(&e), None);
        let tls = e
            .chain()
            .find_map(|c| c.downcast_ref::<AbeHttpError>())
            .unwrap_or_else(|| panic!("no AbeHttpError in {e:#}"));
        assert!(tls.body.starts_with("tls: "), "{e:#}");
        assert!(format!("{e:#}").contains("tls: "), "{e:#}");

        let cfg: crate::config::Config = toml::from_str(&format!(
            "state_file = 'x'
tls_cert = {:?}
tls_key = {:?}
ca_file = {:?}
peer_ca_dir = {:?}",
            paths.server_cert,
            paths.server_key,
            paths.ca_cert,
            dir.path().join("peers")
        ))
        .unwrap();
        let c = Client::with_http(cfg.http_client(TIMEOUT));
        let f = c.version(&url).await.unwrap().unwrap();
        assert!(f.tls && f.features.contains("fence"), "{f:?}");
        // the same client still speaks plain http to an old abe
        let plain = fake_abe(false).await;
        let f = c.version(&plain).await.unwrap().unwrap();
        assert!(!f.tls);
    }

    #[tokio::test]
    async fn a_pre_0_4_abe_has_no_features_and_is_cached() {
        let url = fake_abe(true).await;
        let c = Client::new();
        assert_eq!(c.version(&url).await.unwrap(), None);
        assert!(!c.has(&url, "name").await.unwrap());
        assert!(!c.has(&url, "base").await.unwrap());
        assert_eq!(c.features.lock().unwrap().len(), 1);
        assert_eq!(c.legacy_logged.lock().unwrap().len(), 1);
        // an unreachable abe is an error, not "legacy"
        let e = c.has("http://127.0.0.1:1", "name").await.unwrap_err();
        assert_eq!(http_status(&e), None);
        // old create still works
        let e = c.create(&url, 7).await.unwrap();
        assert_eq!(e.id, "v1");
        assert!(!e.existing);
    }
}
