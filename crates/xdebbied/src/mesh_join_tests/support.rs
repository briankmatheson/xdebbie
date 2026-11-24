// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared fixtures: a cluster CA on disk, enrolment requests, and a CA box
//! serving the real API over mTLS on an ephemeral loopback port.

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
};

use xdebbie_core::{EnrollKind, EnrollRequest, TokenSet};

use crate::{
    abe,
    api::{self, App},
    bootstrap::api_tls::{self, ApiTlsPaths, CaSigner},
    enroll::Enrollments,
    leader::Leadership,
    mesh::{FakeNetOps, MeshConfig, MeshManager},
    store::Store,
    token::Tokens,
};

/// The cluster token every fixture CA box hands out.
pub const TOKEN: &str = "cluster-secret";

/// A fresh cluster CA (and a server cert for `ca-box`) in `dir`.
pub fn ca_in(dir: &Path) -> (ApiTlsPaths, CaSigner) {
    let paths = paths_in(dir);
    api_tls::ensure_api_tls("ca-box", false, &paths).unwrap();
    let signer = CaSigner::load(&paths.ca_cert, &paths.ca_key)
        .unwrap()
        .unwrap();
    (paths, signer)
}

/// Where a box keeps its API TLS files, under `dir`.
pub fn paths_in(dir: &Path) -> ApiTlsPaths {
    ApiTlsPaths {
        ca_cert: dir.join("api-ca.pem"),
        ca_key: dir.join("api-ca-key.pem"),
        server_cert: dir.join("api-cert.pem"),
        server_key: dir.join("api-key.pem"),
    }
}

/// An enrolment queue on a CA box whose own id is 0, persisted in `dir`.
pub fn queue(dir: &Path, reserved: Vec<u8>) -> Enrollments {
    let (_, signer) = ca_in(dir);
    Enrollments::load(dir.join("enroll.json"), Some(signer), reserved).unwrap()
}

/// A box's enrolment request with a fresh key; returns it and the key's
/// fingerprint.
pub fn box_req(host: &str, want: Option<u8>) -> (EnrollRequest, api_tls::Csr) {
    let csr = api_tls::make_csr(host).unwrap();
    (
        EnrollRequest {
            kind: EnrollKind::Box,
            hostname: host.into(),
            box_id: want,
            api_url: format!("https://{host}:23443"),
            csr: csr.csr_pem.clone(),
            wg_pubkey: None,
        },
        csr,
    )
}

/// A CA box serving the real router over mTLS.
pub struct CaBox {
    pub url: String,
    pub ca_pem: String,
    pub ca_fingerprint: String,
    pub enroll: Arc<Enrollments>,
    pub mesh: Arc<MeshManager>,
    _dir: tempfile::TempDir,
    handle: axum_server::Handle<std::net::SocketAddr>,
}

impl Drop for CaBox {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

/// What the CA box's `[network]` says, when it has one.
pub const CLUSTER_NETWORK: &str = r#"
cidr = "10.23.0.0/16"
gateway = "10.23.0.1"
dns = ["10.23.0.1"]
pool_start = "10.23.0.2"
pool_end = "10.23.0.254"
reserved = ["10.23.0.9"]
reserve_from_dns = [{ zone = "mesh.example.org", server = "10.23.0.1" }]
upstream_dns = ["192.168.1.1", "192.168.1.2"]
domain = "vm.example"
ntp = ["10.23.0.1", "192.168.1.123"]
lease_secs = 7200
"#;

impl CaBox {
    /// A CA box (box 0) with no `[network]`.
    pub async fn start() -> Self {
        Self::start_with(None).await
    }

