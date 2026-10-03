//! Affine transformation of tile stores with bilinear resampling.
//!
//! Each destination tile is produced independently (in parallel): every
//! destination pixel centre is mapped back through the inverse transform and
//! the four nearest source pixels are blended. Sampling premultiplied colour
//! is what makes edges against transparency come out right.

use std::sync::Arc;

use lumenply_tiles::{Affine, Rect, Rgba, Tile, TileCoord, TileStore, TILE_SIZE};
use rayon::prelude::*;

/// Transform a store. Whole-pixel translations are exact and cheap; anything
/// else is resampled bilinearly. Returns an empty store for a singular matrix.
pub fn transform_store(src: &TileStore, t: &Affine) -> TileStore {
    if let Some((dx, dy)) = t.integer_translation() {
        return src.translated(dx, dy);
    }
    let Some(inv) = t.inverse() else {
        return TileStore::new();
    };
    let Some(src_bounds) = src.content_bounds() else {
        return TileStore::new();
    };
    if t.is_pixel_exact() {
        return remap_exact(src, t, &inv, src_bounds);
    }
    let dst_bounds = t.transform_rect(src_bounds);
    let padded = Rect::new(
        dst_bounds.x - 1,
        dst_bounds.y - 1,
        dst_bounds.w + 2,
        dst_bounds.h + 2,
    );

    let tiles: Vec<(TileCoord, Option<Tile>)> = padded
        .tiles()
        .into_par_iter()
        .map(|c| (c, resample_tile(src, &inv, c, src_bounds)))
        .collect();

    let mut out = TileStore::new();
    for (c, tile) in tiles {
        if let Some(t) = tile {
            out.insert(c, Arc::new(t));
        }
    }
    out
}

/// Transform a mask's coverage, honouring its `default`.
///
/// [`transform_store`] assumes "no tile" and "alpha 0" both mean empty, which
/// holds for layer pixels but not for masks: with a non-zero default, an
/// all-zero tile is painted-hidden content (it would be pruned) and the area
/// outside the tiles means `default` (it would come back 0). Transforming the
/// complement instead makes 0 mean "default" again, so the plain store
/// transform applies. Masks have a default of exactly 0 or 1.
pub fn transform_mask(mask: &lumenply_doc::Mask, t: &Affine) -> lumenply_doc::Mask {
    let tiles = if mask.default == 0.0 {
        transform_store(&mask.tiles, t)
    } else {
        complement(&transform_store(&complement(&mask.tiles), t))
    };
    let mut out = lumenply_doc::Mask {
        tiles,
        default: mask.default,
        enabled: mask.enabled,
    };
    out.prune_uniform();
    out
}

/// Pixelwise 1 − v over the tiles of a coverage store (missing tiles stay
/// missing).
fn complement(src: &TileStore) -> TileStore {
    let mut out = TileStore::new();
    for c in src.coords() {
        let Some(tile) = src.tile(c) else { continue };
        let px = tile.pixels();
        let mut nt = Tile::new();
        for (p, s) in nt.pixels_mut().iter_mut().zip(px.iter()) {
            let v = 1.0 - s.a;
            *p = Rgba::new(v, v, v, v);
        }
        out.insert(c, Arc::new(nt));
    }
    out
}

/// Exact pixel shuffle for 90° rotations and mirrors: every destination
/// pixel centre maps onto exactly one source pixel centre.
fn remap_exact(src: &TileStore, t: &Affine, inv: &Affine, src_bounds: Rect) -> TileStore {
    let dst_bounds = t.transform_rect(src_bounds);
    let tiles: Vec<(TileCoord, Option<Tile>)> = dst_bounds
        .tiles()
        .into_par_iter()
        .map(|c| {
            let (ox, oy) = c.origin();
            let mut tile = Tile::new();
            let mut any = false;
            for row in 0..TILE_SIZE {
                for col in 0..TILE_SIZE {
                    let (px, py) = (ox + col as i32, oy + row as i32);
                    if !dst_bounds.contains(px, py) {
                        continue;
                    }
                    let (sx, sy) = inv.apply(px as f32 + 0.5, py as f32 + 0.5);
                    let p = src.get_pixel(sx.floor() as i32, sy.floor() as i32);
                    if p.a > 0.0 {
                        any = true;
                        tile.set(col, row, p);
                    }
                }
            }
            (c, any.then_some(tile))
        })
        .collect();
    let mut out = TileStore::new();
    for (c, tile) in tiles {
        if let Some(t) = tile {
            out.insert(c, Arc::new(t));
        }
    }
    out
}

