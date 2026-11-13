// SPDX-License-Identifier: GPL-3.0-or-later
//! API bearer tokens and their rotation.
//!
//! A box holds a *current* token and, while a rotation is in flight, the
//! *previous* one; both authenticate. `POST /v1/token/rotate` on the
//! leader mints a new current and keeps the old as previous. The leader's
//! [`Distributor`] then pushes the pair to every peer (`PUT /v1/token`,
//! authenticated with the token the peer still knows) and confirms each
//! one with `GET /v1/token` *using the new token* — a peer is "in use" once
//! that succeeds. When every peer is confirmed the previous token is
//! retired everywhere, peers first, leader last. A leader restart resumes
//! from the persisted pair; a peer that only appears later is brought up to
//! date by the same loop.
//!
//! On disk: `token_file` holds the current token (so everything that read
//! it before keeps working), `<token_file>.previous` exists only during a
//! rotation, and `export XDEBBIE_TOKEN=` in the sibling `xdebbie_env` is
//! rewritten so a fresh `source` picks up the new token.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tracing::{info, warn};
use xdebbie_core::{LeaderRef, TokenNode, TokenSet, TokenStatus};

use crate::leader::Leadership;

/// While a rotation is in flight; otherwise a slow sweep catches peers that
/// missed it or still hold a retired token.
const ROTATING_INTERVAL: Duration = Duration::from_secs(3);
const STEADY_INTERVAL: Duration = Duration::from_secs(60);

/// First 12 hex of SHA-256: enough to tell tokens apart in status output
/// without showing them.
pub fn fingerprint(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))[..12].to_string()
}

pub struct Tokens {
    /// `None`: in-memory only (tests).
    path: Option<PathBuf>,
    set: RwLock<TokenSet>,
    current_used: AtomicBool,
    /// Leader-side view of the peers, for `GET /v1/token`.
    nodes: RwLock<Vec<TokenNode>>,
}

impl Tokens {
    /// Read `token_file` and, if present, `token_file.previous`.
    pub fn load(path: &Path) -> Result<Self> {
        let current = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?
            .trim()
            .to_owned();
        let previous = match std::fs::read_to_string(previous_path(path)) {
            Ok(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            _ => None,
        };
        if previous.is_some() {
            info!("token rotation in progress (previous token still accepted)");
        }
        Ok(Self::with(
            Some(path.to_path_buf()),
            TokenSet { current, previous },
        ))
    }

    #[cfg(test)]
    pub fn in_memory(set: TokenSet) -> Self {
        Self::with(None, set)
    }

    fn with(path: Option<PathBuf>, set: TokenSet) -> Self {
        Self {
            path,
            set: RwLock::new(set),
            current_used: AtomicBool::new(false),
            nodes: RwLock::new(vec![]),
        }
    }

    /// Does `presented` authenticate? Constant-time against each held token.
    pub async fn accepts(&self, presented: &str) -> bool {
        let set = self.set.read().await;
        if constant_time_eq(presented.as_bytes(), set.current.as_bytes()) {
            self.current_used.store(true, Ordering::Relaxed);
            return true;
        }
        set.previous
            .as_deref()
            .is_some_and(|p| constant_time_eq(presented.as_bytes(), p.as_bytes()))
    }

    pub async fn get(&self) -> TokenSet {
        self.set.read().await.clone()
    }

    pub async fn rotating(&self) -> bool {
        self.set.read().await.previous.is_some()
    }

    /// Replace the held set (what the leader pushes). Returns whether it changed.
    pub async fn install(&self, next: TokenSet) -> Result<bool> {
        let mut set = self.set.write().await;
        if *set == next {
            return Ok(false);
        }
        self.persist(&next)?;
        if next.current != set.current {
            self.current_used.store(false, Ordering::Relaxed);
        }
        info!(
            id = %fingerprint(&next.current),
            previous = next.previous.as_deref().map(fingerprint).unwrap_or_default(),
            "token set installed"
        );
        *set = next;
        Ok(true)
    }

    /// Mint a new current token; the old one stays accepted as previous.
    pub async fn rotate(&self) -> Result<String> {
        let token = crate::bootstrap::token::generate_token()?;
        let mut set = self.set.write().await;
        let next = TokenSet {
            current: token.clone(),
            previous: Some(set.current.clone()),
        };
        self.persist(&next)?;
        self.current_used.store(false, Ordering::Relaxed);
        info!(id = %fingerprint(&next.current), previous = %fingerprint(&set.current), "token rotated");
        *set = next;
        Ok(token)
    }

    /// Stop accepting the previous token.
    pub async fn retire_previous(&self) -> Result<()> {
        let mut set = self.set.write().await;
        if set.previous.is_none() {
            return Ok(());
        }
        let next = TokenSet {
            current: set.current.clone(),
            previous: None,
        };
        self.persist(&next)?;
        info!(retired = %fingerprint(set.previous.as_deref().unwrap_or_default()), "previous token retired");
        *set = next;
        Ok(())
    }

    pub async fn status(&self) -> TokenStatus {
        let set = self.set.read().await;
        TokenStatus {
            id: fingerprint(&set.current),
            previous: set.previous.as_deref().map(fingerprint),
            current_used: self.current_used.load(Ordering::Relaxed),
            rotating: set.previous.is_some(),
            nodes: self.nodes.read().await.clone(),
        }
    }

    fn persist(&self, set: &TokenSet) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        write_secret(path, &set.current)?;
        let prev = previous_path(path);
        match &set.previous {
            Some(p) => write_secret(&prev, p)?,
            None => {
                if let Err(e) = std::fs::remove_file(&prev)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(e).with_context(|| format!("removing {}", prev.display()));
                }
            }
        }
        if let Some(env) = path
            .parent()
            .map(|d| d.join(crate::bootstrap::DEFAULT_ENV_FILE))
            && env.exists()
            && let Err(e) = rewrite_env_token(&env, &set.current)
        {
            warn!(error = %e, "could not update {}", env.display());
        }
        Ok(())
    }
}

