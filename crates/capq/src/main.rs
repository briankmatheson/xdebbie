// SPDX-License-Identifier: GPL-3.0-or-later
//! capq — Cluster API infrastructure provider for Qfodufk8s.

mod controller;
mod crd;
mod qfodufk8s;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use futures::StreamExt;
use kube::{
    Api, Client,
    runtime::{Controller, watcher},
};

#[derive(Parser)]
#[command(version, about = "Cluster API Provider Qfodufk8s")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print CRD manifests (YAML) to stdout.
    Crds,
    /// Run the controllers.
    Run {
        #[arg(long, env = "QFODUFK8S_URL")]
        qfodufk8s_url: String,
        #[arg(long, env = "QFODUFK8S_TOKEN", hide_env_values = true)]
        qfodufk8s_token: Option<String>,
        /// Served version of the CAPI core API to read Cluster / Machine from.
        #[arg(long, env = "CAPI_VERSION", default_value = "v1beta2")]
        capi_version: String,
        /// Only watch this namespace (default: all).
        #[arg(long, env = "WATCH_NAMESPACE")]
        namespace: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,kube=warn".into()),
        )
        .init();
    match Cli::parse().cmd {
        Cmd::Crds => {
            for c in crd::all_crds() {
                print!("---\n{}", serde_yaml::to_string(&c)?);
            }
            Ok(())
        }
        Cmd::Run {
            qfodufk8s_url,
            qfodufk8s_token,
            capi_version,
            namespace,
        } => {
            let client = Client::try_default().await?;
            let ctx = Arc::new(controller::Ctx {
                client: client.clone(),
                qfodufk8s: qfodufk8s::Qfodufk8s::new(qfodufk8s_url, qfodufk8s_token),
                capi_version,
            });
            let (fc, fm): (Api<crd::Qfodufk8sCluster>, Api<crd::Qfodufk8sMachine>) =
                match &namespace {
                    Some(ns) => (
                        Api::namespaced(client.clone(), ns),
                        Api::namespaced(client.clone(), ns),
                    ),
                    None => (Api::all(client.clone()), Api::all(client.clone())),
                };
            let clusters = Controller::new(fc, watcher::Config::default())
                .shutdown_on_signal()
                .run(
                    controller::reconcile_cluster,
                    controller::error_policy,
                    ctx.clone(),
                )
                .for_each(|_| async {});
            let machines = Controller::new(fm, watcher::Config::default())
                .shutdown_on_signal()
                .run(controller::reconcile_machine, controller::error_policy, ctx)
                .for_each(|_| async {});
            tracing::info!("capq controllers running");
            tokio::join!(clusters, machines);
            Ok(())
        }
    }
}
