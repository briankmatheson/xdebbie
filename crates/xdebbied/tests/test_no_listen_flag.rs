// SPDX-License-Identifier: GPL-3.0-or-later
//! Regression test: Ensure bootstrap never sets deprecated --listen flag
//!
//! libvirtd in Debian 13+ fails with exit code 6 if LIBVIRTD_ARGS="--listen"
//! is set. We must use systemd socket activation instead.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    

    /// Test that bootstrap code doesn't write deprecated --listen flag
    #[test]
    fn test_no_listen_flag_in_default_libvirtd() {
        // Search all source files for patterns that would write --listen
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        
        let mut found_violations = Vec::new();
        
        for entry in walkdir::WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            let path = entry.path();
            if let Ok(content) = fs::read_to_string(path) {
                // Check for dangerous patterns that write --listen
                let violations = [
                    (r#"LIBVIRTD_ARGS="--listen""#, "literal --listen flag"),
                    (r#"LIBVIRTD_ARGS=\"--listen\""#, "escaped --listen flag"),
                    (r"--listen", "any --listen reference"),
                ];
                
                for (pattern, description) in &violations {
                    // Allow this test file itself
                    if path.to_string_lossy().contains("test_no_listen_flag.rs") {
                        continue;
                    }
                    
                    // Allow comments and documentation
                    for (line_num, line) in content.lines().enumerate() {
                        let trimmed = line.trim();
                        
                        // Skip comments
                        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
                            continue;
                        }
                        
                        // Skip string literals in info!/warn!/error! macros (those are OK - just messages)
                        if trimmed.contains("info!(") || trimmed.contains("warn!(") || trimmed.contains("error!(") {
                            continue;
                        }
                        
                        if line.contains(pattern) {
                            found_violations.push(format!(
                                "{}:{} - Found {}: '{}'",
                                path.display(),
                                line_num + 1,
                                description,
                                line.trim()
                            ));
                        }
                    }
                }
            }
        }
        
        if !found_violations.is_empty() {
            panic!(
                "\n\n❌ REGRESSION: Found deprecated --listen flag!\n\n\
                Modern libvirtd uses socket activation, NOT --listen.\n\
                This will cause libvirtd to fail with exit code 6.\n\n\
                Violations found:\n{}\n\n\
                Fix: Use 'systemctl enable libvirtd-tls.socket' instead\n",
                found_violations.join("\n")
            );
        }
    }

    /// Test that bootstrap enables socket activation
    #[test]
    fn test_bootstrap_enables_socket_activation() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bootstrap/tls_certs.rs");
        
        let content = fs::read_to_string(&src)
            .expect("Could not read tls_certs.rs");
        
        // Must enable libvirtd-tls.socket
        assert!(
            content.contains("libvirtd-tls.socket") || content.contains("libvirtd-tcp.socket"),
            "Bootstrap must enable libvirtd-tls.socket or libvirtd-tcp.socket for TLS"
        );
        
        // Must NOT write --listen to /etc/default/libvirtd
        assert!(
            !content.contains(r#"LIBVIRTD_ARGS="--listen""#),
            "Bootstrap must NOT set LIBVIRTD_ARGS=\"--listen\" (deprecated)"
        );
    }

    /// Test socket activation helper
    #[test]
    fn test_socket_activation_in_docs() {
        // Check that documentation mentions socket activation
        let readme = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("README.md");
        
        if readme.exists() {
            let content = fs::read_to_string(&readme)
                .expect("Could not read README.md");
            
            // Should mention socket activation or libvirtd-tls.socket
            let has_socket_docs = content.contains("socket activation") 
                || content.contains("libvirtd-tls.socket")
                || content.contains("systemd socket");
            
            if !has_socket_docs {
                eprintln!(
                    "⚠️  WARNING: README.md should document socket activation\n\
                    Consider adding: 'libvirtd uses systemd socket activation (libvirtd-tls.socket)'"
                );
            }
        }
    }
}