    /// A CA box (box 0); `network` is its `[network]` table body.
    pub async fn start_with(network: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = api_tls::generate_localhost(dir.path());
        let signer = CaSigner::load(&paths.ca_cert, &paths.ca_key)
            .unwrap()
            .unwrap();
        let ca_pem = signer.ca_pem().to_string();
        let ca_fingerprint = api_tls::cert_fingerprint(&ca_pem).unwrap();
        let enroll = Arc::new(
            Enrollments::load(dir.path().join("enroll.json"), Some(signer), vec![0]).unwrap(),
        );
        let f = FakeNetOps::new();
        f.add_bridge("br-int", 1500, "52:54:00:23:00:fe");
        f.add_underlay("ul0", 1420, Ipv4Addr::new(10, 22, 0, 1));
        let mesh_cfg = MeshConfig {
            enabled: false,
            underlay: "ul0".into(),
            allow_plain_underlay: true,
            ..Default::default()
        };
        let mesh =
            MeshManager::new_for_test(Arc::new(f), dir.path().into(), Some(&mesh_cfg)).unwrap();
        let store = Store::open(dir.path().join("state.json")).unwrap();
        let leadership = Leadership::standalone("ca-box", "https://unused");
        let dhcp = network.map(|n| {
            let net: crate::dhcp::NetworkConfig = toml::from_str(n).unwrap();
            Arc::new(crate::dhcp::DhcpServer::new(
                net,
                "virbr-xdebbie".into(),
                store.clone(),
                Arc::new(|_: &str| None),
                crate::dhcp::Gate {
                    leader: leadership.flag(),
                    leader_only: false,
                },
            ))
        });
        let app = App {
            store,
            cfg: Arc::new(toml::from_str("state_file = 'unused'").unwrap()),
            token: Some(Arc::new(Tokens::in_memory(TokenSet {
                current: TOKEN.into(),
                previous: None,
            }))),
            mesh: mesh.clone(),
            dhcp,
            abe: abe::Client::new(),
            leadership,
            http: reqwest::Client::new(),
            enroll: enroll.clone(),
            volume_names: Arc::default(),
            abe_ledger: Arc::default(),
            agent: None,
            settings: None,
            client_config: None,
        };
        let roots = crate::tls::client_roots(&paths.ca_cert, &dir.path().join("no-peer-cas"));
        let sc = crate::tls::server_config(&paths.server_cert, &paths.server_key, roots).unwrap();
        let handle: axum_server::Handle<std::net::SocketAddr> = axum_server::Handle::new();
        let h = handle.clone();
        tokio::spawn(async move {
            axum_server::bind("127.0.0.1:0".parse().unwrap())
                .acceptor(crate::tls::PeerCertAcceptor::new(sc))
                .handle(h)
                .serve(api::router(app).into_make_service())
                .await
                .unwrap();
        });
        let addr = handle.listening().await.unwrap();
        CaBox {
            url: format!("https://127.0.0.1:{}", addr.port()),
            ca_pem,
            ca_fingerprint,
            enroll,
            mesh,
            _dir: dir,
            handle,
        }
    }

    /// An HTTPS client trusting the cluster CA, with no credentials.
    pub fn anon(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .use_rustls_tls()
            .add_root_certificate(reqwest::Certificate::from_pem(self.ca_pem.as_bytes()).unwrap())
            .build()
            .unwrap()
    }

    /// Same, presenting `cert` + `key` as its client certificate.
    pub fn with_cert(&self, cert: &str, key: &str) -> reqwest::Client {
        reqwest::Client::builder()
            .use_rustls_tls()
            .add_root_certificate(reqwest::Certificate::from_pem(self.ca_pem.as_bytes()).unwrap())
            .identity(reqwest::Identity::from_pem(format!("{cert}\n{key}").as_bytes()).unwrap())
            .build()
            .unwrap()
    }
}

/// A scratch directory for a joiner: its TLS paths and its state dir.
pub struct Joiner {
    pub dir: tempfile::TempDir,
    pub paths: ApiTlsPaths,
    pub state_dir: PathBuf,
}

impl Joiner {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let state_dir = dir.path().join("state");
        Joiner {
            dir,
            paths,
            state_dir,
        }
    }
}

/// Approve `host` as soon as its request shows up in the queue (what an
/// operator does with `xdctl approve HOST`). Returns once approved.
pub async fn approve_when_pending(enroll: Arc<Enrollments>, host: String) {
    for _ in 0..400 {
        if enroll.list(false).await.iter().any(|e| e.hostname == host) {
            enroll
                .decide(&host, crate::enroll::Decision::Approve)
                .await
                .unwrap();
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("{host} never asked to join");
}

/// What `GET /v1/network` on a CA box with this `[network]` answers.
pub fn cluster_status(network: &str) -> crate::dhcp::NetworkStatus {
    let dir = tempfile::tempdir().unwrap();
    let net: crate::dhcp::NetworkConfig = toml::from_str(network).unwrap();
    crate::dhcp::DhcpServer::new(
        net,
        "virbr-xdebbie".into(),
        Store::open(dir.path().join("state.json")).unwrap(),
        Arc::new(|_: &str| None),
        crate::dhcp::Gate {
            leader: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            leader_only: false,
        },
    )
    .status()
}
