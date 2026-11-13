// SPDX-License-Identifier: GPL-3.0-or-later
//! Bootstrap functionality for xdebbied.
//!
//! This module provides automatic dependency checking, libvirt resource
//! discovery/creation, configuration generation, and token generation to
//! enable single-binary deployment on stock Debian systems.

pub mod addr;
pub mod api_tls;
pub mod config_gen;
pub mod deps;
pub mod discover;
pub mod dns;
pub mod firewall;
pub mod join;
pub mod libvirt_setup;
pub mod tls_certs;
pub mod token;

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Default configuration paths
pub const DEFAULT_CONFIG_DIR: &str = "/etc/xdebbie";
pub const DEFAULT_CONFIG_FILE: &str = "/etc/xdebbie/xdebbied.toml";
pub const DEFAULT_TOKEN_FILE: &str = "/etc/xdebbie/token";
pub const DEFAULT_STATE_DIR: &str = "/var/lib/xdebbie";
pub const DEFAULT_ENV_FILE: &str = "xdebbie_env";

/// Default libvirt URIs and resources
pub const DEFAULT_LIBVIRT_URI: &str = "qemu:///system";
pub const DEFAULT_NETWORK_NAME: &str = "xdebbie";
/// The overlay: one /16, box N owns `10.23.N.0/24` (see `addr`).
pub const DEFAULT_NETWORK_SUBNET: &str = "10.23.0.0/16";
pub const DEFAULT_POOL_NAME: &str = "xdebbie";
pub const DEFAULT_POOL_PATH: &str = "/var/lib/libvirt/xdebbie";

/// Bootstrap options
#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    pub config_path: PathBuf,
    pub token_path: PathBuf,
    pub state_dir: PathBuf,
    pub libvirt_uri: String,
    pub network_name: String,
    pub network_subnet: String,
    pub pool_name: String,
    pub pool_path: String,
    pub listen_addr: String,
    pub force: bool,
    /// Mesh box id (0-253) to ask for. A joiner gets one assigned by the
    /// CA box (this is only a wish); the first box defaults to 0.
    pub box_id: Option<u8>,
    /// Join an existing cluster through the CA box at this URL.
    pub join: Option<String>,
    /// Pinned `sha256:` fingerprint of that cluster CA.
    pub ca_fingerprint: Option<String>,
    /// Do not look for a running cluster: this box starts one.
    pub standalone: bool,
    /// How long to listen for a running cluster when neither `join` nor
    /// `standalone` is given.
    pub discover_secs: u64,
    /// Multicast group to listen on.
    pub multicast_addr: String,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            config_path: PathBuf::from(DEFAULT_CONFIG_FILE),
            token_path: PathBuf::from(DEFAULT_TOKEN_FILE),
            state_dir: PathBuf::from(DEFAULT_STATE_DIR),
            libvirt_uri: DEFAULT_LIBVIRT_URI.to_string(),
            network_name: DEFAULT_NETWORK_NAME.to_string(),
            network_subnet: DEFAULT_NETWORK_SUBNET.to_string(),
            pool_name: DEFAULT_POOL_NAME.to_string(),
            pool_path: DEFAULT_POOL_PATH.to_string(),
            listen_addr: "0.0.0.0:7443".to_string(),
            force: false,
            box_id: None,
            join: None,
            ca_fingerprint: None,
            standalone: false,
            discover_secs: discover::DEFAULT_LISTEN_SECS,
            multicast_addr: crate::discovery::multicast::DEFAULT_MULTICAST_ADDR.to_string(),
        }
    }
}

/// Ensure the `xdebbie` system group exists, creating it if necessary.
/// Members of this group can read `/etc/xdebbie/xdebbie_env` and
/// `/etc/xdebbie/xdebbied.toml` without sudo.
pub fn ensure_xdebbie_group() -> Result<()> {
    if crate::token::xdebbie_gid().is_some() {
        info!("Group 'xdebbie' already exists");
        return Ok(());
    }
    info!("Creating system group 'xdebbie'");
    let status = std::process::Command::new("groupadd")
        .args(["--system", "xdebbie"])
        .status()
        .context("running groupadd")?;
    if !status.success() {
        anyhow::bail!("groupadd --system xdebbie failed: {status}");
    }
    info!("Group 'xdebbie' created");
    Ok(())
}

