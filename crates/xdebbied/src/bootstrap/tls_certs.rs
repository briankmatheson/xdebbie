// SPDX-License-Identifier: GPL-3.0-or-later
//! libvirt TLS from the cluster CA.
//!
//! The leader drives every other box's libvirt over `qemu+tls://`. libvirt
//! wants a CA, a server cert and a client cert in fixed places; all three
//! come from what this box already holds for the API: the cluster CA
//! (`api-ca.pem`) and the certificate it was enrolled with (`api-cert.pem`,
//! `api-key.pem` — CN = hostname, every local address as a SAN, EKU
//! serverAuth + clientAuth). One CA for the cluster means every box's
//! libvirtd trusts every other box's client cert with nothing copied
//! around by hand.
//!
//! Installed to:
//! - CA:     /etc/pki/CA/cacert.pem and /etc/libvirt/pki/cacert.pem
//! - Server: /etc/libvirt/pki/servercert.pem, /etc/libvirt/pki/private/serverkey.pem
//! - Client: /etc/pki/libvirt/clientcert.pem, /etc/pki/libvirt/private/clientkey.pem
//!
//! libvirtd listens on the overlay bridge address (`listen_addr`), so the
//! leader reaches it across the Geneve mesh and VMs cannot without a
//! cluster-issued client cert (`tls_no_verify_certificate = 0`).

use anyhow::{Context, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tracing::{info, warn};

use super::api_tls::ApiTlsPaths;

const CA_CERT: &str = "/etc/pki/CA/cacert.pem";
const SERVER_CA: &str = "/etc/libvirt/pki/cacert.pem";
const SERVER_CERT: &str = "/etc/libvirt/pki/servercert.pem";
const SERVER_KEY: &str = "/etc/libvirt/pki/private/serverkey.pem";
const CLIENT_CERT: &str = "/etc/pki/libvirt/clientcert.pem";
const CLIENT_KEY: &str = "/etc/pki/libvirt/private/clientkey.pem";
const LIBVIRTD_CONF: &str = "/etc/libvirt/libvirtd.conf";

/// What step 7 leaves behind, for the host registration that follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibvirtTls {
    /// Address libvirtd listens on for TLS (`listen_addr`), None = all.
    pub listen_addr: Option<String>,
    /// Whether the server cert has a SAN for `listen_addr`; without one the
    /// leader's libvirt client rejects the connection.
    pub san_covers_listen: bool,
}

impl LibvirtTls {
    /// The URI other boxes use to reach this libvirt.
    pub fn uri(&self, hostname: &str) -> String {
        format!(
            "qemu+tls://{}/system",
            self.listen_addr.as_deref().unwrap_or(hostname)
        )
    }
}

/// Write `content` to `path` (mode `mode`, parents created) unless it is
/// already there byte for byte. Returns whether anything changed.
fn put(path: &str, content: &str, mode: u32) -> Result<bool> {
    if fs::read_to_string(path)
        .map(|c| c == content)
        .unwrap_or(false)
    {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).ok();
        return Ok(false);
    }
    if let Some(dir) = Path::new(path).parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        if dir.ends_with("private") {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).ok();
        }
    }
    fs::write(path, content).with_context(|| format!("writing {path}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {path}"))?;
    Ok(true)
}

/// Install the cluster CA and this box's certificate for libvirt, point
/// libvirtd at them, and (re)start its TLS listener when anything moved.
pub fn ensure_tls_certificates(api: &ApiTlsPaths) -> Result<LibvirtTls> {
    let ca = fs::read_to_string(&api.ca_cert)
        .with_context(|| format!("reading {} (API TLS not set up?)", api.ca_cert.display()))?;
    let cert = fs::read_to_string(&api.server_cert)
        .with_context(|| format!("reading {}", api.server_cert.display()))?;
    let key = fs::read_to_string(&api.server_key)
        .with_context(|| format!("reading {}", api.server_key.display()))?;

    let mut changed = false;
    changed |= put(CA_CERT, &ca, 0o644)?;
    changed |= put(SERVER_CA, &ca, 0o644)?;
    changed |= put(SERVER_CERT, &cert, 0o644)?;
    changed |= put(SERVER_KEY, &key, 0o600)?;
    changed |= put(CLIENT_CERT, &cert, 0o644)?;
    changed |= put(CLIENT_KEY, &key, 0o600)?;
    if changed {
        info!(
            "  Installed the cluster CA and this box's certificate for libvirt (server + client)"
        );
    } else {
        info!("  ✓ libvirt uses the cluster CA and this box's certificate");
    }

    let conf_changed = configure_libvirtd_tls()?;
    let listen_addr = libvirtd_listen_addr();
    let san_covers_listen = match &listen_addr {
        Some(ip) => cert_has_ip_san(&cert, ip),
        None => true,
    };
    if let (Some(ip), false) = (&listen_addr, san_covers_listen) {
        warn!(
            "  This box's certificate has no SAN for {ip}, where libvirtd listens; other boxes' libvirt clients \
             will refuse it. The address changed since enrolment: re-enrol with `xdebbied bootstrap --join <ca_url> --force` \
             (then `xdctl approve` on the CA box)"
        );
    }

    if changed || conf_changed || !tls_socket_active() {
        restart_libvirtd_tls()?;
    }
    Ok(LibvirtTls {
        listen_addr,
        san_covers_listen,
    })
}