fn resample_tile(src: &TileStore, inv: &Affine, coord: TileCoord, src_bounds: Rect) -> Option<Tile> {
    let (ox, oy) = coord.origin();
    // Quick reject: if the whole tile maps outside the source bounds, skip it.
    let back = inv.transform_rect(coord.rect());
    let reach = Rect::new(back.x - 1, back.y - 1, back.w + 2, back.h + 2);
    if reach.intersect(&src_bounds).is_empty() {
        return None;
    }

    // Shrinking: one destination pixel covers several source pixels, and a
    // single bilinear sample would alias (moiré, jagged edges). Average a
    // grid of samples across the pixel's footprint instead — a box filter
    // the footprint's size. Rotations and enlargements keep one sample.
    let taps = |len: f32| {
        if len <= 1.001 {
            1
        } else {
            (len.ceil() as usize).min(8)
        }
    };
    let (nx, ny) = (taps(inv.a.hypot(inv.b)), taps(inv.c.hypot(inv.d)));
    let weight = 1.0 / (nx * ny) as f32;
    let mut tile = Tile::new();
    let mut any = false;
    for row in 0..TILE_SIZE {
        let py = oy + row as i32;
        for col in 0..TILE_SIZE {
            let px = ox + col as i32;
            let p = if nx == 1 && ny == 1 {
                let (sx, sy) = inv.apply(px as f32 + 0.5, py as f32 + 0.5);
                sample_bilinear(src, sx - 0.5, sy - 0.5)
            } else {
                let mut acc = [0f32; 4];
                for j in 0..ny {
                    for i in 0..nx {
                        let (fx, fy) = ((i as f32 + 0.5) / nx as f32, (j as f32 + 0.5) / ny as f32);
                        let (sx, sy) = inv.apply(px as f32 + fx, py as f32 + fy);
                        let q = sample_bilinear(src, sx - 0.5, sy - 0.5);
                        acc[0] += q.r;
                        acc[1] += q.g;
                        acc[2] += q.b;
                        acc[3] += q.a;
                    }
                }
                Rgba::new(acc[0] * weight, acc[1] * weight, acc[2] * weight, acc[3] * weight)
            };
            if p.a > 0.0 {
                any = true;
                tile.set(col, row, p);
            }
        }
    }
    any.then_some(tile)
}

/// A 3×3 projective transform (homography), row-major.
#[derive(Clone, Copy, Debug)]
pub struct Homography {
    pub m: [f32; 9],
}

impl Homography {
    /// Map the corners of `src` onto `quad`, given as (top-left,
    /// top-right, bottom-right, bottom-left). `None` for degenerate quads.
    pub fn rect_to_quad(src: Rect, quad: [(f32, f32); 4]) -> Option<Homography> {
        // Unit square → quad (classic closed form), then compose with the
        // rect → unit-square scale.
        let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = quad;
        let (dx1, dy1) = (x1 - x2, y1 - y2);
        let (dx2, dy2) = (x3 - x2, y3 - y2);
        let (dx3, dy3) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
        let (a, b, c, d, e, f, g, h);
        if dx3.abs() < 1e-6 && dy3.abs() < 1e-6 {
            // Affine case.
            a = x1 - x0;
            b = x2 - x1;
            c = x0;
            d = y1 - y0;
            e = y2 - y1;
            f = y0;
            g = 0.0;
            h = 0.0;
        } else {
            let den = dx1 * dy2 - dx2 * dy1;
            if den.abs() < 1e-9 {
                return None;
            }
            g = (dx3 * dy2 - dx2 * dy3) / den;
            h = (dx1 * dy3 - dx3 * dy1) / den;
            a = x1 - x0 + g * x1;
            b = x3 - x0 + h * x3;
            c = x0;
            d = y1 - y0 + g * y1;
            e = y3 - y0 + h * y3;
            f = y0;
        }
        let unit = Homography {
            m: [a, b, c, d, e, f, g, h, 1.0],
        };
        if src.w == 0 || src.h == 0 {
            return None;
        }
        let (sx, sy) = (1.0 / src.w as f32, 1.0 / src.h as f32);
        let norm = Homography {
            m: [
                sx,
                0.0,
                -(src.x as f32) * sx,
                0.0,
                sy,
                -(src.y as f32) * sy,
                0.0,
                0.0,
                1.0,
            ],
        };
        Some(unit.compose(&norm))
    }

