// SPDX-License-Identifier: GPL-3.0-or-later
//! What this hub tells its xdj clients about the site: `GET`/`PUT
//! /v1/client-config`, and `GET /v1/client-config/xdj` (the same as text,
//! for xdj, which has no JSON library).
//!
//! | key | means |
//! |---|---|
//! | `dns` | zones and the resolver that answers each, `[{zone, server}]` |
//! | `search` | search domains in order of preference: short names try these first |
//! | `welcome` | a banner xdj prints once connected: what is where on this site |
//!
//! Set in `[clients]` in xdebbied.toml; a `PUT` replaces it while running and
//! is kept in `client-config.json` beside `state_file`, which overrides the
//! config file from then on (as `settings.json` does for `/v1/settings`).
//!
//! The `GET`s are public, like `/v1/version`: an xdj client asks before it
//! holds a certificate, over its WireGuard link. Anyone who can reach the API
//! can read them, so nothing secret belongs here. A `PUT` needs the cluster
//! token or a cluster certificate. Box-local: each hub answers for itself.

use std::{collections::BTreeSet, net::IpAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use axum::{
    Extension, Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::Config;

/// The banner's limit: a screenful, not a document.
pub const WELCOME_MAX: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    /// `mesh.example.org`: names under it go to `server`.
    pub zone: String,
    /// The resolver's address.
    pub server: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    #[serde(default)]
    pub dns: Vec<Zone>,
    #[serde(default)]
    pub search: Vec<String>,
    #[serde(default)]
    pub welcome: String,
}

/// A DNS name: dot-separated labels of letters, digits and inner hyphens,
/// 63 bytes a label, 253 in all. A trailing dot is dropped.
fn dns_name(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('.').to_ascii_lowercase();
    let ok = !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    ok.then_some(s)
}

impl ClientConfig {
    /// Checked and tidied (names lower-cased, no trailing dots), or why not.
    pub fn validated(mut self) -> Result<Self> {
        let mut seen = BTreeSet::new();
        for z in &mut self.dns {
            z.zone = dns_name(&z.zone)
                .with_context(|| format!("dns zone {:?} is not a DNS name", z.zone))?;
            z.server = z.server.trim().to_string();
            if z.server.parse::<IpAddr>().is_err() {
                bail!(
                    "dns server {:?} for {} is not an IP address",
                    z.server,
                    z.zone
                );
            }
            if !seen.insert(z.zone.clone()) {
                bail!("dns zone {} is listed twice", z.zone);
            }
        }
        let mut seen = BTreeSet::new();
        for s in &mut self.search {
            *s = dns_name(s).with_context(|| format!("search domain {s:?} is not a DNS name"))?;
            if !seen.insert(s.clone()) {
                bail!("search domain {s} is listed twice");
            }
        }
        if self.welcome.len() > WELCOME_MAX {
            bail!(
                "welcome is {} bytes; at most {WELCOME_MAX}",
                self.welcome.len()
            );
        }
        if self
            .welcome
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            bail!("welcome may hold printable text, tabs and newlines only");
        }
        Ok(self)
    }

    /// For xdj: its own `key = val` style, then the banner, verbatim, after
    /// a `[welcome]` line.
    ///
    /// ```text
    /// zone = mesh.example.org 10.23.0.11
    /// search = mesh.example.org example.org
    /// [welcome]
    /// ...
    /// ```
    pub fn to_xdj(&self) -> String {
        let mut s = String::from("# what this hub tells xdj clients (GET /v1/client-config/xdj)\n");
        for z in &self.dns {
            s.push_str(&format!("zone = {} {}\n", z.zone, z.server));
        }
        if !self.search.is_empty() {
            s.push_str(&format!("search = {}\n", self.search.join(" ")));
        }
        if !self.welcome.is_empty() {
            s.push_str("[welcome]\n");
            s.push_str(&self.welcome);
            if !self.welcome.ends_with('\n') {
                s.push('\n');
            }
        }
        s
    }
}

pub fn path_for(cfg: &Config) -> PathBuf {
    cfg.state_file.with_file_name("client-config.json")
}

/// What a `PUT` last set, else `[clients]`, else nothing.
pub fn load(cfg: &Config) -> ClientConfig {
    std::fs::read_to_string(path_for(cfg))
        .ok()
        .and_then(|t| serde_json::from_str::<ClientConfig>(&t).ok())
        .and_then(|c| c.validated().ok())
        .or_else(|| cfg.clients.clone().and_then(|c| c.validated().ok()))
        .unwrap_or_default()
}

pub struct ClientConfigCtl {
    path: PathBuf,
    now: RwLock<ClientConfig>,
}

