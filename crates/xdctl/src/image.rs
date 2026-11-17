// SPDX-License-Identifier: GPL-3.0-or-later
//! `xdctl image import`: put a disk image into this host's thin pool as
//! `base-<name>`, the LV xdebbied clones or copies root disks from.
//!
//! Local to the box it runs on and independent of xdebbied: it shells out
//! to LVM, qemu-img and virt-sparsify, so it needs root. Only the written
//! parts of the image are copied, and the filesystems inside are then
//! trimmed on the LV, so the base takes no more pool space than its files.
//! The source file is never modified.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};

/// Where xdebbied keeps `volume_group` and `thin_pool`.
pub const CONFIG: &str = "/etc/xdebbie/xdebbied.toml";

pub struct Import {
    pub file: String,
    pub name: Option<String>,
    pub format: String,
    pub vg: Option<String>,
    pub pool: Option<String>,
    pub config: String,
    pub force: bool,
    pub no_sparsify: bool,
}

/// Image name for a file: its stem, without a leading `base-`
/// (`/x/debian-13-k8s-v1.36.raw` → `debian-13-k8s-v1.36`).
pub fn image_name(file: &str) -> Option<String> {
    let base = Path::new(file).file_name()?.to_str()?;
    let stem = ["raw", "img", "qcow2"]
        .iter()
        .find_map(|ext| base.strip_suffix(&format!(".{ext}")))
        .unwrap_or(base);
    let stem = stem.strip_prefix("base-").unwrap_or(stem);
    (!stem.is_empty()).then(|| stem.to_string())
}

/// LVM accepts `[A-Za-z0-9+_.-]` and no leading hyphen; the result also
/// goes into `/dev/<vg>/base-<name>`, so nothing else is let through.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+_.-".contains(c))
}

/// A top-level `key = "value"` of xdebbied.toml. Stops at the first table
/// header, so a same-named key in a section is never returned.
pub fn toml_top_level(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            return None;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.split('#').next().unwrap_or("").trim();
        let v = v.strip_prefix('"')?.strip_suffix('"')?;
        return (!v.is_empty()).then(|| v.to_string());
    }
    None
}

fn bin(name: &str) -> Option<String> {
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
        .iter()
        .map(|d| format!("{d}/{name}"))
        .find(|p| Path::new(p).exists())
}

fn need(name: &str, package: &str) -> Result<String> {
    bin(name).with_context(|| format!("{name} not found (apt install {package})"))
}

/// Run to completion, returning stdout; stderr goes into the error.
fn capture(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run with the terminal attached, for commands that draw their own progress.
fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", args.join(" "));
    }
    Ok(())
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// `qemu-img info` on `file`; with `format` the probe is skipped.
fn image_info(qemu_img: &str, file: &str, format: Option<&str>) -> Result<serde_json::Value> {
    let mut args = vec!["info", "--output=json"];
    if let Some(f) = format {
        args.extend(["-f", f]);
    }
    args.push(file);
    serde_json::from_str(&capture(qemu_img, &args)?).context("parsing qemu-img info")
}