fn previous_path(token_file: &Path) -> PathBuf {
    let mut p = token_file.as_os_str().to_owned();
    p.push(".previous");
    PathBuf::from(p)
}

/// Atomic write, mode 0600.
/// One secret on its own line.
fn write_secret(path: &Path, value: &str) -> Result<()> {
    write_private(path, &format!("{value}\n"))
}

/// Mode of token files: root:root 0400.
/// The daemon and bootstrap run as root and replace files by rename, so
/// nothing needs write permission.
pub const SECRET_MODE: u32 = 0o400;

/// Mode of operator-readable files (config, env): root:xdebbie 0440.
/// Readable by any member of the xdebbie group without sudo.
pub const GROUP_MODE: u32 = 0o440;

/// Return the GID of the `xdebbie` group, or `None` if it does not exist.
pub fn xdebbie_gid() -> Option<u32> {
    let name = match std::ffi::CString::new("xdebbie") {
        Ok(n) => n,
        Err(_) => return None,
    };
    // SAFETY: getgrnam reads /etc/group and returns a pointer to static
    // storage. We copy the gid immediately and do not retain the pointer.
    let grp = unsafe { libc::getgrnam(name.as_ptr()) };
    if grp.is_null() {
        None
    } else {
        Some(unsafe { (*grp).gr_gid })
    }
}

/// Write `content` to `path` atomically as root:root 0400: a temp file in
/// the same directory gets the mode before any byte lands, then replaces
/// `path` by rename. Used for the token files.
pub fn write_private(path: &Path, content: &str) -> Result<()> {
    write_atomic(path, content, SECRET_MODE, 0, 0)
}

/// Write `content` to `path` atomically as root:xdebbie 0440.
/// Falls back to root:root 0400 if the xdebbie group does not exist yet.
/// Used for `xdebbied.toml` and `xdebbie_env`.
pub fn write_group_readable(path: &Path, content: &str) -> Result<()> {
    let gid = xdebbie_gid().unwrap_or(0);
    let mode = if gid != 0 { GROUP_MODE } else { SECRET_MODE };
    write_atomic(path, content, mode, 0, gid)
}

