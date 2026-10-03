//! Layer content as operations (ADR 0025, stage 2): text, fill and shape
//! layers, a smart object's transform, and smart filters (ADR 0011).
//!
//! Each is a whole-image computation in `lumenply-render`, the very function
//! the layer tree's derived caches come from (`text::rasterize`,
//! `fill::render_fill`, `shape::render_shape`, `transform_store`,
//! `smart_filters::render_smart_filters`), so a node's output is
//! bit-identical to the cache the layer tree would hold. These ops make
//! their whole output once per content key ([`Ctx::whole`]) and serve tiles
//! from it; a render makes them before it pulls any tile ([`prepare`]).
//!
//! Smart filters are one node each, but a run of them is evaluated in one
//! fused, chunked pass exactly as the layer tree renders a layer's stack:
//! the box blurs' running sums depend on where a pass starts, so filtering
//! node by node would differ in the last bits.

use std::collections::HashMap;
use std::sync::Arc;

use lumenply_doc::{Fill, Mask, SmartFilter, SmartFilters};
use lumenply_tiles::{Affine, Raster, Rect, Tile, TileCoord, TileStore, TILE_SIZE};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::blob::{BlobId, BlobStore, TileHasher};
use crate::eval::Ctx;
use crate::model::{Node, NodeId};
use crate::ops::Op;

pub(crate) fn is_false(v: &bool) -> bool {
    !*v
}

/// A pattern's pixels, for a pattern fill: a blob holding the pattern image
/// with its top-left corner at (0, 0), and the image's size. Patterns are
/// document-level pixel data, so ops name them by hash like any other
/// pixels; identical patterns are stored once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternPixels {
    pub blob: BlobId,
    /// Width and height in pixels.
    pub size: [u32; 2],
}

impl PatternPixels {
    /// Put `image` in the blob store: every pixel exactly as it is,
    /// transparent ones included (the sampler interpolates all four
    /// channels).
    pub fn store(image: &Raster, blobs: &mut BlobStore, hasher: &TileHasher) -> PatternPixels {
        let rect = Rect::new(0, 0, image.width, image.height);
        let mut store = TileStore::new();
        if image.pixels.len() == (image.width as usize) * (image.height as usize) {
            for c in rect.tiles() {
                let (ox, oy) = c.origin();
                let sub = rect.intersect(&c.rect());
                let mut tile = Tile::new();
                let px = tile.pixels_mut();
                let len = sub.w as usize;
                for y in sub.y..sub.bottom() {
                    let s = y as usize * image.width as usize + sub.x as usize;
                    let d = (y - oy) as usize * TILE_SIZE + (sub.x - ox) as usize;
                    px[d..d + len].copy_from_slice(&image.pixels[s..s + len]);
                }
                store.insert(c, Arc::new(tile));
            }
        }
        PatternPixels {
            blob: blobs.insert(hasher, store),
            size: [image.width, image.height],
        }
    }

    /// The pattern image, read back from the blob store.
    pub fn image(&self, blobs: &BlobStore) -> Option<Raster> {
        let store = blobs.get(&self.blob)?;
        Some(store.to_raster(Rect::new(0, 0, self.size[0], self.size[1])))
    }
}

/// A smart filter that changes nothing (opacity 0 or not a number, as the
/// layer tree skips it) hands its input through.
fn passes_through(op: &Op) -> bool {
    matches!(op, Op::SmartFilter { opacity, .. } if opacity.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater))
}

/// Ops whose output is made in one piece (see [`Ctx::whole`]).
fn is_whole(op: &Op) -> bool {
    match op {
        Op::Text { .. } | Op::Fill { .. } | Op::Shape { .. } | Op::Transform { .. } => true,
        Op::SmartFilter { .. } => !passes_through(op),
        _ => false,
    }
}

/// `fill` sampling the pattern pixels from the blob store: an op's own
/// pattern reference never carries pixels.
fn with_pattern(ctx: &Ctx, fill: &Fill, pixels: &Option<PatternPixels>) -> Fill {
    let mut fill = fill.clone();
    if let Fill::Pattern { pattern, .. } = &mut fill {
        pattern.image = pixels.as_ref().and_then(|p| p.image(ctx.blobs)).map(Arc::new);
    }
    fill
}

/// All of a node's output, tile for tile: what a layer-tree cache holding
/// it would hold.
fn whole_input(ctx: &Ctx, input: Option<NodeId>) -> TileStore {
    let Some(i) = input else {
        return TileStore::new();
    };
    match ctx.extent(i) {
        Some(r) => ctx.area(i, r),
        None => TileStore::new(),
    }
}

/// A run of smart filters fused into one pass, as the layer tree renders a
/// layer's filter stack.
struct Run {
    /// What the first filter reads (the unfiltered layer).
    base: Option<NodeId>,
    /// The filters that run, first first.
    filters: Vec<SmartFilter>,
    /// Fades the run's result back to `base`.
    mask: Option<NodeId>,
    float: bool,
}

/// The run ending at smart filter `top`: down through the smart filters
/// below it with the same precision and no mask of their own (one with a
/// mask ends its own run, and its output is this run's base). Filters that
/// pass their input through are stepped over.
fn run(ctx: &Ctx, top: NodeId) -> Option<Run> {
    let node = ctx.graph.node(top)?;
    let Op::SmartFilter { float, .. } = &node.op else {
        return None;
    };
    let mut filters = Vec::new();
    let mut cur = Some(top);
    while let Some(c) = cur {
        let Some(n) = ctx.graph.node(c) else {
            break;
        };
        let Op::SmartFilter {
            filter,
            opacity,
            blend,
            float: f,
        } = &n.op
        else {
            break;
        };
        let active = !passes_through(&n.op);
        if c != top && active && (f != float || n.input(1).is_some()) {
            break;
        }
        if active {
            filters.push(SmartFilter {
                filter: filter.clone(),
                enabled: true,
                opacity: *opacity,
                blend: *blend,
            });
        }
        cur = n.input(0);
    }
    filters.reverse();
    Some(Run {
        base: cur,
        filters,
        mask: node.input(1),
        float: *float,
    })
}

