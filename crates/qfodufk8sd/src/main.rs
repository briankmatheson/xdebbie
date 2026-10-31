// SPDX-License-Identifier: GPL-3.0-or-later
//! qfodufk8sd — a small VM scheduler and lifecycle manager for libvirt hosts.

mod api;
mod config;
mod libvirt;
mod reconcile;
mod store;

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing::info;

#[derive(Parser)]
#[command(version, about = "Qfodufk8s VM orchestrator daemon")]
struct Cli {
    #[arg(
        short,
        long,
        env = "QFODUFK8SD_CONFIG",
        default_value = "/etc/qfodufk8s/qfodufk8sd.toml"
    )]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the API server and reconciler (default).
    Serve,
    /// Check that every configured host is reachable over libvirt+ssh.
    Check,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let cfg = Arc::new(
        config::Config::load(&cli.config)
            .with_context(|| format!("loading {}", cli.config.display()))?,
    );
    let hv = Arc::new(libvirt::Virsh {
        bin: cfg.virsh.clone(),
        seed_command: cfg.seed_command.clone(),
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
            info!(listen = %cfg.listen, "qfodufk8sd serving https");
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
            info!(listen = %cfg.listen, "qfodufk8sd serving http");
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
