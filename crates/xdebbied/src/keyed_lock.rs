// SPDX-License-Identifier: GPL-3.0-or-later
//! A mutex per key, made on demand and forgotten when nobody holds or
//! waits for it. Used to serialize volume creates by name and base-volume
//! filling by `(abe, image)` without one global lock for all of them.

use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex},
};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

pub struct KeyedLock<K> {
    inner: Mutex<HashMap<K, Arc<AsyncMutex<()>>>>,
}

impl<K> Default for KeyedLock<K> {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

/// Held while the key is locked; dropping it unlocks and, when no one else
/// holds or waits on the key, removes the map entry.
pub struct KeyedGuard<'a, K: Eq + Hash + Clone> {
    owner: &'a KeyedLock<K>,
    key: K,
    slot: Arc<AsyncMutex<()>>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl<K: Eq + Hash + Clone> KeyedLock<K> {
    /// Wait for `k` and hold it until the guard drops.
    pub async fn lock(&self, k: K) -> KeyedGuard<'_, K> {
        let slot = {
            let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(k.clone()).or_default().clone()
        };
        let guard = slot.clone().lock_owned().await;
        KeyedGuard {
            owner: self,
            key: k,
            slot,
            guard: Some(guard),
        }
    }

    /// Keys with a holder or a waiter right now.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K: Eq + Hash + Clone> Drop for KeyedGuard<'_, K> {
    fn drop(&mut self) {
        // Under the map lock so a `lock()` that is about to clone the slot
        // cannot slip in between the count check and the removal. Waiters
        // hold their own clone of the Arc (count > 2), so the entry stays
        // while anyone is queued.
        let mut map = self.owner.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.guard.take();
        if Arc::strong_count(&self.slot) == 2 {
            map.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn same_key_serializes_and_the_map_empties_afterwards() {
        let l = Arc::new(KeyedLock::<String>::default());
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = vec![];
        for i in 0..2 {
            let (l, order) = (l.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let _g = l.lock("k".to_string()).await;
                order.lock().unwrap().push(format!("start{i}"));
                tokio::time::sleep(Duration::from_millis(30)).await;
                order.lock().unwrap().push(format!("end{i}"));
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let o = order.lock().unwrap().clone();
        // whichever went first finished before the other started
        assert!(
            o[0].starts_with("start") && o[1].starts_with("end"),
            "{o:?}"
        );
        assert_eq!(&o[0][5..], &o[1][3..]);
        assert!(l.is_empty(), "entry removed once nobody holds it");
    }

    #[tokio::test]
    async fn different_keys_run_in_parallel() {
        let l = Arc::new(KeyedLock::<(String, String)>::default());
        let started = Arc::new(tokio::sync::Barrier::new(2));
        let mut tasks = vec![];
        for k in ["a", "b"] {
            let (l, started) = (l.clone(), started.clone());
            tasks.push(tokio::spawn(async move {
                let _g = l.lock(("s".to_string(), k.to_string())).await;
                // both must get here while holding their key, or this hangs
                tokio::time::timeout(Duration::from_secs(2), started.wait())
                    .await
                    .expect("the other key was not blocked")
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(l.len(), 0);
    }

    #[tokio::test]
    async fn a_waiter_keeps_the_entry_alive() {
        let l = Arc::new(KeyedLock::<u32>::default());
        let g = l.lock(1).await;
        let l2 = l.clone();
        let waiter = tokio::spawn(async move {
            let _g = l2.lock(1).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(l.len(), 1);
        drop(g);
        waiter.await.unwrap();
        assert_eq!(l.len(), 0);
    }
}