pub fn import(i: Import) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("image import creates and writes an LV on this host: run it with sudo");
    }
    let name = match &i.name {
        Some(n) => n.strip_prefix("base-").unwrap_or(n).to_string(),
        None => image_name(&i.file)
            .with_context(|| format!("no image name in {:?}: pass --name", i.file))?,
    };
    if !valid_name(&name) {
        bail!("image name {name:?}: use letters, digits and + _ . - (pass --name)");
    }
    let config = std::fs::read_to_string(&i.config).unwrap_or_default();
    let vg =
        i.vg.or_else(|| toml_top_level(&config, "volume_group"))
            .with_context(|| format!("no volume_group in {}: pass --vg", i.config))?;
    let pool = i
        .pool
        .or_else(|| toml_top_level(&config, "thin_pool"))
        .with_context(|| format!("no thin_pool in {}: pass --pool", i.config))?;

    let qemu_img = need("qemu-img", "qemu-utils")?;
    let lvs = need("lvs", "lvm2")?;
    let lvcreate = need("lvcreate", "lvm2")?;
    let lvremove = need("lvremove", "lvm2")?;
    let sparsify = if i.no_sparsify {
        None
    } else {
        Some(need("virt-sparsify", "guestfs-tools").context("or pass --no-sparsify")?)
    };

    // The format is stated, never probed, for the copy: a raw image whose
    // first bytes look like a qcow2 header must not be opened as qcow2. The
    // probe is only used to catch the wrong --format before anything exists.
    let probed = image_info(&qemu_img, &i.file, None)?;
    let looks = probed["format"].as_str().unwrap_or("raw");
    if looks != i.format {
        bail!(
            "{} looks like {looks}, not {}: pass --format {looks} if that is what it is",
            i.file,
            i.format
        );
    }
    let bytes = image_info(&qemu_img, &i.file, Some(&i.format))?["virtual-size"]
        .as_u64()
        .context("qemu-img info reported no virtual-size")?;
    if bytes == 0 {
        bail!("{} is empty", i.file);
    }

    let lv = format!("base-{name}");
    let vg_lv = format!("{vg}/{lv}");
    let dev = format!("/dev/{vg}/{lv}");
    let exists = Command::new(&lvs)
        .arg(&vg_lv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("running lvs")?
        .success();
    if exists {
        if !i.force {
            bail!("{vg_lv} already exists: pass --force to replace it");
        }
        println!("removing existing {vg_lv}");
        capture(&lvremove, &["-f", &vg_lv])?;
    }

    println!(
        "creating {vg_lv} in thin pool {pool} ({} virtual)",
        gib(bytes)
    );
    capture(
        &lvcreate,
        &[
            "--yes",
            "-V",
            &format!("{bytes}b"),
            "-T",
            &format!("{vg}/{pool}"),
            "-n",
            &lv,
        ],
    )?;

    let fill = || -> Result<()> {
        // A new thin LV reads as zeros, so zero runs of the image are
        // skipped and never allocated.
        println!("copying {}", i.file);
        run(
            &qemu_img,
            &[
                "convert",
                "-p",
                "-n",
                "-f",
                &i.format,
                "-O",
                "raw",
                "--target-is-zero",
                "-S",
                "4k",
                &i.file,
                &dev,
            ],
        )?;
        if let Some(sparsify) = &sparsify {
            // Discards the filesystems' free space on the LV, which hands
            // deleted-but-not-zeroed blocks back to the pool.
            println!("trimming free space");
            run(sparsify, &["--in-place", &dev])?;
        }
        Ok(())
    };
    if let Err(e) = fill() {
        let _ = capture(&lvremove, &["-f", &vg_lv]);
        return Err(e.context(format!("import failed, {vg_lv} removed")));
    }

    let used = capture(&lvs, &["--noheadings", "-o", "data_percent", &vg_lv])
        .ok()
        .and_then(|s| s.trim().replace(',', ".").parse::<f64>().ok())
        .map(|pct| format!(", {} allocated", gib((bytes as f64 * pct / 100.0) as u64)))
        .unwrap_or_default();
    println!("imported {name} as {dev} ({} virtual{used})", gib(bytes));
    if exists {
        println!(
            "note: an ABE base already sealed from image {name:?} keeps the old contents; \
             see docs/storage.md, \"Base volumes\""
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_name_is_the_stem_without_base_prefix() {
        assert_eq!(
            image_name("/home/x/images/debian-13-k8s-v1.36.raw").as_deref(),
            Some("debian-13-k8s-v1.36")
        );
        assert_eq!(
            image_name("base-debian-13.qcow2").as_deref(),
            Some("debian-13")
        );
        assert_eq!(image_name("debian-13.img").as_deref(), Some("debian-13"));
        // A version's dot is not an extension.
        assert_eq!(image_name("k8s-v1.36").as_deref(), Some("k8s-v1.36"));
        assert_eq!(image_name(".raw"), None);
        assert_eq!(image_name("/"), None);
    }

    #[test]
    fn valid_name_rejects_what_lvm_and_paths_would_not_take() {
        assert!(valid_name("debian-13-k8s-v1.36"));
        assert!(valid_name("a+b_c"));
        assert!(!valid_name(""));
        assert!(!valid_name("-leading"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a b"));
        assert!(!valid_name(&"x".repeat(101)));
    }

    #[test]
    fn toml_top_level_reads_only_keys_above_the_first_table() {
        let text = r#"
# comment
listen = "0.0.0.0:23443"
volume_group = "timesink-vg"   # the VG
thin_pool="abe"
reconcile_interval_secs = 5

[network]
thin_pool = "wrong"
late = "x"
"#;
        assert_eq!(
            toml_top_level(text, "volume_group").as_deref(),
            Some("timesink-vg")
        );
        assert_eq!(toml_top_level(text, "thin_pool").as_deref(), Some("abe"));
        assert_eq!(toml_top_level(text, "late"), None);
        assert_eq!(toml_top_level(text, "missing"), None);
        // Not a string, or commented out: not a value.
        assert_eq!(toml_top_level(text, "reconcile_interval_secs"), None);
        assert_eq!(
            toml_top_level("# volume_group = \"x\"\n", "volume_group"),
            None
        );
        assert_eq!(
            toml_top_level("volume_group = \"\"\n", "volume_group"),
            None
        );
    }
}
