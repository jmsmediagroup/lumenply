//! Brush strokes as operations (ADR 0025, stage 2). A `stroke` node paints
//! one stroke onto its input with `lumenply_render::paint`, the code
//! `PaintStroke` runs, so a stroke replayed from the graph is
//! bit-identical to the one painted into a layer.
//!
//! # Evaluation
//!
//! A stroke changes only the tiles its dabs reach ([`Summary::writes`]);
//! every other tile is its input's, the same `Arc`. [`tile`], which
//! `Ctx::tile` calls for strokes, walks down a chain of strokes past the
//! ones that miss a tile without touching the cache, and paints the ones
//! that reach it bottom-up in a loop (no recursion, however long the
//! chain). Where a stroke reaches a tile, its mode decides how:
//!
//! - **Local modes** (paint, erase, dodge, burn, the sponge): a pixel's
//!   result depends only on that pixel and the dabs over it, in order, so
//!   each tile is painted on its own from the input's tile and the dabs
//!   that reach it. That is bit-identical to painting the whole stroke at
//!   once, runs tiles in parallel, and an edit repaints exactly the tiles
//!   it can change.
//! - **Smudge, blur, sharpen** read around each dab, so a dab sees earlier
//!   dabs' writes across tile borders. The stroke is painted once over
//!   everything it reads ([`paint::dabs_reach`]) as a whole result
//!   ([`crate::WholeCache`], under the stroke's region key), and its tiles
//!   are served from that. [`prepare`] paints these regions before a
//!   render pulls tiles, lower strokes first, so no tile waits for one.
//!
//! # Keys
//!
//! A stroke's tiles are cached under *tile keys* rather than the node's
//! content key: a hash of the stroke's op and the keys of the input tiles
//! it reads (a local stroke: the input tile under it; a smudge: every tile
//! of its read area). A tile a stroke misses has its input's key. So
//! editing stroke 150 of 200 re-renders only tiles under later strokes
//! that overlap what stroke 150 changed: later strokes elsewhere keep
//! hitting the cache although their content keys changed. Every other op
//! keeps caching under its content key.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use lumenply_doc::{Mask, Selection};
use lumenply_render::paint::{self, Brush, BrushDynamics, BrushMode, Dab, StrokePoint, StrokeReach};
use lumenply_tiles::{Rect, Tile, TileCoord, TileStore};
use serde::{Deserialize, Serialize};

use crate::blob::{BlobId, BlobStore, Hash};
use crate::eval::Ctx;
use crate::key::Key;
use crate::model::{Node, NodeId};
use crate::ops::Op;

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

/// Zero, but not −0, which must survive a round trip (its JSON differs).
fn is_zero(v: &f32) -> bool {
    v.to_bits() == 0
}

fn is_paint(m: &BrushMode) -> bool {
    *m == BrushMode::Paint
}

fn is_static(d: &BrushDynamics) -> bool {
    *d == BrushDynamics::default()
}

/// A brush as a `stroke` node stores it: [`Brush`]'s settings, with a
/// sampled tip named by its blob ([`BlobStore::insert_tip`]) instead of
/// held inline. Settings at their defaults are left out of the JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeBrush {
    pub radius: f32,
    pub hardness: f32,
    /// Straight linear RGBA; outside paint mode `color[3]` is the strength.
    pub color: [f32; 4],
    pub spacing: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub jitter: f32,
    #[serde(default, skip_serializing_if = "is_paint")]
    pub mode: BrushMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<BlobId>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub angle: f32,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub roundness: f32,
    #[serde(default, skip_serializing_if = "is_static")]
    pub dynamics: BrushDynamics,
}

impl StrokeBrush {
    /// The brush as a node stores it; its sampled tip goes into `blobs`.
    pub fn new(brush: &Brush, blobs: &mut BlobStore) -> Self {
        StrokeBrush {
            radius: brush.radius,
            hardness: brush.hardness,
            color: brush.color,
            spacing: brush.spacing,
            jitter: brush.jitter,
            mode: brush.mode,
            tip: brush.tip.as_ref().map(|t| blobs.insert_tip(t.clone())),
            angle: brush.angle,
            roundness: brush.roundness,
            dynamics: brush.dynamics,
        }
    }

