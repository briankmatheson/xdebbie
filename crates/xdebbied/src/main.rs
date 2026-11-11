// SPDX-License-Identifier: GPL-3.0-or-later
//! xdebbied — a small VM scheduler and lifecycle manager for libvirt hosts.

mod abe;
mod api;
mod bootstrap;
mod config;
mod dhcp;
mod discovery;
mod election;
mod libvirt;
mod lvm;
mod mesh;
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
        /// Start only the API and the Geneve mesh module: no libvirt, no
        /// LVM, no discovery. Used by the mesh test harness.
        #[arg(long)]
        mesh_only: bool,
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
        Some(Cmd::Serve { bootstrap: true, .. }) => {
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

    // Geneve mesh. The TOML section is validated here; a bad one stops the
    // daemon with an error naming the field.
    let mesh = mesh::MeshManager::new(
        Arc::new(mesh::IpCliNetOps::default()),
        cfg.state_dir(),
        cfg.mesh.as_ref(),
    )
    .context("mesh configuration")?;

    if matches!(cli.cmd, Some(Cmd::Serve { mesh_only: true, .. })) {
        let token = load_token(&cfg)?;
        let listen = cfg.effective_listen();
        tokio::spawn(mesh.clone().run());
        let v1 = api::mesh::router(api::mesh::MeshApi { mesh })
            .layer(axum::middleware::from_fn_with_state(token, api::bearer_auth));
        let app = axum::Router::new()
            .route("/healthz", axum::routing::get(|| async { "ok" }))
            .nest("/v1", v1);
        info!("xdebbied --mesh-only: API + mesh module only");
        return serve(&cfg, listen, app).await;
    }

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
        overlay: mesh.overlay.clone(),
        abe: abe::Client::new(),
        nvme: abe::Nvme {
            nvme_bin: cfg.nvme.clone(),
            qemu_img_bin: cfg.qemu_img.clone(),
            sys_class: cfg.nvme_sys.clone(),
        },
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

    // Second chance for libvirt autostart: domains whose disks weren't ready
    // when libvirtd came up (LVM thin pool inactive after reboot) are started
    // now that storage has been activated.
    for h in &cfg.hosts {
        match hv.start_autostart_domains(h).await {
            Ok(0) => {}
            Ok(n) => tracing::info!("started {} autostart domain(s) on {}", n, h.name),
            Err(e) => tracing::warn!("checking autostart domains on {}: {:#}", h.name, e),
        }
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

    // Give VMs that predate `[network]` an overlay address; new ones get
    // theirs at creation.
    if let Some(net) = cfg.network.clone() {
        let adopted = store.update(move |s| net.adopt(&mut s.vms)).await?;
        for (id, ip) in adopted {
            info!(vm = %id, %ip, "network: adopted overlay address");
        }
    }

    // Give VMs that predate `[network]` an overlay address; new ones get
    // theirs at creation.
    if let Some(net) = cfg.network.clone() {
        let adopted = store.update(move |s| net.adopt(&mut s.vms)).await?;
        for (id, ip) in adopted {
            info!(vm = %id, %ip, "network: adopted overlay address");
        }
    }

    let token = load_token(&cfg)?;

    // Resolve listen address once: explicit config > default-route IP > loopback.
    let listen = cfg.effective_listen();

    // Start discovery engine if enabled
    let discovery_cfg = discovery::DiscoveryConfig {
        enable_discovery: cfg.enable_discovery,
        multicast_addr: cfg.multicast_addr.clone(),
        api_url: format!(
            "{}://{}",
            if cfg.tls_cert.is_some() { "https" } else { "http" },
            listen
        ),
        hostname: hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string()),
        ..Default::default()
    };

    // `leader` mirrors the election result for consumers that cannot await
    // (the DHCP gate). Standalone = always leader.
    let leader = Arc::new(std::sync::atomic::AtomicBool::new(true));
    if let Ok(discovery_engine) = discovery::DiscoveryEngine::new(discovery_cfg) {
        let discovery_arc = Arc::new(discovery_engine);
        info!("Starting discovery engine (node_id: {})", discovery_arc.node_id());
        let discovery_clone = discovery_arc.clone();
        tokio::spawn(async move {
            if let Err(e) = discovery_clone.run().await {
                tracing::error!("Discovery engine failed: {}", e);
            }
        });
        let election = discovery_arc.election();
        let leader = leader.clone();
        tokio::spawn(async move {
            loop {
                let is = election.read().await.is_leader();
                leader.store(is, std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    } else {
        tracing::warn!("Failed to start discovery engine, running in standalone mode");
    }

    let rec = reconcile::Reconciler {
        store: store.clone(),
        hv,
        cfg: cfg.clone(),
        mesh: Some(mesh.clone()),
    };
    tokio::spawn(rec.run());
    tokio::spawn(mesh.clone().run());

    let dhcp = match (cfg.network.clone(), cfg.dhcp_bridge()) {
        (Some(net), Some(bridge)) => {
            let overlay = mesh.overlay.clone();
            let srv = Arc::new(dhcp::DhcpServer::new(
                net.clone(),
                bridge,
                store.clone(),
                Arc::new(move |b: &str| overlay.mtu_for(b)),
                dhcp::Gate { leader, leader_only: net.leader_only },
            ));
            if net.dhcp {
                tokio::spawn(srv.clone().run());
            } else {
                info!("network: dhcp = false, allocating addresses only");
            }
            Some(srv)
        }
        (Some(_), None) => {
            tracing::warn!("[network] set but no bridge known (set network.bridge); allocating addresses only");
            None
        }
        _ => None,
    };

    let app = api::router(api::App {
        store,
        cfg: cfg.clone(),
        token,
        mesh,
        dhcp,
        abe: abe::Client::new(),
    });
    serve(&cfg, listen, app).await
}

fn load_token(cfg: &config::Config) -> anyhow::Result<Option<Arc<String>>> {
    Ok(match &cfg.token_file {
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
    })
}

/// Serve `app` on `listen`, HTTPS if a cert/key pair is configured.
async fn serve(cfg: &config::Config, listen: std::net::SocketAddr, app: axum::Router) -> anyhow::Result<()> {
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
            info!(%listen, "xdebbied serving https");
            axum_server::bind_rustls(listen, tls)
                .handle(h)
                .serve(app.into_make_service())
                .await?;
        }
        (None, None) => {
            if !listen.ip().is_loopback() {
                tracing::warn!(
                    "serving plain HTTP on a non-loopback address; set tls_cert/tls_key"
                );
            }
            let l = tokio::net::TcpListener::bind(listen).await?;
            info!(%listen, "xdebbied serving http");
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


mod ca;
