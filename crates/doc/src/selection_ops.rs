//! Select > Modify: expand, contract, border and smooth.
//!
//! Each works on the selection thresholded at half coverage, measured with
//! an exact Euclidean distance transform, so corners round the way
//! Photoshop's do and edges come out antialiased by a pixel. The work area
//! is the canvas plus a margin whose pixels repeat the canvas edge, so a
//! selection touching the canvas border neither shrinks from nor grows past
//! it (Photoshop's default, "Apply effect at canvas bounds" off). Coverage
//! outside the canvas is left as it was.

use std::sync::Arc;

use lumenply_tiles::{Rect, Rgba, Tile, TILE_SIZE};

use crate::{Mask, Selection};

/// Large enough to mean "no such pixel nearby" (squared distances are f64,
/// exact for integers far beyond any canvas).
const FAR: f64 = 1.0e20;

impl Mask {
    /// Coverage over `rect` as a dense row-major buffer. Tile pixels are
    /// read once per tile.
    pub fn to_dense(&self, rect: Rect) -> Vec<f32> {
        let w = rect.w as usize;
        let mut out = vec![self.default; w * rect.h as usize];
        for c in rect.tiles() {
            let Some(t) = self.tiles.tile(c) else { continue };
            let px = t.pixels();
            let sub = rect.intersect(&c.rect());
            let (ox, oy) = c.origin();
            for y in sub.y..sub.bottom() {
                let trow = (y - oy) as usize * TILE_SIZE;
                let orow = (y - rect.y) as usize * w;
                for x in sub.x..sub.right() {
                    out[orow + (x - rect.x) as usize] = px[trow + (x - ox) as usize].a;
                }
            }
        }
        out
    }

    /// Replace the coverage inside `rect` with `buf` (row-major, `rect`
    /// sized); values are clamped to 0..=1. Uniform tiles are pruned.
    pub fn set_dense(&mut self, rect: Rect, buf: &[f32]) {
        let w = rect.w as usize;
        assert_eq!(buf.len(), w * rect.h as usize, "buffer does not match the rect");
        let d = self.default;
        for c in rect.tiles() {
            let sub = rect.intersect(&c.rect());
            let mut tile = match self.tiles.tile(c) {
                Some(t) => t.clone(),
                None => Tile::filled(Rgba::new(d, d, d, d)),
            };
            let (ox, oy) = c.origin();
            let px = tile.pixels_mut();
            for y in sub.y..sub.bottom() {
                let trow = (y - oy) as usize * TILE_SIZE;
                let brow = (y - rect.y) as usize * w;
                for x in sub.x..sub.right() {
                    let v = buf[brow + (x - rect.x) as usize].clamp(0.0, 1.0);
                    px[trow + (x - ox) as usize] = Rgba::new(v, v, v, v);
                }
            }
            self.tiles.insert(c, Arc::new(tile));
        }
        self.prune_uniform();
    }
}

/// Squared distance from every cell to the nearest cell where `f` is 0,
/// in place (Felzenszwalb & Huttenlocher's lower envelope of parabolas).
/// `f` holds 0 for "in the set" and [`FAR`] otherwise.
fn edt_1d(f: &mut [f64], v: &mut [usize], z: &mut [f64], out: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        let fq = f[q] + (q * q) as f64;
        loop {
            let p = v[k];
            let s = (fq - (f[p] + (p * p) as f64)) / (2.0 * q as f64 - 2.0 * p as f64);
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f64::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        let d = q as f64 - p as f64;
        *o = d * d + f[p];
    }
    f.copy_from_slice(&out[..n]);
}

/// Squared Euclidean distance from each pixel to the nearest pixel with
/// `inside[i] == target` (`w`×`h`, row-major). [`FAR`]-ish where none.
fn distance_sq(inside: &[bool], w: usize, h: usize, target: bool) -> Vec<f64> {
    let mut d: Vec<f64> = inside
        .iter()
        .map(|&b| if b == target { 0.0 } else { FAR })
        .collect();
    let n = w.max(h);
    let mut v = vec![0usize; n];
    let mut z = vec![0f64; n + 1];
    let mut tmp = vec![0f64; n];
    let mut line = vec![0f64; n];
    for y in 0..h {
        edt_1d(&mut d[y * w..(y + 1) * w], &mut v, &mut z, &mut tmp);
    }
    for x in 0..w {
        for y in 0..h {
            line[y] = d[y * w + x];
        }
        edt_1d(&mut line[..h], &mut v, &mut z, &mut tmp);
        for y in 0..h {
            d[y * w + x] = line[y];
        }
    }
    d
}