impl ClientConfigCtl {
    pub fn new(cfg: &Config) -> Self {
        Self {
            path: path_for(cfg),
            now: RwLock::new(load(cfg)),
        }
    }

    #[cfg(test)]
    pub fn in_memory(now: ClientConfig) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "xdebbie-client-config-{}.json",
                uuid::Uuid::new_v4()
            )),
            now: RwLock::new(now),
        }
    }

    pub async fn get(&self) -> ClientConfig {
        self.now.read().await.clone()
    }

    /// Replace it whole: validate, persist, serve.
    pub async fn set(&self, c: ClientConfig) -> Result<ClientConfig> {
        let c = c.validated()?;
        let body = serde_json::to_string_pretty(&c)? + "\n";
        crate::token::write_group_readable(&self.path, &body)
            .with_context(|| format!("saving {}", self.path.display()))?;
        *self.now.write().await = c.clone();
        Ok(c)
    }
}

pub fn router(ctl: Arc<ClientConfigCtl>) -> Router {
    Router::new()
        .route("/client-config", get(get_json).put(put_json))
        .route("/client-config/xdj", get(get_xdj))
        .with_state(ctl)
}

async fn get_json(State(c): State<Arc<ClientConfigCtl>>) -> Json<ClientConfig> {
    Json(c.get().await)
}

async fn get_xdj(State(c): State<Arc<ClientConfigCtl>>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        c.get().await.to_xdj(),
    )
        .into_response()
}

async fn put_json(
    State(c): State<Arc<ClientConfigCtl>>,
    authn: Option<Extension<crate::api::Authn>>,
    Json(body): Json<ClientConfig>,
) -> Response {
    // Only the cluster's own credentials change it; the narrower tokens
    // (settings, enroll, schedule) and xdj client certificates do not.
    if !matches!(
        authn.map(|Extension(a)| a),
        Some(crate::api::Authn::Token | crate::api::Authn::Cert(_))
    ) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "changing the client config needs the cluster token or a cluster certificate"})),
        )
            .into_response();
    }
    match c.set(body).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> ClientConfig {
        ClientConfig {
            dns: vec![
                Zone {
                    zone: "Mesh.Example.org.".into(),
                    server: "10.23.0.11".into(),
                },
                Zone {
                    zone: "int.example.org".into(),
                    server: "10.23.0.243".into(),
                },
            ],
            search: vec![
                "mesh.example.org".into(),
                "int.example.org".into(),
                "example.org".into(),
            ],
            welcome: "Welcome to the mesh.\n\tgitea: gitea.mesh.example.org".into(),
        }
    }

    #[test]
    fn validation_tidies_and_refuses() {
        let c = site().validated().unwrap();
        assert_eq!(c.dns[0].zone, "mesh.example.org");
        let bad = |f: fn(&mut ClientConfig)| {
            let mut c = site();
            f(&mut c);
            c.validated().is_err()
        };
        assert!(bad(|c| c.dns[0].server = "dns.example".into()));
        assert!(bad(|c| c.dns[1].zone = "mesh.example.org".into()));
        assert!(bad(|c| c.dns[0].zone = "bad_name.org".into()));
        assert!(bad(|c| c.dns[0].zone = "-x.org".into()));
        assert!(bad(|c| c.search.push("example.org".into())));
        assert!(bad(|c| c.welcome = "x".repeat(WELCOME_MAX + 1)));
        assert!(bad(|c| c.welcome = "bell\u{7}".into()));
        assert!(ClientConfig::default().validated().is_ok());
    }

    #[test]
    fn xdj_text_is_lines_then_the_banner() {
        let t = site().validated().unwrap().to_xdj();
        assert!(
            t.contains("zone = mesh.example.org 10.23.0.11\nzone = int.example.org 10.23.0.243\n"),
            "{t}"
        );
        assert!(
            t.contains("search = mesh.example.org int.example.org example.org\n"),
            "{t}"
        );
        assert!(
            t.ends_with("[welcome]\nWelcome to the mesh.\n\tgitea: gitea.mesh.example.org\n"),
            "{t}"
        );
        assert!(!ClientConfig::default().to_xdj().contains("[welcome]"));
    }

    #[tokio::test]
    async fn set_replaces_and_refuses_bad() {
        let ctl = ClientConfigCtl::in_memory(ClientConfig::default());
        assert!(ctl.set(site()).await.is_ok());
        assert_eq!(ctl.get().await.search.len(), 3);
        let mut b = site();
        b.dns[0].server = "nope".into();
        assert!(ctl.set(b).await.is_err());
        assert_eq!(ctl.get().await.dns[0].server, "10.23.0.11");
        let _ = std::fs::remove_file(&ctl.path);
    }
}
