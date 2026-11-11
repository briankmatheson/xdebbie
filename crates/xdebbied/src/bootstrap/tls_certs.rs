// SPDX-License-Identifier: GPL-3.0-or-later
//! Automatic TLS certificate generation for libvirt remote connections.
//!
//! Generates and manages certificates in standard locations:
//! - CA: /etc/pki/CA/{cacert.pem, private/cakey.pem}
//! - Server: /etc/libvirt/pki/{servercert.pem, private/serverkey.pem}
//! - Client: /etc/pki/libvirt/{clientcert.pem, private/clientkey.pem}
//!
//! Also copies to /etc/libvirt/pki/ for compatibility.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

const CA_DIR: &str = "/etc/pki/CA";
const CA_PRIVATE_DIR: &str = "/etc/pki/CA/private";
const CA_CERT: &str = "/etc/pki/CA/cacert.pem";
const CA_KEY: &str = "/etc/pki/CA/private/cakey.pem";

const SERVER_DIR: &str = "/etc/libvirt/pki";
const SERVER_PRIVATE_DIR: &str = "/etc/libvirt/pki/private";
const SERVER_CERT: &str = "/etc/libvirt/pki/servercert.pem";
const SERVER_KEY: &str = "/etc/libvirt/pki/private/serverkey.pem";

const CLIENT_DIR: &str = "/etc/pki/libvirt";
const CLIENT_PRIVATE_DIR: &str = "/etc/pki/libvirt/private";
const CLIENT_CERT: &str = "/etc/pki/libvirt/clientcert.pem";
const CLIENT_KEY: &str = "/etc/pki/libvirt/private/clientkey.pem";

/// Check if a certificate file exists and is valid
fn cert_is_valid(path: &str) -> bool {
    if !Path::new(path).exists() {
        return false;
    }
    
    // Try to parse it with openssl
    let output = Command::new("openssl")
        .args(&["x509", "-in", path, "-noout"])
        .output();
    
    matches!(output, Ok(o) if o.status.success())
}

/// Generate CA certificate and private key
fn generate_ca() -> Result<()> {
    info!("Generating CA certificate...");
    
    // Create directories
    fs::create_dir_all(CA_DIR).context("creating CA directory")?;
    fs::create_dir_all(CA_PRIVATE_DIR).context("creating CA private directory")?;
    
    // Generate CA private key (4096-bit RSA, 10-year validity)
    let output = Command::new("openssl")
        .args(&["genrsa", "-out", CA_KEY, "4096"])
        .output()
        .context("generating CA private key")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to generate CA key: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    // Generate self-signed CA certificate
    let output = Command::new("openssl")
        .args(&[
            "req", "-new", "-x509",
            "-days", "3650",
            "-key", CA_KEY,
            "-out", CA_CERT,
            "-subj", "/CN=XDebbie CA/O=XDebbie/OU=Certificate Authority",
        ])
        .output()
        .context("generating CA certificate")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to generate CA cert: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    // Set permissions
    fs::set_permissions(CA_KEY, fs::Permissions::from_mode(0o600))
        .context("setting CA key permissions")?;
    fs::set_permissions(CA_CERT, fs::Permissions::from_mode(0o644))
        .context("setting CA cert permissions")?;
    
    info!("  ✓ CA certificate generated");
    Ok(())
}

/// Every global-scope IPv4 address on this host (LAN, WireGuard underlay,
/// mesh bridge, ...). Used for the server cert's SubjectAltName.
fn local_ipv4_addrs() -> Vec<String> {
    let out = Command::new("ip")
        .args(&["-4", "-o", "addr", "show", "scope", "global"])
        .output();
    let Ok(out) = out else { return vec![] };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        // "2: eth0    inet 192.168.1.156/24 brd ... scope global eth0"
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            f.position(|w| w == "inet")?;
            f.next()?.split('/').next().map(str::to_string)
        })
        .collect()
}

