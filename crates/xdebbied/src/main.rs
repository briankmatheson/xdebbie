// SPDX-License-Identifier: GPL-3.0-or-later
//! xdebbied — a small VM scheduler and lifecycle manager for libvirt hosts.

mod abe;
mod api;
mod bootstrap;
mod config;
mod dhcp;
mod discovery;
mod election;
mod enroll;
mod keyed_lock;
mod leader;
mod libvirt;
mod lvm;
mod mesh;
mod reconcile;
mod store;
mod tls;
mod token;

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
        /// Overlay subnet (CIDR). One /16 for the cluster; box N's bridge
        /// is <subnet>.N.1 and its VMs get <subnet>.N.2-254
        #[arg(long, default_value = "10.23.0.0/16")]
        subnet: String,
        /// Storage pool name to create
        #[arg(long, default_value = "xdebbie")]
        pool: String,
        /// Storage pool path
        #[arg(long, default_value = "/var/lib/libvirt/xdebbie")]
        pool_path: String,
        /// Mesh box id (0-253, unique per cluster). Normally not needed:
        /// the first box takes 0 and a joining box is assigned one by the
        /// CA box at approval. Written to [mesh] and used for the bridge
        /// MAC 52:54:00:23:<box>:fe when the network is created.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=253))]
        r#box: Option<u8>,
        /// Join an existing cluster: API URL of the box bootstrapped first
        /// (it holds the cluster CA), e.g. https://node0:7443. This box
        /// gets a certificate from it once an operator runs
        /// `xdctl approve <this hostname>` there, then the cluster token.
        #[arg(long, value_name = "URL")]
        join: Option<String>,
        /// Expected `sha256:...` fingerprint of the cluster CA (printed by
        /// bootstrap on the CA box). Without it you are asked to confirm.
        #[arg(long, value_name = "FP", requires = "join")]
        ca_fingerprint: Option<String>,
        /// Start a new cluster here without listening for an existing one.
        #[arg(long, conflicts_with = "join")]
        standalone: bool,
        /// Without --join/--standalone: seconds to listen for a running
        /// cluster's announcements before deciding (0 = don't listen).
        #[arg(long, default_value_t = bootstrap::discover::DEFAULT_LISTEN_SECS, value_name = "SECS")]
        discover_secs: u64,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();

    // Bootstrap runs outside the async runtime: it is plain blocking work
    // (shelling out, and HTTP to the CA box when joining).
    match &cli.cmd {
        Some(Cmd::Bootstrap {
            force,
            uri,
            network,
            subnet,
            pool,
            pool_path,
            r#box,
            join,
            ca_fingerprint,
            standalone,
            discover_secs,
        }) => {
            let opts = bootstrap::BootstrapOptions {
                config_path: cli.config.clone(),
                force: *force,
                libvirt_uri: uri.clone(),
                network_name: network.clone(),
                network_subnet: subnet.clone(),
                pool_name: pool.clone(),
                pool_path: pool_path.clone(),
                box_id: *r#box,
                join: join.clone(),
                ca_fingerprint: ca_fingerprint.clone(),
                standalone: *standalone,
                discover_secs: *discover_secs,
                ..Default::default()
            };
            bootstrap::bootstrap(&opts)?;
            return Ok(());
        }
        Some(Cmd::Serve {
            bootstrap: true, ..
        }) => {
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

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting runtime")?
        .block_on(run(cli))
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let cfg = Arc::new(
        config::Config::load(&cli.config)
            .with_context(|| format!("loading {}", cli.config.display()))
            .unwrap_or_else(|e| fatal_config(e)),
    );

    // Geneve mesh. The TOML section is validated here; a bad one stops the
    // daemon with an error naming the field.
    let mesh = mesh::MeshManager::new(
        Arc::new(mesh::IpCliNetOps::default()),
        cfg.state_dir(),
        Some(&cfg.mesh_config()),
    )
    .context("mesh configuration")
    .unwrap_or_else(|e| fatal_config(e));

    if matches!(
        cli.cmd,
        Some(Cmd::Serve {
            mesh_only: true,
            ..
        })
    ) {
        let token = load_token(&cfg)?;
        let listen = cfg.effective_listen();
        tokio::spawn(mesh.clone().run());
        let v1 = api::mesh::router(api::mesh::MeshApi { mesh }).layer(
            axum::middleware::from_fn_with_state(token, api::authenticate),
        );
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
                vg_name,
                vg_name
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
        abe: abe::Client::with_http(cfg.http_client(abe::TIMEOUT)),
        nvme: abe::Nvme {
            nvme_bin: cfg.nvme.clone(),
            qemu_img_bin: cfg.qemu_img.clone(),
            sys_class: cfg.nvme_sys.clone(),
        },
        base_locks: Arc::default(),
    });

    if let Some(Cmd::Check) = cli.cmd {
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

    let store = store::Store::open(cfg.state_file.clone()).unwrap_or_else(|e| fatal_config(e));
    let seeds = cfg.hosts.clone();
    store
        .update(|s| {
            for mut h in seeds {
                if let Some(old) = s.hosts.get(&h.name) {
                    h.capacity = old.capacity;
                    h.abe_capacity = old.abe_capacity.clone();
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

    let token = load_token(&cfg).unwrap_or_else(|e| fatal_config(e));

    // Resolve listen address once: explicit config > default-route IP > loopback.
    let listen = cfg.effective_listen();

    // Discovery and election (docs/autodiscovery.md). The leader runs the
    // reconciler and answers cluster routes; followers forward to it.
    // No discovery (disabled, or no multicast) means this box decides.
    let hostname = leader::local_hostname();
    let api_url = cfg.advertise_url(listen);
    // The election prefers the box whose store holds the VMs; keep that
    // number current (the leader's store moves while it leads).
    let vm_count = Arc::new(std::sync::atomic::AtomicUsize::new(
        store.read().await.vms.len(),
    ));
    {
        let (store, vm_count) = (store.clone(), vm_count.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                vm_count.store(
                    store.read().await.vms.len(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        });
    }
    let discovery_cfg = discovery::DiscoveryConfig {
        enable_discovery: cfg.enable_discovery,
        multicast_addr: cfg.multicast_addr.clone(),
        api_url: api_url.clone(),
        hostname: hostname.clone(),
        vm_count,
        ..Default::default()
    };
    let leadership = if !cfg.enable_discovery {
        info!("discovery disabled; this box is its own leader");
        leader::Leadership::standalone(&hostname, &api_url)
    } else {
        match discovery::DiscoveryEngine::new(discovery_cfg) {
            Ok(engine) => {
                let engine = Arc::new(engine);
                info!(node_id = %engine.node_id(), "starting discovery engine");
                let leadership = leader::Leadership::elected(&engine, &hostname, &api_url);
                tokio::spawn(async move {
                    if let Err(e) = engine.run().await {
                        tracing::error!(error = %e, "discovery engine failed");
                    }
                });
                leadership
            }
            Err(e) => {
                tracing::warn!(error = %e, "discovery engine failed to start; this box is its own leader");
                leader::Leadership::standalone(&hostname, &api_url)
            }
        }
    };

    // Bytes promised to standalone-volume creates still in flight; the API
    // reserves, the scheduler counts them.
    let abe_ledger: reconcile::AbeLedger = Arc::default();
    let rec = reconcile::Reconciler {
        store: store.clone(),
        hv,
        cfg: cfg.clone(),
        mesh: Some(mesh.clone()),
        leadership: leadership.clone(),
        abe_ledger: abe_ledger.clone(),
    };
    tokio::spawn(rec.run());
    tokio::spawn(mesh.clone().run());

    // Trusts this box's CA and every peer CA in `peer_ca_dir`, so forwarding
    // to the leader over its self-signed bootstrap cert verifies.
    let http = cfg.http_client(std::time::Duration::from_secs(300));
    if let Some(t) = &token {
        tokio::spawn(
            token::Distributor {
                tokens: t.clone(),
                leadership: leadership.clone(),
                http: http.clone(),
            }
            .run(),
        );
    }

    let dhcp = match (cfg.network.clone(), cfg.dhcp_bridge()) {
        (Some(net), Some(bridge)) => {
            let overlay = mesh.overlay.clone();
            let srv = Arc::new(dhcp::DhcpServer::new(
                net.clone(),
                bridge,
                store.clone(),
                Arc::new(move |b: &str| overlay.mtu_for(b)),
                dhcp::Gate {
                    leader: leadership.flag(),
                    leader_only: net.leader_only,
                },
            ));
            if net.dhcp {
                tokio::spawn(srv.clone().run());
            } else {
                info!("network: dhcp = false, allocating addresses only");
            }
            Some(srv)
        }
        (Some(_), None) => {
            tracing::warn!(
                "[network] set but no bridge known (set network.bridge); allocating addresses only"
            );
            None
        }
        _ => None,
    };

    // Enrolment queue; this box signs only if it holds the CA key.
    let signer = bootstrap::api_tls::CaSigner::load(&cfg.ca_file, &cfg.ca_key)
        .context("loading cluster CA")
        .unwrap_or_else(|e| fatal_config(e));
    match (&signer, &cfg.ca_url) {
        (Some(_), _) => {
            info!(ca_key = %cfg.ca_key.display(), "this box holds the cluster CA: `xdctl approve` is decided here")
        }
        (None, Some(u)) => info!(ca_url = %u, "enrolment requests are forwarded to the CA box"),
        (None, None) => info!("no cluster CA key and no ca_url: enrolment unavailable on this box"),
    }
    let mesh_cfg = cfg.mesh_config();
    let reserved: Vec<u8> = std::iter::once(mesh_cfg.box_id)
        .chain(mesh_cfg.peers.iter().map(|p| p.box_id))
        .collect();
    let enroll = Arc::new(
        enroll::Enrollments::load(cfg.state_dir().join("enroll.json"), signer, reserved)
            .unwrap_or_else(|e| fatal_config(e)),
    );

    let app = api::router(api::App {
        store,
        cfg: cfg.clone(),
        token,
        mesh,
        dhcp,
        abe: abe::Client::with_http(cfg.http_client(abe::TIMEOUT)),
        leadership,
        http,
        enroll,
        volume_names: Arc::default(),
        abe_ledger,
    });
    serve(&cfg, listen, app).await
}

/// A file on disk is wrong (config, state, token): restarting cannot fix
/// it, so say so and exit with EX_CONFIG, which the unit lists in
/// `RestartPreventExitStatus`. The failure then sits in `systemctl status`
/// instead of cycling through bootstrap every few seconds.
fn fatal_config(e: anyhow::Error) -> ! {
    tracing::error!(
        error = format!("{e:#}"),
        "cannot start; fix the file and `systemctl restart xdebbied` (not retrying)"
    );
    std::process::exit(78)
}

fn load_token(cfg: &config::Config) -> anyhow::Result<Option<Arc<token::Tokens>>> {
    Ok(match &cfg.token_file {
        Some(p) => Some(Arc::new(token::Tokens::load(p)?)),
        None => {
            tracing::warn!("no token_file configured: API is unauthenticated");
            None
        }
    })
}

/// Serve `app` on `listen`, HTTPS if a cert/key pair is configured. Over
/// HTTPS, client certificates signed by the cluster CA are recognised.
async fn serve(
    cfg: &config::Config,
    listen: std::net::SocketAddr,
    app: axum::Router,
) -> anyhow::Result<()> {
    match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(c), Some(k)) => {
            let roots = tls::client_roots(&cfg.ca_file, &cfg.peer_ca_dir);
            let sc = tls::server_config(c, k, roots).context("loading TLS certificate/key")?;
            let h = axum_server::Handle::new();
            let h2 = h.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                h2.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
            });
            info!(%listen, "xdebbied serving https (client certificates accepted)");
            axum_server::bind(listen)
                .acceptor(tls::PeerCertAcceptor::new(sc))
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
