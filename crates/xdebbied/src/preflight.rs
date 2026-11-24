//! What `serve` checks before it starts, read-only and in well under a
//! second or two.
//!
//! `bootstrap` sets a box up; it runs from the package's postinst (fresh
//! install and upgrade) and by hand. It no longer runs before every start
//! of the service: a bootstrap that failed (a package mirror down, a CA box
//! away) kept the hub from serving at all, and every restart re-ran it.
//! `serve` instead looks at what bootstrap should have left behind and
//! says, for each gap, what is missing and the command that fixes it:
//!
//! - **fatal**: serving cannot work and a restart cannot fix it (a file the
//!   operator has to put back). Exit 78, which the unit lists in
//!   `RestartPreventExitStatus`, so it sits in `systemctl status`.
//! - **retry**: not there *yet*, typically at boot (the LVM volume group on
//!   a disk still being scanned). Exit 1; systemd restarts it in 5 s.
//! - **degraded**: one feature is off; the rest serves. Logged with its fix.

use std::path::{Path, PathBuf};

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Fatal,
    Retry,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub what: String,
    pub fix: String,
}

/// What preflight looked at. Each field is `None` when it does not apply
/// to this config (nothing configured, nothing to check).
#[derive(Debug, Default, Clone)]
pub struct Observed {
    /// tls_cert / tls_key configured: (path, exists) for each.
    pub tls_cert: Option<(PathBuf, bool)>,
    pub tls_key: Option<(PathBuf, bool)>,
    /// token_file configured: (path, exists).
    pub token_file: Option<(PathBuf, bool)>,
    /// volume_group configured: (name, exists).
    pub volume_group: Option<(String, bool)>,
    /// A local libvirt host is configured: (uri, answered, error).
    pub libvirt: Option<(String, bool, String)>,
    /// The libvirt TLS server certificate (the leader drives this box's
    /// VMs over qemu+tls): exists. `None` when there is no local host.
    pub libvirt_tls: Option<bool>,
    /// The xdctl env file next to the config: exists.
    pub env_file: Option<(PathBuf, bool)>,
}

/// Fatal, retry and degraded findings, in that order.
pub fn classify(o: &Observed) -> Vec<Finding> {
    let mut out = vec![];
    let boot = "sudo xdebbied bootstrap";
    for (key, f) in [("tls_cert", &o.tls_cert), ("tls_key", &o.tls_key)] {
        if let Some((p, false)) = f {
            out.push(Finding {
                level: Level::Fatal,
                what: format!("{key} {} is configured but missing", p.display()),
                fix: format!("{boot} (re-creates the API certificate), or remove tls_cert/tls_key from the config"),
            });
        }
    }
    if let Some((p, false)) = &o.token_file {
        out.push(Finding {
            level: Level::Fatal,
            what: format!("token_file {} is configured but missing", p.display()),
            fix: format!("{boot} (a joined box gets the cluster's token again)"),
        });
    }
    if let Some((vg, false)) = &o.volume_group {
        out.push(Finding {
            level: Level::Retry,
            what: format!("LVM volume group '{vg}' not found"),
            fix: format!("if its disk is still coming up this clears itself; else: sudo pvcreate /dev/DISK && sudo vgcreate {vg} /dev/DISK"),
        });
    }
    if let Some((uri, false, err)) = &o.libvirt {
        out.push(Finding {
            level: Level::Degraded,
            what: format!("libvirt does not answer on {uri} ({err}): no VM can be started or changed here"),
            fix: "sudo systemctl start libvirtd (see journalctl -u libvirtd); xdebbied retries on its own".into(),
        });
    }
    if o.libvirt_tls == Some(false) {
        out.push(Finding {
            level: Level::Degraded,
            what: "libvirt TLS certificate missing: the leader cannot drive this box's VMs from another box".into(),
            fix: boot.into(),
        });
    }
    if let Some((p, false)) = &o.env_file {
        out.push(Finding {
            level: Level::Degraded,
            what: format!(
                "{} missing: xdctl on this box has no URL, token or CA",
                p.display()
            ),
            fix: boot.into(),
        });
    }
    out.sort_by_key(|f| match f.level {
        Level::Fatal => 0,
        Level::Retry => 1,
        Level::Degraded => 2,
    });
    out
}

/// Look, without changing anything.
pub fn observe(cfg: &Config, config_path: &Path) -> Observed {
    let exists = |p: &Path| (p.to_path_buf(), p.exists());
    let local_uri = cfg
        .hosts
        .iter()
        .map(|h| h.uri.as_str())
        .find(|u| u.contains(":///"));
    Observed {
        tls_cert: cfg.tls_cert.as_deref().map(exists),
        tls_key: cfg.tls_key.as_deref().map(exists),
        token_file: cfg.token_file.as_deref().map(exists),
        volume_group: cfg.volume_group.as_ref().map(|vg| {
            let lvm = crate::lvm::LvmManager::new(vg.clone(), cfg.thin_pool.clone());
            (vg.clone(), lvm.vg_exists().unwrap_or(false))
        }),
        libvirt: local_uri.map(|uri| {
            let (ok, err) = libvirt_answers(&cfg.virsh, uri);
            (uri.to_string(), ok, err)
        }),
        libvirt_tls: local_uri
            .map(|_| Path::new(crate::bootstrap::tls_certs::SERVER_CERT).exists()),
        env_file: config_path
            .parent()
            .map(|d| exists(&d.join(crate::bootstrap::DEFAULT_ENV_FILE))),
    }
}

