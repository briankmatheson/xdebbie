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

/// Write token to file with secure permissions
pub fn write_token(path: &Path, token: &str) -> Result<()> {
    // Create parent directory if needed
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

/// Check if token file exists and is readable
pub fn token_exists(path: &Path) -> bool {
    path.exists() && std::fs::read_to_string(path).is_ok()
}

/// Generate environment file for xdctl
pub fn write_env_file(path: &Path, url: &str, token: &str) -> Result<()> {
    let content = format!(
        r#"# xdebbie environment variables
# Source this file: source {}

export XDEBBIE_URL={}
export XDEBBIE_TOKEN='{}'
"#,
        path.display(),
        url,
        token
    );

    // Holds the token: same protection as the token file.
    crate::token::write_private(path, &content)
        .with_context(|| format!("writing environment file to {}", path.display()))?;

    info!("Environment file written to {} (root:root 0400)", path.display());
    info!("Source with: sudo -s; source {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_token() {
        let token1 = generate_token().unwrap();
        let token2 = generate_token().unwrap();
        
        // Should be 64 hex characters (32 bytes)
        assert_eq!(token1.len(), 64);
        assert_eq!(token2.len(), 64);
        
        // Should be different
        assert_ne!(token1, token2);
        
        // Should be valid hex
        assert!(token1.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
