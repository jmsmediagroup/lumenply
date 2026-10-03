//! The render cache: output tiles keyed by `(content key, tile)`, under a
//! byte budget with least-recently-used eviction. A tile being computed by
//! one thread is waited for by the others, never computed twice.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use lumenply_tiles::{Tile, TileCoord};

use crate::key::Key;

type Cell = Arc<OnceLock<Option<Arc<Tile>>>>;

struct Slot {
    cell: Cell,
    last_use: u64,
    /// Bytes charged to the budget once the tile is known (0 for a tile
    /// shared with another entry, such as an op passing its input through).
    bytes: usize,
}

#[derive(Default)]
struct Inner {
    slots: HashMap<(Key, TileCoord), Slot>,
    bytes: usize,
}

/// Counters for tests and the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub tiles: usize,
    pub bytes: usize,
}

pub struct TileCache {
    inner: Mutex<Inner>,
    budget: AtomicUsize,
    clock: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Default for TileCache {
    fn default() -> Self {
        TileCache::with_budget(1 << 30)
    }
}

impl TileCache {
    pub fn with_budget(bytes: usize) -> Self {
        TileCache {
            inner: Mutex::new(Inner::default()),
            budget: AtomicUsize::new(bytes),
            clock: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn set_budget(&self, bytes: usize) {
        self.budget.store(bytes, Ordering::Relaxed);
        self.evict();
    }

    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.lock().unwrap();
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            tiles: inner.slots.len(),
            bytes: inner.bytes,
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.slots.clear();
        inner.bytes = 0;
    }

    /// Whether `(key, coord)` is cached and finished.
    pub fn contains(&self, key: Key, coord: TileCoord) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .slots
            .get(&(key, coord))
            .is_some_and(|s| s.cell.get().is_some())
    }

    /// The cached tile for `(key, coord)`, computing it with `f` first if
    /// needed. Concurrent callers for the same entry wait for one `f`.
    pub fn get_or_compute(
        &self,
        key: Key,
        coord: TileCoord,
        f: impl FnOnce() -> Option<Arc<Tile>>,
    ) -> Option<Arc<Tile>> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed);
        let cell = {
            let mut inner = self.inner.lock().unwrap();
            let slot = inner.slots.entry((key, coord)).or_insert_with(|| Slot {
                cell: Arc::new(OnceLock::new()),
                last_use: now,
                bytes: 0,
            });
            slot.last_use = now;
            slot.cell.clone()
        };
        if let Some(t) = cell.get() {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return t.clone();
        }
        let mut computed = false;
        let out = cell
            .get_or_init(|| {
                computed = true;
                f()
            })
            .clone();
        if computed {
            self.misses.fetch_add(1, Ordering::Relaxed);
            // Charge new tiles only: an Arc held elsewhere too (an input
            // passed through, a blob's tile) costs no extra memory.
            let bytes = match &out {
                Some(t) if Arc::strong_count(t) <= 2 => t.byte_size(),
                _ => 0,
            };
            if bytes > 0 {
                let mut inner = self.inner.lock().unwrap();
                if let Some(slot) = inner.slots.get_mut(&(key, coord)) {
                    if Arc::ptr_eq(&slot.cell, &cell) {
                        slot.bytes = bytes;
                        inner.bytes += bytes;
                    }
                }
            }
            if self.inner.lock().unwrap().bytes > self.budget.load(Ordering::Relaxed) {
                self.evict();
            }
        } else {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        out
    }

    /// Drop least-recently-used finished tiles until under 90% of the budget.
    fn evict(&self) {
        let budget = self.budget.load(Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        if inner.bytes <= budget {
            return;
        }
        let target = budget / 10 * 9;
        let mut order: Vec<((Key, TileCoord), u64)> = inner
            .slots
            .iter()
            .filter(|(_, s)| s.cell.get().is_some())
            .map(|(k, s)| (*k, s.last_use))
            .collect();
        order.sort_by_key(|(_, t)| *t);
        for (k, _) in order {
            if inner.bytes <= target {
                break;
            }
            if let Some(s) = inner.slots.remove(&k) {
                inner.bytes -= s.bytes;
            }
        }
    }
}