/// Address libvirtd listens on for TLS, per `listen_addr` in libvirtd.conf.
pub fn libvirtd_listen_addr() -> Option<String> {
    let conf = fs::read_to_string(LIBVIRTD_CONF).ok()?;
    conf.lines()
        .filter_map(|l| l.trim().strip_prefix("listen_addr"))
        .filter_map(|rest| rest.trim().strip_prefix('='))
        .filter_map(|v| {
            v.trim()
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .map(str::to_string)
        })
        .next_back()
}

/// Whether `cert_pem` carries `ip` as an IP SAN. Uses openssl's text dump
/// when available; without openssl assume yes rather than nag.
fn cert_has_ip_san(cert_pem: &str, ip: &str) -> bool {
    use std::io::Write;
    let Ok(mut child) = Command::new("openssl")
        .args(["x509", "-noout", "-ext", "subjectAltName"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return true;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(cert_pem.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return true;
    };
    if !out.status.success() {
        return true;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split(',')
        .any(|s| s.trim() == format!("IP Address:{ip}"))
}

fn tls_socket_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", "libvirtd-tls.socket"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Socket activation: the socket must be up before the service starts, so
/// enable+start it, then restart the service to pick it up. Running VMs
/// are separate processes and are not affected.
fn restart_libvirtd_tls() -> Result<()> {
    let _ = Command::new("systemctl")
        .args(["reset-failed", "libvirtd-tls.socket"])
        .status();
    let sock = Command::new("systemctl")
        .args(["enable", "--now", "libvirtd-tls.socket"])
        .output()
        .context("systemctl")?;
    if !sock.status.success() {
        warn!(
            "  Could not enable libvirtd-tls.socket: {}",
            String::from_utf8_lossy(&sock.stderr).trim()
        );
        return Ok(());
    }
    let svc = Command::new("systemctl")
        .args(["restart", "libvirtd.service"])
        .output()
        .context("systemctl")?;
    if svc.status.success() {
        info!(
            "  ✓ libvirtd restarted with TLS on {}",
            libvirtd_listen_addr().unwrap_or_else(|| "all addresses".into())
        );
    } else {
        warn!(
            "  libvirtd restart failed: {}",
            String::from_utf8_lossy(&svc.stderr).trim()
        );
    }
    Ok(())
}

fn get_mesh_ip() -> Option<String> {
    for iface in &["virbr-xdebbie", "virbr-int", "br-int"] {
        let output = Command::new("ip")
            .args(["-4", "-o", "addr", "show", iface])
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let mut f = line.split_whitespace();
                if f.any(|w| w == "inet")
                    && let Some(addr) = f.next().and_then(|s| s.split('/').next())
                {
                    return Some(addr.to_string());
                }
            }
        }
    }
    None
}

/// Make libvirtd listen on TLS, on the overlay bridge address. Returns
/// whether libvirtd.conf was written.
pub fn configure_libvirtd_tls() -> Result<bool> {
    if Path::new(LIBVIRTD_CONF).exists() {
        let content = fs::read_to_string(LIBVIRTD_CONF)?;
        if content.contains("listen_tls = 1") {
            info!("  ✓ libvirtd already configured for TLS");
            return Ok(false);
        }
    }

    info!("  Configuring libvirtd for TLS (overlay only)...");
    let listen_line = match get_mesh_ip() {
        Some(ip) => {
            info!("  Binding to overlay address: {ip}");
            format!("listen_addr = \"{ip}\"")
        }
        None => {
            warn!("  Could not detect the overlay bridge address; binding to all interfaces");
            "# listen_addr not set - will bind to all interfaces".to_string()
        }
    };
    let tls_config = format!(
        r#"
# Added by xdebbied: libvirt over TLS with the cluster CA, overlay only
listen_tls = 1
listen_tcp = 0
tls_port = "16514"
{listen_line}
ca_file = "{SERVER_CA}"
cert_file = "{SERVER_CERT}"
key_file = "{SERVER_KEY}"
tls_no_verify_certificate = 0
auth_tls = "none"
"#
    );
    let mut content = fs::read_to_string(LIBVIRTD_CONF).unwrap_or_default();
    content.push_str(&tls_config);
    fs::write(LIBVIRTD_CONF, content).context("writing libvirtd.conf")?;
    // libvirtd uses socket activation; make sure no stale --listen remains.
    let _ = fs::write(
        "/etc/default/libvirtd",
        "# libvirtd uses systemd socket activation\n",
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_prefers_listen_addr() {
        let t = LibvirtTls {
            listen_addr: Some("10.23.0.2".into()),
            san_covers_listen: true,
        };
        assert_eq!(t.uri("gram.example"), "qemu+tls://10.23.0.2/system");
        let t = LibvirtTls {
            listen_addr: None,
            san_covers_listen: true,
        };
        assert_eq!(t.uri("gram.example"), "qemu+tls://gram.example/system");
    }

    #[test]
    fn san_check_reads_openssl_output() {
        if Command::new("openssl").arg("version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let paths = super::super::api_tls::generate_localhost(dir.path());
        let cert = fs::read_to_string(&paths.server_cert).unwrap();
        assert!(cert_has_ip_san(&cert, "127.0.0.1"));
        assert!(!cert_has_ip_san(&cert, "10.9.9.9"));
    }
}