/// The work area: `roi` (the part of the canvas that can hold coverage
/// other than the default) grown by `margin`, its coverage read with
/// coordinates clamped to the canvas (the border repeats outward).
fn clamped_dense(mask: &Mask, canvas: Rect, roi: Rect, margin: i32) -> (Rect, Vec<f32>) {
    let inner = mask.to_dense(roi);
    let area = Rect::new(
        roi.x - margin,
        roi.y - margin,
        roi.w + 2 * margin as u32,
        roi.h + 2 * margin as u32,
    );
    let mut out = vec![mask.default; area.w as usize * area.h as usize];
    for y in 0..area.h as i32 {
        let cy = (area.y + y).clamp(canvas.y, canvas.bottom() - 1);
        if cy < roi.y || cy >= roi.bottom() {
            continue;
        }
        let row = (cy - roi.y) as usize * roi.w as usize;
        for x in 0..area.w as i32 {
            let cx = (area.x + x).clamp(canvas.x, canvas.right() - 1);
            if cx >= roi.x && cx < roi.right() {
                out[y as usize * area.w as usize + x as usize] = inner[row + (cx - roi.x) as usize];
            }
        }
    }
    (area, out)
}

/// Copy the in-canvas part of a work-area buffer back into the mask.
fn write_canvas(mask: &mut Mask, canvas: Rect, area: Rect, buf: &[f32]) {
    let dst = area.intersect(&canvas);
    if dst.is_empty() {
        return;
    }
    let (ox, oy) = ((dst.x - area.x) as usize, (dst.y - area.y) as usize);
    let (aw, dw) = (area.w as usize, dst.w as usize);
    let mut inner = vec![0f32; dw * dst.h as usize];
    for y in 0..dst.h as usize {
        let start = (y + oy) * aw + ox;
        inner[y * dw..(y + 1) * dw].copy_from_slice(&buf[start..start + dw]);
    }
    mask.set_dense(dst, &inner);
}

/// Signed distance to the selection edge at every work-area pixel:
/// positive inside (half a pixel for an edge pixel), negative outside.
fn signed_distance(cov: &[f32], w: usize, h: usize) -> Vec<f32> {
    let inside: Vec<bool> = cov.iter().map(|&c| c >= 0.5).collect();
    let to_out = distance_sq(&inside, w, h, false);
    let to_in = distance_sq(&inside, w, h, true);
    inside
        .iter()
        .zip(to_out.iter().zip(&to_in))
        .map(|(&i, (&o, &n))| {
            if i {
                (o.sqrt() - 0.5) as f32
            } else {
                -(n.sqrt() - 0.5) as f32
            }
        })
        .collect()
}

/// Which way a Select > Modify command reshapes the selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EdgeOp {
    /// Grow outward by this many pixels (rounded corners).
    Expand(f32),
    /// Shrink inward by this many pixels.
    Contract(f32),
    /// Replace the selection by a band this wide centred on its edge.
    Border(f32),
    /// Majority vote over a square window of this radius: rounds corners
    /// and drops specks and pinholes smaller than the window.
    Smooth(f32),
}

impl EdgeOp {
    pub fn name(self) -> &'static str {
        match self {
            EdgeOp::Expand(_) => "Expand",
            EdgeOp::Contract(_) => "Contract",
            EdgeOp::Border(_) => "Border",
            EdgeOp::Smooth(_) => "Smooth",
        }
    }

    /// The pixel amount, sanitised to 0..=1000.
    pub fn amount(self) -> f32 {
        let (EdgeOp::Expand(v) | EdgeOp::Contract(v) | EdgeOp::Border(v) | EdgeOp::Smooth(v)) = self;
        crate::sane_radius(v)
    }
}

