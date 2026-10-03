//! Render hints: rendered tiles saved with a project, so a document whose
//! graph has a long history opens without replaying it (ADR 0025, 0026).
//!
//! A hint is a tile under `(content key, tile)`, exactly as the render
//! cache keys it. Seeding a renderer's cache with hints makes the first
//! render a cache hit wherever the keys still match; a graph that changed
//! since has other keys there, so a stale hint is never used. Hints are a
//! cache: dropping them never changes a pixel, only the time to the first.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use lumenply_tiles::{Tile, TileCoord};

use crate::blob::BlobStore;
use crate::cache::TileCache;
use crate::eval::Renderer;
use crate::key::{Key, KeyMemo};
use crate::model::{Graph, NodeId};

/// Rendered tiles by `(content key, tile)`; `None` is a tile known to be
/// fully transparent.
#[derive(Clone, Default)]
pub struct RenderHints {
    tiles: BTreeMap<(Key, TileCoord), Option<Arc<Tile>>>,
}

impl RenderHints {
    pub fn new() -> Self {
        Self::default()
    }

    /// The output of each of `nodes` over the whole canvas, rendered by
    /// `renderer` (from its cache where it has them).
    pub fn capture(renderer: &Renderer, graph: &Graph, blobs: &BlobStore, nodes: &[NodeId]) -> Self {
        let mut hints = RenderHints::new();
        for &n in nodes {
            let Some(&key) = renderer.keys(graph, n).get(&n) else {
                continue;
            };
            let store = renderer.render_node(graph, blobs, n, graph.canvas());
            for c in graph.canvas().tiles() {
                hints.insert(key, c, store.tile_arc(c).cloned());
            }
        }
        hints
    }

    pub fn insert(&mut self, key: Key, coord: TileCoord, tile: Option<Arc<Tile>>) {
        self.tiles.insert((key, coord), tile);
    }

    /// The hint for `(key, coord)`: `Some(None)` for a known transparent tile.
    pub fn get(&self, key: Key, coord: TileCoord) -> Option<Option<&Arc<Tile>>> {
        self.tiles.get(&(key, coord)).map(Option::as_ref)
    }

    /// Number of hinted tiles, transparent ones included.
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// The distinct content keys hinted.
    pub fn keys(&self) -> BTreeSet<Key> {
        self.tiles.keys().map(|(k, _)| *k).collect()
    }

    /// Every hint in key, then tile order.
    pub fn iter(&self) -> impl Iterator<Item = (Key, TileCoord, Option<&Arc<Tile>>)> {
        self.tiles.iter().map(|((k, c), t)| (*k, *c, t.as_ref()))
    }

    /// Keep only the hints for which `keep(key)` holds.
    pub fn retain_keys(&mut self, mut keep: impl FnMut(&Key) -> bool) {
        self.tiles.retain(|(k, _), _| keep(k));
    }

    /// Drop hints for content `graph` no longer has (no node's key matches).
    pub fn retain_graph(&mut self, graph: &Graph) {
        let keys = all_keys(graph);
        self.retain_keys(|k| keys.contains(k));
    }

    /// Put every hint into `cache`; returns how many tiles that was.
    pub fn seed(&self, cache: &TileCache) -> usize {
        for ((k, c), t) in &self.tiles {
            cache.seed(*k, *c, t.clone());
        }
        self.tiles.len()
    }
}

