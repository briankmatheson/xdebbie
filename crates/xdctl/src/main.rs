// SPDX-License-Identifier: GPL-3.0-or-later
//! xdctl — command-line client for xdebbied.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, CommandFactory};
use clap_complete::{generate, Shell};
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
    /// Any xdebbied in the cluster (comma-separated to try several). xdctl
    /// asks it who the leader is and talks to the leader from then on.
    #[arg(long, env = "XDEBBIE_URL", default_value = "https://127.0.0.1:7443")]
    url: String,
    #[arg(long, env = "XDEBBIE_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// PEM CA certificate(s) to trust for https:// (comma-separated files
    /// or directories of *.pem). Bootstrap writes /etc/xdebbie/api-ca.pem
    /// and exports it as XDEBBIE_CA; peer CAs live in /etc/xdebbie/peers.
    #[arg(long, env = "XDEBBIE_CA", default_value = "/etc/xdebbie/api-ca.pem,/etc/xdebbie/peers")]
    ca: String,
    /// Skip TLS certificate verification (lab use only).
    #[arg(long, env = "XDEBBIE_INSECURE")]
    insecure: bool,
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
    /// Which xdebbied decides for the cluster.
    Leader,
    /// Boxes asking to join the cluster (`xdebbied bootstrap --join`).
    #[command(subcommand)]
    Enroll(EnrollCmd),
    /// Approve a box that asked to join: shorthand for `enroll approve`.
    Approve {
        /// Hostname (as it appears in `xdctl enroll ls`) or enrolment id.
        host: String,
    },
    /// The cluster CA certificate and its fingerprint (what
    /// `bootstrap --join --ca-fingerprint` expects).
    Ca,
    /// API bearer token: status and rotation.
    #[command(subcommand)]
    Token(TokenCmd),
    /// Geneve mesh between hypervisors.
    #[command(subcommand)]
    Mesh(MeshCmd),
    /// Overlay addressing and the built-in DHCP responder.
    #[command(subcommand)]
    Net(NetCmd),
    /// Standalone ABE volumes, hot-plugged into VMs as virtio disks.
    #[command(subcommand)]
    Volume(VolumeCmd),
    /// Generate shell completion script.
    Completion {
        /// Shell to generate completion for (bash, zsh, fish, elvish, powershell)
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum EnrollCmd {
    /// Pending enrolments (`--all` includes approved and denied).
    Ls {
        #[arg(long)]
        all: bool,
    },
    /// Sign the box's certificate; its bootstrap then finishes on its own.
    /// Check the fingerprint against what that box printed first.
    Approve {
        /// Hostname or enrolment id.
        host: String,
    },
    /// Refuse. The box can ask again with `bootstrap --join --force`.
    Deny {
        /// Hostname or enrolment id.
        host: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Which tokens the leader accepts and where each peer stands.
    Status,
    /// Mint a new token on the leader. The old one keeps working until
    /// every node has taken the new one; then it is retired everywhere.
    Rotate,
}

#[derive(Subcommand)]
enum VolumeCmd {
    /// List.
    #[command(visible_aliases = ["list", "show", "get"])]
    Ls,
    /// Full record of one volume (JSON).
    Describe {
        id: String,
    },
    /// Create a blank volume on a host's ABE.
    Create {
        name: String,
        #[arg(long)]
        gib: u64,
        /// Host whose ABE should hold it (default: any healthy host with one).
        #[arg(long)]
        host: Option<String>,
    },
    Rm {
        id: String,
    },
    /// Plug the volume into a VM (name or id). The guest sees it at
    /// /dev/disk/by-id/virtio-<first 20 chars of the volume id>.
    Attach {
        id: String,
        vm: String,
        /// Take it from a running VM that still has it.
        #[arg(long)]
        force: bool,
    },
    Detach {
        id: String,
    },
}

#[derive(Subcommand)]
enum NetCmd {
    /// `[network]` config and DHCP responder status.
    #[command(visible_aliases = ["list", "show", "get"])]
    Ls,
    /// MACs heard on the bridge and what they were told.
    Seen,
}

#[derive(Subcommand)]
enum MeshCmd {
    /// Peers table and conditions.
    #[command(visible_aliases = ["list", "show", "get"])]
    Ls,
    /// Full {spec, status} document (JSON; same as `ls --json`).
    Describe,
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
    /// This box's WireGuard identity (pubkey, port, address) for enrolment
    /// on other boxes, plus per-peer handshake state.
    Wg,
}

#[derive(Subcommand)]
enum MeshPeerCmd {
    /// Add (or replace) a peer. With a managed WireGuard underlay REMOTE
    /// may be omitted (derived as 10.22.<box>.1) and --pubkey is required.
    Add {
        r#box: u8,
        /// Underlay address of the peer.
        remote: Option<String>,
        #[arg(long)]
        probe: Option<String>,
        /// Peer's WireGuard public key (see `xdctl mesh wg` on that box).
        #[arg(long)]
        pubkey: Option<String>,
        /// Peer's WireGuard endpoint host:port.
        #[arg(long)]
        endpoint: Option<String>,
    },
    Rm {
        r#box: u8,
    },
}

#[derive(Subcommand)]
enum HostCmd {
    /// List.
    #[command(visible_aliases = ["list", "show", "get"])]
    Ls,
    /// Full record of one host (JSON).
    Describe {
        name: String,
    },
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
        /// ABE API on this host (e.g. http://10.22.0.1:80) for `--storage abe` VMs.
        #[arg(long)]
        abe: Option<String>,
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
    /// List.
    #[command(visible_aliases = ["list", "show", "get"])]
    Ls {
        #[arg(short = 'l', long)]
        selector: Option<String>,
    },
    /// Full record of one VM (JSON); accepts a name or id.
    Describe {
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
        /// Pin an overlay IPv4 address (requires `[network]` on the server).
        #[arg(long)]
        ip: Option<std::net::Ipv4Addr>,
        /// Where the disks live: local (host pool/LVM) or abe (NVMe/TCP
        /// volumes from the host's ABE, attachable from any host).
        #[arg(long, default_value = "local")]
        storage: xdebbie_core::Storage,
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
        #[arg(long)]
        force: bool,
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

/// How long `xdctl` waits for an election to settle before giving up.
const LEADER_WAIT_SECS: u32 = 20;

/// Blocking client trusting the system roots plus every PEM in `ca`
/// (comma-separated files or directories). Missing paths are skipped: a
/// box that still serves plain http has no CA and must keep working.
fn http_client(ca: &str, insecure: bool) -> Result<Client> {
    let mut b = Client::builder().use_rustls_tls().danger_accept_invalid_certs(insecure);
    for entry in ca.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let p = std::path::Path::new(entry);
        let files: Vec<std::path::PathBuf> = if p.is_dir() {
            std::fs::read_dir(p)
                .map(|rd| rd.flatten().map(|e| e.path()).filter(|f| f.extension().is_some_and(|x| x == "pem")).collect())
                .unwrap_or_default()
        } else if p.is_file() {
            vec![p.to_path_buf()]
        } else {
            vec![]
        };
        for f in files {
            let pem = std::fs::read(&f).with_context(|| format!("reading CA {}", f.display()))?;
            let cert = reqwest::Certificate::from_pem(&pem).with_context(|| format!("parsing CA {}", f.display()))?;
            b = b.add_root_certificate(cert);
        }
    }
    b.build().context("building HTTP client")
}

struct Api {
    c: Client,
    url: String,
    token: Option<String>,
    /// `X-Xdebbie-CA-Fingerprint` from the last reply, if any.
    last_ca_fingerprint: std::cell::RefCell<Option<String>>,
}
impl Api {
    /// Move to the elected leader. `seeds` is what the user configured; the
    /// first one that answers `/v1/leader` tells us where to go. A daemon
    /// too old to have that route is used as-is. An election in progress
    /// (daemon just started, or a failover) is waited out for a few seconds.
    fn find_leader(&mut self, seeds: &str) -> Result<LeaderStatus> {
        let mut waited = 0;
        loop {
            let mut errs = vec![];
            let mut retry = false;
            for seed in seeds.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                self.url = seed.to_string();
                match self.call_raw(Method::GET, "/leader", None, None) {
                    Ok((code, _)) if code == reqwest::StatusCode::NOT_FOUND => {
                        let this = LeaderRef { node_id: None, hostname: "(pre-0.10 daemon)".into(), api_url: seed.into() };
                        return Ok(LeaderStatus { this: this.clone(), state: "unknown".into(), is_leader: true, leader: Some(this), peers: 0 });
                    }
                    Ok((code, v)) if code.is_success() => {
                        let st: LeaderStatus = serde_json::from_value(v).context("parsing /v1/leader")?;
                        if let Some(l) = &st.leader {
                            self.url = l.api_url.clone();
                            return Ok(st);
                        }
                        errs.push(format!("{seed}: no leader elected yet (state: {})", st.state));
                        if waited == 0 {
                            eprintln!("waiting for a leader election at {seed}...");
                        }
                        if waited < LEADER_WAIT_SECS {
                            waited += 1;
                            retry = true;
                            break;
                        }
                    }
                    Ok((code, v)) => errs.push(format!("{seed}: {code}: {}", api_error_text(&v))),
                    Err(e) => errs.push(format!("{seed}: {e:#}")),
                }
            }
            if !retry {
                bail!("no leader reachable:\n  {}", errs.join("\n  "));
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

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
        if let Some(fp) = resp.headers().get("x-xdebbie-ca-fingerprint").and_then(|v| v.to_str().ok()) {
            *self.last_ca_fingerprint.borrow_mut() = Some(fp.to_string());
        }
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

fn print_net(v: &Value) {
    let d = &v["dhcp"];
    let s = |x: &Value| x.as_str().unwrap_or("-").to_string();
    println!(
        "cidr={} gateway={} pool={}-{} lease={}s dns={}{}",
        s(&v["cidr"]),
        s(&v["gateway"]),
        s(&v["pool_start"]),
        s(&v["pool_end"]),
        v["lease_secs"],
        v["dns"].as_array().map(|a| a.iter().map(s).collect::<Vec<_>>().join(",")).unwrap_or_default(),
        v["domain"].as_str().map(|d| format!(" domain={d}")).unwrap_or_default()
    );
    if let Some(r) = v["reserved"].as_array().filter(|r| !r.is_empty()) {
        println!("reserved={}", r.iter().map(s).collect::<Vec<_>>().join(","));
    }
    let state = if !d["enabled"].as_bool().unwrap_or(false) {
        "disabled (allocate only)".to_string()
    } else if !d["bound"].as_bool().unwrap_or(false) {
        format!("NOT LISTENING: {}", s(&d["error"]))
    } else if !d["active"].as_bool().unwrap_or(false) {
        "standby (not the leader)".to_string()
    } else {
        "serving".to_string()
    };
    println!(
        "dhcp: {state} bridge={} server_id={} mtu={} leader_only={}",
        s(&d["bridge"]),
        s(&d["server_id"]),
        d["mtu"].as_u64().map(|m| m.to_string()).unwrap_or_else(|| "-".into()),
        d["leader_only"]
    );
    let c = &d["counters"];
    println!(
        "counters: offers={} acks={} naks={} unknown_mac={} standby={} other_server={} parse_errors={} send_errors={}",
        c["offers"], c["acks"], c["naks"], c["ignored_unknown"], c["ignored_standby"], c["ignored_other"], c["parse_errors"], c["send_errors"]
    );
}

fn print_seen(v: &Value) {
    let Some(seen) = v["dhcp"]["seen"].as_array() else { return };
    println!("{:<18} {:<16} {:<24} {:<22} {:<20} LAST SEEN", "MAC", "IP", "VM", "CLIENT HOSTNAME", "LAST");
    let mut rows: Vec<&Value> = seen.iter().collect();
    rows.sort_by(|a, b| b["last_seen"].as_str().cmp(&a["last_seen"].as_str()));
    for r in rows {
        println!(
            "{:<18} {:<16} {:<24} {:<22} {:<20} {}",
            r["mac"].as_str().unwrap_or("-"),
            r["ip"].as_str().unwrap_or("-"),
            r["name"].as_str().unwrap_or("-"),
            r["client_hostname"].as_str().unwrap_or("-"),
            r["last"].as_str().unwrap_or("-"),
            r["last_seen"].as_str().unwrap_or("-")
        );
    }
}

fn print_wg_peers(w: &Value) {
    let peers = w["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        return;
    }
    println!();
    println!("{:<5} {:<46} {:<24} {:>10} {:>12} {:>12}", "BOX", "PUBKEY", "ENDPOINT", "HANDSHAKE", "RX", "TX");
    for p in &peers {
        let hs = p["handshake_age_s"].as_u64().map(|s| format!("{s}s ago")).unwrap_or_else(|| "never".into());
        println!(
            "{:<5} {:<46} {:<24} {:>10} {:>12} {:>12}",
            p["box"].to_string(),
            p["pubkey"].as_str().unwrap_or("-"),
            p["endpoint"].as_str().unwrap_or("-"),
            hs,
            p["rx_bytes"].to_string(),
            p["tx_bytes"].to_string()
        );
    }
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
        "{:<5} {:<9} {:<16} {:<16} {:<8} {:>8} {:>4}  LAST CHANGE",
        "BOX", "DEVICE", "REMOTE", "PROBE", "STATE", "RTT", "FDB"
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
    if !st["wireguard"].is_null() {
        let w = &st["wireguard"];
        println!();
        println!(
            "WIREGUARD {} pubkey={} port={} address={}",
            w["device"].as_str().unwrap_or("-"),
            w["pubkey"].as_str().unwrap_or("-"),
            w["listen_port"],
            w["address"].as_str().unwrap_or("-")
        );
        print_wg_peers(w);
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
    // `xdctl ... | head` must not panic: let a closed pipe end the process quietly.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    if let Cmd::Completion { shell } = cli.cmd {
        generate(shell, &mut Cli::command(), "xdctl", &mut std::io::stdout());
        return Ok(());
    }
    let mut api = Api {
        c: http_client(&cli.ca, cli.insecure)?,
        url: cli.url.clone(),
        token: cli.token.clone(),
        last_ca_fingerprint: Default::default(),
    };
    let asked = api.find_leader(&cli.url)?;
    let out = |v: Value| -> Result<()> {
        println!("{}", serde_json::to_string_pretty(&v)?);
        Ok(())
    };
    match cli.cmd {
        Cmd::Net(n) => {
            let v = api.call(Method::GET, "/network", None)?;
            if cli.json {
                return out(v);
            }
            match n {
                NetCmd::Ls => print_net(&v),
                NetCmd::Seen => print_seen(&v),
            }
        }
        Cmd::Mesh(m) => match m {
            MeshCmd::Ls => {
                let v = api.call(Method::GET, "/mesh", None)?;
                if cli.json {
                    return out(v);
                }
                print_mesh(&v);
            }
            MeshCmd::Describe => return out(api.call(Method::GET, "/mesh", None)?),
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
            MeshCmd::Peer(MeshPeerCmd::Add { r#box, remote, probe, pubkey, endpoint }) => {
                let v = mesh_modify(&api, |spec| {
                    if remote.is_none() && spec["wireguard"].is_null() {
                        bail!("REMOTE is required unless the mesh manages WireGuard (spec.wireguard)");
                    }
                    let peers = spec["peers"].as_array_mut().ok_or_else(|| anyhow::anyhow!("spec has no peers array"))?;
                    peers.retain(|p| p["box"].as_u64() != Some(r#box as u64));
                    let mut p = serde_json::json!({ "box": r#box });
                    for (k, v) in [("remote", &remote), ("probe", &probe), ("pubkey", &pubkey), ("endpoint", &endpoint)] {
                        if let Some(v) = v {
                            p[k] = Value::String(v.clone());
                        }
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
            MeshCmd::Wg => {
                let v = api.call(Method::GET, "/mesh", None)?;
                if cli.json {
                    return out(v["status"]["wireguard"].clone());
                }
                let w = &v["status"]["wireguard"];
                if w.is_null() {
                    bail!("this mesh does not manage WireGuard (no spec.wireguard)");
                }
                println!("device   {}", w["device"].as_str().unwrap_or("-"));
                println!("pubkey   {}", w["pubkey"].as_str().unwrap_or("-"));
                println!("port     {}", w["listen_port"]);
                println!("address  {}", w["address"].as_str().unwrap_or("-"));
                println!();
                println!("enrol on another box:");
                println!(
                    "  xdctl mesh peer add {} --pubkey {} --endpoint <this-host>:{}",
                    v["spec"]["box"],
                    w["pubkey"].as_str().unwrap_or("?"),
                    w["listen_port"]
                );
                print_wg_peers(w);
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
        Cmd::Leader => {
            if cli.json {
                return out(serde_json::to_value(&asked)?);
            }
            let who = |r: &LeaderRef| format!("{} ({})", r.hostname, r.api_url);
            println!("asked   {}  [{}, {} peer{}]", who(&asked.this), asked.state, asked.peers, if asked.peers == 1 { "" } else { "s" });
            match &asked.leader {
                Some(l) if asked.is_leader => println!("leader  {}  (same box)", who(l)),
                Some(l) => println!("leader  {}", who(l)),
                None => println!("leader  none yet"),
            }
        }
        Cmd::Enroll(EnrollCmd::Ls { all }) => {
            let v = api.call(Method::GET, if all { "/enroll?all=true" } else { "/enroll" }, None)?;
            if cli.json {
                return out(v);
            }
            let es: Vec<Enrollment> = serde_json::from_value(v)?;
            if es.is_empty() {
                println!("no {}enrolments", if all { "" } else { "pending " });
                return Ok(());
            }
            println!("{:<9} {:<24} {:>3}  {:<30} {:<23} {:<9} AGE", "ID", "HOST", "BOX", "URL", "FINGERPRINT", "STATUS");
            for e in es {
                let fp = e.fingerprint.trim_start_matches("sha256:");
                let fp = format!("{}…{}", &fp[..8.min(fp.len())], &fp[fp.len().saturating_sub(8)..]);
                println!(
                    "{:<9} {:<24} {:>3}  {:<30} {:<23} {:<9} {}",
                    e.id, e.hostname, e.box_id, e.api_url, fp, e.status, format_age(e.requested_at)
                );
            }
        }
        Cmd::Enroll(EnrollCmd::Approve { host }) | Cmd::Approve { host } => {
            let v = api.call(Method::POST, &format!("/enroll/{host}/approve"), None)?;
            if cli.json {
                return out(v);
            }
            let e: Enrollment = serde_json::from_value(v)?;
            println!("approved {} (box {}, {})", e.hostname, e.box_id, e.fingerprint);
            println!("Its `xdebbied bootstrap --join` picks the certificate up within a few seconds.");
        }
        Cmd::Enroll(EnrollCmd::Deny { host }) => {
            let v = api.call(Method::POST, &format!("/enroll/{host}/deny"), None)?;
            if cli.json {
                return out(v);
            }
            let e: Enrollment = serde_json::from_value(v)?;
            println!("denied {} ({})", e.hostname, e.fingerprint);
        }
        Cmd::Ca => {
            let (code, v) = api.call_raw(Method::GET, "/ca", None, None)?;
            if !code.is_success() {
                bail!("{code}: {}", api_error_text(&v));
            }
            let pem = v.as_str().unwrap_or_default().to_string();
            let fp = api.last_ca_fingerprint.borrow().clone().unwrap_or_default();
            if cli.json {
                return out(serde_json::json!({ "fingerprint": fp, "pem": pem }));
            }
            println!("# cluster CA fingerprint {fp}");
            print!("{pem}");
        }
        Cmd::Token(TokenCmd::Status) => {
            let v = api.call(Method::GET, "/token", None)?;
            if cli.json {
                return out(v);
            }
            let st: TokenStatus = serde_json::from_value(v)?;
            println!("current   {}{}", st.id, if st.current_used { "  (in use)" } else { "" });
            match &st.previous {
                Some(p) => println!("previous  {}  (still accepted: rotation in progress)", p),
                None => println!("previous  none"),
            }
            if !st.nodes.is_empty() {
                println!("{:<32} {:<28} STATE", "PEER", "URL");
                for n in &st.nodes {
                    println!("{:<32} {:<28} {}", n.hostname, n.api_url, n.state);
                }
            }
        }
        Cmd::Token(TokenCmd::Rotate) => {
            let (code, v) = api.call_raw(Method::POST, "/token/rotate", None, None)?;
            if !code.is_success() {
                bail!("{code}: {}", api_error_text(&v));
            }
            if cli.json {
                return out(v);
            }
            let r: TokenRotated = serde_json::from_value(v)?;
            println!("new token {}  (replaces {})", r.id, r.previous);
            println!();
            println!("export XDEBBIE_TOKEN='{}'", r.token);
            println!();
            println!("The old token keeps working until every node has the new one, then it is");
            println!("retired. /etc/xdebbie/xdebbie_env is updated on each node as it switches;");
            println!("re-source it, and update any other holder (abe-csi secret, CAPX). Watch with");
            println!("`xdctl token status`.");
        }
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
            HostCmd::Describe { name } => out(api.call(Method::GET, &format!("/hosts/{name}"), None)?)?,
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
                abe,
                reserved_memory_mib,
                reserved_cpus,
                labels,
            } => {
                let h = Host {
                    abe,
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
                    "{:<36} {:<20} {:<13} {:<25} {:<15} {:<10} MESSAGE",
                    "ID", "NAME", "PHASE", "HOST", "IP", "AGE"
                );
                for v in vms {
                    // Allocated overlay address first; else whatever was observed.
                    let ip = v.ip.map(|i| i.to_string()).unwrap_or_else(|| {
                        v.addresses
                            .iter()
                            .find(|a| a.kind == "InternalIP")
                            .map(|a| a.address.clone())
                            .unwrap_or_else(|| "-".into())
                    });
                    let age = format_age(v.created);
                    println!(
                        "{:<36} {:<20.20} {:<13} {:<25} {:<15} {:<10} {}",
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
            VmCmd::Describe { id } => {
                out(api.call(Method::GET, &format!("/vms/{}", api.resolve(&id)?), None)?)?
            }
            VmCmd::Create {
                name,
                cpus,
                memory_mib,
                disk_gib,
                data_disk_gib,
                image,
                ip,
                storage,
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
                    ip,
                    storage,
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
            VmCmd::Rm { id, force } => {
                let path = if force {
                    format!("/vms/{}?force=true", api.resolve(&id)?)
                } else {
                    format!("/vms/{}", api.resolve(&id)?)
                };
                api.call(Method::DELETE, &path, None)?;
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
        Cmd::Volume(c) => match c {
            VolumeCmd::Ls => {
                let v = api.call(Method::GET, "/volumes", None)?;
                if cli.json {
                    return out(v);
                }
                let vols: Vec<Volume> = serde_json::from_value(v)?;
                println!("{:<32} {:<24} {:>8} {:<36} {:<8} {:<6} MESSAGE", "ID", "NAME", "GIB", "ATTACHED", "TARGET", "AGE");
                for v in vols {
                    let gib = v.bytes as f64 / (1u64 << 30) as f64;
                    let (vm, target) = match (&v.attached, v.attach_to) {
                        (Some(a), _) => (a.vm.to_string(), a.target.clone()),
                        (None, Some(w)) => (format!("{w} (pending)"), "-".into()),
                        (None, None) => ("-".into(), "-".into()),
                    };
                    println!(
                        "{:<32} {:<24} {:>8.1} {:<36} {:<8} {:<6} {}",
                        v.id, v.name, gib, vm, target, format_age(v.created),
                        v.message.as_deref().unwrap_or("")
                    );
                }
            }
            VolumeCmd::Describe { id } => out(api.call(Method::GET, &format!("/volumes/{id}"), None)?)?,
            VolumeCmd::Create { name, gib, host } => out(api.call(
                Method::POST,
                "/volumes",
                Some(serde_json::json!({ "name": name, "bytes": gib << 30, "host": host })),
            )?)?,
            VolumeCmd::Rm { id } => {
                api.call(Method::DELETE, &format!("/volumes/{id}"), None)?;
            }
            VolumeCmd::Attach { id, vm, force } => out(api.call(
                Method::POST,
                &format!("/volumes/{id}/attach"),
                Some(serde_json::json!({ "vm": api.resolve(&vm)?, "force": force })),
            )?)?,
            VolumeCmd::Detach { id } => out(api.call(
                Method::POST,
                &format!("/volumes/{id}/detach"),
                None,
            )?)?,
        },
        Cmd::Completion { .. } => unreachable!("handled before contacting xdebbied"),
    }
    Ok(())
}