impl Selection {
    /// Reshape the selection inside `canvas` (see [`EdgeOp`]).
    pub fn modify_edge(&mut self, op: EdgeOp, canvas: Rect) {
        if canvas.is_empty() {
            return;
        }
        let n = op.amount();
        if n <= 0.0 {
            return;
        }
        // Only the selection's tiles (plus the reach) need work, unless it
        // covers everything outside them too.
        let roi = if self.coverage.default > 0.0 {
            canvas
        } else {
            match self.coverage.tiles.bounds() {
                Some(b) => b.intersect(&canvas),
                None => return,
            }
        };
        if roi.is_empty() {
            return;
        }
        let margin = n.ceil() as i32 + 2;
        let (area, cov) = clamped_dense(&self.coverage, canvas, roi, margin);
        let (w, h) = (area.w as usize, area.h as usize);
        let out: Vec<f32> = match op {
            EdgeOp::Expand(_) | EdgeOp::Contract(_) | EdgeOp::Border(_) => {
                let s = signed_distance(&cov, w, h);
                let f: Box<dyn Fn(f32) -> f32> = match op {
                    EdgeOp::Expand(_) => Box::new(move |s: f32| s + n + 0.5),
                    EdgeOp::Contract(_) => Box::new(move |s: f32| s - n + 0.5),
                    _ => Box::new(move |s: f32| n / 2.0 + 0.5 - s.abs()),
                };
                s.into_iter().map(|s| f(s).clamp(0.0, 1.0)).collect()
            }
            EdgeOp::Smooth(_) => smooth(&cov, w, h, n.round().max(1.0) as usize),
        };
        write_canvas(&mut self.coverage, canvas, area, &out);
    }
}

