//! Content-addressed pixel data. Operations never hold pixels; an op that
//! needs them (an imported photo, a mask painted before the graph existed)
//! names a blob by the hash of its content, so identical data is stored and
//! hashed once and the graph's JSON stays small.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use lumenply_tiles::{Tile, TileCoord, TileStore};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A 256-bit BLAKE3 hash, written as 64 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Hash> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(Hash(out))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}…", &self.to_hex()[..12])
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Hash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Hash::from_hex(&s).ok_or_else(|| serde::de::Error::custom("expected 64 hex digits"))
    }
}

/// The id of a blob: the hash of its content.
pub type BlobId = Hash;

/// Hash of one tile's stored pixels.
pub fn tile_hash(tile: &Tile) -> Hash {
    let (format, bytes) = tile.raw_bytes();
    let mut h = blake3::Hasher::new();
    h.update(&[format]);
    h.update(bytes);
    Hash(*h.finalize().as_bytes())
}

/// Hash of a sparse tile store: its tiles' coordinates and hashes, in
/// coordinate order. Tile hashes are memoised by the tile's address, so
/// re-hashing a store that shares most tiles with one seen before (the
/// next version of a layer) only hashes the tiles that changed.
#[derive(Default)]
pub struct TileHasher {
    memo: Mutex<HashMap<usize, (Arc<Tile>, Hash)>>,
}

impl TileHasher {
    pub fn tile(&self, tile: &Arc<Tile>) -> Hash {
        let addr = Arc::as_ptr(tile) as usize;
        if let Some((kept, h)) = self.memo.lock().unwrap().get(&addr) {
            // The memo keeps the tile alive, so its address can't have been
            // reused by another tile.
            if Arc::ptr_eq(kept, tile) {
                return *h;
            }
        }
        let h = tile_hash(tile);
        self.memo.lock().unwrap().insert(addr, (tile.clone(), h));
        h
    }

    pub fn store(&self, store: &TileStore) -> Hash {
        use rayon::prelude::*;
        let mut coords: Vec<TileCoord> = store.coords().collect();
        coords.sort_by_key(|c| (c.y, c.x));
        let tiles: Vec<&Arc<Tile>> = coords
            .iter()
            .map(|c| store.tile_arc(*c).expect("listed coordinate"))
            .collect();
        let mut hashes: Vec<Option<Hash>> = {
            let memo = self.memo.lock().unwrap();
            tiles
                .iter()
                .map(|t| match memo.get(&(Arc::as_ptr(t) as usize)) {
                    Some((kept, h)) if Arc::ptr_eq(kept, t) => Some(*h),
                    _ => None,
                })
                .collect()
        };
        // Tiles not seen before are hashed: in parallel when there are many
        // (a document being opened), on this thread when an edit changed a
        // few (no waiting on a busy pool).
        let missing: Vec<usize> = (0..tiles.len()).filter(|&i| hashes[i].is_none()).collect();
        let fresh: Vec<Hash> = if missing.len() > 8 {
            missing.par_iter().map(|&i| tile_hash(tiles[i])).collect()
        } else {
            missing.iter().map(|&i| tile_hash(tiles[i])).collect()
        };
        if !missing.is_empty() {
            let mut memo = self.memo.lock().unwrap();
            for (&i, h) in missing.iter().zip(fresh) {
                memo.insert(Arc::as_ptr(tiles[i]) as usize, (tiles[i].clone(), h));
                hashes[i] = Some(h);
            }
        }
        let hashes = hashes.into_iter().map(|h| h.expect("hashed above"));
        let mut h = blake3::Hasher::new();
        h.update(b"tilestore");
        for (c, t) in coords.iter().zip(hashes) {
            h.update(&c.x.to_le_bytes());
            h.update(&c.y.to_le_bytes());
            h.update(&t.0);
        }
        Hash(*h.finalize().as_bytes())
    }

    /// Forget tiles nothing else uses any more.
    pub fn prune(&self) {
        self.memo
            .lock()
            .unwrap()
            .retain(|_, (t, _)| Arc::strong_count(t) > 1);
    }
}

/// Pixel data the graph's ops refer to, by content hash.
#[derive(Clone, Default)]
pub struct BlobStore {
    blobs: HashMap<BlobId, Arc<TileStore>>,
}

impl BlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `pixels` (if not already there) and return their id.
    pub fn insert(&mut self, hasher: &TileHasher, pixels: TileStore) -> BlobId {
        let id = hasher.store(&pixels);
        self.blobs.entry(id).or_insert_with(|| Arc::new(pixels));
        id
    }

    /// Store pixels under an id already known to be their hash (a file
    /// being loaded).
    pub fn insert_trusted(&mut self, id: BlobId, pixels: TileStore) {
        self.blobs.insert(id, Arc::new(pixels));
    }

    pub fn get(&self, id: &BlobId) -> Option<&Arc<TileStore>> {
        self.blobs.get(id)
    }

    pub fn contains(&self, id: &BlobId) -> bool {
        self.blobs.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = &BlobId> {
        self.blobs.keys()
    }

    /// Drop blobs not in `keep` (nothing in any kept graph version uses them).
    pub fn retain(&mut self, keep: &std::collections::HashSet<BlobId>) {
        self.blobs.retain(|id, _| keep.contains(id));
    }
}