    /// The brush to paint with; `None` when its tip is not in `blobs`.
    pub fn brush(&self, blobs: &BlobStore) -> Option<Brush> {
        let tip = match &self.tip {
            Some(id) => Some(blobs.tip(id)?.clone()),
            None => None,
        };
        Some(Brush {
            radius: self.radius,
            hardness: self.hardness,
            color: self.color,
            spacing: self.spacing,
            jitter: self.jitter,
            mode: self.mode,
            tip,
            angle: self.angle,
            roundness: self.roundness,
            dynamics: self.dynamics,
        })
    }
}

impl Op {
    /// A `stroke` op: `points` painted with `brush`, whose sampled tip (if
    /// any) is stored in `blobs`.
    pub fn stroke(brush: &Brush, points: Vec<StrokePoint>, blobs: &mut BlobStore) -> Op {
        Op::Stroke {
            brush: StrokeBrush::new(brush, blobs),
            points,
        }
    }
}

/// Stroke points in JSON: `[x, y, pressure]`, with the pen opacity as a
/// fourth number when it isn't 1.
pub(crate) mod points_json {
    use lumenply_render::paint::StrokePoint;
    use serde::de::Error;
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(points: &[StrokePoint], s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(points.len()))?;
        for p in points {
            if p.opacity.to_bits() == 1f32.to_bits() {
                seq.serialize_element(&[p.x, p.y, p.pressure])?;
            } else {
                seq.serialize_element(&[p.x, p.y, p.pressure, p.opacity])?;
            }
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<StrokePoint>, D::Error> {
        Vec::<Vec<f32>>::deserialize(d)?
            .into_iter()
            .map(|p| match p[..] {
                [x, y, pressure] => Ok(StrokePoint::new(x, y, pressure)),
                [x, y, pressure, opacity] => Ok(StrokePoint::new(x, y, pressure).with_opacity(opacity)),
                _ => Err(D::Error::custom(
                    "a stroke point is [x, y, pressure] or [x, y, pressure, opacity]",
                )),
            })
            .collect()
    }
}

/// The key of an empty port or a missing node.
const NONE: Key = Hash([0; 32]);

/// Where a stroke paints: enough to route tiles without its dabs.
pub(crate) struct Summary {
    /// Hash of the op: brush (with its tip's id) and points.
    op: Hash,
    /// False when the stroke changes nothing: no points, the history
    /// mode (a stroke carries no past state), or a tip missing from the
    /// blob store.
    active: bool,
    /// Painted tile by tile ([`BrushMode::is_local`]) or as one region.
    local: bool,
    /// Every pixel it can change.
    write: Rect,
    /// Every pixel whose value can change the result.
    read: Rect,
    /// The tiles it can change.
    tiles: BTreeSet<TileCoord>,
}

impl Summary {
    pub(crate) fn writes(&self, c: TileCoord) -> bool {
        self.active && self.tiles.contains(&c)
    }
}

/// A stroke ready to paint.
struct Painting {
    brush: Brush,
    dabs: Vec<Dab>,
    /// Each dab's [`paint::dab_area`].
    areas: Vec<Rect>,
}

fn op_hash(op: &Op) -> Hash {
    let json = serde_json::to_vec(op).expect("ops always serialise");
    let mut h = blake3::Hasher::new();
    h.update(b"op");
    h.update(&json);
    Hash(*h.finalize().as_bytes())
}

/// The summary and painting of a stroke node, and whether they may be
/// remembered (not when its tip is missing: the blob may arrive later).
fn build(node: &Node, blobs: &BlobStore, canvas: Rect) -> (Summary, Option<Painting>, bool) {
    let Op::Stroke { brush, points } = &node.op else {
        unreachable!("only strokes are summarised")
    };
    let op = op_hash(&node.op);
    let idle = Summary {
        op,
        active: false,
        local: true,
        write: Rect::default(),
        read: Rect::default(),
        tiles: BTreeSet::new(),
    };
    let Some(brush) = brush.brush(blobs) else {
        return (idle, None, false);
    };
    if points.is_empty() || brush.mode == BrushMode::History {
        return (idle, None, true);
    }
    let dabs = paint::interpolate_dabs(&brush, points);
    let areas: Vec<Rect> = dabs.iter().map(|d| paint::dab_area(&brush, d, canvas)).collect();
    let StrokeReach { write, read } = paint::dabs_reach(&brush, &dabs, canvas);
    let local = brush.mode.is_local();
    let tiles = if local {
        areas.iter().flat_map(|a| a.tiles()).collect()
    } else {
        write.tiles().into_iter().collect()
    };
    let summary = Summary {
        op,
        active: true,
        local,
        write,
        read,
        tiles,
    };
    (summary, Some(Painting { brush, dabs, areas }), true)
}

/// Most dabs kept ready to paint (about 60 bytes each); older paintings
/// are dropped and rebuilt when needed. Summaries are always kept.
const DAB_BUDGET: usize = 1 << 20;

/// Most tile keys remembered before the memo starts over.
const KEY_MEMO_LIMIT: usize = 1 << 18;

struct StrokeEntry {
    /// Keeps the address meaningful; a node no graph holds any more is
    /// dropped at the next sweep.
    node: Arc<Node>,
    canvas: Rect,
    summary: Arc<Summary>,
    painting: Option<Arc<Painting>>,
    last_use: u64,
}

#[derive(Default)]
struct Strokes {
    /// By the node's address (unchanged nodes are the same `Arc` in every
    /// graph version).
    map: HashMap<usize, StrokeEntry>,
    clock: u64,
    /// Dabs held by the kept paintings.
    dabs: usize,
    /// Entries after the last sweep for dead nodes.
    swept: usize,
}

impl Strokes {
    fn insert(&mut self, addr: usize, entry: StrokeEntry) {
        let n = |e: &StrokeEntry| e.painting.as_ref().map_or(0, |p| p.dabs.len());
        self.dabs += n(&entry);
        if let Some(old) = self.map.insert(addr, entry) {
            self.dabs -= n(&old);
        }
        if self.dabs > DAB_BUDGET {
            let mut order: Vec<(u64, usize)> = self
                .map
                .iter()
                .filter(|(_, e)| e.painting.is_some())
                .map(|(a, e)| (e.last_use, *a))
                .collect();
            order.sort_unstable();
            for (_, a) in order {
                if self.dabs <= DAB_BUDGET / 4 * 3 {
                    break;
                }
                if let Some(e) = self.map.get_mut(&a) {
                    let gone = e.painting.take().map_or(0, |p| p.dabs.len());
                    self.dabs -= gone;
                }
            }
        }
        if self.map.len() > 2 * self.swept + 256 {
            self.sweep();
        }
    }

