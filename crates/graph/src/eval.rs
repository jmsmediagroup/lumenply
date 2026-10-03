//! Rendering a graph: the output node is asked for the tiles in view and
//! every op pulls the input tiles it needs, through the cache.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lumenply_tiles::{Rect, Tile, TileCoord, TileStore};
use rayon::prelude::*;

use crate::blob::{BlobId, BlobStore, TileHasher};
use crate::cache::TileCache;
use crate::key::{Key, KeyMemo};
use crate::model::{Graph, NodeId};
use crate::whole::WholeCache;

/// Everything an op sees while one render runs.
pub struct Ctx<'a> {
    pub graph: &'a Graph,
    pub blobs: &'a BlobStore,
    pub keys: HashMap<NodeId, Key>,
    pub cache: &'a TileCache,
    pub wholes: &'a WholeCache,
    pub canvas: Rect,
    extents: Mutex<HashMap<NodeId, Option<Rect>>>,
    pub(crate) paint: &'a crate::ops_paint::PaintMemo,
}

impl Ctx<'_> {
    /// Where a node's output can be non-transparent (see [`crate::ops`]);
    /// memoised for the duration of the render.
    pub fn extent(&self, node: NodeId) -> Option<Rect> {
        if let Some(e) = self.extents.lock().unwrap().get(&node) {
            return *e;
        }
        let e = crate::ops::extent(self, node);
        self.extents.lock().unwrap().insert(node, e);
        e
    }

    /// One tile of a node's output (`None` for an empty port or a fully
    /// transparent tile).
    pub fn tile(&self, node: Option<NodeId>, coord: TileCoord) -> Option<Arc<Tile>> {
        let id = node?;
        let n = self.graph.node(id)?;
        // Strokes cache their tiles under tile keys (see `ops_paint`).
        if let crate::ops::Op::Stroke { .. } = n.op {
            return crate::ops_paint::tile(self, id, coord);
        }
        let key = *self.keys.get(&id)?;
        self.cache
            .get_or_compute(key, coord, || crate::ops::eval_tile(self, id, n, coord))
    }

    /// The whole output of `node`, for an op that computes everything at
    /// once (text, fills, smart filters): made by `compute` once per content
    /// key and kept, so the op can serve its tiles one at a time. Renders
    /// make these before pulling tiles (see [`Renderer::render_node`]), so a
    /// tile normally finds its node's result ready.
    pub fn whole(&self, node: NodeId, compute: impl FnOnce() -> TileStore) -> Arc<TileStore> {
        match self.keys.get(&node) {
            Some(key) => self.wholes.get_or_compute(*key, compute),
            None => Arc::new(compute()),
        }
    }

    /// A node's output over every tile touching `area`, computed in parallel.
    pub fn area(&self, node: NodeId, area: Rect) -> TileStore {
        let tiles: Vec<(TileCoord, Option<Arc<Tile>>)> = area
            .tiles()
            .into_par_iter()
            .map(|c| (c, self.tile(Some(node), c)))
            .collect();
        let mut out = TileStore::new();
        for (c, t) in tiles {
            if let Some(t) = t {
                out.insert(c, t);
            }
        }
        out
    }

    pub fn blob(&self, id: &BlobId) -> Option<&Arc<TileStore>> {
        self.blobs.get(id)
    }

    /// Compute every tile `root` needs over `rect` from the bottom up, one
    /// node at a time (each node's tiles in parallel), so that when a node
    /// is evaluated its inputs are already cached and no two threads race
    /// to compute the same tile. Works from the ops' declared needs
    /// ([`crate::ops::input_needs`]); anything read beyond them is simply
    /// computed when read.
    pub fn schedule(&self, root: NodeId, rect: Rect) {
        // Consumers before producers: the reverse of a post-order.
        let mut post = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![(root, false)];
        while let Some((id, ready)) = stack.pop() {
            if ready {
                post.push(id);
                continue;
            }
            if !seen.insert(id) {
                continue;
            }
            stack.push((id, true));
            if let Some(n) = self.graph.node(id) {
                stack.extend(
                    n.inputs
                        .iter()
                        .flatten()
                        .filter(|i| !seen.contains(*i))
                        .map(|i| (*i, false)),
                );
            }
        }
        let mut need: HashMap<NodeId, std::collections::BTreeSet<(i32, i32)>> = HashMap::new();
        need.entry(root)
            .or_default()
            .extend(rect.tiles().iter().map(|c| (c.x, c.y)));
        for &id in post.iter().rev() {
            let Some(n) = self.graph.node(id) else {
                continue;
            };
            let Some(key) = self.keys.get(&id).copied() else {
                continue;
            };
            let coords: Vec<TileCoord> = need
                .get(&id)
                .map(|s| s.iter().map(|&(x, y)| TileCoord::new(x, y)).collect())
                .unwrap_or_default();
            for c in coords {
                if self.cache.contains(key, c) {
                    continue;
                }
                for (input, area) in crate::ops::input_needs(self, n, c) {
                    need.entry(input)
                        .or_default()
                        .extend(area.tiles().iter().map(|c| (c.x, c.y)));
                }
            }
        }
        for &id in &post {
            let Some(coords) = need.get(&id) else {
                continue;
            };
            let coords: Vec<TileCoord> = coords.iter().map(|&(x, y)| TileCoord::new(x, y)).collect();
            coords.into_par_iter().for_each(|c| {
                self.tile(Some(id), c);
            });
        }
    }
}

