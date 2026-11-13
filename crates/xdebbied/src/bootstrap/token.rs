// SPDX-License-Identifier: GPL-3.0-or-later
//! Secure token generation for API authentication.

use anyhow::{Context, Result};
use std::path::Path;
use tracing::info;

/// Generate a cryptographically secure random token
pub fn generate_token() -> Result<String> {
    use rand::RngCore;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    Ok(hex::encode(bytes))
}

/// Write token to file with secure permissions (root:root 0400).
pub fn write_token(path: &Path, token: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("creating token directory")?;
    }
    crate::token::write_private(path, &format!("{token}\n"))
        .with_context(|| format!("writing token to {}", path.display()))?;
    info!("Token written to {} (root:root 0400)", path.display());
    Ok(())
}

/// Read existing token from file
pub fn read_token(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("reading token from {}", path.display()))
        .map(|s| s.trim().to_string())
}

/// Check if token file exists and is non-empty
pub fn token_exists(path: &Path) -> bool {
    path.exists() && std::fs::read_to_string(path).is_ok()
}

/// Write the xdctl environment file as root:xdebbie 0440 so any member of
/// the xdebbie group can `source` it without sudo.
/// Falls back to root:root 0400 if the xdebbie group does not exist yet.
pub fn write_env_file(path: &Path, url: &str, token: &str, ca: Option<&Path>) -> Result<()> {
    let ca_line = match ca {
        Some(c) => format!("export XDEBBIE_CA={}\n", c.display()),
        None => String::new(),
    };
    let content = format!(
        r#"# xdebbie environment variables
# Source this file: source {path}

export XDEBBIE_URL={url}
export XDEBBIE_TOKEN='{token}'
{ca_line}"#,
        path = path.display(),
    );

    crate::token::write_group_readable(path, &content)
        .with_context(|| format!("writing environment file to {}", path.display()))?;

    let gid = crate::token::xdebbie_gid();
    if gid.is_some() {
        info!("Environment file written to {} (root:xdebbie 0440)", path.display());
        info!("Source with: source {}", path.display());
    } else {
        info!("Environment file written to {} (root:root 0400 — xdebbie group not found)", path.display());
        info!("Run `sudo usermod -aG xdebbie $USER` after the group is created, then re-bootstrap");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_token() {
        let token1 = generate_token().unwrap();
        let token2 = generate_token().unwrap();
        assert_eq!(token1.len(), 64);
        assert_ne!(token1, token2);
        assert!(token1.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