    /// Forget strokes no graph holds any more.
    fn sweep(&mut self) {
        let mut freed = 0;
        self.map.retain(|_, e| {
            let live = Arc::strong_count(&e.node) > 1;
            if !live {
                freed += e.painting.as_ref().map_or(0, |p| p.dabs.len());
            }
            live
        });
        self.dabs -= freed;
        self.swept = self.map.len();
    }
}

/// Tile keys by (node content key, tile, canvas size).
type TileKeys = HashMap<(Key, TileCoord, (u32, u32)), Key>;

/// What a [`crate::Renderer`] remembers about strokes between renders:
/// their dabs and footprints (by node) and their tile keys. Painted
/// regions are whole results in the renderer's [`crate::WholeCache`].
#[derive(Default)]
pub struct PaintMemo {
    strokes: Mutex<Strokes>,
    keys: Mutex<TileKeys>,
}

impl PaintMemo {
    /// The summary of stroke `id` and, if `painting`, its dabs (`None` when
    /// it paints nothing).
    fn stroke(&self, ctx: &Ctx, id: NodeId, painting: bool) -> Option<(Arc<Summary>, Option<Arc<Painting>>)> {
        let node = ctx.graph.node_arc(id)?;
        let addr = Arc::as_ptr(node) as usize;
        {
            let mut s = self.strokes.lock().unwrap();
            s.clock += 1;
            let now = s.clock;
            if let Some(e) = s.map.get_mut(&addr) {
                let fresh = Arc::ptr_eq(&e.node, node) && e.canvas == ctx.canvas;
                if fresh && (!painting || e.painting.is_some() || !e.summary.active) {
                    e.last_use = now;
                    return Some((e.summary.clone(), e.painting.clone()));
                }
            }
        }
        let (summary, p, keep) = build(node, ctx.blobs, ctx.canvas);
        let (summary, p) = (Arc::new(summary), p.map(Arc::new));
        if keep {
            let mut s = self.strokes.lock().unwrap();
            s.clock += 1;
            let last_use = s.clock;
            s.insert(
                addr,
                StrokeEntry {
                    node: node.clone(),
                    canvas: ctx.canvas,
                    summary: summary.clone(),
                    painting: p.clone(),
                    last_use,
                },
            );
        }
        Some((summary, p))
    }

