// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal async client for the xdebbied REST API.

use anyhow::{Result, bail};
use reqwest::{Client, StatusCode};
use uuid::Uuid;
use xdebbie_core::{FailureDomain, Vm, VmSpec};

#[derive(Clone)]
pub struct XDebbie8s {
    c: Client,
    url: String,
    token: Option<String>,
}

impl XDebbie8s {
    /// `ca`: the PEM bundle to trust for an https URL, given as a file path
    /// or as the PEM text itself (what a Secret key holds). Without it the
    /// system roots apply, which do not know the xdebbie cluster CA.
    pub fn new(url: String, token: Option<String>, ca: Option<&str>) -> anyhow::Result<Self> {
        let mut b = Client::builder().timeout(std::time::Duration::from_secs(30));
        if let Some(ca) = ca.map(str::trim).filter(|s| !s.is_empty()) {
            let pem = if ca.starts_with("-----BEGIN") {
                ca.as_bytes().to_vec()
            } else {
                std::fs::read(ca).map_err(|e| anyhow::anyhow!("XDEBBIE_CA {ca}: {e}"))?
            };
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|e| anyhow::anyhow!("XDEBBIE_CA: not a PEM certificate bundle: {e}"))?;
            anyhow::ensure!(
                !certs.is_empty(),
                "XDEBBIE_CA: no certificates in the bundle"
            );
            tracing::info!(cas = certs.len(), url = %url, "trusting the xdebbie CA for the API");
            for c in certs {
                b = b.add_root_certificate(c);
            }
        } else if url.starts_with("https://") {
            tracing::warn!(url = %url, "https without XDEBBIE_CA: only system roots are trusted");
        }
        Ok(Self {
            c: b.build()?,
            url,
            token,
        })
    }
    fn req(&self, m: reqwest::Method, p: &str) -> reqwest::RequestBuilder {
        let r = self
            .c
            .request(m, format!("{}/v1{p}", self.url.trim_end_matches('/')));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }
    pub async fn get_vm(&self, id: Uuid) -> Result<Option<Vm>> {
        let r = self
            .req(reqwest::Method::GET, &format!("/vms/{id}"))
            .send()
            .await?;
        match r.status() {
            StatusCode::NOT_FOUND => Ok(None),
            s if s.is_success() => Ok(Some(r.json().await?)),
            s => bail!(
                "xdebbied GET vm {id}: {s}: {}",
                r.text().await.unwrap_or_default()
            ),
        }
    }
    pub async fn put_vm(&self, id: Uuid, spec: &VmSpec) -> Result<Vm> {
        let r = self
            .req(reqwest::Method::PUT, &format!("/vms/{id}"))
            .json(spec)
            .send()
            .await?;
        if !r.status().is_success() {
            bail!(
                "xdebbied PUT vm {id}: {}: {}",
                r.status(),
                r.text().await.unwrap_or_default()
            );
        }
        Ok(r.json().await?)
    }
    /// Returns true once the VM is gone.
    pub async fn delete_vm(&self, id: Uuid) -> Result<bool> {
        let r = self
            .req(reqwest::Method::DELETE, &format!("/vms/{id}"))
            .send()
            .await?;
        match r.status() {
            StatusCode::NOT_FOUND => Ok(true),
            s if s.is_success() => Ok(false),
            s => bail!("xdebbied DELETE vm {id}: {s}"),
        }
    }
    pub async fn list_vms(&self, selector: &str) -> Result<Vec<Vm>> {
        let r = self
            .req(reqwest::Method::GET, "/vms")
            .query(&[("selector", selector)])
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("xdebbied list vms: {}", r.status());
        }
        Ok(r.json().await?)
    }
    pub async fn failure_domains(&self) -> Result<Vec<FailureDomain>> {
        let r = self
            .req(reqwest::Method::GET, "/failure-domains")
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("xdebbied failure-domains: {}", r.status());
        }
        Ok(r.json().await?)
    }
}

#[cfg(test)]
mod ca_tests {
    use super::XDebbie8s;

    #[test]
    fn no_ca_builds_a_plain_client() {
        assert!(XDebbie8s::new("http://x:7443".into(), None, None).is_ok());
        assert!(XDebbie8s::new("http://x:7443".into(), None, Some("  ")).is_ok());
    }

    #[test]
    fn a_missing_ca_file_is_named() {
        let e = XDebbie8s::new("https://x:7443".into(), None, Some("/nonexistent/ca.pem"))
            .err()
            .expect("error")
            .to_string();
        assert!(e.contains("XDEBBIE_CA /nonexistent/ca.pem"), "{e}");
    }

    #[test]
    fn inline_text_that_is_not_a_certificate_is_refused() {
        let e = XDebbie8s::new(
            "https://x:7443".into(),
            None,
            Some("-----BEGIN CERTIFICATE-----\nnope\n-----END CERTIFICATE-----\n"),
        )
        .err()
        .expect("error")
        .to_string();
        assert!(e.contains("XDEBBIE_CA"), "{e}");
    }
}