    /// `self` after `other` (matrix product `self · other`).
    pub fn compose(&self, other: &Homography) -> Homography {
        let (a, b) = (&self.m, &other.m);
        let mut m = [0f32; 9];
        for r in 0..3 {
            for c in 0..3 {
                m[r * 3 + c] = a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c] + a[r * 3 + 2] * b[6 + c];
            }
        }
        Homography { m }
    }

    pub fn inverse(&self) -> Option<Homography> {
        let m = &self.m;
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = |i: usize, j: usize, k: usize, l: usize| (m[i] * m[j] - m[k] * m[l]) / det;
        Some(Homography {
            m: [
                inv(4, 8, 5, 7),
                inv(2, 7, 1, 8),
                inv(1, 5, 2, 4),
                inv(5, 6, 3, 8),
                inv(0, 8, 2, 6),
                inv(2, 3, 0, 5),
                inv(3, 7, 4, 6),
                inv(1, 6, 0, 7),
                inv(0, 4, 1, 3),
            ],
        })
    }

    /// Apply with the perspective division; `None` behind the horizon.
    #[inline]
    pub fn apply(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let m = &self.m;
        let w = m[6] * x + m[7] * y + m[8];
        if w.abs() < 1e-6 {
            return None;
        }
        Some(((m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w))
    }
}

/// Resample a store so its content bounds land on `quad` (tl, tr, br, bl),
/// bilinearly. Returns an empty store for degenerate quads.
pub fn perspective_store(src: &TileStore, quad: [(f32, f32); 4]) -> TileStore {
    let Some(src_bounds) = src.content_bounds() else {
        return TileStore::new();
    };
    match Homography::rect_to_quad(src_bounds, quad) {
        Some(h) => perspective_store_h(src, &h),
        None => TileStore::new(),
    }
}

