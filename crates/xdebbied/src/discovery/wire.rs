// SPDX-License-Identifier: GPL-3.0-or-later
//! Signed discovery datagrams (docs/autodiscovery.md, "Authentication").
//!
//! Multicast is open to anyone on the LAN, so a box that holds the cluster
//! token signs what it sends and drops what does not verify. Without that,
//! one forged heartbeat with a high score makes the real leader step down
//! and points every follower's forwarding at an address of the sender's
//! choosing.
//!
//! A signed datagram is a JSON envelope around the plain message:
//!
//! ```json
//! {"v":1,"ts":1763400000,"msg":"{\"type\":\"HEARTBEAT\",…}","mac":"<64 hex>"}
//! ```
//!
//! `msg` is the message's JSON as a string, so the MAC covers exact bytes
//! and nothing depends on how a JSON library orders keys. The key is
//! derived from a cluster token, never the token itself:
//!
//! ```text
//! key = HMAC-SHA256(token, "xdebbie discovery v1")
//! mac = HMAC-SHA256(key,   "v1" || ts as 8 big-endian bytes || msg)
//! ```
//!
//! `ts` (Unix seconds) bounds replay: a datagram more than [`MAX_SKEW`]
//! from the receiver's clock is dropped, so boxes need their clocks within
//! that of each other (NTP).

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::messages::DiscoveryMessage;

type HmacSha256 = Hmac<Sha256>;

const VERSION: u8 = 1;
const KEY_LABEL: &[u8] = b"xdebbie discovery v1";

/// How far a datagram's timestamp may be from the receiver's clock.
pub const MAX_SKEW: Duration = Duration::from_secs(30);

/// A discovery signing key, derived from one cluster token.
#[derive(Clone)]
pub struct Key([u8; 32]);

impl Key {
    pub fn from_token(token: &str) -> Self {
        let mut m =
            HmacSha256::new_from_slice(token.as_bytes()).expect("HMAC takes any key length");
        m.update(KEY_LABEL);
        Self(m.finalize().into_bytes().into())
    }

    fn mac(&self, ts: u64, msg: &str) -> HmacSha256 {
        let mut m = HmacSha256::new_from_slice(&self.0).expect("HMAC takes any key length");
        m.update(b"v1");
        m.update(&ts.to_be_bytes());
        m.update(msg.as_bytes());
        m
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u8,
    ts: u64,
    msg: String,
    mac: String,
}

/// What a received datagram turned out to be.
#[derive(Debug)]
pub enum Opened {
    /// Signed with one of our keys, within the clock window.
    Verified(DiscoveryMessage),
    /// A plain message (a daemon before signing, or one without a token).
    Unsigned(DiscoveryMessage),
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bytes to send: signed with `key`, or the plain message without one.
pub fn seal(msg: &DiscoveryMessage, key: Option<&Key>) -> Result<Vec<u8>> {
    let json = msg.to_json()?;
    let Some(key) = key else {
        return Ok(json.into_bytes());
    };
    seal_at(json, key, now())
}

fn seal_at(msg: String, key: &Key, ts: u64) -> Result<Vec<u8>> {
    let mac = hex::encode(key.mac(ts, &msg).finalize().into_bytes());
    Ok(serde_json::to_vec(&Envelope {
        v: VERSION,
        ts,
        msg,
        mac,
    })?)
}

/// Check a received datagram against `keys` (current token first, then a
/// previous one during a rotation). An envelope that fails verification is
/// an error; a plain message comes back as [`Opened::Unsigned`] and the
/// caller decides whether it is welcome.
pub fn open(bytes: &[u8], keys: &[Key]) -> Result<Opened> {
    open_at(bytes, keys, now())
}

fn open_at(bytes: &[u8], keys: &[Key], now: u64) -> Result<Opened> {
    let Some(env) = envelope(bytes)? else {
        return Ok(Opened::Unsigned(plain(bytes)?));
    };
    if env.v != VERSION {
        bail!("unknown envelope version {}", env.v);
    }
    let mac = hex::decode(&env.mac).context("mac is not hex")?;
    // verify_slice compares in constant time.
    if !keys
        .iter()
        .any(|k| k.mac(env.ts, &env.msg).verify_slice(&mac).is_ok())
    {
        bail!("bad signature (a different cluster token, or forged)");
    }
    if now.abs_diff(env.ts) > MAX_SKEW.as_secs() {
        bail!(
            "timestamp {}s from our clock (allowed {}s): replayed, or clocks out of sync",
            now as i64 - env.ts as i64,
            MAX_SKEW.as_secs()
        );
    }
    Ok(Opened::Verified(
        DiscoveryMessage::from_json(&env.msg).context("signed message")?,
    ))
}

/// The message inside, verified or not. For `xdebbied bootstrap`, which
/// listens before it holds a token only to offer the operator a URL to join;
/// the join itself is what has to be trusted.
pub fn peek(bytes: &[u8]) -> Result<DiscoveryMessage> {
    match envelope(bytes)? {
        Some(env) => DiscoveryMessage::from_json(&env.msg),
        None => plain(bytes),
    }
}

fn envelope(bytes: &[u8]) -> Result<Option<Envelope>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).context("not JSON")?;
    if v.get("mac").is_some() && v.get("type").is_none() {
        Ok(Some(
            serde_json::from_value(v).context("malformed envelope")?,
        ))
    } else {
        Ok(None)
    }
}

