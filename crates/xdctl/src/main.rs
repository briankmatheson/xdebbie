// SPDX-License-Identifier: GPL-3.0-or-later
//! xdctl — command-line client for xdebbied.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use reqwest::{Method, blocking::Client};
use serde_json::Value;
use xdebbie_core::*;

mod image;

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
    /// asks it who the leader is and talks to the leader from then on —
    /// except `mesh`, which is per-box state and talks to this box itself.
    #[arg(long, env = "XDEBBIE_URL", default_value = "https://127.0.0.1:23443")]
    url: String,
    #[arg(long, env = "XDEBBIE_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// PEM CA certificate(s) to trust for https:// (comma-separated files
    /// or directories of *.pem). Bootstrap writes /etc/xdebbie/api-ca.pem
    /// and exports it as XDEBBIE_CA; peer CAs live in /etc/xdebbie/peers.
    #[arg(
        long,
        env = "XDEBBIE_CA",
        default_value = "/etc/xdebbie/api-ca.pem,/etc/xdebbie/peers"
    )]
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
    Ca {
        /// The whole signing chain the box at --url serves, leaf first:
        /// its own certificate, then the CA that signed it. Each PEM is
        /// preceded by a `#` line with subject, issuer, expiry and
        /// fingerprint; pipe it into a file for a bundle.
        #[arg(long)]
        chain: bool,
    },
    /// API bearer token: status and rotation.
    #[command(subcommand)]
    Token(TokenCmd),
    /// Geneve mesh between hypervisors. Per-box state: these talk to the
    /// box at --url, not the leader.
    #[command(subcommand)]
    Mesh(MeshCmd),
    /// Overlay addressing and the built-in DHCP responder.
    #[command(subcommand)]
    Net(NetCmd),
    /// Standalone ABE volumes, hot-plugged into VMs as virtio disks.
    #[command(subcommand)]
    Volume(VolumeCmd),
    /// Base images in this host's thin pool. Local to the box it runs on;
    /// never contacts xdebbied.
    #[command(subcommand)]
    Image(ImageCmd),
    /// Show xdctl client version and the daemon version on the local box and
    /// the cluster leader (if different). Per-box state; never redirected.
    Version,
    /// Generate shell completion script.
    Completion {
        /// Shell to generate completion for (bash, zsh, fish, elvish, powershell)
        #[arg(value_enum)]
        shell: Shell,
    },
}

/// One link of the chain `GET /v1/cert` returns (xdebbied api_tls::CertInfo).
#[derive(serde::Deserialize)]
struct CertInfo {
    subject: String,
    issuer: String,
    not_before: String,
    not_after: String,
    fingerprint: String,
    self_signed: bool,
    pem: String,
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
enum ImageCmd {
    /// Copy a disk image into the thin pool as `base-<name>` and trim its
    /// free space, so it takes no more of the pool than its files. Needs
    /// root. The file itself is not modified.
    Import {
        /// Image file (raw unless --format says otherwise).
        file: String,
        /// Image name, as used by `--image` (default: the file name
        /// without its extension).
        #[arg(long)]
        name: Option<String>,
        /// Format of the file: raw or qcow2.
        #[arg(long, default_value = "raw")]
        format: String,
        /// Volume group (default: `volume_group` in the xdebbied config).
        #[arg(long)]
        vg: Option<String>,
        /// Thin pool (default: `thin_pool` in the xdebbied config).
        #[arg(long)]
        pool: Option<String>,
        /// xdebbied config to read the volume group and thin pool from.
        #[arg(long, default_value = image::CONFIG)]
        config: String,
        /// Replace an existing base of the same name.
        #[arg(long)]
        force: bool,
        /// Skip trimming the filesystems' free space (no virt-sparsify).
        #[arg(long)]
        no_sparsify: bool,
    },
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
    /// Add or remove an xdj client: a WireGuard-only peer (laptop, server)
    /// with a box number picked here. No Geneve, never makes the mesh Degraded.
    #[command(subcommand)]
    Client(MeshClientCmd),
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
    /// What this hub tells its xdj clients: DNS zones (each with its
    /// resolver), search domains in order, and a welcome banner. With no
    /// subcommand, show it. Public to read: put nothing secret here.
    #[command(name = "client-config")]
    ClientConfig {
        #[command(subcommand)]
        cmd: Option<ClientConfigCmd>,
    },
}

#[derive(Subcommand)]
enum ClientConfigCmd {
    /// Show it (the default).
    Show,
    /// Change it. Each option given replaces that part; the rest stay.
    Set {
        /// ZONE=SERVER, repeatable: names under ZONE are resolved by SERVER.
        /// Replaces every zone. `--zone none` clears them.
        #[arg(long = "zone", value_name = "ZONE=SERVER")]
        zones: Vec<String>,
        /// Search domains in order of preference, comma separated (short
        /// names try these first). `none` clears them.
        #[arg(long, value_name = "A,B,C")]
        search: Option<String>,
        /// The welcome banner, from a file (`-` for stdin).
        #[arg(long, value_name = "FILE", conflicts_with_all = ["welcome", "clear_welcome"])]
        welcome_file: Option<String>,
        /// The welcome banner, as text.
        #[arg(long, value_name = "TEXT", conflicts_with = "clear_welcome")]
        welcome: Option<String>,
        /// Remove the welcome banner.
        #[arg(long)]
        clear_welcome: bool,
    },
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
enum MeshClientCmd {
    /// Register an xdj client. Takes the lowest free box number unless
    /// --box is given (never replaces anything), and prints the
    /// `xdj join ...` line to run on the client. Adding a key that is
    /// already a client just reports it.
    Add {
        /// The client's WireGuard public key (`xdj key` on the client).
        #[arg(long)]
        pubkey: String,
        /// A label for `xdctl mesh ls` and `client rm` (letters, digits, . _ -).
        #[arg(long)]
        name: Option<String>,
        /// The client's endpoint host:port, if this box can reach it (usually
        /// not needed: clients connect to the hub).
        #[arg(long)]
        endpoint: Option<String>,
        /// Use this box number (1-254) instead of the lowest free one.
        #[arg(long = "box")]
        r#box: Option<u8>,
        /// How the client reaches this box, HOST or HOST:PORT (port defaults
        /// to the WireGuard listen port). Only used in the printed join line.
        #[arg(long, value_name = "HOST[:PORT]")]
        hub_endpoint: Option<String>,
    },
    /// Remove a client by box number, name, or public key.
    Rm {
        /// BOX, NAME or PUBKEY
        client: String,
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
        /// ABE API on this host (e.g. https://10.22.0.1:443) for `--storage abe` VMs.
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
    let mut b = Client::builder()
        .use_rustls_tls()
        .danger_accept_invalid_certs(insecure);
    for entry in ca.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let p = std::path::Path::new(entry);
        let files: Vec<std::path::PathBuf> = if p.is_dir() {
            std::fs::read_dir(p)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.path())
                        .filter(|f| f.extension().is_some_and(|x| x == "pem"))
                        .collect()
                })
                .unwrap_or_default()
        } else if p.is_file() {
            vec![p.to_path_buf()]
        } else {
            vec![]
        };
        for f in files {
            let pem = std::fs::read(&f).with_context(|| format!("reading CA {}", f.display()))?;
            let cert = reqwest::Certificate::from_pem(&pem)
                .with_context(|| format!("parsing CA {}", f.display()))?;
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
        self.find(seeds, true)
    }