/// Resample a store through an explicit homography (used so a layer's mask
/// warps through the same mapping as its pixels).
pub fn perspective_store_h(src: &TileStore, h: &Homography) -> TileStore {
    let Some(src_bounds) = src.content_bounds() else {
        return TileStore::new();
    };
    let Some(inv) = h.inverse() else {
        return TileStore::new();
    };
    // Destination bounds: the mapped source corners' bounding box.
    let corners = [
        (src_bounds.x as f32, src_bounds.y as f32),
        (src_bounds.right() as f32, src_bounds.y as f32),
        (src_bounds.right() as f32, src_bounds.bottom() as f32),
        (src_bounds.x as f32, src_bounds.bottom() as f32),
    ];
    let (mut x0, mut y0, mut x1, mut y1) =
        (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for (cx, cy) in corners {
        let Some((mx, my)) = h.apply(cx, cy) else {
            return TileStore::new();
        };
        x0 = x0.min(mx);
        y0 = y0.min(my);
        x1 = x1.max(mx);
        y1 = y1.max(my);
    }
    if !(x0.is_finite() && y0.is_finite() && x1 > x0 && y1 > y0) {
        return TileStore::new();
    }
    let dst_bounds = Rect::new(
        x0.floor() as i32 - 1,
        y0.floor() as i32 - 1,
        (x1 - x0).ceil() as u32 + 2,
        (y1 - y0).ceil() as u32 + 2,
    );
    let reach = Rect::new(
        src_bounds.x - 1,
        src_bounds.y - 1,
        src_bounds.w + 2,
        src_bounds.h + 2,
    );
    let tiles: Vec<(TileCoord, Option<Tile>)> = dst_bounds
        .tiles()
        .into_par_iter()
        .map(|c| {
            let (ox, oy) = c.origin();
            let mut tile = Tile::new();
            let mut any = false;
            for row in 0..TILE_SIZE {
                let py = oy + row as i32;
                for col in 0..TILE_SIZE {
                    let px = ox + col as i32;
                    if !dst_bounds.contains(px, py) {
                        continue;
                    }
                    let Some((sx, sy)) = inv.apply(px as f32 + 0.5, py as f32 + 0.5) else {
                        continue;
                    };
                    if !reach.contains(sx.floor() as i32, sy.floor() as i32) {
                        continue;
                    }
                    let p = sample_bilinear(src, sx - 0.5, sy - 0.5);
                    if p.a > 0.0 {
                        any = true;
                        tile.set(col, row, p);
                    }
                }
            }
            (c, any.then_some(tile))
        })
        .collect();
    let mut out = TileStore::new();
    for (c, tile) in tiles {
        if let Some(t) = tile {
            out.insert(c, Arc::new(t));
        }
    }
    out
}

/// Perspective-warp a mask's coverage (same complement trick as
/// [`transform_mask`]), through the layer's homography so mask and pixels
/// stay registered.
pub fn perspective_mask(mask: &lumenply_doc::Mask, h: &Homography) -> lumenply_doc::Mask {
    let tiles = if mask.default == 0.0 {
        perspective_store_h(&mask.tiles, h)
    } else {
        complement(&perspective_store_h(&complement(&mask.tiles), h))
    };
    let mut out = lumenply_doc::Mask {
        tiles,
        default: mask.default,
        enabled: mask.enabled,
    };
    out.prune_uniform();
    out
}

/// A warp mesh: `(cols + 1) × (rows + 1)` destination points, row-major,
/// for a regular grid laid over `src`. Point `(i, j)` is where the grid
/// intersection at column line `i`, row line `j` of `src` lands.
#[derive(Clone, Debug, PartialEq)]
pub struct WarpGrid {
    pub src: Rect,
    pub cols: usize,
    pub rows: usize,
    pub points: Vec<(f32, f32)>,
}

impl WarpGrid {
    /// The undistorted grid over `src`.
    pub fn identity(src: Rect, cols: usize, rows: usize) -> WarpGrid {
        let mut points = Vec::with_capacity((cols + 1) * (rows + 1));
        for j in 0..=rows {
            for i in 0..=cols {
                points.push((
                    src.x as f32 + src.w as f32 * i as f32 / cols.max(1) as f32,
                    src.y as f32 + src.h as f32 * j as f32 / rows.max(1) as f32,
                ));
            }
        }
        WarpGrid {
            src,
            cols,
            rows,
            points,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.cols >= 1
            && self.rows >= 1
            && self.cols <= 64
            && self.rows <= 64
            && !self.src.is_empty()
            && self.points.len() == (self.cols + 1) * (self.rows + 1)
            && self.points.iter().all(|p| p.0.is_finite() && p.1.is_finite())
    }

    #[inline]
    pub fn point(&self, i: usize, j: usize) -> (f32, f32) {
        self.points[j * (self.cols + 1) + i]
    }
}

/// Inverse of the bilinear map of quad `(a, b, c, d)` = corners at
/// (u,v) = (0,0), (1,0), (1,1), (0,1): the `(u, v)` that lands on `p`, if
/// it lies inside the quad (closed form; Quilez's formulation).
fn inverse_bilinear(
    p: (f32, f32),
    a: (f32, f32),
    b: (f32, f32),
    c: (f32, f32),
    d: (f32, f32),
) -> Option<(f32, f32)> {
    let cross = |x: (f32, f32), y: (f32, f32)| x.0 * y.1 - x.1 * y.0;
    let e = (b.0 - a.0, b.1 - a.1);
    let f = (d.0 - a.0, d.1 - a.1);
    let g = (a.0 - b.0 + c.0 - d.0, a.1 - b.1 + c.1 - d.1);
    let h = (p.0 - a.0, p.1 - a.1);
    let k2 = cross(g, f);
    let k1 = cross(e, f) + cross(h, g);
    let k0 = cross(h, e);
    // u from v, through whichever axis is better conditioned.
    let u_of = |v: f32| {
        let (dx, dy) = (e.0 + g.0 * v, e.1 + g.1 * v);
        if dx.abs() >= dy.abs() {
            (h.0 - f.0 * v) / dx
        } else {
            (h.1 - f.1 * v) / dy
        }
    };
    const EPS: f32 = 1e-4;
    let inside = |u: f32, v: f32| (-EPS..=1.0 + EPS).contains(&u) && (-EPS..=1.0 + EPS).contains(&v);
    if k2.abs() < 1e-6 * (k1.abs() + 1.0) {
        // Parallelogram-like cell: the quadratic degenerates to linear.
        if k1.abs() < 1e-12 {
            return None;
        }
        let v = -k0 / k1;
        let u = u_of(v);
        return inside(u, v).then_some((u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)));
    }
    let disc = k1 * k1 - 4.0 * k0 * k2;
    if disc < 0.0 {
        return None;
    }
    let w = disc.sqrt();
    for v in [(-k1 - w) / (2.0 * k2), (-k1 + w) / (2.0 * k2)] {
        let u = u_of(v);
        if u.is_finite() && inside(u, v) {
            return Some((u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)));
        }
    }
    None
}

