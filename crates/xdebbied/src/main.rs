// SPDX-License-Identifier: GPL-3.0-or-later
//! xdebbied — a small VM scheduler and lifecycle manager for libvirt hosts.

mod api;
mod bootstrap;
mod config;
mod discovery;
mod election;
mod libvirt;
mod lvm;
mod reconcile;
mod store;

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing::info;

#[derive(Parser)]
#[command(version, about = "XDebbie8s VM orchestrator daemon")]
struct Cli {
    #[arg(
        short,
        long,
        env = "XDEBBIED_CONFIG",
        default_value = "/etc/xdebbie/xdebbied.toml"
    )]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the API server and reconciler (default).
    Serve {
        /// Run bootstrap before starting server
        #[arg(long)]
        bootstrap: bool,
    },
    /// Check that every configured host is reachable over libvirt+ssh.
    Check,
    /// Bootstrap xdebbied: install deps, create resources, generate config & token.
    Bootstrap {
        /// Overwrite existing config and token files
        #[arg(long)]
        force: bool,
        /// Libvirt URI to use
        #[arg(long, default_value = "qemu:///system")]
        uri: String,
        /// Network name to create
        #[arg(long, default_value = "xdebbie")]
        network: String,
        /// Network subnet (CIDR)
        #[arg(long, default_value = "192.168.100.0/24")]
        subnet: String,
        /// Storage pool name to create
        #[arg(long, default_value = "xdebbie")]
        pool: String,
        /// Storage pool path
        #[arg(long, default_value = "/var/lib/libvirt/xdebbie")]
        pool_path: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();

    // Handle bootstrap command
    match &cli.cmd {
        Some(Cmd::Bootstrap {
            force,
            uri,
            network,
            subnet,
            pool,
            pool_path,
        }) => {
            let opts = bootstrap::BootstrapOptions {
                config_path: cli.config.clone(),
                force: *force,
                libvirt_uri: uri.clone(),
                network_name: network.clone(),
                network_subnet: subnet.clone(),
                pool_name: pool.clone(),
                pool_path: pool_path.clone(),
                ..Default::default()
            };
            bootstrap::bootstrap(&opts)?;
            return Ok(());
        }
        Some(Cmd::Serve { bootstrap: true }) => {
            // Run bootstrap first
            let opts = bootstrap::BootstrapOptions {
                config_path: cli.config.clone(),
                ..Default::default()
            };
            if let Err(e) = bootstrap::bootstrap(&opts) {
                tracing::warn!("Bootstrap failed, continuing anyway: {:#}", e);
            }
        }
        _ => {}
    }

    let cfg = Arc::new(
        config::Config::load(&cli.config)
            .with_context(|| format!("loading {}", cli.config.display()))?,
    );
    // Initialize LVM if configured
    let lvm_manager = if let Some(ref vg_name) = cfg.volume_group {
        let lvm = lvm::LvmManager::new(vg_name.clone(), cfg.thin_pool.clone());
        if !lvm.vg_exists()? {
            anyhow::bail!(
                "Volume group '{}' not found. Create it with:\n  \
                 sudo pvcreate /dev/sdX\n  \
                 sudo vgcreate {} /dev/sdX",
                vg_name, vg_name
            );
        }
        // Self-heal after a host reboot: make sure the thin pool and VM
        // disks are active before libvirt tries to (re)start anything.
        if let Err(e) = lvm.ensure_activated() {
            tracing::error!("{:#}", e);
        }
        tracing::info!("Using LVM volume group: {}", vg_name);
        Some(lvm)
    } else {
        tracing::info!("LVM not configured, using qcow2 storage");
        None
    };

    let hv = Arc::new(libvirt::Virsh {
        bin: cfg.virsh.clone(),
        seed_command: cfg.seed_command.clone(),
        lvm: lvm_manager,
    });

    match cli.cmd {
        Some(Cmd::Check) => {
        use libvirt::Hypervisor;
        let mut bad = 0;
        for h in &cfg.hosts {
            match hv.inventory(h).await {
                Ok(c) => println!(
                    "{:<16} ok   {} cpus, {} MiB, {} GiB free in pool {}",
                    h.name, c.cpus, c.memory_mib, c.disk_free_gib, h.storage_pool
                ),
                Err(e) => {
                    bad += 1;
                    println!("{:<16} FAIL {e:#}", h.name)
                }
            }
        }
        std::process::exit(if bad == 0 { 0 } else { 1 });
        }
        _ => {}
    }

    let store = store::Store::open(cfg.state_file.clone())?;
    let seeds = cfg.hosts.clone();
    store
        .update(|s| {
            for mut h in seeds {
                if let Some(old) = s.hosts.get(&h.name) {
                    h.capacity = old.capacity;
                    h.last_seen = old.last_seen;
                    h.cordoned |= old.cordoned;
                }
                s.hosts.insert(h.name.clone(), h);
            }
        })
        .await?;

    let token = match &cfg.token_file {
        Some(p) => Some(Arc::new(
            std::fs::read_to_string(p)
                .with_context(|| format!("reading {}", p.display()))?
                .trim()
                .to_owned(),
        )),
        None => {
            tracing::warn!("no token_file configured: API is unauthenticated");
            None
        }
    };

    // Start discovery engine if enabled
    let discovery_cfg = discovery::DiscoveryConfig {
        enable_discovery: cfg.enable_discovery,
        multicast_addr: cfg.multicast_addr.clone(),
        api_url: format!(
            "{}://{}",
            if cfg.tls_cert.is_some() { "https" } else { "http" },
            cfg.listen
        ),
        hostname: hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string()),
        ..Default::default()
    };

    if let Ok(discovery_engine) = discovery::DiscoveryEngine::new(discovery_cfg) {
        let discovery_arc = Arc::new(discovery_engine);
        info!("Starting discovery engine (node_id: {})", discovery_arc.node_id());
        let discovery_clone = discovery_arc.clone();
        tokio::spawn(async move {
            if let Err(e) = discovery_clone.run().await {
                tracing::error!("Discovery engine failed: {}", e);
            }
        });
    } else {
        tracing::warn!("Failed to start discovery engine, running in standalone mode");
    }

    let rec = reconcile::Reconciler {
        store: store.clone(),
        hv,
        cfg: cfg.clone(),
    };
    tokio::spawn(rec.run());

    let app = api::router(api::App {
        store,
        cfg: cfg.clone(),
        token,
    });
    match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(c), Some(k)) => {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(c, k)
                .await
                .context("loading TLS certificate/key")?;
            let h = axum_server::Handle::new();
            let h2 = h.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                h2.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
            });
            info!(listen = %cfg.listen, "xdebbied serving https");
            axum_server::bind_rustls(cfg.listen, tls)
                .handle(h)
                .serve(app.into_make_service())
                .await?;
        }
        (None, None) => {
            if !cfg.listen.ip().is_loopback() {
                tracing::warn!(
                    "serving plain HTTP on a non-loopback address; set tls_cert/tls_key"
                );
            }
            let l = tokio::net::TcpListener::bind(cfg.listen).await?;
            info!(listen = %cfg.listen, "xdebbied serving http");
            axum::serve(l, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
        _ => anyhow::bail!("set both tls_cert and tls_key, or neither"),
    }
    Ok(())
}


