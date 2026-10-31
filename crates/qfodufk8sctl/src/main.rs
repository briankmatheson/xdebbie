// SPDX-License-Identifier: GPL-3.0-or-later
//! qfodufk8sctl — command-line client for qfodufk8sd.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use qfodufk8s_core::*;
use reqwest::{Method, blocking::Client};
use serde_json::Value;

#[derive(Parser)]
#[command(version, about = "Qfodufk8s CLI")]
struct Cli {
    #[arg(long, env = "QFODUFK8S_URL", default_value = "http://127.0.0.1:7443")]
    url: String,
    #[arg(long, env = "QFODUFK8S_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Print raw JSON.
    #[arg(short, long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage hypervisor hosts.
    #[command(subcommand)]
    Host(HostCmd),
    /// Manage virtual machines.
    #[command(subcommand)]
    Vm(VmCmd),
    /// List failure domains.
    Fd,
}

#[derive(Subcommand)]
enum HostCmd {
    Ls,
    /// Register or update a host.
    Add {
        name: String,
        #[arg(long)]
        uri: String,
        #[arg(long, default_value = "default")]
        failure_domain: String,
        #[arg(long, default_value = "default")]
        pool: String,
        #[arg(long, default_value = "br0")]
        bridge: String,
        #[arg(long)]
        no_control_plane: bool,
        #[arg(long, default_value_t = 4.0)]
        cpu_overcommit: f64,
        #[arg(long, default_value_t = 4096)]
        reserved_memory_mib: u64,
        #[arg(long, default_value_t = 2)]
        reserved_cpus: u32,
        /// key=value, repeatable
        #[arg(long = "label", value_parser = kv)]
        labels: Vec<(String, String)>,
    },
    Rm {
        name: String,
    },
    Cordon {
        name: String,
    },
    Uncordon {
        name: String,
    },
    /// Cordon and live-migrate every VM off the host.
    Drain {
        name: String,
    },
}

#[derive(Subcommand)]
enum VmCmd {
    Ls {
        #[arg(short = 'l', long)]
        selector: Option<String>,
    },
    Get {
        id: String,
    },
    Create {
        name: String,
        #[arg(long, default_value_t = 2)]
        cpus: u32,
        #[arg(long, default_value_t = 4096)]
        memory_mib: u64,
        #[arg(long, default_value_t = 40)]
        disk_gib: u64,
        #[arg(long)]
        image: String,
        #[arg(long)]
        user_data: Option<std::path::PathBuf>,
        #[arg(long)]
        failure_domain: Option<String>,
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        host: Option<String>,
        #[arg(long = "label", value_parser = kv)]
        labels: Vec<(String, String)>,
        /// Only show where it would go.
        #[arg(long)]
        dry_run: bool,
    },
    Rm {
        id: String,
    },
    Start {
        id: String,
    },
    Stop {
        id: String,
    },
    Reboot {
        id: String,
    },
    Migrate {
        id: String,
        #[arg(long)]
        to: Option<String>,
    },
}

fn kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.into(), v.into()))
        .ok_or_else(|| "expected key=value".into())
}

