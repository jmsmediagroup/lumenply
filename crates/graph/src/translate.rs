//! The `translate` op: content shifted by whole pixels, exactly as
//! [`TileStore::translated`](lumenply_tiles::TileStore::translated) shifts
//! a store (the Move tool on a pixel layer).

use std::sync::Arc;

use lumenply_tiles::{Rect, Tile, TileCoord, TILE_SIZE};

use crate::eval::Ctx;
use crate::model::NodeId;

/// Where `r` lands when shifted by `(dx, dy)`.
pub(crate) fn shift(r: Rect, dx: i32, dy: i32) -> Rect {
    Rect::new(r.x + dx, r.y + dy, r.w, r.h)
}

/// One output tile: the input pixels that land on it.
pub(crate) fn tile(
    ctx: &Ctx,
    input: Option<NodeId>,
    dx: i32,
    dy: i32,
    coord: TileCoord,
) -> Option<Arc<Tile>> {
    let t = TILE_SIZE as i32;
    if dx % t == 0 && dy % t == 0 {
        // Tile-aligned: the input's own tile, shared.
        return ctx.tile(input, TileCoord::new(coord.x - dx / t, coord.y - dy / t));
    }
    let (ox, oy) = coord.origin();
    let from = Rect::new(ox - dx, oy - dy, TILE_SIZE as u32, TILE_SIZE as u32);
    let sources: Vec<(TileCoord, Arc<Tile>)> = from
        .tiles()
        .into_iter()
        .filter_map(|c| ctx.tile(input, c).map(|t| (c, t)))
        .collect();
    if sources.is_empty() {
        return None;
    }
    let mut out = Tile::new();
    let dst = out.pixels_mut();
    for (c, tile) in &sources {
        let px = tile.pixels();
        let part = from.intersect(&c.rect());
        let (cx, cy) = c.origin();
        let w = part.w as usize;
        for y in part.y..part.bottom() {
            let s = (y - cy) as usize * TILE_SIZE + (part.x - cx) as usize;
            let d = (y + dy - oy) as usize * TILE_SIZE + (part.x + dx - ox) as usize;
            dst[d..d + w].copy_from_slice(&px[s..s + w]);
        }
    }
    if out.is_blank() {
        return None;
    }
    // Moving changes no value, so tiles at rest stay at rest (16-bit).
    if sources.iter().all(|(_, t)| t.is_compact()) {
        out.compact();
    }
    Some(Arc::new(out))
}