/// Resample `src` through a warp mesh: each destination pixel finds the
/// cell covering it, inverts that cell's bilinear map, and samples the
/// source bilinearly at the matching spot of the regular source grid.
/// Content outside `grid.src` is dropped (the mesh defines no mapping
/// there). Returns an empty store for an invalid grid.
pub fn warp_store(src: &TileStore, grid: &WarpGrid) -> TileStore {
    if !grid.is_valid() {
        return TileStore::new();
    }
    struct Cell {
        i: usize,
        j: usize,
        quad: [(f32, f32); 4],
        bounds: Rect,
    }
    let mut cells = Vec::with_capacity(grid.cols * grid.rows);
    for j in 0..grid.rows {
        for i in 0..grid.cols {
            let quad = [
                grid.point(i, j),
                grid.point(i + 1, j),
                grid.point(i + 1, j + 1),
                grid.point(i, j + 1),
            ];
            let x0 = quad.iter().map(|p| p.0).fold(f32::INFINITY, f32::min).floor() as i32 - 1;
            let y0 = quad.iter().map(|p| p.1).fold(f32::INFINITY, f32::min).floor() as i32 - 1;
            let x1 = quad.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 1;
            let y1 = quad.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 1;
            cells.push(Cell {
                i,
                j,
                quad,
                bounds: Rect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32),
            });
        }
    }
    let dst_bounds = cells
        .iter()
        .map(|c| c.bounds)
        .reduce(|a, b| a.union(&b))
        .unwrap_or_default();
    let (sw, sh) = (grid.src.w as f32, grid.src.h as f32);
    let (cols, rows) = (grid.cols as f32, grid.rows as f32);
    let tiles: Vec<(TileCoord, Option<Tile>)> = dst_bounds
        .tiles()
        .into_par_iter()
        .map(|tc| {
            let tr = tc.rect();
            let (ox, oy) = tc.origin();
            let mut tile = Tile::new();
            let mut any = false;
            for cell in cells.iter().filter(|c| !c.bounds.intersect(&tr).is_empty()) {
                let r = cell.bounds.intersect(&tr);
                let [a, b, c, d] = cell.quad;
                for py in r.y..r.bottom() {
                    for px in r.x..r.right() {
                        let q = (px as f32 + 0.5, py as f32 + 0.5);
                        let Some((u, v)) = inverse_bilinear(q, a, b, c, d) else {
                            continue;
                        };
                        let sx = grid.src.x as f32 + (cell.i as f32 + u) / cols * sw;
                        let sy = grid.src.y as f32 + (cell.j as f32 + v) / rows * sh;
                        let p = sample_bilinear(src, sx - 0.5, sy - 0.5);
                        if p.a > 0.0 {
                            any = true;
                            tile.set((px - ox) as usize, (py - oy) as usize, p);
                        }
                    }
                }
            }
            (tc, any.then_some(tile))
        })
        .collect();
    let mut out = TileStore::new();
    for (c, tile) in tiles {
        if let Some(t) = tile {
            out.insert(c, Arc::new(t));
        }
    }
    out
}

/// Warp a mask's coverage through the same mesh as its layer.
pub fn warp_mask(mask: &lumenply_doc::Mask, grid: &WarpGrid) -> lumenply_doc::Mask {
    let tiles = if mask.default == 0.0 {
        warp_store(&mask.tiles, grid)
    } else {
        complement(&warp_store(&complement(&mask.tiles), grid))
    };
    let mut out = lumenply_doc::Mask {
        tiles,
        default: mask.default,
        enabled: mask.enabled,
    };
    out.prune_uniform();
    out
}