struct Api {
    c: Client,
    url: String,
    token: Option<String>,
}
impl Api {
    fn call(&self, m: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut r = self
            .c
            .request(m, format!("{}/v1{path}", self.url.trim_end_matches('/')));
        if let Some(t) = &self.token {
            r = r.bearer_auth(t);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().context("contacting qfodufk8sd")?;
        let code = resp.status();
        let text = resp.text()?;
        let v: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        if !code.is_success() {
            bail!(
                "{code}: {}",
                v.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or(&v.to_string())
            );
        }
        Ok(v)
    }
    /// Accept a full UUID or a unique VM name.
    fn resolve(&self, id: &str) -> Result<String> {
        if uuid::Uuid::parse_str(id).is_ok() {
            return Ok(id.into());
        }
        let vms: Vec<Vm> = serde_json::from_value(self.call(Method::GET, "/vms", None)?)?;
        let m: Vec<_> = vms.into_iter().filter(|v| v.spec.name == id).collect();
        match m.as_slice() {
            [v] => Ok(v.id.to_string()),
            [] => bail!("no vm named {id}"),
            _ => bail!("{id} is ambiguous"),
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let api = Api {
        c: Client::new(),
        url: cli.url.clone(),
        token: cli.token.clone(),
    };
    let out = |v: Value| -> Result<()> {
        println!("{}", serde_json::to_string_pretty(&v)?);
        Ok(())
    };
    match cli.cmd {
        Cmd::Fd => {
            let v = api.call(Method::GET, "/failure-domains", None)?;
            if cli.json {
                return out(v);
            }
            let fds: Vec<FailureDomain> = serde_json::from_value(v)?;
            println!("{:<16} {:<6} HOSTS", "NAME", "CP");
            for f in fds {
                println!(
                    "{:<16} {:<6} {}",
                    f.name,
                    f.control_plane,
                    f.hosts.join(",")
                );
            }
        }
        Cmd::Host(h) => match h {
            HostCmd::Ls => {
                let v = api.call(Method::GET, "/hosts", None)?;
                if cli.json {
                    return out(v);
                }
                let hs: Vec<Host> = serde_json::from_value(v)?;
                let vms: Vec<Vm> = serde_json::from_value(api.call(Method::GET, "/vms", None)?)?;
                println!(
                    "{:<12} {:<10} {:<9} {:>11} {:>15} {:>9}  STATUS",
                    "NAME", "FD", "CP", "VCPU", "MEM GiB", "DISK GiB"
                );
                for h in hs {
                    let mine: Vec<_> = vms
                        .iter()
                        .filter(|v| v.consumes_capacity() && v.host.as_deref() == Some(&h.name))
                        .collect();
                    let (c, m) = mine.iter().fold((0u64, 0u64), |a, v| {
                        (a.0 + v.spec.vcpus as u64, a.1 + v.spec.memory_mib)
                    });
                    let a = h.allocatable().unwrap_or_default();
                    let status = match (h.cordoned, h.last_seen) {
                        (true, _) => "Cordoned",
                        (_, None) => "Unknown",
                        _ => "Ready",
                    };
                    println!(
                        "{:<12} {:<10} {:<9} {:>5}/{:<5} {:>7}/{:<7} {:>9}  {status}",
                        h.name,
                        h.failure_domain,
                        h.control_plane_eligible,
                        c,
                        a.cpus,
                        m / 1024,
                        a.memory_mib / 1024,
                        a.disk_free_gib
                    );
                }
            }
            HostCmd::Add {
                name,
                uri,
                failure_domain,
                pool,
                bridge,
                no_control_plane,
                cpu_overcommit,
                reserved_memory_mib,
                reserved_cpus,
                labels,
            } => {
                let h = Host {
                    name: name.clone(),
                    uri,
                    failure_domain,
                    control_plane_eligible: !no_control_plane,
                    labels: labels.into_iter().collect(),
                    storage_pool: pool,
                    bridge,
                    cpu_overcommit,
                    mem_overcommit: 1.0,
                    reserved_memory_mib,
                    reserved_cpus,
                    cordoned: false,
                    capacity: None,
                    last_seen: None,
                };
                out(api.call(
                    Method::PUT,
                    &format!("/hosts/{name}"),
                    Some(serde_json::to_value(h)?),
                )?)?;
            }
            HostCmd::Rm { name } => {
                api.call(Method::DELETE, &format!("/hosts/{name}"), None)?;
            }
            HostCmd::Cordon { name } => {
                api.call(Method::POST, &format!("/hosts/{name}/cordon"), None)?;
            }
            HostCmd::Uncordon { name } => {
                api.call(Method::POST, &format!("/hosts/{name}/uncordon"), None)?;
            }
            HostCmd::Drain { name } => {
                out(api.call(Method::POST, &format!("/hosts/{name}/drain"), None)?)?
            }
        },
        Cmd::Vm(v) => match v {
            VmCmd::Ls { selector } => {
                let q = selector
                    .map(|s| format!("?selector={s}"))
                    .unwrap_or_default();
                let v = api.call(Method::GET, &format!("/vms{q}"), None)?;
                if cli.json {
                    return out(v);
                }
                let vms: Vec<Vm> = serde_json::from_value(v)?;
                println!(
                    "{:<36} {:<24} {:<13} {:<10} {:<15} MESSAGE",
                    "ID", "NAME", "PHASE", "HOST", "IP"
                );
                for v in vms {
                    let ip = v
                        .addresses
                        .iter()
                        .find(|a| a.kind == "InternalIP")
                        .map(|a| a.address.as_str())
                        .unwrap_or("-");
                    println!(
                        "{:<36} {:<24} {:<13} {:<10} {:<15} {}",
                        v.id,
                        v.spec.name,
                        format!("{:?}", v.phase),
                        v.host.as_deref().unwrap_or("-"),
                        ip,
                        v.message.as_deref().unwrap_or("")
                    );
                }
            }
            VmCmd::Get { id } => {
                out(api.call(Method::GET, &format!("/vms/{}", api.resolve(&id)?), None)?)?
            }
            VmCmd::Create {
                name,
                cpus,
                memory_mib,
                disk_gib,
                image,
                user_data,
                failure_domain,
                group,
                strict,
                host,
                labels,
                dry_run,
            } => {
                let user_data = user_data.map(std::fs::read_to_string).transpose()?;
                let spec = VmSpec {
                    name,
                    vcpus: cpus,
                    memory_mib,
                    disk_gib,
                    image,
                    placement: Placement {
                        failure_domain,
                        anti_affinity_group: group,
                        strict_anti_affinity: strict,
                        host,
                        ..Default::default()
                    },
                    user_data,
                    meta_data: Default::default(),
                    labels: labels.into_iter().collect(),
                };
                let body = serde_json::to_value(spec)?;
                if dry_run {
                    return out(api.call(Method::POST, "/schedule", Some(body))?);
                }
                out(api.call(
                    Method::PUT,
                    &format!("/vms/{}", uuid::Uuid::new_v4()),
                    Some(body),
                )?)?;
            }
            VmCmd::Rm { id } => {
                api.call(Method::DELETE, &format!("/vms/{}", api.resolve(&id)?), None)?;
            }
            VmCmd::Start { id } => {
                api.call(
                    Method::POST,
                    &format!("/vms/{}/start", api.resolve(&id)?),
                    None,
                )?;
            }
            VmCmd::Stop { id } => {
                api.call(
                    Method::POST,
                    &format!("/vms/{}/stop", api.resolve(&id)?),
                    None,
                )?;
            }
            VmCmd::Reboot { id } => {
                api.call(
                    Method::POST,
                    &format!("/vms/{}/reboot", api.resolve(&id)?),
                    None,
                )?;
            }
            VmCmd::Migrate { id, to } => out(api.call(
                Method::POST,
                &format!("/vms/{}/migrate", api.resolve(&id)?),
                Some(serde_json::json!({ "host": to })),
            )?)?,
        },
    }
    Ok(())
}