fn plain(bytes: &[u8]) -> Result<DiscoveryMessage> {
    DiscoveryMessage::from_json(std::str::from_utf8(bytes).context("not UTF-8")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn hb() -> DiscoveryMessage {
        DiscoveryMessage::Heartbeat {
            node_id: Uuid::new_v4(),
            score: 4321.0,
            is_leader: true,
            sequence: 7,
        }
    }

    fn signed(key: &Key, ts: u64) -> Vec<u8> {
        seal_at(hb().to_json().unwrap(), key, ts).unwrap()
    }

    #[test]
    fn round_trip() {
        let k = Key::from_token("tok");
        let b = seal(&hb(), Some(&k)).unwrap();
        assert!(matches!(open(&b, &[k]).unwrap(), Opened::Verified(_)));
    }

    #[test]
    fn wrong_token_rejected() {
        let b = signed(&Key::from_token("theirs"), 1000);
        let e = open_at(&b, &[Key::from_token("ours")], 1000).unwrap_err();
        assert!(e.to_string().contains("bad signature"), "{e}");
    }

    #[test]
    fn previous_token_still_verifies() {
        let old = Key::from_token("old");
        let b = signed(&old, 1000);
        let held = [Key::from_token("new"), old];
        assert!(matches!(
            open_at(&b, &held, 1000).unwrap(),
            Opened::Verified(_)
        ));
    }

    #[test]
    fn tampered_message_rejected() {
        let k = Key::from_token("tok");
        let b = String::from_utf8(signed(&k, 1000)).unwrap();
        let forged = b.replace("4321", "9999");
        assert_ne!(b, forged);
        assert!(open_at(forged.as_bytes(), &[k], 1000).is_err());
    }

    #[test]
    fn tampered_timestamp_rejected() {
        let k = Key::from_token("tok");
        let b = String::from_utf8(signed(&k, 1000)).unwrap();
        let forged = b.replace("\"ts\":1000", "\"ts\":1001");
        assert_ne!(b, forged);
        assert!(open_at(forged.as_bytes(), &[k], 1001).is_err());
    }

    #[test]
    fn stale_or_future_rejected() {
        let k = Key::from_token("tok");
        let skew = MAX_SKEW.as_secs();
        assert!(open_at(&signed(&k, 1000), std::slice::from_ref(&k), 1000 + skew).is_ok());
        assert!(open_at(&signed(&k, 1000), std::slice::from_ref(&k), 1000 + skew + 1).is_err());
        assert!(open_at(&signed(&k, 1000 + skew + 1), &[k], 1000).is_err());
    }

    #[test]
    fn plain_message_is_unsigned() {
        let b = hb().to_json().unwrap().into_bytes();
        assert!(matches!(
            open(&b, &[Key::from_token("tok")]).unwrap(),
            Opened::Unsigned(_)
        ));
        assert!(matches!(open(&b, &[]).unwrap(), Opened::Unsigned(_)));
    }

    #[test]
    fn no_keys_rejects_envelopes() {
        let b = signed(&Key::from_token("tok"), now());
        assert!(open(&b, &[]).is_err());
    }

    #[test]
    fn peek_reads_either() {
        let k = Key::from_token("tok");
        assert!(matches!(
            peek(&signed(&k, 1)).unwrap(),
            DiscoveryMessage::Heartbeat { .. }
        ));
        let b = hb().to_json().unwrap().into_bytes();
        assert!(matches!(
            peek(&b).unwrap(),
            DiscoveryMessage::Heartbeat { .. }
        ));
    }

    #[test]
    fn key_is_not_the_token() {
        let k = Key::from_token("tok");
        assert_ne!(&k.0[..3], b"tok");
        assert_eq!(format!("{k:?}"), "Key(..)");
    }
}