/// Set ownership and permissions on the config directory and every file in it:
///   /etc/xdebbie/          root:xdebbie 0750  (group can enter + read)
///   xdebbied.toml          root:xdebbie 0440
///   xdebbie_env            root:xdebbie 0440
///   api-ca.pem, api-cert.pem  root:xdebbie 0440
///   peers/                 root:xdebbie 0750
///   token, token.previous  root:root    0400  (never group-readable)
///   api-key.pem            root:root    0400
///
/// Non-fatal: a warning is logged for each failure so a partial install
/// doesn't abort an otherwise successful bootstrap.
fn chown_config_dir(cfg_dir: &Path) {
    let gid = match crate::token::xdebbie_gid() {
        Some(g) => g,
        None => {
            warn!("xdebbie group not found; skipping config dir ownership fixup");
            return;
        }
    };

    // Helper: chown + chmod a path, log on error.
    let fix = |path: &Path, mode: u32, owner_gid: u32| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
                warn!("chmod {:o} {}: {e}", mode, path.display());
            }
            let cstr = match std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
                Ok(c) => c,
                Err(_) => return,
            };
            // SAFETY: standard libc call; cstr lives for the duration.
            let rc = unsafe { libc::chown(cstr.as_ptr(), 0, owner_gid) };
            if rc != 0 {
                warn!(
                    "chown root:{} {}: {}",
                    owner_gid,
                    path.display(),
                    std::io::Error::last_os_error()
                );
            }
        }
    };

    // The directory itself: group-executable so members can cd into it.
    fix(cfg_dir, 0o750, gid);

    // Walk immediate children only — no recursion needed.
    let rd = match std::fs::read_dir(cfg_dir) {
        Ok(r) => r,
        Err(e) => {
            warn!("read_dir {}: {e}", cfg_dir.display());
            return;
        }
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        match name.as_ref() {
            "token" | "token.previous" | "api-key.pem" | "api-ca-key.pem" => fix(&path, 0o400, 0), // root:root
            "xdebbied.toml" | "xdebbie_env" | "api-ca.pem" | "api-cert.pem" => {
                fix(&path, 0o440, gid)
            } // root:xdebbie
            "peers" => fix(&path, 0o750, gid),
            _ => {} // backups etc. — leave alone
        }
    }

    info!(
        "Config dir {}: root:xdebbie 0750; toml+env: 0440; token: 0400",
        cfg_dir.display()
    );
}