/// Build the subjectAltName list for the server cert: short name, FQDN and
/// every local IPv4 address. libvirt clients verify the server cert against
/// whatever host appears in the URI (`qemu+tls://192.168.1.156/system`), so a
/// cert with only CN=<name> is rejected with "Certificate owner does not
/// match the hostname".
fn server_san(hostname: &str) -> String {
    let mut san = vec![format!("DNS:{}", hostname)];
    if let Ok(fqdn) = Command::new("hostname").arg("-f").output() {
        let fqdn = String::from_utf8_lossy(&fqdn.stdout).trim().to_string();
        if !fqdn.is_empty() && fqdn != hostname {
            san.push(format!("DNS:{}", fqdn));
        }
    }
    for ip in local_ipv4_addrs() {
        san.push(format!("IP:{}", ip));
    }
    san.join(",")
}

/// Does the server cert carry a SubjectAltName that covers `hostname` and the
/// host's current IPs? Certs issued before SANs were added (or after an IP
/// change) must be reissued or every qemu+tls connection fails.
fn server_cert_san_ok(hostname: &str) -> bool {
    let out = Command::new("openssl")
        .args(&["x509", "-in", SERVER_CERT, "-noout", "-ext", "subjectAltName"])
        .output();
    let Ok(out) = out else { return false };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if !text.contains(&format!("DNS:{}", hostname)) {
        return false;
    }
    local_ipv4_addrs()
        .iter()
        .all(|ip| text.contains(&format!("IP Address:{}", ip)))
}