    fn summary(&self, ctx: &Ctx, id: NodeId) -> Option<Arc<Summary>> {
        self.stroke(ctx, id, false).map(|(s, _)| s)
    }

    /// Forget strokes no graph holds any more.
    pub(crate) fn prune(&self) {
        self.strokes.lock().unwrap().sweep();
    }

    fn known_key(&self, node: Key, c: TileCoord, canvas: (u32, u32)) -> Option<Key> {
        self.keys.lock().unwrap().get(&(node, c, canvas)).copied()
    }

    fn remember_key(&self, node: Key, c: TileCoord, canvas: (u32, u32), key: Key) {
        let mut keys = self.keys.lock().unwrap();
        if keys.len() >= KEY_MEMO_LIMIT {
            keys.clear();
        }
        keys.insert((node, c, canvas), key);
    }
}

fn canvas_size(ctx: &Ctx) -> (u32, u32) {
    (ctx.canvas.w, ctx.canvas.h)
}

/// The key of a local stroke's tile from the key of the input tile under it.
fn local_key(op: Hash, canvas: (u32, u32), input: Key, sel: Key) -> Key {
    let mut h = blake3::Hasher::new();
    h.update(b"stroke-tile");
    h.update(&op.0);
    h.update(&canvas.0.to_le_bytes());
    h.update(&canvas.1.to_le_bytes());
    h.update(&input.0);
    h.update(&sel.0);
    Hash(*h.finalize().as_bytes())
}

/// The key of a smudge, blur or sharpen stroke's tiles: its op and the
/// keys of every input tile it reads.
fn region_key(ctx: &Ctx, node: &Node, s: &Summary, sel: Key) -> Key {
    // The input keys first: finding them can recurse into strokes below,
    // so the hasher stays off the stack meanwhile.
    let inputs: Vec<(TileCoord, Key)> = s
        .read
        .tiles()
        .into_iter()
        .map(|c| (c, resolved_key(ctx, node.input(0), c)))
        .collect();
    let canvas = canvas_size(ctx);
    let mut h = blake3::Hasher::new();
    h.update(b"stroke-region");
    h.update(&s.op.0);
    h.update(&canvas.0.to_le_bytes());
    h.update(&canvas.1.to_le_bytes());
    h.update(&sel.0);
    for (c, k) in inputs {
        h.update(&c.x.to_le_bytes());
        h.update(&c.y.to_le_bytes());
        h.update(&k.0);
    }
    Hash(*h.finalize().as_bytes())
}

/// The selection's content key (the whole selection, not per tile).
fn sel_key(ctx: &Ctx, node: &Node) -> Key {
    node.input(1)
        .and_then(|s| ctx.keys.get(&s).copied())
        .unwrap_or(NONE)
}

/// The key `node`'s tile at `c` is cached under: a stroke's tile key, or
/// the content key of the first other node below the strokes that miss
/// `c`. Walks down a chain of strokes in a loop and remembers every step.
fn resolved_key(ctx: &Ctx, node: Option<NodeId>, c: TileCoord) -> Key {
    let canvas = canvas_size(ctx);
    // Strokes passed on the way down, with (op, selection) for those that
    // paint `c`.
    let mut path: Vec<(Key, Option<(Hash, Key)>)> = Vec::new();
    let mut cur = node;
    let mut key = loop {
        let Some(id) = cur else { break NONE };
        let Some(&content) = ctx.keys.get(&id) else {
            break NONE;
        };
        if let Some(k) = ctx.paint.known_key(content, c, canvas) {
            break k;
        }
        let Some(n) = ctx.graph.node(id) else { break NONE };
        if !matches!(n.op, Op::Stroke { .. }) {
            break content;
        }
        let Some(s) = ctx.paint.summary(ctx, id) else {
            break NONE;
        };
        if !s.writes(c) {
            path.push((content, None));
            cur = n.input(0);
            continue;
        }
        let sel = sel_key(ctx, n);
        if !s.local {
            let k = region_key(ctx, n, &s, sel);
            ctx.paint.remember_key(content, c, canvas, k);
            break k;
        }
        path.push((content, Some((s.op, sel))));
        cur = n.input(0);
    };
    for (content, paints) in path.into_iter().rev() {
        if let Some((op, sel)) = paints {
            key = local_key(op, canvas, key, sel);
        }
        ctx.paint.remember_key(content, c, canvas, key);
    }
    key
}

/// The selection port's coverage over `coords`, as the reference painter
/// reads it (`None`: no selection).
fn selection(ctx: &Ctx, port: Option<NodeId>, coords: &[TileCoord]) -> Option<Selection> {
    let id = port?;
    let mut tiles = TileStore::new();
    for &c in coords {
        if let Some(t) = ctx.tile(Some(id), c) {
            tiles.insert(c, t);
        }
    }
    Some(Selection {
        coverage: Mask {
            tiles,
            default: 0.0,
            enabled: true,
        },
    })
}

/// One tile of a local stroke: the dabs that reach it, painted onto the
/// input's tile with the canvas narrowed to the tile.
fn local_tile(ctx: &Ctx, id: NodeId, input: Option<Arc<Tile>>, coord: TileCoord) -> Option<Arc<Tile>> {
    let (Some(node), Some((_, Some(p)))) = (ctx.graph.node(id), ctx.paint.stroke(ctx, id, true)) else {
        return input;
    };
    let area = coord.rect();
    let dabs: Vec<Dab> = p
        .dabs
        .iter()
        .zip(&p.areas)
        .filter(|(_, a)| !a.intersect(&area).is_empty())
        .map(|(d, _)| *d)
        .collect();
    if dabs.is_empty() {
        return input;
    }
    let mut store = TileStore::new();
    if let Some(t) = input {
        store.insert(coord, t);
    }
    let sel = selection(ctx, node.input(1), &[coord]);
    // Local modes always paint (a history stroke is never active).
    let _ = paint::paint_dabs(
        &mut store,
        &p.brush,
        &dabs,
        ctx.canvas.intersect(&area),
        sel.as_ref(),
    );
    store.tile_arc(coord).cloned()
}

/// A smudge, blur or sharpen stroke painted over its whole read area.
fn paint_region(ctx: &Ctx, id: NodeId) -> TileStore {
    let (Some(node), Some((s, Some(p)))) = (ctx.graph.node(id), ctx.paint.stroke(ctx, id, true)) else {
        return TileStore::new();
    };
    let mut store = match node.input(0) {
        Some(input) => ctx.area(input, s.read),
        None => TileStore::new(),
    };
    let sel = selection(ctx, node.input(1), &s.write.tiles());
    let _ = paint::paint_dabs(&mut store, &p.brush, &p.dabs, ctx.canvas, sel.as_ref());
    store
}

/// A region stroke's painted region, kept under its region key `key`.
fn region(ctx: &Ctx, id: NodeId, key: Key) -> Arc<TileStore> {
    ctx.wholes.get_or_compute(key, || paint_region(ctx, id))
}

/// Paint the regions of the smudge, blur and sharpen strokes a render of
/// `root` reads, before it pulls tiles: lower strokes first, independent
/// ones in parallel, so no tile waits for a region (or paints one twice).
/// Regions already kept are skipped.
pub(crate) fn prepare(ctx: &Ctx, root: NodeId) {
    // Post-order over every node read. A node's level is the most region
    // strokes on any path below it; a region stroke is painted once every
    // level below its own is.
    let mut level: HashMap<NodeId, usize> = HashMap::new();
    let mut by_level: Vec<Vec<NodeId>> = Vec::new();
    let mut stack = vec![(root, false)];
    while let Some((id, ready)) = stack.pop() {
        if level.contains_key(&id) {
            continue;
        }
        let Some(node) = ctx.graph.node(id) else {
            level.insert(id, 0);
            continue;
        };
        if !ready {
            stack.push((id, true));
            stack.extend(
                node.inputs
                    .iter()
                    .flatten()
                    .filter(|i| !level.contains_key(i))
                    .map(|&i| (i, false)),
            );
            continue;
        }
        let below = node
            .inputs
            .iter()
            .flatten()
            .filter_map(|i| level.get(i))
            .copied()
            .max()
            .unwrap_or(0);
        let is_region = matches!(node.op, Op::Stroke { .. })
            && ctx.paint.summary(ctx, id).is_some_and(|s| s.active && !s.local);
        if is_region {
            if by_level.len() <= below {
                by_level.resize(below + 1, Vec::new());
            }
            by_level[below].push(id);
        }
        level.insert(id, below + usize::from(is_region));
    }
    for ids in by_level {
        ids.par_iter().for_each(|&id| {
            let (Some(node), Some(s)) = (ctx.graph.node(id), ctx.paint.summary(ctx, id)) else {
                return;
            };
            let key = region_key(ctx, node, &s, sel_key(ctx, node));
            region(ctx, id, key);
        });
    }
}

/// One tile of stroke `id`'s output: what `Ctx::tile` (and
/// [`crate::ops::eval_tile`]) returns for strokes.
pub(crate) fn tile(ctx: &Ctx, id: NodeId, coord: TileCoord) -> Option<Arc<Tile>> {
    // Local strokes that paint `coord`, top first, down to the first tile
    // that is cached or isn't a local stroke's.
    let mut todo: Vec<(NodeId, Key)> = Vec::new();
    let mut cur = Some(id);
    let mut tile = loop {
        let Some(c) = cur else { break None };
        let node = ctx.graph.node(c)?;
        if !matches!(node.op, Op::Stroke { .. }) {
            break ctx.tile(Some(c), coord);
        }
        let s = ctx.paint.summary(ctx, c)?;
        if !s.writes(coord) {
            cur = node.input(0);
            continue;
        }
        let key = resolved_key(ctx, Some(c), coord);
        if !s.local {
            break ctx
                .cache
                .get_or_compute(key, coord, || region(ctx, c, key).tile_arc(coord).cloned());
        }
        if ctx.cache.contains(key, coord) {
            break ctx.cache.get_or_compute(key, coord, || {
                local_tile(ctx, c, ctx.tile(node.input(0), coord), coord)
            });
        }
        todo.push((c, key));
        cur = node.input(0);
    };
    for &(c, key) in todo.iter().rev() {
        let below = tile;
        tile = ctx
            .cache
            .get_or_compute(key, coord, || local_tile(ctx, c, below, coord));
    }
    tile
}

/// Where stroke `id`'s output can be non-transparent: its input's extent
/// and everything the strokes down the chain paint.
pub(crate) fn extent(ctx: &Ctx, id: NodeId) -> Option<Rect> {
    let mut painted: Option<Rect> = None;
    let mut cur = Some(id);
    while let Some(c) = cur {
        let Some(node) = ctx.graph.node(c) else { break };
        if !matches!(node.op, Op::Stroke { .. }) {
            return match (painted, ctx.extent(c)) {
                (Some(a), Some(b)) => Some(a.union(&b)),
                (a, b) => a.or(b),
            };
        }
        if let Some(s) = ctx.paint.summary(ctx, c) {
            if s.active && !s.write.is_empty() {
                painted = Some(painted.map_or(s.write, |p| p.union(&s.write)));
            }
        }
        cur = node.input(0);
    }
    painted
}

#[cfg(test)]
#[path = "ops_paint_tests.rs"]
mod tests;