/// Box-window majority with a one-pixel antialiased transition.
fn smooth(cov: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    // Integral image of the thresholded selection.
    let iw = w + 1;
    let mut sum = vec![0f64; iw * (h + 1)];
    for y in 0..h {
        let mut row = 0f64;
        for x in 0..w {
            row += if cov[y * w + x] >= 0.5 { 1.0 } else { 0.0 };
            sum[(y + 1) * iw + x + 1] = sum[y * iw + x + 1] + row;
        }
    }
    let k = (2 * r + 1) as f32;
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
            let s = sum[y1 * iw + x1] - sum[y0 * iw + x1] - sum[y1 * iw + x0] + sum[y0 * iw + x0];
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            let mean = s as f32 / n;
            out[y * w + x] = ((mean - 0.5) * k + 0.5).clamp(0.0, 1.0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: Rect = Rect::new(0, 0, 100, 80);

    fn row(s: &Selection, y: i32, xs: std::ops::Range<i32>) -> Vec<f32> {
        xs.map(|x| (s.value(x, y) * 1000.0).round() / 1000.0).collect()
    }

    #[test]
    fn distance_transform_is_exact_euclidean() {
        // One set pixel in the middle of a 7×5 grid.
        let mut inside = vec![false; 35];
        inside[2 * 7 + 3] = true;
        let d = distance_sq(&inside, 7, 5, true);
        assert_eq!(d[2 * 7 + 3], 0.0);
        assert_eq!(d[2 * 7 + 6], 9.0);
        assert_eq!(d[4 * 7 + 5], 8.0);
        assert_eq!(d[0], 13.0);
    }

    #[test]
    fn expand_grows_straight_edges_exactly_and_rounds_corners() {
        let mut s = Selection::rect(Rect::new(20, 20, 30, 20)); // x 20..50, y 20..40
        s.modify_edge(EdgeOp::Expand(3.0), CANVAS);
        // Straight edges: exactly three more pixels, crisp.
        assert_eq!(row(&s, 30, 15..20), vec![0.0, 0.0, 1.0, 1.0, 1.0]);
        assert_eq!(row(&s, 30, 50..55), vec![1.0, 1.0, 1.0, 0.0, 0.0]);
        // The corner rounds: (17, 17) is √18 ≈ 4.24 px from (20, 20),
        // coverage 3.5 − (4.243 − 0.5) = −0.243 → 0; (18, 18) is 2.83 px → 1;
        // (17, 18) is √13 = 3.606 px → 3.5 − 3.106 = 0.394 (antialiased).
        assert_eq!(s.value(17, 17), 0.0);
        assert_eq!(s.value(18, 18), 1.0);
        assert!((s.value(17, 18) - 0.394).abs() < 1e-3, "{}", s.value(17, 18));
        assert_eq!(s.value(35, 30), 1.0, "inside stays selected");
    }

    #[test]
    fn contract_shrinks_and_keeps_square_corners() {
        let mut s = Selection::rect(Rect::new(20, 20, 30, 20));
        s.modify_edge(EdgeOp::Contract(4.0), CANVAS);
        assert_eq!(row(&s, 30, 22..26), vec![0.0, 0.0, 1.0, 1.0]);
        assert_eq!(row(&s, 30, 44..48), vec![1.0, 1.0, 0.0, 0.0]);
        assert_eq!(s.value(24, 24), 1.0, "a convex corner stays square");
        assert_eq!(s.value(23, 24), 0.0);
        // Contracting more than half the height empties it.
        let mut t = Selection::rect(Rect::new(20, 20, 30, 20));
        t.modify_edge(EdgeOp::Contract(10.0), CANVAS);
        assert!(t.tight_bounds(CANVAS).is_empty());
    }

    #[test]
    fn contract_does_not_shrink_from_the_canvas_edge() {
        let mut s = Selection::all();
        s.modify_edge(EdgeOp::Contract(5.0), CANVAS);
        assert_eq!(s.value(0, 0), 1.0);
        assert_eq!(s.value(99, 79), 1.0);
        let mut r = Selection::rect(Rect::new(0, 0, 40, 80)); // touches three edges
        r.modify_edge(EdgeOp::Contract(5.0), CANVAS);
        assert_eq!(row(&r, 40, 0..2), vec![1.0, 1.0], "left edge stays");
        assert_eq!(row(&r, 40, 34..36), vec![1.0, 0.0], "right side contracts by 5");
        assert_eq!(r.value(10, 0), 1.0, "top edge stays");
    }

    #[test]
    fn border_is_a_band_centred_on_the_edge() {
        let mut s = Selection::rect(Rect::new(20, 20, 30, 20));
        s.modify_edge(EdgeOp::Border(4.0), CANVAS);
        // Two pixels outside and two inside the old left edge at x = 20.
        assert_eq!(row(&s, 30, 16..24), vec![0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
        assert_eq!(s.value(35, 30), 0.0, "the middle is no longer selected");
        // An odd width splits its middle pixel's worth across both sides.
        let mut t = Selection::rect(Rect::new(20, 20, 30, 20));
        t.modify_edge(EdgeOp::Border(3.0), CANVAS);
        assert_eq!(row(&t, 30, 16..24), vec![0.0, 0.0, 0.5, 1.0, 1.0, 0.5, 0.0, 0.0]);
    }

    #[test]
    fn smooth_drops_specks_and_keeps_straight_edges() {
        let mut s = Selection::rect(Rect::new(20, 20, 30, 20));
        s.coverage.fill_rect(Rect::new(70, 10, 2, 2), 1.0); // a speck
        s.coverage.fill_rect(Rect::new(30, 30, 1, 1), 0.0); // a pinhole
        s.modify_edge(EdgeOp::Smooth(3.0), CANVAS);
        assert_eq!(s.value(70, 10), 0.0, "speck removed");
        assert_eq!(s.value(30, 30), 1.0, "pinhole filled");
        assert_eq!(
            row(&s, 30, 18..22),
            vec![0.0, 0.0, 1.0, 1.0],
            "edge kept in place"
        );
        assert_eq!(s.value(20, 20), 0.0, "the corner rounds off");
        assert_eq!(s.value(22, 22), 1.0);
    }

    #[test]
    fn a_small_selection_on_a_big_canvas_only_works_around_itself() {
        // Tiles 2..4 of a 2000×1500 canvas; the result matches the small
        // canvas case shifted, including across a tile edge (x = 768).
        let big = Rect::new(0, 0, 2000, 1500);
        let mut s = Selection::rect(Rect::new(760, 600, 30, 20));
        s.modify_edge(EdgeOp::Expand(3.0), big);
        assert_eq!(row(&s, 610, 755..760), vec![0.0, 0.0, 1.0, 1.0, 1.0]);
        assert_eq!(row(&s, 610, 790..795), vec![1.0, 1.0, 1.0, 0.0, 0.0]);
        assert!((s.value(757, 598) - 0.394).abs() < 1e-3, "{}", s.value(757, 598));
        assert_eq!(s.tight_bounds(big), Rect::new(757, 597, 36, 26));
        // Contract back: the original rectangle, square corners and all.
        s.modify_edge(EdgeOp::Contract(3.0), big);
        assert_eq!(s.tight_bounds(big), Rect::new(760, 600, 30, 20));
    }

    #[test]
    fn dense_round_trip_keeps_coverage_and_prunes() {
        let s = Selection::ellipse(Rect::new(3, 3, 300, 40));
        let rect = Rect::new(0, 0, 320, 50);
        let d = s.coverage.to_dense(rect);
        let mut m = Mask::hide_all();
        m.set_dense(rect, &d);
        assert_eq!(m.to_dense(rect), d);
        let mut z = Mask::hide_all();
        z.set_dense(rect, &vec![0.0; d.len()]);
        assert!(z.tiles.is_empty(), "all-default tiles are pruned");
    }
}