/// Renders graphs, keeping the caches that make the next render cheap.
/// One renderer serves every version of a document (and several documents),
/// since everything it caches is keyed by content.
#[derive(Default)]
pub struct Renderer {
    pub cache: TileCache,
    pub hasher: TileHasher,
    /// Whole-region results of ops computed in one piece (see [`Ctx::whole`]).
    pub wholes: WholeCache,
    /// Plan renders node by node ([`Ctx::schedule`]) so no tile is ever
    /// computed twice. Off by default: pulling tiles in parallel through
    /// the whole stack is faster even though threads occasionally compute
    /// the same tile at once (measured on the corpus: up to 3% of tiles,
    /// and 30-50% faster cold renders).
    pub plan: bool,
    keys: KeyMemo,
    pub(crate) paint: crate::ops_paint::PaintMemo,
}

impl Renderer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_budget(bytes: usize) -> Self {
        Renderer {
            cache: TileCache::with_budget(bytes),
            wholes: WholeCache::with_budget(bytes),
            ..Self::default()
        }
    }

    /// Content keys of `node` and everything upstream of it.
    pub fn keys(&self, graph: &Graph, node: NodeId) -> HashMap<NodeId, Key> {
        self.keys.keys(graph, node)
    }

    fn ctx<'a>(&'a self, graph: &'a Graph, blobs: &'a BlobStore, node: NodeId) -> Ctx<'a> {
        Ctx {
            graph,
            blobs,
            keys: self.keys.keys(graph, node),
            cache: &self.cache,
            wholes: &self.wholes,
            canvas: graph.canvas(),
            extents: Mutex::new(HashMap::new()),
            paint: &self.paint,
        }
    }

    /// `node`'s output over the tiles touching `rect`. Ops computed in one
    /// piece upstream of it are made first, independent ones in parallel,
    /// so that pulling tiles never waits for one.
    pub fn render_node(&self, graph: &Graph, blobs: &BlobStore, node: NodeId, rect: Rect) -> TileStore {
        let ctx = self.ctx(graph, blobs, node);
        crate::ops_content::prepare(&ctx, node);
        crate::ops_paint::prepare(&ctx, node);
        if self.plan {
            ctx.schedule(node, rect);
        }
        ctx.area(node, rect)
    }

    /// The image (the output node) over the tiles touching `rect`; empty
    /// when the graph has no output.
    pub fn render(&self, graph: &Graph, blobs: &BlobStore, rect: Rect) -> TileStore {
        match graph.output {
            Some(o) => self.render_node(graph, blobs, o, rect),
            None => TileStore::new(),
        }
    }

    /// The whole canvas as one store.
    pub fn render_canvas(&self, graph: &Graph, blobs: &BlobStore) -> TileStore {
        self.render(graph, blobs, graph.canvas())
    }

    /// Forget memoised hashes of nodes and tiles nothing uses any more.
    pub fn prune(&self) {
        self.keys.prune();
        self.hasher.prune();
        self.paint.prune();
    }
}
