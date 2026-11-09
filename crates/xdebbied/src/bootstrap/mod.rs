// SPDX-License-Identifier: GPL-3.0-or-later
//! Bootstrap functionality for xdebbied.
//!
//! This module provides automatic dependency checking, libvirt resource
//! discovery/creation, configuration generation, and token generation to
//! enable single-binary deployment on stock Debian systems.

pub mod config_gen;
pub mod deps;
pub mod libvirt_setup;
pub mod token;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Default configuration paths
pub const DEFAULT_CONFIG_DIR: &str = "/etc/xdebbie";
pub const DEFAULT_CONFIG_FILE: &str = "/etc/xdebbie/xdebbied.toml";
pub const DEFAULT_TOKEN_FILE: &str = "/etc/xdebbie/token";
pub const DEFAULT_STATE_DIR: &str = "/var/lib/xdebbie";
pub const DEFAULT_STATE_FILE: &str = "/var/lib/xdebbie/state.json";
pub const DEFAULT_ENV_FILE: &str = "xdebbie_env";

/// Default libvirt URIs and resources
pub const DEFAULT_LIBVIRT_URI: &str = "qemu:///system";
pub const DEFAULT_NETWORK_NAME: &str = "xdebbie";
pub const DEFAULT_NETWORK_SUBNET: &str = "192.168.100.0/24";
pub const DEFAULT_NETWORK_DHCP_START: &str = "192.168.100.100";
pub const DEFAULT_NETWORK_DHCP_END: &str = "192.168.100.250";
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
        }
    }
}

/// Perform full bootstrap process
pub fn bootstrap(opts: &BootstrapOptions) -> Result<()> {
    info!("=== xdebbied Bootstrap ===");

    // Step 1: Check dependencies
    info!("Step 1/6: Checking dependencies");
    let missing = deps::check_dependencies()?;
    if !missing.is_empty() {
        warn!("Missing dependencies: {}", missing.join(", "));
        deps::install_dependencies(&missing)?;
    } else {
        info!("All dependencies satisfied");
    }

    // Step 2: Check libvirtd (optional - socket activation may be used)
    info!("Step 2/6: Checking libvirtd availability");
    if let Err(e) = deps::ensure_libvirtd_running() {
        warn!("libvirtd service not available (this is OK if using socket activation): {}", e);
        info!("Continuing - libvirt may work via socket activation");
    }

    // Step 3: Discover existing resources
    info!("Step 3/6: Discovering libvirt resources");
    let resources = libvirt_setup::discover_libvirt(&opts.libvirt_uri)?;
    info!(
        "Found {} networks and {} storage pools",
        resources.networks.len(),
        resources.pools.len()
    );

    // Step 4: Create resources if needed
    info!("Step 4/6: Ensuring required resources exist");
    
    // Check if we need to create default network
    let network_exists = resources
        .networks
        .iter()
        .any(|n| n.name == opts.network_name);
    
    if !network_exists {
        let parts: Vec<&str> = opts.network_subnet.split('/').collect();
        let network_base = parts[0].rsplitn(2, '.').nth(1).unwrap_or("192.168.100");
        let dhcp_start = format!("{}.100", network_base);
        let dhcp_end = format!("{}.250", network_base);
        
        libvirt_setup::ensure_network(
            &opts.libvirt_uri,
            &opts.network_name,
            &opts.network_subnet,
            &dhcp_start,
            &dhcp_end,
        )?;
    }

    // Check if we need to create default pool
    let pool_exists = resources
        .pools
        .iter()
        .any(|p| p.name == opts.pool_name);
    
    if !pool_exists {
        libvirt_setup::ensure_storage_pool(
            &opts.libvirt_uri,
            &opts.pool_name,
            &opts.pool_path,
        )?;
    }

    // Re-discover after creating resources
    let resources = libvirt_setup::discover_libvirt(&opts.libvirt_uri)?;

    // Step 5: Generate configuration
    info!("Step 5/6: Generating configuration");
    
    if config_gen::config_exists(&opts.config_path) && !opts.force {
        info!(
            "Configuration already exists at {}, skipping (use --force to overwrite)",
            opts.config_path.display()
        );
    } else {
        let config_content = config_gen::generate_config(&resources, &opts.libvirt_uri);
        config_gen::write_config(&opts.config_path, &config_content)?;
    }

    // Step 6: Generate token
    info!("Step 6/6: Generating API token");
    
    let token = if token::token_exists(&opts.token_path) && !opts.force {
        info!(
            "Token already exists at {}, reusing",
            opts.token_path.display()
        );
        token::read_token(&opts.token_path)?
    } else {
        let new_token = token::generate_token()?;
        token::write_token(&opts.token_path, &new_token)?;
        new_token
    };

    // Create state directory
    std::fs::create_dir_all(&opts.state_dir).context("creating state directory")?;

    // Write environment file to /etc/xdebbie/ (system-wide)
    let cfg_dir = opts.config_path.parent().unwrap_or_else(|| Path::new(DEFAULT_CONFIG_DIR));
    let env_path = cfg_dir.join("xdebbie_env");
    let url = format!("http://{}", opts.listen_addr);
    if let Err(e) = token::write_env_file(&env_path, &url, &token) {
        warn!("Failed to write environment file: {}", e);
        info!("You can manually create it later with:");
        info!("  echo 'export XDEBBIE_URL={}' > /etc/xdebbie/xdebbie_env", url);
        info!("  echo 'export XDEBBIE_TOKEN=<token>' >> /etc/xdebbie/xdebbie_env");
    }

    info!("=== Bootstrap Complete ===");
    info!("Configuration: {}", opts.config_path.display());
    info!("Token: {}", opts.token_path.display());
    info!("State: {}", opts.state_dir.display());
    info!("");
    info!("To start xdebbied:");
    info!("  sudo xdebbied serve");
    info!("");
    info!("To use xdctl:");
    info!("  source /etc/xdebbie/{}", DEFAULT_ENV_FILE);
    info!("  xdctl host ls");

    Ok(())
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
}