/// `virsh -c URI version`, given at most 3 s.
fn libvirt_answers(virsh: &str, uri: &str) -> (bool, String) {
    match std::process::Command::new("timeout")
        .args(["3", virsh, "-c", uri, "version"])
        .output()
    {
        Ok(o) if o.status.success() => (true, String::new()),
        Ok(o) => (
            false,
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .next()
                .unwrap_or("no answer within 3 s")
                .trim()
                .to_string(),
        ),
        Err(e) => (false, e.to_string()),
    }
}

/// Log every finding; exit on a fatal (78) or retry (1) one.
pub fn run(cfg: &Config, config_path: &Path) {
    let found = classify(&observe(cfg, config_path));
    for f in &found {
        match f.level {
            Level::Fatal => tracing::error!(fix = %f.fix, "preflight: {}", f.what),
            Level::Retry => tracing::error!(fix = %f.fix, "preflight: {}", f.what),
            Level::Degraded => {
                tracing::warn!(fix = %f.fix, "preflight: {} (serving without it)", f.what)
            }
        }
    }
    if found.iter().any(|f| f.level == Level::Fatal) {
        tracing::error!(
            "cannot start; fix the above and `systemctl restart xdebbied` (not retrying)"
        );
        std::process::exit(78);
    }
    if found.iter().any(|f| f.level == Level::Retry) {
        tracing::error!("not ready yet; systemd retries in a few seconds");
        std::process::exit(1);
    }
    if found.is_empty() {
        tracing::info!("preflight: everything bootstrap sets up is in place");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_good() -> Observed {
        Observed {
            tls_cert: Some(("/etc/xdebbie/api-cert.pem".into(), true)),
            tls_key: Some(("/etc/xdebbie/api-key.pem".into(), true)),
            token_file: Some(("/etc/xdebbie/token".into(), true)),
            volume_group: Some(("xdebbie".into(), true)),
            libvirt: Some(("qemu:///system".into(), true, String::new())),
            libvirt_tls: Some(true),
            env_file: Some(("/etc/xdebbie/xdebbie_env".into(), true)),
        }
    }

    #[test]
    fn nothing_missing_nothing_said() {
        assert!(classify(&all_good()).is_empty());
        assert!(classify(&Observed::default()).is_empty());
    }

    #[test]
    fn missing_files_serve_needs_are_fatal() {
        let mut o = all_good();
        o.tls_key = Some(("/etc/xdebbie/api-key.pem".into(), false));
        o.token_file = Some(("/etc/xdebbie/token".into(), false));
        let f = classify(&o);
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|x| x.level == Level::Fatal));
        assert!(f.iter().all(|x| x.fix.contains("xdebbied bootstrap")));
    }

    #[test]
    fn a_volume_group_not_there_yet_is_retried() {
        let mut o = all_good();
        o.volume_group = Some(("xdebbie".into(), false));
        let f = classify(&o);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, Level::Retry);
        assert!(f[0].fix.contains("vgcreate xdebbie"));
    }

    #[test]
    fn libvirt_tls_and_env_gaps_only_degrade() {
        let mut o = all_good();
        o.libvirt = Some((
            "qemu:///system".into(),
            false,
            "failed to connect socket".into(),
        ));
        o.libvirt_tls = Some(false);
        o.env_file = Some(("/etc/xdebbie/xdebbie_env".into(), false));
        let f = classify(&o);
        assert_eq!(f.len(), 3);
        assert!(f.iter().all(|x| x.level == Level::Degraded));
        assert!(f[0].what.contains("failed to connect socket"));
    }

    #[test]
    fn fatal_comes_first() {
        let mut o = all_good();
        o.env_file = Some(("/etc/xdebbie/xdebbie_env".into(), false));
        o.volume_group = Some(("xdebbie".into(), false));
        o.tls_cert = Some(("/etc/xdebbie/api-cert.pem".into(), false));
        let lv: Vec<Level> = classify(&o).iter().map(|x| x.level).collect();
        assert_eq!(lv, vec![Level::Fatal, Level::Retry, Level::Degraded]);
    }

    /// The shipped unit must not run bootstrap before every start: a
    /// failing bootstrap kept the hub from serving (2026-11-24).
    #[test]
    fn unit_does_not_bootstrap_on_start() {
        let unit = include_str!("../debian/xdebbied.service");
        assert!(
            !unit
                .lines()
                .any(|l| l.trim_start().starts_with("ExecStartPre") && l.contains("bootstrap")),
            "xdebbied.service runs bootstrap on start"
        );
        assert!(unit.contains("RestartPreventExitStatus=78"));
    }
}