/// Perform full bootstrap process
pub fn bootstrap(opts: &BootstrapOptions) -> Result<()> {
    info!("=== xdebbied Bootstrap ===");
    if !deps::is_root() {
        bail!(
            "bootstrap writes /etc/xdebbie and talks to libvirt; run it as root: sudo xdebbied bootstrap ..."
        );
    }

    // Step 0: Ensure the xdebbie system group exists before writing any files
    // so that write_group_readable() gets the right gid on the first run.
    if let Err(e) = ensure_xdebbie_group() {
        warn!("Could not create xdebbie group: {e:#}; config files will be root:root 0400");
    }

    // Step 0b: Which cluster? Explicit --join / --standalone win. A box that
    // already has an identity (CA key here, or ca_url in its config) keeps
    // it. Otherwise listen for a running cluster and ask.
    let mut opts = opts.clone();
    if opts.join.is_none() && !opts.standalone {
        let tls = api_tls::ApiTlsPaths::default();
        let known_ca_url = crate::config::Config::load(&opts.config_path)
            .ok()
            .and_then(|c| c.ca_url);
        if tls.is_ca() {
            info!("This box holds the cluster CA; not looking for another cluster");
        } else if let Some(u) = known_ca_url {
            info!("Config says ca_url = {u}; re-joining that cluster");
            opts.join = Some(u);
        } else {
            let hostname = libvirt_setup::get_hostname();
            match discover::choose(
                &opts.multicast_addr,
                std::time::Duration::from_secs(opts.discover_secs),
                &hostname,
            )? {
                discover::Choice::Join(url) => {
                    info!("Joining the cluster at {url}");
                    opts.join = Some(url);
                }
                discover::Choice::First => {
                    info!("Starting a new cluster: this box will hold the CA")
                }
            }
        }
    }
    let opts = &opts;

    // Step 1: Check dependencies
    info!("Step 1/9: Checking dependencies");
    let missing = deps::check_dependencies()?;
    if !missing.is_empty() {
        warn!("Missing dependencies: {}", missing.join(", "));
        deps::install_dependencies(&missing)?;
    } else {
        info!("All dependencies satisfied");
    }

    // Step 2: Check libvirtd (optional - socket activation may be used)
    info!("Step 2/9: Checking libvirtd availability");
    if let Err(e) = deps::ensure_libvirtd_running() {
        warn!(
            "libvirtd service not available (this is OK if using socket activation): {}",
            e
        );
        // Socket activation is fine, but if nothing answers on the URI there is
        // no point continuing: every later step would fail with a bare
        // "failed to connect socket". Fail here with the service status.
        if let Err(probe) = libvirt_setup::probe(&opts.libvirt_uri) {
            let status = deps::libvirtd_status();
            anyhow::bail!(
                "libvirt is not reachable on {} ({probe}); libvirtd failed to start:\n{status}\n\
                 Fix libvirtd (see `journalctl -u libvirtd`) and re-run `xdebbied bootstrap`.",
                opts.libvirt_uri
            );
        }
        info!(
            "libvirt answers on {} via socket activation - continuing",
            opts.libvirt_uri
        );
    }

    // Step 3: Discover existing resources
    info!("Step 3/9: Discovering libvirt resources");
    let resources = libvirt_setup::discover_libvirt(&opts.libvirt_uri)?;
    info!(
        "Found {} networks and {} storage pools",
        resources.networks.len(),
        resources.pools.len()
    );

    // Create state directory
    std::fs::create_dir_all(&opts.state_dir).context("creating state directory")?;

    let api_tls_paths = api_tls::ApiTlsPaths::default();
    let hostname = libvirt_setup::get_hostname();
    let reach = reach_host(&hostname, &opts.multicast_addr);
    let cfg_dir = opts
        .config_path
        .parent()
        .unwrap_or_else(|| Path::new(DEFAULT_CONFIG_DIR));

    // Step 4: identity. A joining box gets its certificate, the token and
    // its box id from the cluster CA (after `xdctl approve`); the first box
    // mints its own. This comes before the overlay network exists because
    // the box id fixes the bridge MAC.
    std::fs::create_dir_all(cfg_dir).with_context(|| format!("creating {}", cfg_dir.display()))?;
    let existing_box: Option<u8> = crate::config::Config::load(&opts.config_path)
        .ok()
        .and_then(|c| c.mesh.as_ref().map(|m| m.box_id));
    let (token, box_id) = if let Some(ca_url) = &opts.join {
        info!("Step 4/9: Enrolling with the cluster CA at {ca_url}");
        if api_tls_paths.all_exist() && token::token_exists(&opts.token_path) && !opts.force {
            info!(
                "Already enrolled (certificate and token present); reusing. Use --force to re-enrol"
            );
            let b = existing_box.or(opts.box_id).unwrap_or(0);
            (token::read_token(&opts.token_path)?, b)
        } else {
            // A kept advertise_url is the operator's, unless the name it
            // was built from no longer resolves: then it is a URL nobody
            // can open, and the address wins.
            let api_url = match crate::config::Config::load(&opts.config_path) {
                Ok(cfg) if reach.by_name => cfg.advertise_url(cfg.effective_listen()),
                _ => derive_advertise_url(&opts.listen_addr, &reach.host),
            };
            let joined = join::join(&join::JoinOptions {
                ca_url,
                expect_fingerprint: opts.ca_fingerprint.as_deref(),
                hostname: &hostname,
                box_id: opts.box_id.or(existing_box),
                api_url: &api_url,
                force: opts.force,
                paths: &api_tls_paths,
                state_dir: &opts.state_dir,
            })
            .context("joining the cluster")?;
            if opts.token_path.exists() {
                let bak = config_gen::back_up(&opts.token_path)?;
                info!(
                    "Replacing the local token with the cluster's; previous one saved as {}",
                    bak.display()
                );
            }
            let set = joined.tokens;
            token::write_token(&opts.token_path, &set.current)?;
            let prev_path = opts.token_path.with_file_name("token.previous");
            match set.previous {
                Some(p) => crate::token::write_private(&prev_path, &p)?,
                None => {
                    let _ = std::fs::remove_file(&prev_path);
                }
            }
            match existing_box {
                Some(b) if b != joined.box_id => {
                    warn!(
                        "the cluster assigned box id {} (this config had {b}); the bridge MAC changes with it",
                        joined.box_id
                    )
                }
                _ => info!("The cluster assigned this host box id {}", joined.box_id),
            }
            (set.current, joined.box_id)
        }
    } else {
        info!("Step 4/9: Generating API token");
        let token = if token::token_exists(&opts.token_path) && !opts.force {
            info!(
                "Token already exists at {}, reusing",
                opts.token_path.display()
            );
            token::read_token(&opts.token_path)?
        } else {
            if opts.token_path.exists() {
                let bak = config_gen::back_up(&opts.token_path)?;
                warn!(
                    "Replacing the API token (--force); previous one saved as {}. Clients holding the old token (xdctl env, abe-csi, CAPX) must be updated",
                    bak.display()
                );
            }
            let new_token = token::generate_token()?;
            token::write_token(&opts.token_path, &new_token)?;
            new_token
        };

        // API TLS: the cluster CA (kept here) and this box's cert.
        info!("Generating API TLS certificates");
        if let Err(e) = api_tls::ensure_api_tls(&hostname, opts.force, &api_tls_paths) {
            warn!("Failed to generate API TLS certs: {e:#}");
            warn!("xdebbied will serve plain HTTP until certs are fixed; re-run bootstrap");
        }
        // The first box takes 0 unless told otherwise; later boxes are
        // numbered by the CA as they enrol.
        let b = opts.box_id.or(existing_box).unwrap_or(0);
        (token, b)
    };
    info!("Mesh box id: {box_id}");

    // A joiner copies the overlay the cluster already runs ([network] on
    // the CA box) so every box agrees on cidr/gateway and this one gets its
    // own slice to allocate from.
    let cluster_net = match &opts.join {
        Some(ca_url) => match join::fetch_network(ca_url, &api_tls_paths) {
            Ok(Some(n)) => {
                info!(
                    "Cluster overlay: {} gateway {} (from {ca_url})",
                    n.cidr, n.gateway
                );
                Some(n)
            }
            Ok(None) => {
                info!("The CA box has no [network]; this box's [network] is generated locally");
                None
            }
            Err(e) => {
                warn!(
                    "Could not read the cluster's network from {ca_url}: {e:#}; [network] is generated locally"
                );
                None
            }
        },
        None => None,
    };

    // Step 4: Create resources if needed
    info!("Step 5/9: Ensuring required resources exist");

    // Check if we need to create default network
    let network_exists = resources
        .networks
        .iter()
        .any(|n| n.name == opts.network_name);

    // No libvirt DHCP range: xdebbied's own responder serves the bridge
    // (`[network]` in the generated config). Two DHCP servers on one bridge
    // — worse, on one meshed L2 — is the thing we are avoiding.
    let want_mac = crate::mesh::bridge_mac(box_id);
    // `created_subnet` is Some when xdebbied may run DHCP on the bridge:
    // the network is ours (just created) or an adopted one without a
    // <dhcp> range — typically ours from an earlier bootstrap.
    let created_subnet = if !network_exists {
        libvirt_setup::ensure_network(
            &opts.libvirt_uri,
            &opts.network_name,
            &opts.network_subnet,
            box_id,
            "",
            "",
            &want_mac,
        )?;
        Some(opts.network_subnet.clone())
    } else {
        // Not ours to change live (a MAC change resets every guest's ARP
        // entry for the gateway); say what the mesh will keep reporting.
        match libvirt_setup::network_mac(&opts.libvirt_uri, &opts.network_name) {
            Some(mac) if mac != want_mac => warn!(
                "network '{}' has MAC {mac}; the mesh (box {}) expects {want_mac}. Fix with: \
                 `virsh net-edit {}` (set <mac address='{want_mac}'/>) and, to apply now, \
                 `ip link set <bridge> address {want_mac}`",
                opts.network_name, box_id, opts.network_name
            ),
            _ => {}
        }
        match libvirt_setup::network_shape(&opts.libvirt_uri, &opts.network_name) {
            Some(shape) if !shape.has_dhcp => {
                let subnet = shape.subnet.unwrap_or_else(|| opts.network_subnet.clone());
                info!(
                    "Adopted network '{}' has no DHCP range; xdebbied serves {subnet} on its bridge",
                    opts.network_name
                );
                // Same rule as a created network: box N's bridge is <subnet>.N.1,
                // where the mesh probes it and where its VMs are routed.
                match (shape.address, addr::box_block_str(&subnet, box_id)) {
                    (Some(have), Ok(want)) if have != want.bridge => warn!(
                        "network '{}' bridge is {have}; box {box_id} owns {}/24 so it should be {}. Fix with: \
                         `virsh net-edit {}` (set <ip address='{}'/>) and, to apply now, \
                         `ip addr replace {}/{} dev <bridge>` — peers probe that address",
                        opts.network_name,
                        want.network(),
                        want.bridge,
                        opts.network_name,
                        want.bridge,
                        want.bridge,
                        subnet.rsplit('/').next().unwrap_or("16"),
                    ),
                    (_, Err(e)) => warn!("network '{}': {e:#}", opts.network_name),
                    _ => {}
                }
                if shape.forward.is_none() {
                    warn!(
                        "network '{}' has no <forward>: libvirt rejects every forwarded packet, so VMs cannot leave this box. \
                         Fix with: `virsh net-edit {}` (add <forward mode='route'/>), then `virsh net-destroy {}; virsh net-start {}` \
                         (VMs reattach on their next restart)",
                        opts.network_name, opts.network_name, opts.network_name, opts.network_name
                    );
                }
                Some(subnet)
            }
            Some(_) => {
                info!(
                    "Adopted network '{}' runs its own DHCP; [network] left commented out",
                    opts.network_name
                );
                None
            }
            None => None,
        }
    };

    // Check if we need to create default pool
    let pool_exists = resources.pools.iter().any(|p| p.name == opts.pool_name);

    if !pool_exists {
        libvirt_setup::ensure_storage_pool(&opts.libvirt_uri, &opts.pool_name, &opts.pool_path)?;
    }

    // Re-discover after creating resources
    let resources = libvirt_setup::discover_libvirt(&opts.libvirt_uri)?;

    // Step 5: Generate configuration
    info!("Step 6/9: Generating configuration");

    // Where this box's VMs' DNS goes. A joiner copies the cluster's (in
    // `cluster_net`); the first box uses its own resolvers.
    let upstream_dns: Vec<std::net::Ipv4Addr> = match &cluster_net {
        Some(n) if !n.upstream_dns.is_empty() => n.upstream_dns.clone(),
        _ => {
            let r = dns::upstream_resolvers();
            if r.is_empty() {
                warn!(
                    "No upstream resolver found in resolv.conf; set [network] upstream_dns or VMs get no DNS"
                );
            } else {
                info!(
                    "VM DNS is forwarded to {}",
                    r.iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            r
        }
    };

    let exists = opts.config_path.exists();
    if let Some(n) = &cluster_net {
        let bridge = resources
            .networks
            .iter()
            .find(|x| x.name == opts.network_name)
            .map(|x| x.bridge.as_str())
            .unwrap_or("virbr-xdebbie");
        for w in config_gen::network_block(n, box_id, bridge).1 {
            warn!("{w}");
        }
    }
    if exists && !opts.force {
        // A kept config still learns what the cluster decided: the box id,
        // for a joiner which box holds the CA, and a [network] if it has
        // none yet. Nothing else is touched.
        let net_block: Option<String> =
            match (&cluster_net, config_gen::config_parses(&opts.config_path)) {
                (Some(n), Ok(()))
                    if !config_gen::has_table(
                        &std::fs::read_to_string(&opts.config_path)?,
                        "network",
                    ) =>
                {
                    let bridge = resources
                        .networks
                        .iter()
                        .find(|x| x.name == opts.network_name)
                        .map(|x| x.bridge.as_str())
                        .unwrap_or("virbr-xdebbie");
                    Some(config_gen::network_block(n, box_id, bridge).0)
                }
                _ => None,
            };
        let existing_ca_url = crate::config::Config::load(&opts.config_path)
            .ok()
            .and_then(|c| c.ca_url);
        let want_ca_url = opts
            .join
            .as_deref()
            .map(|u| u.trim_end_matches('/').to_string());
        let box_moves = existing_box.is_some_and(|b| b != box_id);
        let ca_moves = want_ca_url.is_some() && want_ca_url != existing_ca_url;
        match config_gen::config_parses(&opts.config_path) {
            Ok(()) if box_moves || ca_moves => {
                let mut c = std::fs::read_to_string(&opts.config_path)?;
                let mut what = vec![];
                if box_moves {
                    c = config_gen::set_mesh_box(&c, box_id);
                    what.push(format!("[mesh] box = {box_id}"));
                }
                if let (true, Some(u)) = (ca_moves, &want_ca_url) {
                    c = config_gen::set_top_level(&c, "ca_url", &format!("{u:?}"));
                    what.push(format!("ca_url = {u:?}"));
                }
                if let Some(block) = &net_block {
                    c = config_gen::append_table(&c, block);
                    what.push("[network] (copied from the cluster)".to_string());
                }
                config_gen::write_config(&opts.config_path, &c)?;
                info!(
                    "Configuration kept at {}; set {}",
                    opts.config_path.display(),
                    what.join(", ")
                );
            }
            Ok(()) if net_block.is_some() => {
                let c = config_gen::append_table(
                    &std::fs::read_to_string(&opts.config_path)?,
                    net_block.as_deref().unwrap(),
                );
                config_gen::write_config(&opts.config_path, &c)?;
                info!(
                    "Configuration kept at {}; added [network] copied from the cluster",
                    opts.config_path.display()
                );
            }
            Ok(()) => info!(
                "Configuration already exists at {}, skipping (use --force to regenerate)",
                opts.config_path.display()
            ),
            // Never replace a hand-edited file because of a typo: serve will
            // refuse to start and say why.
            Err(e) => warn!(
                "{e:#}; left untouched - fix it, `xdebbied serve` will not start until it parses"
            ),
        }
    } else {
        let generated = config_gen::generate_config_for(
            &resources,
            &opts.libvirt_uri,
            created_subnet.as_deref(),
            box_id,
            opts.join.as_deref(),
            &opts.network_name,
            &opts.pool_name,
            cluster_net.as_ref(),
            &reach.host,
            &upstream_dns,
        );
        let content = if exists {
            let bak = config_gen::back_up(&opts.config_path)?;
            info!("Previous configuration saved as {}", bak.display());
            let old = std::fs::read_to_string(&opts.config_path)?;
            let mut fresh: Vec<&str> = if opts.join.is_some() {
                vec!["ca_url"]
            } else {
                vec![]
            };
            if !reach.by_name {
                fresh.push("advertise_url");
            }
            match config_gen::carry_over_except(&old, &generated, &fresh) {
                Ok(c) => {
                    info!("Regenerated configuration; operator settings carried over");
                    // `[mesh]` is carried over whole; the box id is not the
                    // operator's to keep.
                    config_gen::set_mesh_box(&c, box_id)
                }
                Err(e) => {
                    warn!("{e:#}; regenerated from scratch (previous file is in the backup)");
                    generated
                }
            }
        } else {
            generated
        };
        config_gen::write_config(&opts.config_path, &content)?;
    }

    // Step 7: libvirt TLS from the cluster CA and this box's certificate.
    info!("Step 7/9: libvirt TLS (cluster CA)");
    let libvirt_tls = match tls_certs::ensure_tls_certificates(&api_tls_paths) {
        Ok(t) => Some(t),
        Err(e) => {
            warn!("libvirt TLS not set up: {e:#}");
            warn!(
                "The leader cannot drive this box's VMs until it is; re-run bootstrap once the API certificate exists"
            );
            None
        }
    };

    // Step 8: Firewall (ufw) from the config written/kept above.
    info!("Step 8/9: Configuring firewall");
    let spec = match crate::config::Config::load(&opts.config_path) {
        Ok(cfg) => firewall::FirewallSpec::from_config(&cfg),
        Err(e) => {
            warn!(
                "could not load {} for the firewall ({e:#}); using defaults",
                opts.config_path.display()
            );
            firewall::FirewallSpec::minimal()
        }
    };
    if let Err(e) = firewall::configure_firewall(&spec) {
        warn!("Failed to configure firewall: {:#}", e);
    }

    // Write environment file to /etc/xdebbie/ (system-wide). The URL is the
    // one the config advertises (same thing peers see in discovery), never
    // the 0.0.0.0 listen address; the CA lets xdctl verify the cert.
    let env_path = cfg_dir.join("xdebbie_env");
    let (advertise_url, ca) = match crate::config::Config::load(&opts.config_path) {
        Ok(cfg) => {
            let ca = cfg.tls_cert.is_some().then(|| cfg.ca_file.clone());
            (cfg.advertise_url(cfg.effective_listen()), ca)
        }
        Err(_) => (derive_advertise_url(&opts.listen_addr, &reach.host), None),
    };
    if let Err(e) = token::write_env_file(&env_path, &advertise_url, &token, ca.as_deref()) {
        warn!("Failed to write environment file: {}", e);
        info!("You can manually create it later with:");
        info!(
            "  echo 'export XDEBBIE_URL={}' > /etc/xdebbie/xdebbie_env",
            advertise_url
        );
        info!("  echo 'export XDEBBIE_TOKEN=<token>' >> /etc/xdebbie/xdebbie_env");
    }
    // Peer CA drop directory: `scp other:/etc/xdebbie/api-ca.pem /etc/xdebbie/peers/other.pem`
    if let Err(e) = std::fs::create_dir_all(cfg_dir.join("peers")) {
        warn!("creating {}/peers: {e}", cfg_dir.display());
    }

    // Final ownership pass: set root:xdebbie 0440 on config+env,
    // root:root 0400 on token, root:xdebbie 0750 on the directory.
    // Done last so every file exists before we walk the directory.
    chown_config_dir(cfg_dir);

    // Step 9: a joiner registers its libvirt with the cluster. The first
    // box's [[hosts]] seeds its own store; a joiner's seeds a store nobody
    // reads, so tell the leader — via the CA box — with the remote URI.
    if let Some(ca_url) = &opts.join {
        info!("Step 9/9: Registering this box with the cluster");
        let registered = match (crate::config::Config::load(&opts.config_path), &libvirt_tls) {
            (Ok(cfg), Some(tls)) => match cfg.hosts.first() {
                Some(local) => {
                    let mut host = local.clone();
                    host.name = hostname.clone();
                    host.uri = tls.uri(&reach.host);
                    match join::register_host(ca_url, &api_tls_paths, &host) {
                        Ok(()) => {
                            info!(
                                "  ✓ Registered {} as {} (pool {}, bridge {})",
                                host.name, host.uri, host.storage_pool, host.bridge
                            );
                            if !tls.san_covers_listen {
                                warn!(
                                    "  The leader will not be able to connect until this box is re-enrolled (see step 7)"
                                );
                            } else {
                                info!(
                                    "  The leader reaches it over the overlay; `xdctl host ls` shows Ready once the mesh is peered"
                                );
                            }
                            true
                        }
                        Err(e) => {
                            warn!("  Could not register with the cluster: {e:#}");
                            false
                        }
                    }
                }
                None => {
                    warn!(
                        "  No [[hosts]] entry in {}; nothing to register",
                        opts.config_path.display()
                    );
                    false
                }
            },
            (Err(e), _) => {
                warn!("  Could not load {}: {e:#}", opts.config_path.display());
                false
            }
            (_, None) => {
                warn!(
                    "  libvirt TLS is not set up (step 7); not registering a host the leader could not reach"
                );
                false
            }
        };
        if !registered {
            info!(
                "  Register by hand once fixed: xdctl host add {hostname} --uri qemu+tls://<overlay address>/system --pool <pool> --bridge <bridge>"
            );
        }
    }

    info!("=== Bootstrap Complete ===");
    info!("Configuration: {}", opts.config_path.display());
    info!("Token:         {}", opts.token_path.display());
    info!("State:         {}", opts.state_dir.display());
    info!("Env file:      {}", env_path.display());
    info!("Mesh box id:   {box_id}");
    info!("");
    info!("To start xdebbied:");
    info!("  sudo systemctl enable --now xdebbied");
    info!("");
    info!("To use xdctl without sudo, add yourself to the xdebbie group");
    info!("(log out and back in after running this):");
    info!("  sudo usermod -aG xdebbie $USER");
    info!("");
    info!("Then source the env file once per shell (or add to ~/.bashrc):");
    info!("  source {}", env_path.display());
    info!("  xdctl host ls");
    info!("");
    info!("Or add this line to ~/.bashrc for permanent access:");
    info!(
        "  [ -r {} ] && source {}",
        env_path.display(),
        env_path.display()
    );
    if api_tls_paths.is_ca() {
        let fp = std::fs::read_to_string(&api_tls_paths.ca_cert)
            .ok()
            .and_then(|pem| api_tls::cert_fingerprint(&pem).ok())
            .unwrap_or_else(|| "<unreadable>".into());
        info!("");
        info!(
            "This box holds the cluster CA ({}).",
            api_tls_paths.ca_key.display()
        );
        info!("  CA fingerprint: {fp}");
        info!("To add another box, run there:");
        info!("  sudo xdebbied bootstrap --join {advertise_url} --ca-fingerprint {fp}");
        info!("(it gets the next free box id; `xdctl enroll ls` shows it)");
        info!("then approve it from any member:");
        info!("  xdctl enroll ls");
        info!("  xdctl approve <hostname>");
    } else if let Some(ca_url) = &opts.join {
        info!("");
        info!(
            "Enrolled with the cluster CA at {ca_url}; this box's cert is {}.",
            api_tls_paths.server_cert.display()
        );
    } else if ca.is_some() {
        info!("");
        info!("No cluster CA key here. Either this is the CA box upgraded from 0.13 (run");
        info!("`bootstrap --force` once to create the key) or it should join another:");
        info!("  sudo xdebbied bootstrap --join https://<ca-box>:7443");
    }

    Ok(())
}

/// How long bootstrap gives the resolver to confirm this box's own name.
const HOSTNAME_LOOKUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// What goes in a URL another box opens to reach this one.
///
/// The hostname stays the box's *name* everywhere (enrolment record,
/// `xdctl host ls`, certificate CN). This is only about what to connect
/// to: the FQDN when it resolves here — a name that does not resolve on
/// its own box will not resolve on any other — else the address this box
/// announces on discovery, which the certificate also lists as a SAN.
struct ReachHost {
    host: String,
    /// `host` is the hostname (true) or the announced address (false).
    by_name: bool,
}

fn reach_host(hostname: &str, multicast_addr: &str) -> ReachHost {
    if crate::discovery::reach::resolves_within(hostname, HOSTNAME_LOOKUP_BUDGET) {
        return ReachHost {
            host: hostname.to_string(),
            by_name: true,
        };
    }
    match crate::discovery::reach::announced_addr(multicast_addr) {
        Some(ip) => {
            warn!(
                "{hostname} does not resolve here (within {}s); other boxes will reach this one by address {ip}",
                HOSTNAME_LOOKUP_BUDGET.as_secs()
            );
            ReachHost {
                host: ip.to_string(),
                by_name: false,
            }
        }
        None => {
            warn!(
                "{hostname} does not resolve here and no global address was found; URLs use the name anyway"
            );
            ReachHost {
                host: hostname.to_string(),
                by_name: true,
            }
        }
    }
}

/// Build a usable advertise URL from the listen address.
///
/// If the listen address binds to a wildcard (`0.0.0.0` or `[::]`) we
/// replace it with `host` — the FQDN or, when that does not resolve, the
/// announced address (see [`reach_host`]) — so that peers and xdctl on
/// other machines can actually reach this box. A literal IP in the listen
/// address is kept as-is.
fn derive_advertise_url(listen_addr: &str, host: &str) -> String {
    // Split off the port — listen_addr is "host:port" or "[::]:port".
    let port = listen_addr.rsplit(':').next().unwrap_or("7443");

    let host = if listen_addr.starts_with("0.0.0.0:")
        || listen_addr.starts_with("[::]:")
        || listen_addr == "0.0.0.0"
    {
        // Wildcard: whatever other boxes can open.
        host.to_string()
    } else {
        // Literal IP or hostname already specified — strip the port.
        let h = listen_addr
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(listen_addr);
        h.trim_matches(|c| c == '[' || c == ']').to_string()
    };

    format!("https://{}:{}", host, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_options() {
        let opts = BootstrapOptions::default();
        assert_eq!(opts.config_path, PathBuf::from(DEFAULT_CONFIG_FILE));
        assert_eq!(opts.libvirt_uri, DEFAULT_LIBVIRT_URI);
    }

    #[test]
    fn test_derive_advertise_url_wildcard() {
        assert_eq!(
            derive_advertise_url("0.0.0.0:7443", "gram.example"),
            "https://gram.example:7443"
        );
        assert_eq!(
            derive_advertise_url("[::]:7443", "192.168.1.164"),
            "https://192.168.1.164:7443"
        );
    }

    #[test]
    fn test_derive_advertise_url_literal_ip() {
        let url = derive_advertise_url("192.168.1.10:7443", "gram.example");
        assert_eq!(url, "https://192.168.1.10:7443");
    }

    #[test]
    fn reach_host_falls_back_to_address_when_name_is_dead() {
        let r = reach_host(
            "no-such-box.invalid",
            crate::discovery::multicast::DEFAULT_MULTICAST_ADDR,
        );
        if r.by_name {
            // no global address on this machine (CI sandbox): the name is kept
            assert_eq!(r.host, "no-such-box.invalid");
        } else {
            assert!(
                r.host.parse::<std::net::IpAddr>().is_ok(),
                "got: {}",
                r.host
            );
        }
    }

    #[test]
    fn reach_host_keeps_a_name_that_resolves() {
        let r = reach_host("192.0.2.7", "239.0.0.42:7442");
        assert!(r.by_name);
        assert_eq!(r.host, "192.0.2.7");
    }
}