/// Generate server certificate for libvirtd
fn generate_server_cert(hostname: &str) -> Result<()> {
    info!("Generating server certificate for {}...", hostname);
    
    // Create directories
    fs::create_dir_all(SERVER_DIR).context("creating server cert directory")?;
    fs::create_dir_all(SERVER_PRIVATE_DIR).context("creating server private directory")?;
    
    // Generate server private key
    let output = Command::new("openssl")
        .args(&["genrsa", "-out", SERVER_KEY, "2048"])
        .output()
        .context("generating server private key")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to generate server key: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    // Generate CSR
    let csr_path = "/tmp/server.csr";
    let subject = format!("/CN={}/O=libvirt/OU=server", hostname);
    let output = Command::new("openssl")
        .args(&[
            "req", "-new",
            "-key", SERVER_KEY,
            "-out", csr_path,
            "-subj", &subject,
        ])
        .output()
        .context("generating server CSR")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to generate server CSR: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    // x509 extensions: SAN + server usage. `openssl x509 -req` drops CSR
    // extensions unless given an -extfile, so write one explicitly.
    let san = server_san(hostname);
    info!("  SAN: {}", san);
    let ext_path = "/tmp/server.ext";
    fs::write(
        ext_path,
        format!(
            "subjectAltName={}\nextendedKeyUsage=serverAuth\nkeyUsage=digitalSignature,keyEncipherment\n",
            san
        ),
    )
    .context("writing server cert extensions")?;

    // Sign with CA
    let output = Command::new("openssl")
        .args(&[
            "x509", "-req",
            "-in", csr_path,
            "-CA", CA_CERT,
            "-CAkey", CA_KEY,
            "-CAcreateserial",
            "-extfile", ext_path,
            "-out", SERVER_CERT,
            "-days", "365",
            "-sha256",
        ])
        .output()
        .context("signing server certificate")?;
    
    let _ = fs::remove_file(csr_path);
    let _ = fs::remove_file(ext_path);

    if !output.status.success() {
        anyhow::bail!("Failed to sign server cert: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    // Set permissions
    fs::set_permissions(SERVER_KEY, fs::Permissions::from_mode(0o600))
        .context("setting server key permissions")?;
    fs::set_permissions(SERVER_CERT, fs::Permissions::from_mode(0o644))
        .context("setting server cert permissions")?;
    
    // Copy CA cert to server location for libvirtd
    fs::copy(CA_CERT, format!("{}/cacert.pem", SERVER_DIR))
        .context("copying CA cert to server directory")?;
    
    info!("  ✓ Server certificate generated");
    Ok(())
}

/// Generate client certificate for connecting to remote libvirt
fn generate_client_cert(hostname: &str) -> Result<()> {
    info!("Generating client certificate for {}...", hostname);
    
    // Create directories
    fs::create_dir_all(CLIENT_DIR).context("creating client cert directory")?;
    fs::create_dir_all(CLIENT_PRIVATE_DIR).context("creating client private directory")?;
    
    // Generate client private key
    let output = Command::new("openssl")
        .args(&["genrsa", "-out", CLIENT_KEY, "2048"])
        .output()
        .context("generating client private key")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to generate client key: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    // Generate CSR
    let csr_path = "/tmp/client.csr";
    let subject = format!("/CN={}/O=libvirt/OU=client", hostname);
    let output = Command::new("openssl")
        .args(&[
            "req", "-new",
            "-key", CLIENT_KEY,
            "-out", csr_path,
            "-subj", &subject,
        ])
        .output()
        .context("generating client CSR")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to generate client CSR: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    // Sign with CA
    let output = Command::new("openssl")
        .args(&[
            "x509", "-req",
            "-in", csr_path,
            "-CA", CA_CERT,
            "-CAkey", CA_KEY,
            "-CAcreateserial",
            "-out", CLIENT_CERT,
            "-days", "365",
            "-sha256",
        ])
        .output()
        .context("signing client certificate")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to sign client cert: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    // Clean up CSR
    let _ = fs::remove_file(csr_path);
    
    // Set permissions
    fs::set_permissions(CLIENT_KEY, fs::Permissions::from_mode(0o600))
        .context("setting client key permissions")?;
    fs::set_permissions(CLIENT_CERT, fs::Permissions::from_mode(0o644))
        .context("setting client cert permissions")?;
    
    // Also copy to /etc/libvirt/pki/ for compatibility
    let libvirt_client_dir = "/etc/libvirt/pki";
    let libvirt_client_private = "/etc/libvirt/pki/private";
    fs::create_dir_all(libvirt_client_dir).ok();
    fs::create_dir_all(libvirt_client_private).ok();
    
    fs::copy(CLIENT_CERT, format!("{}/clientcert.pem", libvirt_client_dir))
        .context("copying client cert to /etc/libvirt/pki")?;
    fs::copy(CLIENT_KEY, format!("{}/clientkey.pem", libvirt_client_private))
        .context("copying client key to /etc/libvirt/pki")?;
    
    info!("  ✓ Client certificate generated");
    Ok(())
}

/// Get mesh interface IP address
fn get_mesh_ip() -> Option<String> {
    // Try common mesh interface names
    for iface in &["virbr-xdebbie", "virbr-int", "br-int"] {
        let output = Command::new("ip")
            .args(&["addr", "show", iface])
            .output();
        
        if let Ok(output) = output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Parse "inet 10.23.0.1/16" line
                for line in stdout.lines() {
                    if line.contains("inet 10.23") || line.contains("inet 192.168") {
                        if let Some(ip) = line.trim().split_whitespace().nth(1) {
                            if let Some(ip_only) = ip.split('/').next() {
                                return Some(ip_only.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Configure libvirtd to listen on TLS (mesh network only)
pub fn configure_libvirtd_tls() -> Result<()> {
    let config_path = "/etc/libvirt/libvirtd.conf";
    
    // Check if already configured
    if Path::new(config_path).exists() {
        let content = fs::read_to_string(config_path)?;
        if content.contains("listen_tls = 1") {
            info!("  libvirtd already configured for TLS");
            return Ok(());
        }
    }
    
    info!("Configuring libvirtd for TLS (mesh network only)...");
    
    // Get mesh IP for listen_addr
    let mesh_ip = get_mesh_ip();
    let listen_line = if let Some(ip) = mesh_ip {
        info!("  Binding to mesh IP: {}", ip);
        format!("listen_addr = \"{}\"", ip)
    } else {
        warn!("  Could not detect mesh IP, binding to all interfaces");
        "# listen_addr not set - will bind to all interfaces".to_string()
    };
    
    // Append TLS configuration
    let tls_config = format!(r#"
# Added by xdebbied for multi-host support - MESH NETWORK ONLY
listen_tls = 1
listen_tcp = 0
tls_port = "16514"
{}
ca_file = "/etc/libvirt/pki/cacert.pem"
cert_file = "/etc/libvirt/pki/servercert.pem"
key_file = "/etc/libvirt/pki/private/serverkey.pem"
tls_no_verify_certificate = 0
auth_tls = "none"
"#, listen_line);
    
    let mut content = String::new();
    if Path::new(config_path).exists() {
        content = fs::read_to_string(config_path)?;
    }
    
    content.push_str(&tls_config);
    fs::write(config_path, content)
        .context("writing libvirtd.conf")?;
    
    // Modern libvirtd uses socket activation, not --listen
    // Enable libvirtd-tls.socket for TLS listening
    let socket_enable = Command::new("systemctl")
        .args(&["enable", "libvirtd-tls.socket"])
        .output();
    
    if let Ok(output) = socket_enable {
        if output.status.success() {
            info!("  ✓ libvirtd-tls.socket enabled");
        } else {
            warn!("  Could not enable libvirtd-tls.socket (may not exist on older systems)");
        }
    }
    
    // Remove deprecated --listen flag if present
    let default_path = "/etc/default/libvirtd";
    let default_content = "# libvirtd uses systemd socket activation\n";
    let _ = fs::write(default_path, default_content); // Don't fail if this doesn't work
    
    info!("  ✓ libvirtd configured for TLS (using socket activation)");
    info!("  Note: Enable TLS socket: sudo systemctl enable --now libvirtd-tls.socket");
    
    Ok(())
}

/// Ensure all TLS certificates exist and are valid
pub fn ensure_tls_certificates(force: bool) -> Result<()> {
    info!("Step 7/8: Ensuring TLS certificates for libvirt");
    
    // Get hostname
    let hostname = hostname::get()
        .context("getting hostname")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("invalid hostname"))?;
    let hostname = hostname.split('.').next().unwrap_or(&hostname);
    
    // Check/generate CA
    if !cert_is_valid(CA_CERT) || force {
        generate_ca()?;
    } else {
        info!("  ✓ CA certificate exists and is valid");
    }
    
    // Check/generate server certificate. A parseable cert is not enough: it
    // must also carry a SAN matching this host's name and addresses.
    if !cert_is_valid(SERVER_CERT) || force {
        generate_server_cert(hostname)?;
    } else if !server_cert_san_ok(hostname) {
        warn!("  Server certificate lacks a SAN for this host's name/IPs; reissuing");
        generate_server_cert(hostname)?;
    } else {
        info!("  ✓ Server certificate exists and is valid");
    }
    
    // Check/generate client certificate
    if !cert_is_valid(CLIENT_CERT) || force {
        generate_client_cert(hostname)?;
    } else {
        info!("  ✓ Client certificate exists and is valid");
    }
    
    // Configure libvirtd
    configure_libvirtd_tls()?;
    
    info!("");
    info!("TLS certificates ready:");
    info!("  CA:     {}", CA_CERT);
    info!("  Server: {}", SERVER_CERT);
    info!("  Client: {}", CLIENT_CERT);
    info!("");
    info!("For multi-host setup:");
    info!("  1. Copy {} from THIS host to /etc/pki/CA/cacert.pem on OTHER hosts", CA_CERT);
    info!("  2. Or serve it: cd /etc/pki/CA && python3 -m http.server 7080");
    info!("  3. Then fetch: curl http://<this-host>:7080/cacert.pem | sudo tee /etc/pki/CA/cacert.pem");
    info!("  4. Restart libvirtd on ALL hosts: sudo systemctl restart libvirtd");
    
    Ok(())
}

// Use std::os::unix::fs::PermissionsExt for from_mode
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