    /// Stay on the first seed that answers: for per-box state (`/v1/mesh`)
    /// the leader is the wrong box to talk to. No election is needed, so
    /// none is waited for.
    fn find_box(&mut self, seeds: &str) -> Result<LeaderStatus> {
        self.find(seeds, false)
    }

    fn find(&mut self, seeds: &str, follow_leader: bool) -> Result<LeaderStatus> {
        let mut waited = 0;
        loop {
            let mut errs = vec![];
            let mut retry = false;
            for seed in seeds.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                self.url = seed.to_string();
                match self.call_raw(Method::GET, "/leader", None, None) {
                    Ok((code, _)) if code == reqwest::StatusCode::NOT_FOUND => {
                        let this = LeaderRef {
                            node_id: None,
                            hostname: "(pre-0.10 daemon)".into(),
                            api_url: seed.into(),
                        };
                        return Ok(LeaderStatus {
                            this: this.clone(),
                            state: "unknown".into(),
                            is_leader: true,
                            leader: Some(this),
                            peers: 0,
                        });
                    }
                    Ok((code, v)) if code.is_success() => {
                        let st: LeaderStatus =
                            serde_json::from_value(v).context("parsing /v1/leader")?;
                        if !follow_leader {
                            return Ok(st);
                        }
                        if let Some(l) = &st.leader {
                            self.url = l.api_url.clone();
                            return Ok(st);
                        }
                        errs.push(format!(
                            "{seed}: no leader elected yet (state: {})",
                            st.state
                        ));
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
                let what = if follow_leader {
                    "no leader reachable"
                } else {
                    "no xdebbied reachable"
                };
                bail!("{what}:\n  {}", errs.join("\n  "));
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
    fn call_raw(
        &self,
        m: Method,
        path: &str,
        body: Option<Value>,
        if_match: Option<u64>,
    ) -> Result<(reqwest::StatusCode, Value)> {
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
        if let Some(fp) = resp
            .headers()
            .get("x-xdebbie-ca-fingerprint")
            .and_then(|v| v.to_str().ok())
        {
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

/// Read-modify-PUT with If-Match; one retry on 409. `f` returns false when
/// there is nothing to change: no PUT, the document read is returned.
fn mesh_modify(api: &Api, mut f: impl FnMut(&mut Value) -> Result<bool>) -> Result<Value> {
    for attempt in 0..2 {
        let doc = api.call(Method::GET, "/mesh", None)?;
        let generation = doc["status"]["generation"].as_u64().unwrap_or(0);
        let mut spec = doc["spec"].clone();
        if !f(&mut spec)? {
            return Ok(doc);
        }
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
        v["dns"]
            .as_array()
            .map(|a| a.iter().map(s).collect::<Vec<_>>().join(","))
            .unwrap_or_default(),
        v["domain"]
            .as_str()
            .map(|d| format!(" domain={d}"))
            .unwrap_or_default()
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
        d["mtu"]
            .as_u64()
            .map(|m| m.to_string())
            .unwrap_or_else(|| "-".into()),
        d["leader_only"]
    );
    let c = &d["counters"];
    println!(
        "counters: offers={} acks={} naks={} unknown_mac={} standby={} other_server={} parse_errors={} send_errors={}",
        c["offers"],
        c["acks"],
        c["naks"],
        c["ignored_unknown"],
        c["ignored_standby"],
        c["ignored_other"],
        c["parse_errors"],
        c["send_errors"]
    );
}

fn print_seen(v: &Value) {
    let Some(seen) = v["dhcp"]["seen"].as_array() else {
        return;
    };
    println!(
        "{:<18} {:<16} {:<24} {:<22} {:<20} LAST SEEN",
        "MAC", "IP", "VM", "CLIENT HOSTNAME", "LAST"
    );
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

// ---------- mesh clients (xdj) ----------

/// Box numbers taken in a mesh spec: this box, every peer, every client.
fn mesh_used_boxes(spec: &Value) -> std::collections::BTreeSet<u64> {
    let mut used = std::collections::BTreeSet::new();
    used.insert(spec["box"].as_u64().unwrap_or(0));
    for list in ["peers", "clients"] {
        for e in spec[list].as_array().into_iter().flatten() {
            if let Some(b) = e["box"].as_u64() {
                used.insert(b);
            }
        }
    }
    used
}

/// Who holds `b` in the spec, for error messages.
fn mesh_box_owner(spec: &Value, b: u64) -> Option<String> {
    if spec["box"].as_u64().unwrap_or(0) == b {
        return Some("this box".into());
    }
    if spec["peers"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|p| p["box"].as_u64() == Some(b))
    {
        return Some(format!("peer {b} (a Geneve peer)"));
    }
    spec["clients"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["box"].as_u64() == Some(b))
        .map(|c| match c["name"].as_str() {
            Some(n) => format!("client {b} ({n})"),
            None => format!("client {b}"),
        })
}

/// What `client add` did to the spec.
#[derive(Debug, PartialEq)]
enum ClientAdd {
    /// Appended at this box.
    Added(u64),
    /// A client with this pubkey was already there (spec unchanged).
    Exists(u64),
}

/// Add an xdj client to `spec` (a `/v1/mesh` spec as JSON): lowest free box
/// in 1..=254 unless `want` is given; never replaces an entry.
fn mesh_add_client(
    spec: &mut Value,
    pubkey: &str,
    name: Option<&str>,
    endpoint: Option<&str>,
    want: Option<u8>,
) -> Result<ClientAdd> {
    if spec["wireguard"].is_null() {
        bail!(
            "this mesh does not manage WireGuard (no spec.wireguard): it cannot have xdj clients"
        );
    }
    if let Some(c) = spec["clients"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["pubkey"].as_str() == Some(pubkey))
    {
        return Ok(ClientAdd::Exists(c["box"].as_u64().unwrap_or(0)));
    }
    if let Some(p) = spec["peers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["pubkey"].as_str() == Some(pubkey))
    {
        let b = p["box"].as_u64().unwrap_or(0);
        bail!(
            "{pubkey} is the key of peer {b} (a Geneve peer). If that is an xdj box, move it: \
             xdctl mesh peer rm {b} && xdctl mesh client add --box {b} --pubkey {pubkey}"
        );
    }
    if let Some(n) = name
        && spec["clients"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["name"].as_str() == Some(n))
    {
        bail!("there is already a client named {n:?}");
    }
    let used = mesh_used_boxes(spec);
    let b = match want {
        Some(b) => {
            let b = b as u64;
            if !(1..=254).contains(&b) {
                bail!("--box must be 1-254, got {b}");
            }
            if let Some(owner) = mesh_box_owner(spec, b) {
                bail!("box {b} is taken by {owner}; pick another or leave out --box");
            }
            b
        }
        None => (1..=254u64)
            .find(|b| !used.contains(b))
            .ok_or_else(|| anyhow::anyhow!("no free box number left (1-254)"))?,
    };
    let mut c = serde_json::json!({ "box": b, "pubkey": pubkey });
    if let Some(n) = name {
        c["name"] = n.into();
    }
    if let Some(e) = endpoint {
        c["endpoint"] = e.into();
    }
    if !spec["clients"].is_array() {
        spec["clients"] = Value::Array(vec![]);
    }
    spec["clients"].as_array_mut().unwrap().push(c);
    Ok(ClientAdd::Added(b))
}

/// Remove the client matching BOX, NAME or PUBKEY; returns its box.
fn mesh_rm_client(spec: &mut Value, which: &str) -> Result<u64> {
    let Some(list) = spec["clients"].as_array_mut() else {
        bail!("this mesh has no clients");
    };
    let by_box = which.parse::<u64>().ok();
    let hits: Vec<usize> = list
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            by_box.is_some_and(|b| c["box"].as_u64() == Some(b))
                || c["name"].as_str() == Some(which)
                || c["pubkey"].as_str() == Some(which)
        })
        .map(|(i, _)| i)
        .collect();
    match hits.as_slice() {
        [i] => {
            let c = list.remove(*i);
            Ok(c["box"].as_u64().unwrap_or(0))
        }
        [] => bail!("no client with box, name or key {which:?} (see xdctl mesh ls)"),
        _ => bail!("{which:?} matches more than one client; use the box number"),
    }
}

/// `HOST[:PORT]` for the join line; the port defaults to `port`.
fn hub_endpoint(arg: &str, port: u64) -> String {
    if arg.parse::<std::net::SocketAddr>().is_ok() {
        return arg.into();
    }
    if let Ok(v6) = arg
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::Ipv6Addr>()
    {
        return format!("[{v6}]:{port}");
    }
    match arg.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => arg.into(),
        _ => format!("{arg}:{port}"),
    }
}

/// The line to run on the client, and whether HUB_HOST is a placeholder.
/// `xdj join BOX HUB_PUBKEY HUB_HOST:PORT`
fn xdj_join_line(b: u64, doc: &Value, hub: Option<&str>) -> (String, bool, bool) {
    let w = &doc["status"]["wireguard"];
    let port = w["listen_port"]
        .as_u64()
        .or_else(|| doc["spec"]["wireguard"]["listen_port"].as_u64())
        .unwrap_or(51820);
    let pubkey = w["pubkey"].as_str();
    let ep = match hub {
        Some(h) => hub_endpoint(h, port),
        None => format!("HUB_HOST:{port}"),
    };
    (
        format!("xdj join {b} {} {ep}", pubkey.unwrap_or("HUB_PUBKEY")),
        hub.is_none(),
        pubkey.is_none(),
    )
}

/// `CLIENTS` table for `mesh ls`: spec.clients joined with their WireGuard rows.
fn print_mesh_clients(doc: &Value) {
    let clients = doc["spec"]["clients"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if clients.is_empty() {
        return;
    }
    let wg = doc["status"]["wireguard"]["peers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    println!();
    println!(
        "CLIENTS (xdj)\n{:<5} {:<20} {:<12} {:<24} {:>10}",
        "BOX", "NAME", "PUBKEY", "ENDPOINT", "HANDSHAKE"
    );
    for c in &clients {
        let key = c["pubkey"].as_str().unwrap_or("-");
        let row = wg.iter().find(|p| p["pubkey"].as_str() == Some(key));
        let hs = match row {
            None => "-".to_string(),
            Some(r) => r["handshake_age_s"]
                .as_u64()
                .map(|s| format!("{s}s ago"))
                .unwrap_or_else(|| "never".into()),
        };
        let ep = row
            .and_then(|r| r["endpoint"].as_str())
            .or_else(|| c["endpoint"].as_str())
            .unwrap_or("-");
        println!(
            "{:<5} {:<20} {:<12} {:<24} {:>10}",
            c["box"].to_string(),
            c["name"].as_str().unwrap_or("-"),
            format!("{}…", key.chars().take(10).collect::<String>()),
            ep,
            hs
        );
    }
}

fn print_wg_peers(w: &Value) {
    let peers = w["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        return;
    }
    println!();
    println!(
        "{:<5} {:<46} {:<24} {:>10} {:>12} {:>12}",
        "BOX", "PUBKEY", "ENDPOINT", "HANDSHAKE", "RX", "TX"
    );
    for p in &peers {
        let hs = p["handshake_age_s"]
            .as_u64()
            .map(|s| format!("{s}s ago"))
            .unwrap_or_else(|| "never".into());
        let b = if p["client"].as_bool() == Some(true) {
            format!("{}c", p["box"])
        } else {
            p["box"].to_string()
        };
        println!(
            "{:<5} {:<46} {:<24} {:>10} {:>12} {:>12}",
            b,
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
        let rtt = p["rtt_ms"]
            .as_f64()
            .map(|r| format!("{r:.2}ms"))
            .unwrap_or_else(|| "-".into());
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
    print_mesh_clients(doc);
    println!();
    println!("CONDITIONS");
    for c in st["conditions"].as_array().into_iter().flatten() {
        let msg = c["message"]
            .as_str()
            .map(|m| format!("  {m}"))
            .unwrap_or_default();
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
        println!(
            "VM NICs with stale MTU (fixed on recreate): {}",
            m.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

fn cmd_version(api: &mut Api, cli: &Cli) -> Result<()> {
    let xdctl_ver = env!("CARGO_PKG_VERSION");

    // Query the box at --url directly (no leader redirect).
    let local_st = api.find_box(&cli.url).ok();
    let local_ver: Option<Value> = local_st
        .as_ref()
        .and_then(|_| api.call(Method::GET, "/version", None).ok());
    let local_url = api.url.clone();

    // If the local box is a follower, also query the leader directly.
    let leader_ver: Option<(String, Value)> = local_st
        .as_ref()
        .filter(|st| !st.is_leader)
        .and_then(|st| st.leader.as_ref())
        .and_then(|leader| {
            let la = Api {
                c: api.c.clone(),
                url: leader.api_url.clone(),
                token: api.token.clone(),
                last_ca_fingerprint: Default::default(),
            };
            la.call(Method::GET, "/version", None)
                .ok()
                .map(|v| (leader.api_url.clone(), v))
        });

    if cli.json {
        let mut doc = serde_json::json!({ "xdctl": xdctl_ver });
        if let Some(v) = local_ver {
            doc["local"] = v;
        }
        if let Some((_, v)) = leader_ver {
            doc["leader"] = v;
        }
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    let host = |v: &serde_json::Value| v["hostname"].as_str().unwrap_or("?").to_string();
    let local_host = local_ver.as_ref().map(host);
    let leader_host = leader_ver.as_ref().map(|(_, v)| host(v));
    let w = [
        Some("hostname".to_string()),
        local_host.clone(),
        leader_host.clone(),
    ]
    .iter()
    .flatten()
    .map(|h| h.len())
    .max()
    .unwrap_or(8);

    println!(
        "{:<10} {:<14} {:<w$} url",
        "component", "version", "hostname"
    );
    println!("{:-<10} {:-<14} {:-<w$} {:-<36}", "", "", "", "");
    println!("{:<10} {:<14} {:<w$}", "xdctl", xdctl_ver, "(client)");

    match &local_ver {
        Some(v) => println!(
            "{:<10} {:<14} {:<w$} {}",
            "local",
            v["version"].as_str().unwrap_or("?"),
            local_host.as_deref().unwrap_or("?"),
            local_url,
        ),
        None => println!("{:<10} unreachable ({})", "local", local_url),
    }

    match leader_ver {
        Some((url, v)) => println!(
            "{:<10} {:<14} {:<w$} {}",
            "leader",
            v["version"].as_str().unwrap_or("?"),
            leader_host.as_deref().unwrap_or("?"),
            url,
        ),
        None if local_st.as_ref().map(|s| s.is_leader).unwrap_or(false) => {
            println!("{:<10} (same box)", "leader");
        }
        None => {}
    }

    Ok(())
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
    if let Cmd::Image(ImageCmd::Import {
        file,
        name,
        format,
        vg,
        pool,
        config,
        force,
        no_sparsify,
    }) = cli.cmd
    {
        return image::import(image::Import {
            file,
            name,
            format,
            vg,
            pool,
            config,
            force,
            no_sparsify,
        });
    }
    let mut api = Api {
        c: http_client(&cli.ca, cli.insecure)?,
        url: cli.url.clone(),
        token: cli.token.clone(),
        last_ca_fingerprint: Default::default(),
    };
    // `version` queries each box directly (never follows leader).
    if matches!(cli.cmd, Cmd::Version) {
        return cmd_version(&mut api, &cli);
    }
    // `/v1/mesh` and the box's own certificate chain are per-box state;
    // everything else is cluster state the leader owns.
    let asked = if matches!(cli.cmd, Cmd::Mesh(_) | Cmd::Ca { chain: true }) {
        api.find_box(&cli.url)?
    } else {
        api.find_leader(&cli.url)?
    };
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
                let body: Value =
                    serde_json::from_str(&text).context("mesh document is not valid JSON")?;
                // Accept either a bare spec or a {spec, status} document.
                let spec = body.get("spec").cloned().unwrap_or(body);
                let v = api.call(Method::PUT, "/mesh", Some(spec))?;
                if cli.json {
                    return out(v);
                }
                print_mesh(&v);
            }
            MeshCmd::Peer(MeshPeerCmd::Add {
                r#box,
                remote,
                probe,
                pubkey,
                endpoint,
            }) => {
                let v = mesh_modify(&api, |spec| {
                    if remote.is_none() && spec["wireguard"].is_null() {
                        bail!(
                            "REMOTE is required unless the mesh manages WireGuard (spec.wireguard)"
                        );
                    }
                    if let Some(c) = spec["clients"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|c| c["box"].as_u64() == Some(r#box as u64))
                    {
                        bail!(
                            "box {} is an xdj client{} (spec.clients), not a peer; \
                             xdj boxes are managed with `xdctl mesh client add|rm`",
                            r#box,
                            c["name"]
                                .as_str()
                                .map(|n| format!(" ({n})"))
                                .unwrap_or_default()
                        );
                    }
                    let peers = spec["peers"]
                        .as_array_mut()
                        .ok_or_else(|| anyhow::anyhow!("spec has no peers array"))?;
                    peers.retain(|p| p["box"].as_u64() != Some(r#box as u64));
                    let mut p = serde_json::json!({ "box": r#box });
                    for (k, v) in [
                        ("remote", &remote),
                        ("probe", &probe),
                        ("pubkey", &pubkey),
                        ("endpoint", &endpoint),
                    ] {
                        if let Some(v) = v {
                            p[k] = Value::String(v.clone());
                        }
                    }
                    peers.push(p);
                    Ok(true)
                })?;
                if cli.json {
                    return out(v);
                }
                print_mesh(&v);
            }
            MeshCmd::Peer(MeshPeerCmd::Rm { r#box }) => {
                let v = mesh_modify(&api, |spec| {
                    let peers = spec["peers"]
                        .as_array_mut()
                        .ok_or_else(|| anyhow::anyhow!("spec has no peers array"))?;
                    let before = peers.len();
                    peers.retain(|p| p["box"].as_u64() != Some(r#box as u64));
                    if peers.len() == before {
                        bail!("no peer with box {}", r#box);
                    }
                    Ok(true)
                })?;
                if cli.json {
                    return out(v);
                }
                print_mesh(&v);
            }
            MeshCmd::Client(MeshClientCmd::Add {
                pubkey,
                name,
                endpoint,
                r#box,
                hub_endpoint,
            }) => {
                let mut done = None;
                let v = mesh_modify(&api, |spec| {
                    let r = mesh_add_client(
                        spec,
                        &pubkey,
                        name.as_deref(),
                        endpoint.as_deref(),
                        r#box,
                    )?;
                    let changed = matches!(r, ClientAdd::Added(_));
                    done = Some(r);
                    Ok(changed)
                })?;
                let done = done.expect("mesh_modify ran the closure");
                if cli.json {
                    return out(v);
                }
                let b = match done {
                    ClientAdd::Added(b) => {
                        println!("added xdj client box {b} (10.22.{b}.1)");
                        b
                    }
                    ClientAdd::Exists(b) => {
                        println!("already a client: box {b} (10.22.{b}.1); nothing changed");
                        if r#box.is_some_and(|w| w as u64 != b) {
                            println!(
                                "  (--box ignored; to move it: xdctl mesh client rm {b}, then add again)"
                            );
                        }
                        b
                    }
                };
                let (line, host_placeholder, key_placeholder) =
                    xdj_join_line(b, &v, hub_endpoint.as_deref());
                println!();
                println!("run on the client:");
                println!("  {line}");
                if host_placeholder {
                    println!();
                    println!(
                        "HUB_HOST is a placeholder: put the address the client reaches this box at \
                         (or pass --hub-endpoint HOST[:PORT])"
                    );
                }
                if key_placeholder {
                    println!(
                        "HUB_PUBKEY: this box has no WireGuard status yet; take it from `xdctl mesh wg`"
                    );
                }
            }
            MeshCmd::Client(MeshClientCmd::Rm { client }) => {
                let mut removed = 0;
                let v = mesh_modify(&api, |spec| {
                    removed = mesh_rm_client(spec, &client)?;
                    if spec["clients"].as_array().is_some_and(|l| l.is_empty()) {
                        // the daemon leaves an empty list out; keep the PUT tidy
                        if let Some(o) = spec.as_object_mut() {
                            o.remove("clients");
                        }
                    }
                    Ok(true)
                })?;
                if cli.json {
                    return out(v);
                }
                println!("removed xdj client box {removed}");
            }
            MeshCmd::ClientConfig { cmd } => {
                let mut v = api.call(Method::GET, "/client-config", None)?;
                if let Some(ClientConfigCmd::Set {
                    zones,
                    search,
                    welcome_file,
                    welcome,
                    clear_welcome,
                }) = cmd
                {
                    client_config_apply(
                        &mut v,
                        &zones,
                        search.as_deref(),
                        welcome_file.as_deref(),
                        welcome.as_deref(),
                        clear_welcome,
                    )?;
                    v = api.call(Method::PUT, "/client-config", Some(v))?;
                }
                if cli.json {
                    return out(v);
                }
                print_client_config(&v);
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
                    eprint!(
                        "Tear down the Geneve mesh on this host and remove its API document? [y/N] "
                    );
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
                    println!(
                        "mesh torn down; [mesh] in xdebbied.toml has enabled = true, so it was rebuilt from config"
                    );
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
            println!(
                "asked   {}  [{}, {} peer{}]",
                who(&asked.this),
                asked.state,
                asked.peers,
                if asked.peers == 1 { "" } else { "s" }
            );
            match &asked.leader {
                Some(l) if asked.is_leader => println!("leader  {}  (same box)", who(l)),
                Some(l) => println!("leader  {}", who(l)),
                None => println!("leader  none yet"),
            }
        }
        Cmd::Enroll(EnrollCmd::Ls { all }) => {
            let v = api.call(
                Method::GET,
                if all { "/enroll?all=true" } else { "/enroll" },
                None,
            )?;
            if cli.json {
                return out(v);
            }
            let es: Vec<Enrollment> = serde_json::from_value(v)?;
            if es.is_empty() {
                println!("no {}enrolments", if all { "" } else { "pending " });
                return Ok(());
            }
            println!(
                "{:<9} {:<24} {:>3}  {:<30} {:<23} {:<9} AGE",
                "ID", "HOST", "BOX", "URL", "FINGERPRINT", "STATUS"
            );
            for e in es {
                let fp = e.fingerprint.trim_start_matches("sha256:");
                let fp = format!(
                    "{}…{}",
                    &fp[..8.min(fp.len())],
                    &fp[fp.len().saturating_sub(8)..]
                );
                println!(
                    "{:<9} {:<24} {:>3}  {:<30} {:<23} {:<9} {}",
                    e.id,
                    e.hostname,
                    e.box_id,
                    e.api_url,
                    fp,
                    e.status,
                    format_age(e.requested_at)
                );
            }
        }
        Cmd::Enroll(EnrollCmd::Approve { host }) | Cmd::Approve { host } => {
            let v = api.call(Method::POST, &format!("/enroll/{host}/approve"), None)?;
            if cli.json {
                return out(v);
            }
            let e: Enrollment = serde_json::from_value(v)?;
            println!(
                "approved {} (box {}, {})",
                e.hostname, e.box_id, e.fingerprint
            );
            println!(
                "Its `xdebbied bootstrap --join` picks the certificate up within a few seconds."
            );
        }
        Cmd::Enroll(EnrollCmd::Deny { host }) => {
            let v = api.call(Method::POST, &format!("/enroll/{host}/deny"), None)?;
            if cli.json {
                return out(v);
            }
            let e: Enrollment = serde_json::from_value(v)?;
            println!("denied {} ({})", e.hostname, e.fingerprint);
        }
        Cmd::Ca { chain: true } => {
            let (code, v) = api.call_raw(Method::GET, "/cert", None, None)?;
            if !code.is_success() {
                bail!("{code}: {}", api_error_text(&v));
            }
            if cli.json {
                return out(v);
            }
            let chain: Vec<CertInfo> = serde_json::from_value(v)?;
            println!("# certificate chain served by {}", api.url);
            for (i, c) in chain.iter().enumerate() {
                let role = if c.self_signed {
                    "root CA"
                } else if i == 0 {
                    "server"
                } else {
                    "intermediate"
                };
                println!("# {i}: {role}: {}", c.subject);
                println!("#    issuer {}", c.issuer);
                println!("#    valid {} to {}", c.not_before, c.not_after);
                println!("#    fingerprint {}", c.fingerprint);
                print!("{}", c.pem);
            }
        }
        Cmd::Ca { chain: false } => {
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
            println!(
                "current   {}{}",
                st.id,
                if st.current_used { "  (in use)" } else { "" }
            );
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
            println!(
                "re-source it, and update any other holder (abe-csi secret, CAPX). Watch with"
            );
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
            HostCmd::Describe { name } => {
                out(api.call(Method::GET, &format!("/hosts/{name}"), None)?)?
            }
            HostCmd::Ls => {
                let v = api.call(Method::GET, "/hosts", None)?;
                if cli.json {
                    return out(v);
                }
                let hs: Vec<Host> = serde_json::from_value(v)?;
                let vms: Vec<Vm> = serde_json::from_value(api.call(Method::GET, "/vms", None)?)?;
                // the name column fits the longest host name (FQDNs are common)
                let w = hs.iter().map(|h| h.name.len()).max().unwrap_or(0).max(4);
                println!(
                    "{:<w$} {:<10} {:<5} {:^11} {:^15} {:>9} {:>13} {:<3}  STATUS",
                    "NAME", "FD", "CP", "VCPU", "MEM GiB", "DISK GiB", "ABE GiB", "TLS"
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
                    // free/total of what the ABE may still promise: thin
                    // pools against bytes × overcommit, thick against bytes.
                    let abe_col = match &h.abe_capacity {
                        Some(c) => {
                            let limit = c.limit(h.abe_overcommit);
                            format!(
                                "{}/{}",
                                limit.saturating_sub(c.committed()) >> 30,
                                limit >> 30
                            )
                        }
                        None => "-".into(),
                    };
                    // whether that ABE's API is behind mutual TLS (abe >= 0.5.0)
                    let tls = match &h.abe_capacity {
                        Some(c) if c.tls => "yes",
                        Some(_) => "no",
                        None => "-",
                    };
                    println!(
                        "{:<w$} {:<10} {:<5} {:>5}/{:<5} {:>7}/{:<7} {:>9} {:>13} {:<3}  {status}",
                        h.name,
                        h.failure_domain,
                        h.control_plane_eligible,
                        c,
                        a.cpus,
                        m / 1024,
                        a.memory_mib / 1024,
                        a.disk_free_gib,
                        abe_col,
                        tls
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
                    agent: None,
                    name: name.clone(),
                    uri,
                    failure_domain,
                    control_plane_eligible: !no_control_plane,
                    labels: labels.into_iter().collect(),
                    storage_pool: pool,
                    bridge,
                    storage_bridge: None,
                    cpu_overcommit,
                    mem_overcommit: 1.0,
                    reserved_memory_mib,
                    reserved_cpus,
                    cordoned: false,
                    capacity: None,
                    abe_capacity: None,
                    abe_remote: false,
                    abe_overcommit: 1.0,
                    abe_max_data_percent: 85.0,
                    abe_max_metadata_percent: 80.0,
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
                println!(
                    "{:<32} {:<24} {:>8} {:<36} {:<8} {:<6} MESSAGE",
                    "ID", "NAME", "GIB", "ATTACHED", "TARGET", "AGE"
                );
                for v in vols {
                    let gib = v.bytes as f64 / (1u64 << 30) as f64;
                    let (vm, target) = match (&v.attached, v.attach_to) {
                        (Some(a), _) => (a.vm.to_string(), a.target.clone()),
                        (None, Some(w)) => (format!("{w} (pending)"), "-".into()),
                        (None, None) => ("-".into(), "-".into()),
                    };
                    println!(
                        "{:<32} {:<24} {:>8.1} {:<36} {:<8} {:<6} {}",
                        v.id,
                        v.name,
                        gib,
                        vm,
                        target,
                        format_age(v.created),
                        v.message.as_deref().unwrap_or("")
                    );
                }
            }
            VolumeCmd::Describe { id } => {
                out(api.call(Method::GET, &format!("/volumes/{id}"), None)?)?
            }
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
            VolumeCmd::Detach { id } => {
                out(api.call(Method::POST, &format!("/volumes/{id}/detach"), None)?)?
            }
        },
        Cmd::Image(_) => unreachable!("handled before contacting xdebbied"),
        Cmd::Version => unreachable!("handled before contacting xdebbied"),
        Cmd::Completion { .. } => unreachable!("handled before contacting xdebbied"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KA: &str = "EtEvrro2LlkElPgm+ge5ndqtzL2o9ehIWwyIFnKsBQU=";
    const KB: &str = "2Mt9XkPn7iVObR5nCjQYhTe1rtVf6hVuvIG1cyL+zGM=";
    const KC: &str = "4ZJxNbbSoUq4tgYKAa0Ow6EYwlANqldjfPGYdPZ9P2I=";

    fn spec() -> Value {
        json!({
            "box": 0,
            "wireguard": {"listen_port": 51820},
            "peers": [{"box": 2, "pubkey": KA}],
            "clients": [{"box": 1, "pubkey": KB, "name": "gram"}],
        })
    }

    #[test]
    fn client_add_takes_the_lowest_free_box() {
        let mut s = spec();
        assert_eq!(
            mesh_add_client(&mut s, KC, Some("laptop"), None, None).unwrap(),
            ClientAdd::Added(3),
            "0 = self, 1 = client, 2 = peer"
        );
        assert_eq!(
            s["clients"][1],
            json!({"box": 3, "pubkey": KC, "name": "laptop"})
        );

        // a hole is reused
        let mut s = spec();
        s["box"] = json!(5);
        s["clients"] = json!([]);
        assert_eq!(
            mesh_add_client(&mut s, KC, None, Some("h:1"), None).unwrap(),
            ClientAdd::Added(1)
        );
        assert_eq!(s["clients"][0]["endpoint"], "h:1");

        // no clients key at all (the daemon leaves an empty list out)
        let mut s = json!({"box": 0, "wireguard": {}, "peers": []});
        assert_eq!(
            mesh_add_client(&mut s, KC, None, None, None).unwrap(),
            ClientAdd::Added(1)
        );
    }

    #[test]
    fn client_add_is_idempotent_and_never_replaces() {
        let mut s = spec();
        let before = s.clone();
        assert_eq!(
            mesh_add_client(&mut s, KB, Some("other"), None, Some(9)).unwrap(),
            ClientAdd::Exists(1)
        );
        assert_eq!(s, before, "spec untouched");

        for taken in [1u8, 2] {
            let e = mesh_add_client(&mut s, KC, None, None, Some(taken))
                .unwrap_err()
                .to_string();
            assert!(e.contains("taken"), "{e}");
        }
        for bad in [0u8, 255] {
            let e = mesh_add_client(&mut s, KC, None, None, Some(bad)).unwrap_err();
            assert!(e.to_string().contains("1-254"), "{e}");
        }
        assert_eq!(s, before);
        let mut s7 = spec();
        s7["box"] = json!(7);
        let e = mesh_add_client(&mut s7, KC, None, None, Some(7)).unwrap_err();
        assert!(e.to_string().contains("this box"), "{e}");
        assert_eq!(
            mesh_add_client(&mut s, KC, None, None, Some(254)).unwrap(),
            ClientAdd::Added(254)
        );

        // a peer's key: says how to migrate
        let mut s = spec();
        let e = mesh_add_client(&mut s, KA, None, None, None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("xdctl mesh peer rm 2"), "{e}");
        // duplicate name
        let e = mesh_add_client(&mut s, KC, Some("gram"), None, None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("gram"), "{e}");
        // no managed WireGuard
        let mut s = json!({"box": 0, "peers": []});
        assert!(mesh_add_client(&mut s, KC, None, None, None).is_err());
    }

    #[test]
    fn client_rm_by_box_name_or_key() {
        for which in ["1", "gram", KB] {
            let mut s = spec();
            assert_eq!(mesh_rm_client(&mut s, which).unwrap(), 1);
            assert_eq!(s["clients"], json!([]));
            assert_eq!(s["peers"].as_array().unwrap().len(), 1, "peers untouched");
        }
        let mut s = spec();
        assert!(
            mesh_rm_client(&mut s, "2").is_err(),
            "box 2 is a peer, not a client"
        );
        assert!(mesh_rm_client(&mut s, "nobody").is_err());
    }

    #[test]
    fn join_line() {
        let doc = json!({
            "spec": {"wireguard": {"listen_port": 23444}},
            "status": {"wireguard": {"pubkey": KA, "listen_port": 23444}},
        });
        assert_eq!(
            xdj_join_line(3, &doc, None),
            (format!("xdj join 3 {KA} HUB_HOST:23444"), true, false)
        );
        assert_eq!(
            xdj_join_line(3, &doc, Some("timesink.example")).0,
            format!("xdj join 3 {KA} timesink.example:23444")
        );
        assert_eq!(
            xdj_join_line(3, &doc, Some("timesink.example:51820")).0,
            format!("xdj join 3 {KA} timesink.example:51820")
        );
        assert_eq!(hub_endpoint("192.0.2.1", 1), "192.0.2.1:1");
        assert_eq!(hub_endpoint("2001:db8::1", 1), "[2001:db8::1]:1");
        assert_eq!(hub_endpoint("[2001:db8::1]:7", 1), "[2001:db8::1]:7");
        let nokey = json!({"spec": {"wireguard": {}}, "status": {}});
        let (l, _, k) = xdj_join_line(4, &nokey, Some("h"));
        assert_eq!((l.as_str(), k), ("xdj join 4 HUB_PUBKEY h:51820", true));
    }
}

/// Lay `xdctl mesh client-config set`'s options over the current document.
fn client_config_apply(
    v: &mut Value,
    zones: &[String],
    search: Option<&str>,
    welcome_file: Option<&str>,
    welcome: Option<&str>,
    clear_welcome: bool,
) -> Result<()> {
    if !v.is_object() {
        *v = serde_json::json!({});
    }
    if !zones.is_empty() {
        let mut z = vec![];
        if !(zones.len() == 1 && zones[0] == "none") {
            for one in zones {
                let (zone, server) = one
                    .split_once('=')
                    .with_context(|| format!("--zone {one:?}: want ZONE=SERVER"))?;
                z.push(serde_json::json!({"zone": zone.trim(), "server": server.trim()}));
            }
        }
        v["dns"] = Value::Array(z);
    }
    if let Some(s) = search {
        v["search"] = if s.trim() == "none" {
            Value::Array(vec![])
        } else {
            s.split([',', ' '])
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|x| Value::String(x.into()))
                .collect()
        };
    }
    if let Some(f) = welcome_file {
        let text = if f == "-" {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            s
        } else {
            std::fs::read_to_string(f).with_context(|| format!("reading {f}"))?
        };
        v["welcome"] = Value::String(text);
    }
    if let Some(t) = welcome {
        v["welcome"] = Value::String(t.replace("\\n", "\n"));
    }
    if clear_welcome {
        v["welcome"] = Value::String(String::new());
    }
    Ok(())
}

fn print_client_config(v: &Value) {
    let zones = v["dns"].as_array().cloned().unwrap_or_default();
    if zones.is_empty() {
        println!("zones    (none)");
    }
    for z in zones {
        println!(
            "zone     {:<28} -> {}",
            z["zone"].as_str().unwrap_or("?"),
            z["server"].as_str().unwrap_or("?")
        );
    }
    let search: Vec<&str> = v["search"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    println!(
        "search   {}",
        if search.is_empty() {
            "(none)".into()
        } else {
            search.join(" ")
        }
    );
    match v["welcome"].as_str().filter(|w| !w.is_empty()) {
        Some(w) => {
            println!("welcome  |");
            for l in w.lines() {
                println!("  {l}");
            }
        }
        None => println!("welcome  (none)"),
    }
}

#[cfg(test)]
mod client_config_tests {
    use super::*;

    #[test]
    fn set_lays_options_over_the_document() {
        let mut v = serde_json::json!({"dns": [{"zone": "a.example", "server": "10.0.0.1"}], "search": ["a.example"], "welcome": "hi"});
        client_config_apply(
            &mut v,
            &[
                "m.example=10.23.0.11".into(),
                "i.example = 10.23.0.243".into(),
            ],
            Some("m.example, i.example,example"),
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(v["dns"][1]["zone"], "i.example");
        assert_eq!(v["dns"][1]["server"], "10.23.0.243");
        assert_eq!(
            v["search"],
            serde_json::json!(["m.example", "i.example", "example"])
        );
        assert_eq!(v["welcome"], "hi");
        client_config_apply(
            &mut v,
            &["none".into()],
            Some("none"),
            None,
            Some("line 1\\nline 2"),
            false,
        )
        .unwrap();
        assert_eq!(v["dns"], serde_json::json!([]));
        assert_eq!(v["search"], serde_json::json!([]));
        assert_eq!(v["welcome"], "line 1\nline 2");
        client_config_apply(&mut v, &[], None, None, None, true).unwrap();
        assert_eq!(v["welcome"], "");
        assert!(client_config_apply(&mut v, &["nozone".into()], None, None, None, false).is_err());
    }
}
