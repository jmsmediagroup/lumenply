//! Selections and the coverage operations shared with masks.
//!
//! A selection is a [`Mask`] whose default is 0 (nothing selected outside
//! painted tiles). Shapes are antialiased, boolean operations work pixelwise
//! over the union of both tile sets, and feathering is a three-pass box blur
//! that closely approximates a Gaussian.

use std::sync::Arc;

use lumenply_tiles::{Rect, Rgba, Tile, TileCoord};
use serde::{Deserialize, Serialize};

use crate::Mask;

/// How a new shape combines with an existing selection or mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CombineOp {
    #[default]
    Replace,
    Union,
    Intersect,
    Subtract,
}

impl CombineOp {
    #[inline]
    fn apply(self, a: f32, b: f32) -> f32 {
        match self {
            CombineOp::Replace => b,
            CombineOp::Union => a.max(b),
            CombineOp::Intersect => a.min(b),
            CombineOp::Subtract => a * (1.0 - b),
        }
    }
}

impl Mask {
    /// Set every pixel inside `rect` to `v`.
    pub fn fill_rect(&mut self, rect: Rect, v: f32) {
        let v = v.clamp(0.0, 1.0);
        for c in rect.tiles() {
            let sub = rect.intersect(&c.rect());
            if sub == c.rect() {
                self.tiles
                    .insert(c, Arc::new(Tile::filled(Rgba::new(v, v, v, v))));
                continue;
            }
            for py in sub.y..sub.bottom() {
                for px in sub.x..sub.right() {
                    self.set_value(px, py, v);
                }
            }
        }
    }

    /// Set the ellipse inscribed in `rect` to `v`, antialiased by 3×3
    /// supersampling at the edge.
    pub fn fill_ellipse(&mut self, rect: Rect, v: f32) {
        let v = v.clamp(0.0, 1.0);
        let rx = rect.w as f32 / 2.0;
        let ry = rect.h as f32 / 2.0;
        if rx <= 0.0 || ry <= 0.0 {
            return;
        }
        let cx = rect.x as f32 + rx;
        let cy = rect.y as f32 + ry;
        let inside = |x: f32, y: f32| {
            let dx = (x - cx) / rx;
            let dy = (y - cy) / ry;
            dx * dx + dy * dy <= 1.0
        };
        for py in rect.y..rect.bottom() {
            for px in rect.x..rect.right() {
                // Quick classification by the pixel's corners.
                let corners = [
                    inside(px as f32, py as f32),
                    inside(px as f32 + 1.0, py as f32),
                    inside(px as f32, py as f32 + 1.0),
                    inside(px as f32 + 1.0, py as f32 + 1.0),
                ];
                let n = corners.iter().filter(|&&c| c).count();
                let cover = if n == 4 {
                    1.0
                } else if n == 0 && !inside(px as f32 + 0.5, py as f32 + 0.5) {
                    0.0
                } else {
                    let mut hit = 0;
                    for sy in 0..3 {
                        for sx in 0..3 {
                            if inside(
                                px as f32 + (sx as f32 + 0.5) / 3.0,
                                py as f32 + (sy as f32 + 0.5) / 3.0,
                            ) {
                                hit += 1;
                            }
                        }
                    }
                    hit as f32 / 9.0
                };
                if cover > 0.0 {
                    let existing = self.value(px, py);
                    self.set_value(px, py, existing + (v - existing) * cover);
                }
            }
        }
    }

