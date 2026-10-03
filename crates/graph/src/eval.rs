//! Rendering a graph: the output node is asked for the tiles in view and
//! every op pulls the input tiles it needs, through the cache.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Compositing nodes whose tiles this render hands to their one reader
    /// without keeping them (see [`Renderer::set_transient_composites`]).
    transient: HashSet<NodeId>,
    /// Image and empty nodes are read straight from the blob store rather
    /// than through the cache (with transient composites on).
    lean: bool,
    patterns: &'a PatternImages,
}

/// Pattern images read from their blobs, by blob id.
#[derive(Default)]
pub(crate) struct PatternImages(Mutex<HashMap<BlobId, Arc<lumenply_tiles::Raster>>>);

impl PatternImages {
    /// Most images kept; past this the memo starts over.
    const LIMIT: usize = 64;
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
        match &n.op {
            // Strokes cache their tiles under tile keys (see `ops_paint`).
            crate::ops::Op::Stroke { .. } => return crate::ops_paint::tile(self, id, coord),
            // Nothing to compute: the blob's own tile.
            crate::ops::Op::Image { .. } | crate::ops::Op::Empty if self.lean => {
                return crate::ops::eval_tile(self, id, n, coord);
            }
            _ => {}
        }
        let key = *self.keys.get(&id)?;
        if self.transient.contains(&id) {
            // Kept from an earlier render, or made for its one reader, which
            // takes it over without a copy.
            return match self.cache.get(key, coord) {
                Some(t) => t,
                None => crate::ops::eval_tile(self, id, n, coord),
            };
        }
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

    /// A pattern's image from the blob an op names, read once per blob.
    pub fn pattern(&self, pixels: &crate::PatternPixels) -> Option<Arc<lumenply_tiles::Raster>> {
        if let Some(image) = self.patterns.0.lock().unwrap().get(&pixels.blob) {
            if (image.width, image.height) == (pixels.size[0], pixels.size[1]) {
                return Some(image.clone());
            }
        }
        let image = Arc::new(pixels.image(self.blobs)?);
        let mut memo = self.patterns.0.lock().unwrap();
        if memo.len() >= PatternImages::LIMIT {
            memo.clear();
        }
        memo.insert(pixels.blob, image.clone());
        Some(image)
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
    /// Keep only the tiles a later render can reuse (see
    /// [`Renderer::set_transient_composites`]).
    transient: AtomicBool,
    /// The content keys earlier renders saw.
    seen: Mutex<Seen>,
    /// Pattern images read from blobs.
    patterns: PatternImages,
}

/// Content keys renders have seen, each with the last render that did.
/// A node whose key no earlier render saw is one an edit just made (or
/// changed); see [`Renderer::set_transient_composites`].
#[derive(Default)]
struct Seen {
    render: u64,
    keys: HashMap<Key, u64>,
}

impl Seen {
    /// Most keys remembered; past this, keys unseen for the last
    /// [`Seen::KEEP`] renders are forgotten.
    const LIMIT: usize = 1 << 18;
    const KEEP: u64 = 256;