fn grow(r: Rect, pad: i32) -> Rect {
    let pad = pad.max(0);
    Rect::new(r.x - pad, r.y - pad, r.w + 2 * pad as u32, r.h + 2 * pad as u32)
}

/// The smart filter run ending at `top`, through
/// `lumenply_render::smart_filters` as the layer tree renders it.
fn smart_filters(ctx: &Ctx, top: NodeId) -> TileStore {
    let Some(run) = run(ctx, top) else {
        return TileStore::new();
    };
    let src = whole_input(ctx, run.base);
    let mut sf = SmartFilters {
        filters: run.filters,
        ..SmartFilters::default()
    };
    if let (Some(m), Some(b)) = (run.mask, src.bounds()) {
        // The render paints the tiles within one pad of the source and
        // reads around each chunk of them one pad further.
        let pad = sf.pad();
        let painted = grow(b, pad)
            .tiles()
            .iter()
            .map(|c| c.rect())
            .reduce(|a, b| a.union(&b))
            .unwrap_or(b);
        sf.mask = Some(Mask {
            tiles: ctx.area(m, grow(painted, pad)),
            default: 0.0,
            enabled: true,
        });
    }
    lumenply_render::smart_filters::render_smart_filters(&src, &sf, ctx.canvas, run.float)
}

/// A content op's whole output.
fn compute(ctx: &Ctx, id: NodeId, node: &Node) -> TileStore {
    match &node.op {
        Op::Text { text } => lumenply_render::text::rasterize(text),
        Op::Fill {
            fill,
            pattern_pixels,
            float,
        } => lumenply_render::fill::render_fill(&with_pattern(ctx, fill, pattern_pixels), ctx.canvas, *float),
        Op::Shape {
            shape,
            pattern_pixels,
            float,
        } => {
            let mut shape = shape.as_ref().clone();
            shape.fill = shape.fill.map(|f| with_pattern(ctx, &f, pattern_pixels));
            lumenply_render::shape::render_shape(&shape, ctx.canvas, *float)
        }
        Op::Transform { matrix } => {
            lumenply_render::transform_store(&whole_input(ctx, node.input(0)), &Affine::from_coeffs(*matrix))
        }
        Op::SmartFilter { .. } => smart_filters(ctx, id),
        _ => TileStore::new(),
    }
}

/// One tile of a content op, from its whole output.
pub(crate) fn eval_tile(ctx: &Ctx, id: NodeId, node: &Node, coord: TileCoord) -> Option<Arc<Tile>> {
    if passes_through(&node.op) {
        return ctx.tile(node.input(0), coord);
    }
    ctx.whole(id, || compute(ctx, id, node)).tile_arc(coord).cloned()
}

/// Where a content op's output can be non-transparent: the tiles of its
/// whole output (so effects laid out over a layer's bounds read all of it).
pub(crate) fn extent(ctx: &Ctx, id: NodeId, node: &Node) -> Option<Rect> {
    if passes_through(&node.op) {
        return node.input(0).and_then(|i| ctx.extent(i));
    }
    ctx.whole(id, || compute(ctx, id, node)).bounds()
}

/// The nodes evaluating `node` reads: its inputs, except that a hidden
/// layer reads only its backdrop and a smart filter reads its run's base
/// and mask (the filters below it are fused into it).
fn reads(ctx: &Ctx, id: NodeId, node: &Node) -> Vec<NodeId> {
    let backdrop_only = match &node.op {
        Op::Layer { props } => !props.visible || props.opacity <= 0.0,
        Op::Adjustment { visible, opacity, .. }
        | Op::FilterLayer { visible, opacity, .. }
        | Op::PassThrough { visible, opacity } => !*visible || *opacity <= 0.0,
        Op::ClipGroup { base, .. } => !base.visible || base.opacity <= 0.0,
        Op::SmartFilter { .. } if !passes_through(&node.op) => {
            return run(ctx, id)
                .map(|r| r.base.into_iter().chain(r.mask).collect())
                .unwrap_or_default();
        }
        _ => false,
    };
    if backdrop_only {
        node.input(0).into_iter().collect()
    } else {
        node.inputs.iter().flatten().copied().collect()
    }
}

/// Make the whole outputs of the content ops a render of `root` reads, in
/// dependency order and independent ones in parallel, so that no tile ever
/// waits for one (or computes one twice). Results already kept are skipped.
pub(crate) fn prepare(ctx: &Ctx, root: NodeId) {
    // Post-order over what is read. A node's level is the most whole
    // outputs any path below it passes through; a whole node is made once
    // everything on the levels below its own is.
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
        let reads = reads(ctx, id, node);
        if !ready {
            stack.push((id, true));
            stack.extend(
                reads
                    .into_iter()
                    .filter(|r| !level.contains_key(r))
                    .map(|r| (r, false)),
            );
            continue;
        }
        let below = reads
            .iter()
            .filter_map(|r| level.get(r))
            .copied()
            .max()
            .unwrap_or(0);
        let whole = is_whole(&node.op);
        if whole {
            if by_level.len() <= below {
                by_level.resize(below + 1, Vec::new());
            }
            by_level[below].push(id);
        }
        level.insert(id, below + usize::from(whole));
    }
    for ids in by_level {
        ids.par_iter().for_each(|&id| {
            if let Some(node) = ctx.graph.node(id) {
                ctx.whole(id, || compute(ctx, id, node));
            }
        });
    }
}