    /// Set the inside of a closed polygon to `v`, antialiased by 4×
    /// vertical supersampling of a scanline fill (even-odd rule).
    pub fn fill_polygon(&mut self, points: &[(f32, f32)], v: f32) {
        if points.len() < 3 {
            return;
        }
        let v = v.clamp(0.0, 1.0);
        let min_y = points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min).floor() as i32;
        let max_y = points
            .iter()
            .map(|p| p.1)
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil() as i32;
        let min_x = points.iter().map(|p| p.0).fold(f32::INFINITY, f32::min).floor() as i32;
        let max_x = points
            .iter()
            .map(|p| p.0)
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil() as i32;
        if max_x <= min_x || max_y <= min_y {
            return;
        }
        const SUB: usize = 4;
        let width = (max_x - min_x + 1) as usize;
        let mut row_cov = vec![0f32; width];
        for py in min_y..=max_y {
            row_cov.iter_mut().for_each(|c| *c = 0.0);
            let mut any = false;
            for s in 0..SUB {
                let sy = py as f32 + (s as f32 + 0.5) / SUB as f32;
                // Crossings of this sample line with polygon edges.
                let mut xs: Vec<f32> = Vec::new();
                for i in 0..points.len() {
                    let (x0, y0) = points[i];
                    let (x1, y1) = points[(i + 1) % points.len()];
                    if (y0 <= sy && y1 > sy) || (y1 <= sy && y0 > sy) {
                        xs.push(x0 + (sy - y0) / (y1 - y0) * (x1 - x0));
                    }
                }
                xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                for pair in xs.chunks(2) {
                    if pair.len() < 2 {
                        break;
                    }
                    let (xa, xb) = (pair[0], pair[1]);
                    // Horizontal coverage with fractional ends.
                    let ia = xa.floor() as i32;
                    let ib = xb.ceil() as i32 - 1;
                    for px in ia.max(min_x)..=ib.min(max_x) {
                        let l = xa.max(px as f32);
                        let r = xb.min(px as f32 + 1.0);
                        if r > l {
                            row_cov[(px - min_x) as usize] += (r - l) / SUB as f32;
                            any = true;
                        }
                    }
                }
            }
            if !any {
                continue;
            }
            for (i, cov) in row_cov.iter().enumerate() {
                if *cov > 0.0 {
                    let px = min_x + i as i32;
                    let existing = self.value(px, py);
                    self.set_value(px, py, existing + (v - existing) * cov.min(1.0));
                }
            }
        }
    }

    /// Flip coverage everywhere.
    pub fn invert(&mut self) {
        self.default = 1.0 - self.default;
        let coords: Vec<TileCoord> = self.tiles.coords().collect();
        for c in coords {
            for p in self.tiles.tile_mut(c).pixels_mut() {
                let v = 1.0 - p.a;
                *p = Rgba::new(v, v, v, v);
            }
        }
    }

    /// Combine another coverage map into this one, pixel by pixel.
    pub fn combine(&mut self, other: &Mask, op: CombineOp) {
        if op == CombineOp::Replace {
            self.tiles = other.tiles.clone();
            self.default = other.default;
            return;
        }
        let mut coords: Vec<TileCoord> = self.tiles.coords().chain(other.tiles.coords()).collect();
        coords.sort();
        coords.dedup();
        let (da, db) = (self.default, other.default);
        for c in coords {
            let b_px = other.tiles.tile(c).map(|t| t.pixels().into_owned());
            if self.tiles.tile(c).is_none() {
                // A fresh tile must start at this mask's default, not
                // transparent (same rule as `Mask::set_value`).
                self.tiles
                    .insert(c, Arc::new(Tile::filled(Rgba::new(da, da, da, da))));
            }
            let a_tile = self.tiles.tile_mut(c);
            for (i, p) in a_tile.pixels_mut().iter_mut().enumerate() {
                let a = p.a;
                let b = b_px.as_ref().map_or(db, |t| t[i].a);
                let v = op.apply(a, b);
                *p = Rgba::new(v, v, v, v);
            }
        }
        self.default = op.apply(da, db);
        self.prune_uniform();
    }

    /// Soften edges with a Gaussian-like blur of roughly `radius` pixels.
    pub fn feather(&mut self, radius: f32) {
        let Some(bounds) = self.tiles.bounds() else { return };
        let box_r = crate::box_radius(radius);
        let pad = box_r * 3;
        let area = Rect::new(
            bounds.x - pad,
            bounds.y - pad,
            bounds.w + 2 * pad as u32,
            bounds.h + 2 * pad as u32,
        );
        let w = area.w as usize;
        let h = area.h as usize;
        let mut buf = vec![0f32; w * h];
        for (i, v) in buf.iter_mut().enumerate() {
            *v = self.value(area.x + (i % w) as i32, area.y + (i / w) as i32);
        }
        let mut tmp = vec![0f32; w * h];
        for _ in 0..3 {
            box_blur_h(&buf, &mut tmp, w, h, box_r, self.default);
            box_blur_v(&tmp, &mut buf, w, h, box_r, self.default);
        }
        for (i, v) in buf.iter().enumerate() {
            self.set_value(area.x + (i % w) as i32, area.y + (i / w) as i32, *v);
        }
        self.prune_uniform();
    }

    /// Drop tiles whose every pixel equals the default.
    pub fn prune_uniform(&mut self) {
        let d = self.default;
        let drop: Vec<TileCoord> = self
            .tiles
            .coords()
            .filter(|c| {
                self.tiles
                    .tile(*c)
                    .is_some_and(|t| t.pixels().iter().all(|p| p.a == d))
            })
            .collect();
        for c in drop {
            self.tiles.remove(c);
        }
    }

    /// Pixel rectangle that may contain non-default coverage, clipped to
    /// `canvas`. If the default is non-zero this is the whole canvas.
    pub fn bounds_within(&self, canvas: Rect) -> Rect {
        if self.default > 0.0 {
            return canvas;
        }
        self.tiles
            .bounds()
            .map_or(Rect::default(), |b| b.intersect(&canvas))
    }
}

