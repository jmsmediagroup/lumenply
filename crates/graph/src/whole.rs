//! Whole-region results: the complete output of an op that is computed in
//! one piece (a text layer's glyphs, a fill over the canvas, a smart filter
//! stack), kept per content key so its tiles can be served one at a time.
//!
//! A whole result is computed once per content key, normally before the
//! tiles of a render are pulled (see [`crate::Ctx::whole`] and the prepare
//! pass in [`crate::Renderer`]), and never while another thread waits for
//! it: waiting inside rayon's work-stealing threads can deadlock, so two
//! threads that miss the same key at once both compute it and the first to
//! finish is kept.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lumenply_tiles::TileStore;

use crate::key::Key;

struct Entry {
    store: Arc<TileStore>,
    bytes: usize,
    last_use: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<Key, Entry>,
    bytes: usize,
}

/// Whole-region results under a byte budget, least recently used first out.
pub struct WholeCache {
    inner: Mutex<Inner>,
    budget: AtomicUsize,
    clock: AtomicU64,
    computed: AtomicU64,
}

impl Default for WholeCache {
    fn default() -> Self {
        WholeCache::with_budget(1 << 30)
    }
}

impl WholeCache {
    pub fn with_budget(bytes: usize) -> Self {
        WholeCache {
            inner: Mutex::new(Inner::default()),
            budget: AtomicUsize::new(bytes),
            clock: AtomicU64::new(0),
            computed: AtomicU64::new(0),
        }
    }

    pub fn set_budget(&self, bytes: usize) {
        self.budget.store(bytes, Ordering::Relaxed);
        self.evict(None);
    }

    /// The kept result for `key`, if any (counts as a use).
    pub fn get(&self, key: Key) -> Option<Arc<TileStore>> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        let e = inner.entries.get_mut(&key)?;
        e.last_use = now;
        Some(e.store.clone())
    }

    pub fn contains(&self, key: Key) -> bool {
        self.inner.lock().unwrap().entries.contains_key(&key)
    }

    /// The result for `key`, computing it with `f` when it isn't kept.
    pub fn get_or_compute(&self, key: Key, f: impl FnOnce() -> TileStore) -> Arc<TileStore> {
        if let Some(s) = self.get(key) {
            return s;
        }
        let store = Arc::new(f());
        self.computed.fetch_add(1, Ordering::Relaxed);
        self.insert(key, store)
    }

    /// Keep `store` under `key` and return what is kept (an earlier result
    /// for the key wins: equal keys mean equal pixels).
    fn insert(&self, key: Key, store: Arc<TileStore>) -> Arc<TileStore> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed);
        let kept = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(e) = inner.entries.get_mut(&key) {
                e.last_use = now;
                return e.store.clone();
            }
            let bytes = store.byte_size();
            inner.bytes += bytes;
            inner.entries.insert(
                key,
                Entry {
                    store: store.clone(),
                    bytes,
                    last_use: now,
                },
            );
            inner.bytes > self.budget.load(Ordering::Relaxed)
        };
        if kept {
            self.evict(Some(key));
        }
        store
    }

    /// Drop least recently used results until under 90% of the budget,
    /// sparing `keep` (the result just made).
    fn evict(&self, keep: Option<Key>) {
        let budget = self.budget.load(Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        if inner.bytes <= budget {
            return;
        }
        let target = budget / 10 * 9;
        let mut order: Vec<(Key, u64)> = inner
            .entries
            .iter()
            .filter(|(k, _)| Some(**k) != keep)
            .map(|(k, e)| (*k, e.last_use))
            .collect();
        order.sort_by_key(|(_, t)| *t);
        for (k, _) in order {
            if inner.bytes <= target {
                break;
            }
            if let Some(e) = inner.entries.remove(&k) {
                inner.bytes -= e.bytes;
            }
        }
    }

    /// Results kept, bytes they use, and how many were ever computed.
    pub fn stats(&self) -> (usize, usize, u64) {
        let inner = self.inner.lock().unwrap();
        (
            inner.entries.len(),
            inner.bytes,
            self.computed.load(Ordering::Relaxed),
        )
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.entries.clear();
        inner.bytes = 0;
    }
}