/// The content key of every node in `graph`, not only those the output
/// depends on.
fn all_keys(graph: &Graph) -> HashSet<Key> {
    let used: HashSet<NodeId> = graph
        .nodes()
        .flat_map(|(_, n)| n.inputs.iter().flatten().copied())
        .collect();
    let memo = KeyMemo::default();
    graph
        .nodes()
        .filter(|(id, _)| !used.contains(id))
        .flat_map(|(id, _)| memo.keys(graph, id).into_values())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::{blob_refs, Hash, TileHasher};
    use crate::cache::CacheStats;
    use crate::model::Node;
    use crate::ops::{LayerProps, Op};
    use lumenply_tiles::{Rgba, TileStore};

    /// A 300×200 canvas: one opaque layer over the left 100 px, masked by
    /// its own pixels.
    fn one_layer() -> (Graph, BlobStore) {
        let mut pixels = TileStore::new();
        for y in 0..200 {
            for x in 0..100 {
                pixels.set_pixel(x, y, Rgba::new(0.25, 0.5, 0.75, 1.0));
            }
        }
        let mut blobs = BlobStore::new();
        let blob = blobs.insert(&TileHasher::default(), pixels);
        let mut g = Graph::new(300, 200);
        let empty = g.add(Node::new(Op::Empty, vec![]));
        let image = g.add(Node::new(Op::Image { blob }, vec![]));
        let mask = g.add(Node::new(
            Op::Mask {
                blob: Some(blob),
                default: 0.0,
            },
            vec![],
        ));
        let layer = g.add(Node::new(
            Op::Layer {
                props: LayerProps::default(),
            },
            vec![Some(empty), Some(image), Some(mask)],
        ));
        g.output = Some(layer);
        (g, blobs)
    }

    #[test]
    fn seeded_hints_are_cache_hits() {
        let cache = TileCache::default();
        let (k, c) = (Hash([3; 32]), TileCoord::new(1, 0));
        let tile = Arc::new(Tile::filled(Rgba::new(0.5, 0.5, 0.5, 1.0)));
        let mut hints = RenderHints::new();
        hints.insert(k, c, Some(tile.clone()));
        hints.insert(k, TileCoord::new(2, 0), None);
        assert_eq!(hints.seed(&cache), 2);
        let s: CacheStats = cache.stats();
        assert_eq!((s.hits, s.misses, s.tiles, s.bytes), (0, 0, 2, 1 << 20));
        let got = cache.get_or_compute(k, c, || panic!("seeded, so never computed"));
        assert!(Arc::ptr_eq(&got.unwrap(), &tile));
        assert!(cache
            .get_or_compute(k, TileCoord::new(2, 0), || panic!("seeded"))
            .is_none());
        assert_eq!((cache.stats().hits, cache.stats().misses), (2, 0));
    }

    #[test]
    fn hints_capture_a_render_and_serve_it_again() {
        let (g, blobs) = one_layer();
        let out = g.output.unwrap();
        let r = Renderer::new();
        let hints = RenderHints::capture(&r, &g, &blobs, &[out]);
        // 300×200 is 2×1 tiles; the right one is fully transparent.
        assert_eq!(hints.len(), 2);
        let key = r.keys(&g, out)[&out];
        assert!(hints.get(key, TileCoord::new(0, 0)).unwrap().is_some());
        assert_eq!(hints.get(key, TileCoord::new(1, 0)), Some(None));
        assert_eq!(hints.keys().len(), 1);

        let fresh = Renderer::new();
        assert_eq!(hints.seed(&fresh.cache), 2);
        let px = fresh.render_canvas(&g, &blobs).get_pixel(50, 50);
        assert_eq!(px, Rgba::new(0.25, 0.5, 0.75, 1.0));
        assert_eq!((fresh.cache.stats().hits, fresh.cache.stats().misses), (2, 0));

        // Once the layer changes, its hints describe nothing in the graph.
        let mut kept = hints.clone();
        kept.retain_graph(&g);
        assert_eq!(kept.len(), 2);
        let mut changed = g.clone();
        changed
            .update(out, |n| {
                if let Op::Layer { props } = &mut n.op {
                    props.opacity = 0.5;
                }
            })
            .unwrap();
        kept.retain_graph(&changed);
        assert!(kept.is_empty());
    }

    #[test]
    fn blob_references_are_found_in_any_op_parameter() {
        let (g, blobs) = one_layer();
        let refs = blob_refs(&g);
        let id = *blobs.ids().next().unwrap();
        assert_eq!(refs.len(), 1);
        // The image node and the mask node both name it.
        let users: Vec<_> = refs[&id]
            .iter()
            .map(|n| g.node(*n).unwrap().op.type_name())
            .collect();
        assert_eq!(users, vec!["image", "mask"]);
    }
}