fn box_blur_h(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: i32, edge: f32) {
    let r = r as usize;
    let n = (2 * r + 1) as f32;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let at = |x: isize| -> f32 {
            if x < 0 || x >= w as isize {
                edge
            } else {
                row[x as usize]
            }
        };
        let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for x in 0..w {
            dst[y * w + x] = sum / n;
            sum += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
        }
    }
}

fn box_blur_v(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: i32, edge: f32) {
    let r = r as usize;
    let n = (2 * r + 1) as f32;
    for x in 0..w {
        let at = |y: isize| -> f32 {
            if y < 0 || y >= h as isize {
                edge
            } else {
                src[y as usize * w + x]
            }
        };
        let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for y in 0..h {
            dst[y * w + x] = sum / n;
            sum += at(y as isize + r as isize + 1) - at(y as isize - r as isize);
        }
    }
}

/// The active selection: a coverage map with default 0.
#[derive(Clone, Debug)]
pub struct Selection {
    pub coverage: Mask,
}

impl Selection {
    pub fn none() -> Self {
        Selection {
            coverage: Mask::hide_all(),
        }
    }

    pub fn all() -> Self {
        Selection {
            coverage: Mask::reveal_all(),
        }
    }

    pub fn rect(rect: Rect) -> Self {
        let mut s = Selection::none();
        s.coverage.fill_rect(rect, 1.0);
        s
    }

    pub fn ellipse(rect: Rect) -> Self {
        let mut s = Selection::none();
        s.coverage.fill_ellipse(rect, 1.0);
        s
    }

    /// A closed polygon (lasso or polygonal lasso), antialiased.
    pub fn polygon(points: &[(f32, f32)]) -> Self {
        let mut s = Selection::none();
        s.coverage.fill_polygon(points, 1.0);
        s
    }

    /// Build a selection from an existing mask's coverage.
    pub fn from_mask(mask: &Mask) -> Self {
        Selection {
            coverage: Mask {
                tiles: mask.tiles.clone(),
                default: mask.default,
                enabled: true,
            },
        }
    }

    #[inline]
    pub fn value(&self, x: i32, y: i32) -> f32 {
        self.coverage.value(x, y)
    }

    /// True when nothing at all is selected.
    pub fn is_empty(&self) -> bool {
        self.coverage.default <= 0.0 && self.coverage.tiles.is_empty()
    }

    pub fn invert(&mut self) {
        self.coverage.invert();
    }

    pub fn combine(&mut self, other: &Selection, op: CombineOp) {
        self.coverage.combine(&other.coverage, op);
    }

    pub fn feather(&mut self, radius: f32) {
        self.coverage.feather(radius);
    }

    pub fn bounds_within(&self, canvas: Rect) -> Rect {
        self.coverage.bounds_within(canvas)
    }

    /// Pixel-tight bounds of the selected area within `canvas` (scans
    /// pixels; use for crop and status display, not per-frame).
    pub fn tight_bounds(&self, canvas: Rect) -> Rect {
        if self.coverage.default > 0.0 {
            return canvas;
        }
        self.coverage
            .tiles
            .content_bounds()
            .map_or(Rect::default(), |b| b.intersect(&canvas))
    }

