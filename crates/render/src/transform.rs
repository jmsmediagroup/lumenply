//! Affine transformation of tile stores with bilinear resampling.
//!
//! Each destination tile is produced independently (in parallel): every
//! destination pixel centre is mapped back through the inverse transform and
//! the four nearest source pixels are blended. Sampling premultiplied colour
//! is what makes edges against transparency come out right.

use std::sync::Arc;

use nge_tiles::{Affine, Rect, Rgba, Tile, TileCoord, TileStore, TILE_SIZE};
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
pub fn transform_mask(mask: &nge_doc::Mask, t: &Affine) -> nge_doc::Mask {
    let tiles = if mask.default == 0.0 {
        transform_store(&mask.tiles, t)
    } else {
        complement(&transform_store(&complement(&mask.tiles), t))
    };
    let mut out = nge_doc::Mask {
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

    let mut tile = Tile::new();
    let mut any = false;
    for row in 0..TILE_SIZE {
        let py = oy + row as i32;
        for col in 0..TILE_SIZE {
            let px = ox + col as i32;
            let (sx, sy) = inv.apply(px as f32 + 0.5, py as f32 + 0.5);
            let p = sample_bilinear(src, sx - 0.5, sy - 0.5);
            if p.a > 0.0 {
                any = true;
                tile.set(col, row, p);
            }
        }
    }
    any.then_some(tile)
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
    use nge_doc::Mask;
    use nge_tiles::Raster;

    /// A reveal-all mask with a painted-hidden rect must keep hiding that
    /// rect after any transform, and keep revealing everywhere else. The
    /// plain store transform would prune the zero tiles (hidden = a 0) and
    /// treat everything outside them as 0 too.
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