/// Atomic write with explicit mode and ownership.
/// Writes a temp file in the same directory, sets mode/owner, then renames.
fn write_atomic(path: &Path, content: &str, mode: u32, uid: u32, gid: u32) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut f = tempfile::Builder::new()
        .prefix(".xdebbie")
        .tempfile_in(dir)
        .with_context(|| format!("temp file next to {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))?;
        if nix_is_root() {
            std::os::unix::fs::fchown(f.as_file(), Some(uid), Some(gid))
                .with_context(|| format!("chown {}:{} {}", uid, gid, path.display()))?;
        }
    }
    f.write_all(content.as_bytes())?;
    f.as_file().sync_all()?;
    f.persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn nix_is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

fn rewrite_env_token(env: &Path, token: &str) -> Result<()> {
    let text = std::fs::read_to_string(env)?;
    let mut found = false;
    let out: String = text
        .lines()
        .map(|l| {
            if l.starts_with("export XDEBBIE_TOKEN=") {
                found = true;
                format!("export XDEBBIE_TOKEN='{token}'")
            } else {
                l.to_string()
            }
        })
        .map(|l| l + "\n")
        .collect();
    if !found {
        return Ok(());
    }
    // The env file is group-readable (root:xdebbie 0440); preserve that on
    // every token rotation rather than reverting to root:root 0400.
    write_group_readable(env, &out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------- distribution (leader) ----------

/// Keeps every peer on the leader's token set. Runs on every box; acts
/// only while this box leads.
pub struct Distributor {
    pub tokens: Arc<Tokens>,
    pub leadership: Leadership,
    pub http: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Seen {
    /// Accepts the current token; `overlap` says it still holds a previous one.
    Current {
        overlap: bool,
    },
    Installing,
    Stale,
    Unreachable(String),
}

impl Seen {
    fn label(&self) -> String {
        match self {
            Seen::Current { overlap: false } => "current".into(),
            Seen::Current { overlap: true } => "overlap".into(),
            Seen::Installing => "installing".into(),
            Seen::Stale => "stale".into(),
            Seen::Unreachable(e) => format!("unreachable: {e}"),
        }
    }
}

impl Distributor {
    pub async fn run(self) {
        let mut last: Option<BTreeMap<String, Seen>> = None;
        let mut swept: Option<std::time::Instant> = None;
        loop {
            tokio::time::sleep(ROTATING_INTERVAL).await;
            if !self.leadership.is_leader().await {
                last = None;
                continue;
            }
            if !self.tokens.rotating().await && swept.is_some_and(|t| t.elapsed() < STEADY_INTERVAL)
            {
                continue;
            }
            let seen = self.sweep().await;
            swept = Some(std::time::Instant::now());
            if last.as_ref() != Some(&seen) {
                for (url, s) in &seen {
                    info!(peer = %url, state = %s.label(), "token: peer");
                }
                last = Some(seen);
            }
        }
    }

    /// One pass over the peers: install where needed, retire when everyone
    /// is confirmed on the current token.
    pub(crate) async fn sweep(&self) -> BTreeMap<String, Seen> {
        let set = self.tokens.get().await;
        let peers = self.leadership.peers().await;
        let mut seen = BTreeMap::new();
        let mut nodes = vec![];
        for p in &peers {
            let s = self.bring_up_to_date(p, &set).await;
            nodes.push(TokenNode {
                hostname: p.hostname.clone(),
                api_url: p.api_url.clone(),
                state: s.label(),
            });
            seen.insert(p.api_url.clone(), s);
        }
        *self.tokens.nodes.write().await = nodes;

        if set.previous.is_some() && seen.values().all(|s| matches!(s, Seen::Current { .. })) {
            // Everyone takes the new token: drop the old one, peers first so
            // the leader can still reach a peer that fails here with either.
            let retired = TokenSet {
                current: set.current.clone(),
                previous: None,
            };
            let mut all = true;
            for p in &peers {
                if let Err(e) = self.put(p, &retired, &set.current).await {
                    warn!(peer = %p.api_url, error = %e, "token: could not retire previous token");
                    all = false;
                }
            }
            if all {
                match self.tokens.retire_previous().await {
                    Ok(()) => info!(peers = peers.len(), "token rotation complete"),
                    Err(e) => warn!(error = %e, "token: retiring previous token locally"),
                }
            }
        }
        seen
    }

    async fn bring_up_to_date(&self, p: &LeaderRef, set: &TokenSet) -> Seen {
        match self.get(p, &set.current).await {
            // Answered, but not with our token: it let us in on our client
            // certificate (a box that enrolled with its own bootstrap token,
            // or missed a rotation). Install ours.
            Ok(st) if st.id != fingerprint(&set.current) => match self.put(p, set, &set.current).await {
                Ok(()) => Seen::Installing,
                Err(Reach::Unauthorized) => Seen::Stale,
                Err(Reach::Other(e)) => Seen::Unreachable(e),
            },
            Ok(st) => {
                // Holds a retired token after a rotation finished: tidy it.
                if st.previous.is_some() && set.previous.is_none() {
                    let retired = TokenSet {
                        current: set.current.clone(),
                        previous: None,
                    };
                    if let Err(e) = self.put(p, &retired, &set.current).await {
                        warn!(peer = %p.api_url, error = %e, "token: could not retire previous token");
                    }
                }
                Seen::Current {
                    overlap: st.previous.is_some(),
                }
            }
            Err(Reach::Unauthorized) => {
                let Some(prev) = &set.previous else {
                    return Seen::Stale;
                };
                match self.put(p, set, prev).await {
                    Ok(()) => Seen::Installing,
                    Err(Reach::Unauthorized) => Seen::Stale,
                    Err(Reach::Other(e)) => Seen::Unreachable(e),
                }
            }
            Err(Reach::Other(e)) => Seen::Unreachable(e),
        }
    }

    async fn get(&self, p: &LeaderRef, bearer: &str) -> std::result::Result<TokenStatus, Reach> {
        let r = self
            .http
            .get(format!("{}/v1/token", p.api_url.trim_end_matches('/')))
            .bearer_auth(bearer)
            .send()
            .await
            .map_err(|e| Reach::Other(e.to_string()))?;
        match r.status() {
            s if s.is_success() => r.json().await.map_err(|e| Reach::Other(e.to_string())),
            reqwest::StatusCode::UNAUTHORIZED => Err(Reach::Unauthorized),
            s => Err(Reach::Other(format!("{s}"))),
        }
    }

    async fn put(
        &self,
        p: &LeaderRef,
        set: &TokenSet,
        bearer: &str,
    ) -> std::result::Result<(), Reach> {
        let r = self
            .http
            .put(format!("{}/v1/token", p.api_url.trim_end_matches('/')))
            .bearer_auth(bearer)
            .json(set)
            .send()
            .await
            .map_err(|e| Reach::Other(e.to_string()))?;
        match r.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::UNAUTHORIZED => Err(Reach::Unauthorized),
            s => Err(Reach::Other(format!("{s}"))),
        }
    }
}

enum Reach {
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reach::Unauthorized => f.write_str("401 unauthorized"),
            Reach::Other(e) => f.write_str(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepts_current_and_previous_until_retired() {
        let t = Tokens::in_memory(TokenSet {
            current: "a".into(),
            previous: None,
        });
        assert!(t.accepts("a").await);
        assert!(!t.accepts("b").await);
        assert!(t.status().await.current_used);

        let b = t.rotate().await.unwrap();
        assert!(t.accepts("a").await, "old token still works during overlap");
        assert!(t.accepts(&b).await);
        let st = t.status().await;
        assert!(st.rotating);
        assert_eq!(st.previous, Some(fingerprint("a")));
        assert!(st.current_used, "the new token has been used");

        t.retire_previous().await.unwrap();
        assert!(!t.accepts("a").await);
        assert!(t.accepts(&b).await);
        assert!(!t.status().await.rotating);
    }

    #[tokio::test]
    async fn persists_pair_and_env_file() {
        let dir = tempfile::tempdir().unwrap();
        let tf = dir.path().join("token");
        std::fs::write(&tf, "first\n").unwrap();
        let env = dir.path().join("xdebbie_env");
        std::fs::write(
            &env,
            "# env\nexport XDEBBIE_URL=http://x:7443\nexport XDEBBIE_TOKEN='first'\n",
        )
        .unwrap();

        let t = Tokens::load(&tf).unwrap();
        let second = t.rotate().await.unwrap();
        assert_eq!(std::fs::read_to_string(&tf).unwrap().trim(), second);
        assert_eq!(
            std::fs::read_to_string(previous_path(&tf)).unwrap().trim(),
            "first"
        );
        let envtext = std::fs::read_to_string(&env).unwrap();
        assert!(envtext.contains(&format!("export XDEBBIE_TOKEN='{second}'")));
        assert!(envtext.contains("export XDEBBIE_URL=http://x:7443"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [&tf, &tf.with_extension("previous")] {
                assert_eq!(std::fs::metadata(f).unwrap().permissions().mode() & 0o777, SECRET_MODE, "{}", f.display());
            }
            // The env file is group-readable when the xdebbie group exists on
            // the machine running the tests, root-only otherwise.
            let want = if xdebbie_gid().is_some() { GROUP_MODE } else { SECRET_MODE };
            assert_eq!(std::fs::metadata(&env).unwrap().permissions().mode() & 0o777, want, "{}", env.display());
        }

        // A restart resumes the overlap.
        let t2 = Tokens::load(&tf).unwrap();
        assert!(t2.accepts("first").await);
        assert!(t2.rotating().await);

        t2.retire_previous().await.unwrap();
        assert!(!previous_path(&tf).exists());
        assert!(!Tokens::load(&tf).unwrap().accepts("first").await);

        // install() is idempotent and reports change.
        let set = t2.get().await;
        assert!(!t2.install(set.clone()).await.unwrap());
        assert!(
            t2.install(TokenSet {
                current: "third".into(),
                previous: Some(set.current)
            })
            .await
            .unwrap()
        );
        assert!(t2.accepts(&second).await);
        assert!(t2.accepts("third").await);
    }
}
