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
    keys: KeyMemo,
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
        }
    }

    /// `node`'s output over the tiles touching `rect`. Ops computed in one
    /// piece upstream of it are made first, independent ones in parallel,
    /// so that pulling tiles never waits for one.
    pub fn render_node(&self, graph: &Graph, blobs: &BlobStore, node: NodeId, rect: Rect) -> TileStore {
        let ctx = self.ctx(graph, blobs, node);
        crate::ops_content::prepare(&ctx, node);
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
    }
}
