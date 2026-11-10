// SPDX-License-Identifier: GPL-3.0-or-later
//! xdctl — command-line client for xdebbied.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use xdebbie_core::*;
use reqwest::{Method, blocking::Client};
use serde_json::Value;
use chrono::{DateTime, Utc};

/// Format a timestamp as a Kubernetes-style age (e.g., "5m", "2h", "3d")
fn format_age(created: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(created);
    
    let seconds = duration.num_seconds();
    if seconds < 0 {
        return "0s".to_string();
    }
    
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

#[derive(Parser)]
#[command(version, about = "XDebbie CLI - Self-Forming Cloud Orchestration")]
struct Cli {
    #[arg(long, env = "XDEBBIE_URL", default_value = "http://127.0.0.1:7443")]
    url: String,
    #[arg(long, env = "XDEBBIE_TOKEN", hide_env_values = true)]
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
    /// Geneve mesh between hypervisors.
    #[command(subcommand)]
    Mesh(MeshCmd),
}

#[derive(Subcommand)]
enum MeshCmd {
    /// Peers table and conditions.
    Ls,
    /// Raw GET /v1/mesh (same as `ls --json`).
    Get,
    /// PUT /v1/mesh from a JSON file (`-` for stdin).
    Apply {
        #[arg(short, long)]
        file: String,
    },
    /// Add or remove a peer (read-modify-PUT with If-Match).
    #[command(subcommand)]
    Peer(MeshPeerCmd),
    /// DELETE /v1/mesh: tear down and forget the API document.
    Delete {
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Ask the daemon to reconcile now.
    Reconcile,
}

#[derive(Subcommand)]
enum MeshPeerCmd {
    Add {
        r#box: u8,
        remote: String,
        #[arg(long)]
        probe: Option<String>,
    },
    Rm {
        r#box: u8,
    },
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
        data_disk_gib: Option<u64>,
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
        let (code, v) = self.call_raw(m, path, body, None)?;
        if !code.is_success() {
            bail!("{code}: {}", api_error_text(&v));
        }
        Ok(v)
    }
    /// Like `call` but returns the status so callers can handle 409 etc.
    fn call_raw(&self, m: Method, path: &str, body: Option<Value>, if_match: Option<u64>) -> Result<(reqwest::StatusCode, Value)> {
        let mut r = self
            .c
            .request(m, format!("{}/v1{path}", self.url.trim_end_matches('/')));
        if let Some(t) = &self.token {
            r = r.bearer_auth(t);
        }
        if let Some(g) = if_match {
            r = r.header("If-Match", g.to_string());
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().context("contacting xdebbied")?;
        let code = resp.status();
        let text = resp.text()?;
        let v: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok((code, v))
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

/// Error text from an API body: `{"error": ..}` or `{"errors": [{field, message}]}`.
fn api_error_text(v: &Value) -> String {
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return e.to_string();
    }
    if let Some(list) = v.get("errors").and_then(Value::as_array) {
        return list
            .iter()
            .map(|e| {
                format!(
                    "{}: {}",
                    e.get("field").and_then(Value::as_str).unwrap_or("?"),
                    e.get("message").and_then(Value::as_str).unwrap_or("?")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
    }
    v.to_string()
}

/// Read-modify-PUT with If-Match; one retry on 409.
fn mesh_modify(api: &Api, f: impl Fn(&mut Value) -> Result<()>) -> Result<Value> {
    for attempt in 0..2 {
        let doc = api.call(Method::GET, "/mesh", None)?;
        let generation = doc["status"]["generation"].as_u64().unwrap_or(0);
        let mut spec = doc["spec"].clone();
        f(&mut spec)?;
        let (code, v) = api.call_raw(Method::PUT, "/mesh", Some(spec), Some(generation))?;
        if code == reqwest::StatusCode::CONFLICT && attempt == 0 {
            continue;
        }
        if !code.is_success() {
            bail!("{code}: {}", api_error_text(&v));
        }
        return Ok(v);
    }
    bail!("mesh: generation changed twice while updating; try again")
}

fn print_mesh(doc: &Value) {
    let st = &doc["status"];
    let spec = &doc["spec"];
    println!(
        "enabled={} box={} bridge={} vni={} underlay={} fdb={} mtu={} source={} generation={}/{}",
        spec["enabled"],
        spec["box"],
        spec["networks"][0]["bridge"].as_str().unwrap_or("-"),
        spec["networks"][0]["vni"],
        spec["underlay"].as_str().unwrap_or("-"),
        spec["fdb"].as_str().unwrap_or("-"),
        st["mtu"],
        st["source"].as_str().unwrap_or("-"),
        st["observed_generation"],
        st["generation"],
    );
    println!();
    println!(
        "{:<5} {:<9} {:<16} {:<16} {:<8} {:>8} {:>4}  {}",
        "BOX", "DEVICE", "REMOTE", "PROBE", "STATE", "RTT", "FDB", "LAST CHANGE"
    );
    for p in st["peers"].as_array().into_iter().flatten() {
        let rtt = p["rtt_ms"].as_f64().map(|r| format!("{r:.2}ms")).unwrap_or_else(|| "-".into());
        let state = match p["reason"].as_str() {
            Some(r) => format!("{} ({r})", p["state"].as_str().unwrap_or("?")),
            None => p["state"].as_str().unwrap_or("?").to_string(),
        };
        let age = p["last_change"]
            .as_str()
            .and_then(|t| t.parse::<DateTime<Utc>>().ok())
            .map(format_age)
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<5} {:<9} {:<16} {:<16} {:<8} {:>8} {:>4}  {}",
            p["box"].to_string(),
            p["device"].as_str().unwrap_or("-"),
            p["remote"].as_str().unwrap_or("-"),
            p["probe"].as_str().unwrap_or("-"),
            state,
            rtt,
            p["fdb_entries"].to_string(),
            age
        );
    }
    println!();
    println!("CONDITIONS");
    for c in st["conditions"].as_array().into_iter().flatten() {
        let msg = c["message"].as_str().map(|m| format!("  {m}")).unwrap_or_default();
        println!(
            "  {:<20} {:<5} {}{}",
            c["type"].as_str().unwrap_or("?"),
            c["status"].to_string(),
            c["reason"].as_str().unwrap_or(""),
            msg
        );
    }
    if let Some(m) = st["vms_mtu_mismatch"].as_array()
        && !m.is_empty()
    {
        println!();
        println!("VM NICs with stale MTU (fixed on recreate): {}", m.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "));
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
        Cmd::Mesh(m) => match m {
            MeshCmd::Ls => {
                let v = api.call(Method::GET, "/mesh", None)?;
                if cli.json {
                    return out(v);
                }
                print_mesh(&v);
            }
            MeshCmd::Get => return out(api.call(Method::GET, "/mesh", None)?),
            MeshCmd::Apply { file } => {
                let text = if file == "-" {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                    s
                } else {
                    std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?
                };
                let body: Value = serde_json::from_str(&text).context("mesh document is not valid JSON")?;
                // Accept either a bare spec or a {spec, status} document.
                let spec = body.get("spec").cloned().unwrap_or(body);
                let v = api.call(Method::PUT, "/mesh", Some(spec))?;
                if cli.json {
                    return out(v);
                }
                print_mesh(&v);
            }
            MeshCmd::Peer(MeshPeerCmd::Add { r#box, remote, probe }) => {
                let v = mesh_modify(&api, |spec| {
                    let peers = spec["peers"].as_array_mut().ok_or_else(|| anyhow::anyhow!("spec has no peers array"))?;
                    peers.retain(|p| p["box"].as_u64() != Some(r#box as u64));
                    let mut p = serde_json::json!({ "box": r#box, "remote": remote });
                    if let Some(pr) = &probe {
                        p["probe"] = Value::String(pr.clone());
                    }
                    peers.push(p);
                    Ok(())
                })?;
                if cli.json {
                    return out(v);
                }
                print_mesh(&v);
            }
            MeshCmd::Peer(MeshPeerCmd::Rm { r#box }) => {
                let v = mesh_modify(&api, |spec| {
                    let peers = spec["peers"].as_array_mut().ok_or_else(|| anyhow::anyhow!("spec has no peers array"))?;
                    let before = peers.len();
                    peers.retain(|p| p["box"].as_u64() != Some(r#box as u64));
                    if peers.len() == before {
                        bail!("no peer with box {}", r#box);
                    }
                    Ok(())
                })?;
                if cli.json {
                    return out(v);
                }
                print_mesh(&v);
            }
            MeshCmd::Delete { yes } => {
                if !yes {
                    eprint!("Tear down the Geneve mesh on this host and remove its API document? [y/N] ");
                    let mut ans = String::new();
                    std::io::stdin().read_line(&mut ans)?;
                    if !matches!(ans.trim(), "y" | "Y" | "yes") {
                        bail!("aborted");
                    }
                }
                let v = api.call(Method::DELETE, "/mesh", None)?;
                if cli.json {
                    return out(v);
                }
                if v["reenabled_from_config"].as_bool() == Some(true) {
                    println!("mesh torn down; [mesh] in xdebbied.toml has enabled = true, so it was rebuilt from config");
                } else {
                    println!("mesh torn down");
                }
            }
            MeshCmd::Reconcile => {
                api.call(Method::POST, "/mesh/reconcile", None)?;
                println!("reconcile requested");
            }
        },
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
                    "{:<36} {:<24} {:<13} {:<10} {:<15} {:<10} MESSAGE",
                    "ID", "NAME", "PHASE", "HOST", "IP", "AGE"
                );
                for v in vms {
                    let ip = v
                        .addresses
                        .iter()
                        .find(|a| a.kind == "InternalIP")
                        .map(|a| a.address.as_str())
                        .unwrap_or("-");
                    let age = format_age(v.created);
                    println!(
                        "{:<36} {:<24} {:<13} {:<10} {:<15} {:<10} {}",
                        v.id,
                        v.spec.name,
                        format!("{:?}", v.phase),
                        v.host.as_deref().unwrap_or("-"),
                        ip,
                        age,
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
                data_disk_gib,
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
                    data_disk_gib,
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
