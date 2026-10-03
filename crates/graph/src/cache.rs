//! The render cache: output tiles keyed by `(content key, tile)`, under a
//! byte budget with least-recently-used eviction.
//!
//! A caller never waits for another computation: if the tile it wants is
//! being computed elsewhere, it computes the tile itself. Waiting would
//! deadlock under rayon, whose threads run other queued jobs while they
//! wait for their own, so a thread can end up waiting for a tile its own
//! stack is still computing. Renders plan their work so inputs are ready
//! before anyone asks for them (see [`crate::Renderer::render_node`]), which
//! keeps such duplicate work rare.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lumenply_tiles::{Tile, TileCoord};

use crate::key::Key;

enum State {
    Computing,
    Ready(Option<Arc<Tile>>),
}

struct Slot {
    state: State,
    last_use: u64,
    /// Bytes charged to the budget (0 for a tile shared with another entry,
    /// such as an op passing its input through, or while computing).
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
    /// Tiles computed a second time because another thread was already
    /// computing them.
    pub duplicates: u64,
    pub tiles: usize,
    pub bytes: usize,
}

pub struct TileCache {
    inner: Mutex<Inner>,
    budget: AtomicUsize,
    clock: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    duplicates: AtomicU64,
}

impl Default for TileCache {
    fn default() -> Self {
        TileCache::with_budget(1 << 30)
    }
}

/// Removes a slot left `Computing` if its computation panics, so later
/// callers compute the tile instead of finding a slot that never fills.
struct Pending<'a> {
    cache: &'a TileCache,
    slot: (Key, TileCoord),
    done: bool,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if !self.done {
            if let Ok(mut inner) = self.cache.inner.lock() {
                if matches!(
                    inner.slots.get(&self.slot).map(|s| &s.state),
                    Some(State::Computing)
                ) {
                    inner.slots.remove(&self.slot);
                }
            }
        }
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
            duplicates: AtomicU64::new(0),
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
            duplicates: self.duplicates.load(Ordering::Relaxed),
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
        matches!(
            inner.slots.get(&(key, coord)).map(|s| &s.state),
            Some(State::Ready(_))
        )
    }

    /// The cached tile for `(key, coord)`, computing it with `f` first if
    /// needed. Never blocks on another thread's computation.
    pub fn get_or_compute(
        &self,
        key: Key,
        coord: TileCoord,
        f: impl FnOnce() -> Option<Arc<Tile>>,
    ) -> Option<Arc<Tile>> {
        let now = self.clock.fetch_add(1, Ordering::Relaxed);
        {
            let mut inner = self.inner.lock().unwrap();
            match inner.slots.get_mut(&(key, coord)) {
                Some(slot) => {
                    slot.last_use = now;
                    if let State::Ready(t) = &slot.state {
                        let t = t.clone();
                        drop(inner);
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        return t;
                    }
                    // Someone else is computing it: do it too, don't wait.
                    drop(inner);
                    self.duplicates.fetch_add(1, Ordering::Relaxed);
                    return f();
                }
                None => {
                    inner.slots.insert(
                        (key, coord),
                        Slot {
                            state: State::Computing,
                            last_use: now,
                            bytes: 0,
                        },
                    );
                }
            }
        }
        let mut pending = Pending {
            cache: self,
            slot: (key, coord),
            done: false,
        };
        let out = f();
        self.misses.fetch_add(1, Ordering::Relaxed);
        // Charge new tiles only: an Arc held elsewhere too (an input passed
        // through, a blob's tile) costs no extra memory.
        let bytes = match &out {
            Some(t) if Arc::strong_count(t) == 1 => t.byte_size(),
            _ => 0,
        };
        let over = {
            let mut inner = self.inner.lock().unwrap();
            let slot = inner.slots.entry((key, coord)).or_insert(Slot {
                state: State::Computing,
                last_use: now,
                bytes: 0,
            });
            let old = slot.bytes;
            slot.state = State::Ready(out.clone());
            slot.bytes = bytes;
            inner.bytes = inner.bytes - old + bytes;
            inner.bytes > self.budget.load(Ordering::Relaxed)
        };
        pending.done = true;
        if over {
            self.evict();
        }
        out
    }

    /// Put a tile known to be `(key, coord)`'s output into the cache (render
    /// hints loaded with a project).
    pub fn seed(&self, key: Key, coord: TileCoord, tile: Option<Arc<Tile>>) {
        let now = self.clock.fetch_add(1, Ordering::Relaxed);
        let bytes = tile.as_ref().map_or(0, |t| t.byte_size());
        let mut inner = self.inner.lock().unwrap();
        let old = inner.slots.get(&(key, coord)).map_or(0, |s| s.bytes);
        inner.slots.insert(
            (key, coord),
            Slot {
                state: State::Ready(tile),
                last_use: now,
                bytes,
            },
        );
        inner.bytes = inner.bytes - old + bytes;
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
            .filter(|(_, s)| matches!(s.state, State::Ready(_)))
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