    /// Convert to a layer mask (same coverage, default preserved).
    pub fn to_mask(&self) -> Mask {
        Mask {
            tiles: self.coverage.tiles.clone(),
            default: self.coverage.default,
            enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn rect_selection_is_crisp_and_sparse() {
        let s = Selection::rect(Rect::new(10, 10, 20, 20));
        assert_eq!(s.value(10, 10), 1.0);
        assert_eq!(s.value(29, 29), 1.0);
        assert_eq!(s.value(30, 30), 0.0);
        assert_eq!(s.value(5000, 5000), 0.0);
        assert_eq!(s.coverage.tiles.len(), 1);
        assert_eq!(
            s.bounds_within(Rect::new(0, 0, 1000, 1000)),
            Rect::new(0, 0, 256, 256)
        );
    }

    #[test]
    fn ellipse_is_antialiased() {
        let s = Selection::ellipse(Rect::new(0, 0, 100, 60));
        assert_eq!(s.value(50, 30), 1.0);
        assert_eq!(s.value(0, 0), 0.0);
        // The top-centre pixel row lies entirely inside the flat top of the
        // ellipse, so it is fully covered; a pixel on the 45° diagonal is partial.
        assert_eq!(s.value(50, 0), 1.0);
        let edge = s.value(85, 51);
        assert!(edge > 0.0 && edge < 1.0, "edge coverage {edge}");
        assert_eq!(s.value(86, 52), 0.0);
    }

    #[test]
    fn polygon_selection_is_filled_and_antialiased() {
        // A diamond centred at (20, 20) with radius 10.
        let s = Selection::polygon(&[(20.0, 10.0), (30.0, 20.0), (20.0, 30.0), (10.0, 20.0)]);
        assert_eq!(s.value(20, 20), 1.0);
        assert_eq!(s.value(20, 12), 1.0);
        assert_eq!(s.value(2, 2), 0.0);
        assert_eq!(s.value(29, 10), 0.0, "outside the diamond's corner");
        let edge = s.value(14, 15); // the edge x + y = 30 bisects this pixel
        assert!(edge > 0.2 && edge < 0.8, "antialiased edge: {edge}");
        // Degenerate input is ignored.
        assert!(Selection::polygon(&[(0.0, 0.0), (5.0, 5.0)]).is_empty());
        // Total coverage ≈ area of the diamond (2 r²).
        let mut total = 0.0;
        for y in 0..40 {
            for x in 0..40 {
                total += s.value(x, y);
            }
        }
        assert!((total - 200.0).abs() < 3.0, "area {total}");
    }

    #[test]
    fn boolean_ops_and_invert() {
        let mut a = Selection::rect(Rect::new(0, 0, 10, 10));
        let b = Selection::rect(Rect::new(5, 0, 10, 10));

        let mut u = a.clone();
        u.combine(&b, CombineOp::Union);
        assert_eq!(u.value(0, 0), 1.0);
        assert_eq!(u.value(14, 0), 1.0);

        let mut i = a.clone();
        i.combine(&b, CombineOp::Intersect);
        assert_eq!(i.value(0, 0), 0.0);
        assert_eq!(i.value(7, 0), 1.0);

        a.combine(&b, CombineOp::Subtract);
        assert_eq!(a.value(2, 0), 1.0);
        assert_eq!(a.value(7, 0), 0.0);

        a.invert();
        assert_eq!(a.value(2, 0), 0.0);
        assert_eq!(a.value(7, 0), 1.0);
        assert_eq!(a.value(9000, 9000), 1.0);
        assert!(!a.is_empty());

        // Subtracting a selection from itself leaves nothing.
        let mut z = b.clone();
        z.combine(&b, CombineOp::Subtract);
        assert!(z.is_empty());
    }

    #[test]
    fn combining_with_select_all_keeps_the_default_coverage() {
        // Select All has default 1.0 and no tiles. Combining a shape into it
        // creates tiles that must start at the default, not at 0.
        let r = Selection::rect(Rect::new(10, 10, 20, 20));

        let mut s = Selection::all();
        s.combine(&r, CombineOp::Subtract);
        assert_eq!(s.value(15, 15), 0.0, "inside the subtracted rect");
        assert_eq!(s.value(100, 100), 1.0, "same tile, outside the rect");
        assert_eq!(s.value(5000, 5000), 1.0, "far away (default)");

        let mut i = Selection::all();
        i.combine(&r, CombineOp::Intersect);
        assert_eq!(i.value(15, 15), 1.0, "inside the intersected rect");
        assert_eq!(i.value(100, 100), 0.0, "same tile, outside the rect");

        // Union with an inverted (default 1.0) selection on the other side.
        let mut inv = Selection::rect(Rect::new(0, 0, 10, 10));
        inv.invert(); // default 1.0, one tile with a 10×10 hole
        let mut u = Selection::rect(Rect::new(300, 300, 10, 10));
        u.combine(&inv, CombineOp::Union);
        assert_eq!(u.value(305, 305), 1.0);
        assert_eq!(u.value(5, 5), 0.0, "the hole survives the union");
        assert_eq!(u.value(5000, 5000), 1.0, "defaults combine");
    }

    #[test]
    fn feather_softens_edges_but_keeps_the_centre() {
        let mut s = Selection::rect(Rect::new(100, 100, 50, 50));
        s.feather(6.0);
        assert!(close(s.value(125, 125), 1.0));
        let on_edge = s.value(100, 125);
        assert!(on_edge > 0.3 && on_edge < 0.7, "edge {on_edge}");
        let outside = s.value(95, 125);
        assert!(outside > 0.0 && outside < on_edge, "outside {outside}");
        assert_eq!(s.value(70, 125), 0.0);
        assert_eq!(s.coverage.default, 0.0);
    }

    #[test]
    fn mask_from_selection_round_trips() {
        let s = Selection::ellipse(Rect::new(0, 0, 40, 40));
        let m = s.to_mask();
        let back = Selection::from_mask(&m);
        assert_eq!(back.value(20, 20), s.value(20, 20));
        assert_eq!(back.value(1, 1), s.value(1, 1));
    }
}
