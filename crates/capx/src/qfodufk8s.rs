// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal async client for the xdebbied REST API.

use anyhow::{Result, bail};
use xdebbie_core::{FailureDomain, Vm, VmSpec};
use reqwest::{Client, StatusCode};
use uuid::Uuid;

#[derive(Clone)]
pub struct XDebbie8s {
    c: Client,
    url: String,
    token: Option<String>,
}

impl XDebbie8s {
    pub fn new(url: String, token: Option<String>) -> Self {
        Self {
            c: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap(),
            url,
            token,
        }
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