    /// Note `keys` as rendered now; returns the nodes whose key no earlier
    /// render saw.
    fn mark(&mut self, keys: &HashMap<NodeId, Key>) -> HashSet<NodeId> {
        self.render += 1;
        let now = self.render;
        let mut fresh = HashSet::new();
        for (id, k) in keys {
            match self.keys.insert(*k, now) {
                Some(then) if then < now => {}
                // Unseen, or seen only by another node of this render with
                // the same key (it computes the same thing).
                Some(_) | None => {
                    fresh.insert(*id);
                }
            }
        }
        if self.keys.len() > Self::LIMIT {
            let cut = now.saturating_sub(Self::KEEP);
            self.keys.retain(|_, t| *t >= cut);
        }
        fresh
    }
}

/// Whether `op` composites a layer or an adjustment onto a backdrop.
fn compositing(op: &crate::ops::Op) -> bool {
    use crate::ops::Op;
    matches!(
        op,
        Op::Layer { .. }
            | Op::Adjustment { .. }
            | Op::FilterLayer { .. }
            | Op::PassThrough { .. }
            | Op::ClipGroup { .. }
    )
}

/// Whether `reader` reads its input `port` one tile at a time, at the
/// tile it is making (a backdrop), rather than over an area.
fn reads_pointwise(reader: &crate::ops::Op, port: usize) -> bool {
    use crate::ops::Op;
    match reader {
        // Effects read the content around the tile.
        Op::Layer { props } => port == 0 || (port == 1 && props.effects.is_empty()),
        Op::Adjustment { .. } | Op::ClipGroup { .. } => port == 0,
        Op::PassThrough { .. } => port <= 1,
        _ => false,
    }
}

/// The compositing nodes upstream of `root` whose tiles a render can hand
/// to their reader without keeping them: each has one reader, which reads
/// it tile by tile (so it is computed once per tile), and isn't the
/// unchanged backdrop under a node an edit changed (`fresh`), which is
/// what the next edit of that layer reads again.
fn transient_nodes(
    graph: &Graph,
    keys: &HashMap<NodeId, Key>,
    fresh: &HashSet<NodeId>,
    root: NodeId,
) -> HashSet<NodeId> {
    let mut readers: HashMap<NodeId, Vec<(NodeId, usize)>> = HashMap::new();
    for &id in keys.keys() {
        if let Some(n) = graph.node(id) {
            for (port, input) in n.inputs.iter().enumerate() {
                if let Some(i) = input {
                    readers.entry(*i).or_default().push((id, port));
                }
            }
        }
    }
    let mut out = HashSet::new();
    for &id in keys.keys() {
        if id == root || !graph.node(id).is_some_and(|n| compositing(&n.op)) {
            continue;
        }
        let Some([(reader, port)]) = readers.get(&id).map(Vec::as_slice) else {
            continue;
        };
        let Some(r) = graph.node(*reader) else {
            continue;
        };
        let backdrop_of_edit = !fresh.contains(&id) && fresh.contains(reader);
        if reads_pointwise(&r.op, *port) && !backdrop_of_edit {
            out.insert(id);
        }
    }
    out
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

    /// Keep only the tiles a later render can reuse: the output (or
    /// whatever node a render asks for), layers' content, and the backdrop
    /// directly under a node an edit changed. Any other compositing node
    /// that one reader reads tile by tile hands its tiles straight to it,
    /// uncached and uncopied, as the reference compositor passes its one
    /// working tile up the stack. Without this every layer of a stack
    /// above an edit gets a fresh tile per render (a megabyte each), and a
    /// slider drag on a large document spends most of its time allocating
    /// them. Off by default: every node's tiles are cached.
    pub fn set_transient_composites(&self, on: bool) {
        self.transient.store(on, Ordering::Relaxed);
    }

    pub fn transient_composites(&self) -> bool {
        self.transient.load(Ordering::Relaxed)
    }

    pub(crate) fn ctx<'a>(&'a self, graph: &'a Graph, blobs: &'a BlobStore, node: NodeId) -> Ctx<'a> {
        Ctx {
            graph,
            blobs,
            keys: self.keys.keys(graph, node),
            cache: &self.cache,
            wholes: &self.wholes,
            canvas: graph.canvas(),
            extents: Mutex::new(HashMap::new()),
            paint: &self.paint,
            transient: HashSet::new(),
            lean: false,
            patterns: &self.patterns,
        }
    }

    /// A context for rendering tiles of `node`: with transient composites
    /// on, notes the keys as seen and works out which nodes pass through.
    fn render_ctx<'a>(&'a self, graph: &'a Graph, blobs: &'a BlobStore, node: NodeId) -> Ctx<'a> {
        let mut ctx = self.ctx(graph, blobs, node);
        if self.transient_composites() && !self.plan {
            let fresh = self.seen.lock().unwrap().mark(&ctx.keys);
            ctx.transient = transient_nodes(graph, &ctx.keys, &fresh, node);
            ctx.lean = true;
        }
        ctx
    }

    /// A bound on where `node`'s output can be non-transparent (`None`:
    /// transparent everywhere). Not limited to the canvas.
    pub fn extent(&self, graph: &Graph, blobs: &BlobStore, node: NodeId) -> Option<Rect> {
        let ctx = self.ctx(graph, blobs, node);
        crate::ops_content::prepare(&ctx, node);
        ctx.extent(node)
    }

    /// All of `node`'s output: every tile within its extent, on the canvas
    /// or off it (a layer's pixels may reach past the canvas).
    pub fn render_all(&self, graph: &Graph, blobs: &BlobStore, node: NodeId) -> TileStore {
        // One context: content keys cost a walk of everything upstream,
        // which for a long chain of strokes is most of the work.
        let ctx = self.render_ctx(graph, blobs, node);
        crate::ops_content::prepare(&ctx, node);
        crate::ops_paint::prepare(&ctx, node);
        let Some(rect) = ctx.extent(node) else {
            return TileStore::new();
        };
        if self.plan {
            ctx.schedule(node, rect);
        }
        ctx.area(node, rect)
    }

    /// `node`'s output over the tiles touching `rect`. Ops computed in one
    /// piece upstream of it are made first, independent ones in parallel,
    /// so that pulling tiles never waits for one.
    pub fn render_node(&self, graph: &Graph, blobs: &BlobStore, node: NodeId, rect: Rect) -> TileStore {
        let ctx = self.render_ctx(graph, blobs, node);
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