/// Bilinear sample at a continuous source position (pixel centres at
/// integer coordinates).
#[inline]
pub fn sample_bilinear(src: &TileStore, x: f32, y: f32) -> Rgba {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let (ix, iy) = (x0 as i32, y0 as i32);
    let p00 = src.get_pixel(ix, iy);
    let p10 = src.get_pixel(ix + 1, iy);
    let p01 = src.get_pixel(ix, iy + 1);
    let p11 = src.get_pixel(ix + 1, iy + 1);
    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;
    Rgba::new(
        p00.r * w00 + p10.r * w10 + p01.r * w01 + p11.r * w11,
        p00.g * w00 + p10.g * w10 + p01.g * w01 + p11.g * w11,
        p00.b * w00 + p10.b * w10 + p01.b * w01 + p11.b * w11,
        p00.a * w00 + p10.a * w10 + p01.a * w01 + p11.a * w11,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::Mask;
    use lumenply_tiles::Raster;

    #[test]
    fn shrinking_averages_instead_of_aliasing() {
        // One-pixel black/white stripes: any single sample per output pixel
        // lands on one stripe or between two; the true average is 0.5.
        let mut r = Raster::new(64, 64);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            let v = if (i % 64) % 2 == 0 { 1.0 } else { 0.0 };
            *p = Rgba::new(v, v, v, 1.0);
        }
        let src = TileStore::from_raster(&r, 0, 0);
        for k in [2.0f32, 4.0] {
            let out = transform_store(&src, &Affine::scale(1.0 / k, 1.0 / k));
            for x in 1..(64 / k as i32 - 1) {
                let p = out.get_pixel(x, 2);
                assert!((p.r - 0.5).abs() < 0.02, "1/{k} at x = {x}: {}", p.r);
                assert!((p.a - 1.0).abs() < 1e-4);
            }
        }
        // A third: each output pixel covers three stripes, so 1/3 or 2/3.
        let out = transform_store(&src, &Affine::scale(1.0 / 3.0, 1.0 / 3.0));
        for x in 1..20 {
            let v = out.get_pixel(x, 2).r;
            assert!(
                (v - 1.0 / 3.0).abs() < 0.02 || (v - 2.0 / 3.0).abs() < 0.02,
                "x = {x}: {v}"
            );
        }
        // Rotations keep a single sample: a 90° turn stays exact.
        let turned = transform_store(
            &src,
            &Affine::rotate(std::f32::consts::FRAC_PI_2).then(&Affine::translate(64.0, 0.0)),
        );
        assert!(turned.get_pixel(10, 10).r == 0.0 || turned.get_pixel(10, 10).r == 1.0);
    }

    /// A reveal-all mask with a painted-hidden rect must keep hiding that
    /// rect after any transform, and keep revealing everywhere else. The
    /// plain store transform would prune the zero tiles (hidden = a 0) and
    /// treat everything outside them as 0 too.
    #[test]
    fn homography_maps_corners_and_identity_quad_is_identity() {
        let src = Rect::new(10, 20, 40, 30);
        let quad = [(100.0, 50.0), (180.0, 60.0), (170.0, 140.0), (95.0, 120.0)];
        let h = Homography::rect_to_quad(src, quad).unwrap();
        let corners = [
            (10.0, 20.0, quad[0]),
            (50.0, 20.0, quad[1]),
            (50.0, 50.0, quad[2]),
            (10.0, 50.0, quad[3]),
        ];
        for (x, y, (ex, ey)) in corners {
            let (mx, my) = h.apply(x, y).unwrap();
            assert!(
                (mx - ex).abs() < 1e-3 && (my - ey).abs() < 1e-3,
                "corner ({x},{y}) → ({mx},{my}), expected ({ex},{ey})"
            );
        }
        // Inverse round-trips an interior point.
        let inv = h.inverse().unwrap();
        let (mx, my) = h.apply(30.0, 35.0).unwrap();
        let (bx, by) = inv.apply(mx, my).unwrap();
        assert!((bx - 30.0).abs() < 1e-3 && (by - 35.0).abs() < 1e-3);

        // The identity quad resamples to (almost) the same pixels.
        let store = checker(32, 32);
        let b = store.content_bounds().unwrap();
        let same = [
            (b.x as f32, b.y as f32),
            (b.right() as f32, b.y as f32),
            (b.right() as f32, b.bottom() as f32),
            (b.x as f32, b.bottom() as f32),
        ];
        let out = perspective_store(&store, same);
        for y in 0..32 {
            for x in 0..32 {
                let (a, c) = (store.get_pixel(x, y), out.get_pixel(x, y));
                assert!((a.r - c.r).abs() < 1e-3 && (a.a - c.a).abs() < 1e-3, "at {x},{y}");
            }
        }
    }

    #[test]
    fn inverse_bilinear_recovers_parameters() {
        // A skewed, non-parallelogram quad: map known (u, v) forward, then
        // invert.
        let (a, b, c, d) = ((0.0, 0.0), (10.0, 1.0), (12.0, 9.0), (-1.0, 8.0));
        let fwd = |u: f32, v: f32| {
            let lerp = |p: (f32, f32), q: (f32, f32), t: f32| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
            let top = lerp(a, b, u);
            let bot = lerp(d, c, u);
            lerp(top, bot, v)
        };
        for &(u, v) in &[(0.25, 0.25), (0.5, 0.5), (0.9, 0.1), (0.1, 0.8)] {
            let (iu, iv) = inverse_bilinear(fwd(u, v), a, b, c, d).unwrap();
            assert!(
                (iu - u).abs() < 1e-3 && (iv - v).abs() < 1e-3,
                "({u},{v}) -> ({iu},{iv})"
            );
        }
        assert!(
            inverse_bilinear((50.0, 50.0), a, b, c, d).is_none(),
            "outside the quad"
        );
    }

    #[test]
    fn identity_warp_reproduces_and_a_moved_centre_bends_the_middle() {
        // A 40×40 square with a single bright vertical stripe at x = 20.
        let mut r = Raster::new(40, 40);
        for y in 0..40 {
            for x in 0..40 {
                let v = if x == 20 { 1.0 } else { 0.2 };
                r.set(x, y, Rgba::from_straight(v, v, v, 1.0));
            }
        }
        let store = TileStore::from_raster(&r, 0, 0);
        let src = store.content_bounds().unwrap();

        let id = WarpGrid::identity(src, 2, 2);
        let out = warp_store(&store, &id);
        for y in 0..40 {
            for x in 0..40 {
                let (p, q) = (store.get_pixel(x, y), out.get_pixel(x, y));
                assert!(
                    (p.r - q.r).abs() < 1e-3 && (p.a - q.a).abs() < 1e-3,
                    "identity at {x},{y}"
                );
            }
        }

        // Push the centre point (index 4 of the 3×3 grid) right by 8 px.
        let mut bent = WarpGrid::identity(src, 2, 2);
        bent.points[4].0 += 8.0;
        let out = warp_store(&store, &bent);
        let brightest = |y: i32| {
            (0..40).max_by(|&a, &b| out.get_pixel(a, y).r.partial_cmp(&out.get_pixel(b, y).r).unwrap())
        };
        assert_eq!(
            brightest(20),
            Some(28),
            "stripe follows the centre point at mid-height"
        );
        assert_eq!(brightest(0), Some(20), "the pinned top edge keeps the stripe");
        assert_eq!(brightest(39), Some(20), "the pinned bottom edge keeps the stripe");
        // The outline is unchanged: the corners and edge points did not move.
        let b = out.content_bounds().unwrap();
        assert_eq!((b.x, b.y, b.w, b.h), (0, 0, 40, 40));

        // Invalid grids produce nothing rather than panicking.
        let mut bad = WarpGrid::identity(src, 2, 2);
        bad.points.pop();
        assert!(warp_store(&store, &bad).is_empty());
    }

    #[test]
    fn perspective_squeezes_the_far_edge() {
        // A solid 40×40 square warped so the top edge narrows to half:
        // classic "lean back" keystone.
        let mut r = Raster::new(40, 40);
        for y in 0..40 {
            for x in 0..40 {
                r.set(x, y, Rgba::from_straight(1.0, 0.0, 0.0, 1.0));
            }
        }
        let store = TileStore::from_raster(&r, 0, 0);
        let quad = [(10.0, 0.0), (30.0, 0.0), (40.0, 40.0), (0.0, 40.0)];
        let out = perspective_store(&store, quad);
        // Top row: filled strictly between x=10 and x=30, empty outside.
        assert!(out.get_pixel(20, 1).a > 0.9, "top centre filled");
        assert!(out.get_pixel(5, 1).a < 0.05, "top left outside the keystone");
        assert!(out.get_pixel(35, 1).a < 0.05, "top right outside the keystone");
        // Bottom row spans the full width.
        assert!(out.get_pixel(2, 38).a > 0.9 && out.get_pixel(38, 38).a > 0.9);
        // The warped content stays inside the quad's bounding box.
        let b = out.content_bounds().unwrap();
        assert!(
            b.x >= -2 && b.right() <= 42 && b.y >= -2 && b.bottom() <= 42,
            "{b:?}"
        );
    }

    #[test]
    fn mask_transform_respects_the_default() {
        let mut m = Mask::reveal_all();
        m.fill_rect(Rect::new(10, 10, 40, 40), 0.0);

        // Unaligned integer translation (the `translated` + prune path).
        let t = transform_mask(&m, &Affine::translate(30.0, 7.0));
        assert_eq!(t.default, 1.0);
        assert_eq!(t.value(60, 30), 0.0, "hidden rect moved");
        assert_eq!(t.value(20, 20), 1.0, "old spot revealed again");
        assert_eq!(t.value(5000, 5000), 1.0, "far away stays default");

        // Pixel-exact quarter turn about the canvas-ish centre.
        let rot = Affine::rotate(std::f32::consts::FRAC_PI_2).then(&Affine::translate(100.0, 0.0));
        assert!(rot.is_pixel_exact());
        let r = transform_mask(&m, &rot);
        // (x, y) → (99 - y, x) for pixel centres: (30, 30) → (69, 30).
        assert_eq!(r.value(69, 30), 0.0, "hidden rect rotated");
        assert_eq!(r.value(30, 30), 1.0);
        assert_eq!(r.value(-5000, 5000), 1.0, "far away stays default");

        // Bilinear resample path.
        let s = transform_mask(&m, &Affine::around(30.0, 30.0, 1.0, 1.0, 0.3));
        assert!(s.value(30, 30) < 0.05, "hidden centre stays hidden");
        assert_eq!(s.value(5000, 5000), 1.0, "far away stays default");

        // A hide-all mask with a revealed rect (default 0) still works.
        let mut h = Mask::hide_all();
        h.fill_rect(Rect::new(10, 10, 40, 40), 1.0);
        let th = transform_mask(&h, &Affine::translate(30.0, 7.0));
        assert_eq!(th.value(60, 30), 1.0);
        assert_eq!(th.value(5000, 5000), 0.0);
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn checker(w: u32, h: u32) -> TileStore {
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 8 + y / 8) % 2 == 0 { 1.0 } else { 0.25 };
                r.set(x, y, Rgba::new(v, v * 0.5, 0.1, 1.0));
            }
        }
        TileStore::from_raster(&r, 0, 0)
    }

    #[test]
    fn identity_and_integer_moves_are_exact() {
        let src = checker(100, 60);
        let same = transform_store(&src, &Affine::IDENTITY);
        assert_eq!(same.get_pixel(37, 21), src.get_pixel(37, 21));
        let moved = transform_store(&src, &Affine::translate(300.0, -7.0));
        assert_eq!(moved.get_pixel(337, 14), src.get_pixel(37, 21));
    }

    #[test]
    fn rotation_by_90_degrees_maps_corners() {
        let src = checker(40, 20);
        // Rotate around the centre; the result is a 20×40 block centred at the same point.
        let t = Affine::around(20.0, 10.0, 1.0, 1.0, std::f32::consts::FRAC_PI_2);
        let out = transform_store(&src, &t);
        let b = out.bounds().unwrap().intersect(&Rect::new(-100, -100, 400, 400));
        assert!(b.contains(15, 0) && b.contains(25, 29), "{b:?}");
        // The pixel at source (0,0) lands near (29, 0) after a CCW quarter turn in y-down coords.
        let p = out.get_pixel(29, 0);
        assert!(p.a > 0.9, "rotated corner missing: {p:?}");
        assert!(out.get_pixel(2, 2).is_transparent(), "outside the rotated block");
        // Nothing invented: total alpha is conserved to within resampling tolerance.
        let sum = |s: &TileStore| {
            let bb = s.bounds().unwrap();
            let mut a = 0.0;
            for y in bb.y..bb.bottom() {
                for x in bb.x..bb.right() {
                    a += s.get_pixel(x, y).a;
                }
            }
            a
        };
        assert!((sum(&out) - sum(&src)).abs() / sum(&src) < 0.02);
    }

    #[test]
    fn quarter_turns_and_mirrors_are_exact() {
        let src = checker(37, 21);
        let t = Affine::rotate(std::f32::consts::FRAC_PI_2).then(&Affine::translate(21.0, 0.0));
        assert!(t.is_pixel_exact());
        let out = transform_store(&src, &t);
        // (x, y) → (20 - y, x): source (0,0) lands at (20,0), source (36,20) at (0,36).
        assert_eq!(out.get_pixel(20, 0), src.get_pixel(0, 0));
        assert_eq!(out.get_pixel(0, 36), src.get_pixel(36, 20));
        assert_eq!(out.get_pixel(13, 5), src.get_pixel(5, 7));
        let back = transform_store(&out, &t.inverse().unwrap());
        assert_eq!(
            back.to_raster(Rect::new(0, 0, 37, 21)),
            src.to_raster(Rect::new(0, 0, 37, 21))
        );
        let flip = Affine::scale(-1.0, 1.0).then(&Affine::translate(37.0, 0.0));
        assert!(flip.is_pixel_exact());
        assert_eq!(transform_store(&src, &flip).get_pixel(36, 3), src.get_pixel(0, 3));
        assert!(!Affine::rotate(0.3).is_pixel_exact());
        assert!(!Affine::scale(2.0, 2.0).is_pixel_exact());
    }

    #[test]
    fn half_pixel_shift_blends_neighbours() {
        let mut r = Raster::new(4, 1);
        r.set(1, 0, Rgba::WHITE);
        let src = TileStore::from_raster(&r, 0, 0);
        let out = transform_store(&src, &Affine::translate(0.5, 0.0));
        let a = out.get_pixel(1, 0).a;
        let b = out.get_pixel(2, 0).a;
        assert!(close(a, 0.5) && close(b, 0.5), "{a} {b}");
    }

    #[test]
    fn scaling_down_shrinks_bounds() {
        let src = checker(200, 100);
        let out = transform_store(&src, &Affine::scale(0.5, 0.5));
        let b = out.bounds().unwrap();
        assert!(out.get_pixel(99, 49).a > 0.9);
        assert!(out.get_pixel(101, 49).is_transparent());
        assert!(b.right() <= 256);
        assert!(transform_store(&src, &Affine::scale(0.0, 1.0)).is_empty());
    }
}
