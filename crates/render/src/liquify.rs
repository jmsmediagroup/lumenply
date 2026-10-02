//! Liquify: a displacement field painted with warp brushes, applied as a
//! backward map with bilinear sampling.
//!
//! The field lives in canvas pixels on a grid of nodes `step` pixels apart
//! and is interpolated bilinearly between them. Output pixel `p` shows the
//! source at `p + d(p)`. Every brush that moves content is an advection of
//! the field: a node takes the field value at the point its content comes
//! from, `q = map(p)`, giving `d'(p) = d(q) + (q - p)`. Repeated small dabs
//! therefore compose like one smooth warp instead of tearing.

use lumenply_tiles::{Raster, Rect, Rgba, Tile, TileStore, TILE_SIZE};
use rayon::prelude::*;

/// The Liquify brushes (Photoshop's names and keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquifyTool {
    /// Forward warp (W): pushes content along the drag.
    Push,
    /// Reconstruct (R): eases the warp back towards the original.
    Reconstruct,
    /// Smooth (E): evens out the warp without undoing it.
    Smooth,
    /// Twirl clockwise (C); with Alt, counter-clockwise.
    TwirlCw,
    TwirlCcw,
    /// Pucker (S): pulls content towards the brush centre.
    Pucker,
    /// Bloat (B): pushes content away from the brush centre.
    Bloat,
}

impl LiquifyTool {
    pub const ALL: [LiquifyTool; 7] = [
        LiquifyTool::Push,
        LiquifyTool::Reconstruct,
        LiquifyTool::Smooth,
        LiquifyTool::TwirlCw,
        LiquifyTool::TwirlCcw,
        LiquifyTool::Pucker,
        LiquifyTool::Bloat,
    ];

    pub fn name(self) -> &'static str {
        match self {
            LiquifyTool::Push => "Forward warp",
            LiquifyTool::Reconstruct => "Reconstruct",
            LiquifyTool::Smooth => "Smooth",
            LiquifyTool::TwirlCw => "Twirl clockwise",
            LiquifyTool::TwirlCcw => "Twirl counter-clockwise",
            LiquifyTool::Pucker => "Pucker",
            LiquifyTool::Bloat => "Bloat",
        }
    }
}

/// A dense displacement field over `rect`.
#[derive(Clone, Debug, PartialEq)]
pub struct Displacement {
    /// Canvas area the field covers; outside it the displacement is zero.
    pub rect: Rect,
    /// Pixels between grid nodes (1 = one node per pixel).
    pub step: u32,
    pub cols: usize,
    pub rows: usize,
    /// Source offset per node, row-major, `cols × rows`.
    pub d: Vec<[f32; 2]>,
}

/// Twirl angle (radians) of one full-strength dab at the brush centre.
pub const TWIRL_PER_DAB: f32 = std::f32::consts::FRAC_PI_4;
/// Pucker/Bloat scale change of one full-strength dab at the centre.
pub const SCALE_PER_DAB: f32 = 0.3;

/// Brush falloff: 1 at the centre, 0 at the rim, smooth at both.
fn falloff(dist: f32, radius: f32) -> f32 {
    if dist >= radius {
        return 0.0;
    }
    let t = 1.0 - (dist / radius) * (dist / radius);
    t * t
}

impl Displacement {
    /// The identity field over `rect`, nodes `step` pixels apart.
    pub fn new(rect: Rect, step: u32) -> Self {
        let step = step.max(1);
        let cols = rect.w.div_ceil(step) as usize + 1;
        let rows = rect.h.div_ceil(step) as usize + 1;
        Displacement {
            rect,
            step,
            cols,
            rows,
            d: vec![[0.0, 0.0]; cols * rows],
        }
    }

    /// A node step that keeps the field near `max_nodes` nodes.
    pub fn step_for(rect: Rect, max_nodes: u64) -> u32 {
        let px = rect.w as u64 * rect.h as u64;
        let mut step = 1u32;
        while px / (step as u64 * step as u64) > max_nodes {
            step += 1;
        }
        step
    }

    pub fn is_identity(&self) -> bool {
        self.d.iter().all(|v| v[0] == 0.0 && v[1] == 0.0)
    }

    fn node_pos(&self, i: usize, j: usize) -> (f32, f32) {
        (
            self.rect.x as f32 + (i as u32 * self.step) as f32,
            self.rect.y as f32 + (j as u32 * self.step) as f32,
        )
    }

    /// The displacement at canvas point `(x, y)`, bilinear between nodes.
    pub fn at(&self, x: f32, y: f32) -> [f32; 2] {
        let gx = (x - self.rect.x as f32) / self.step as f32;
        let gy = (y - self.rect.y as f32) / self.step as f32;
        let max_x = (self.cols - 1) as f32;
        let max_y = (self.rows - 1) as f32;
        if !(0.0..=max_x).contains(&gx) || !(0.0..=max_y).contains(&gy) {
            return [0.0, 0.0];
        }
        let i0 = (gx.floor() as usize).min(self.cols - 1);
        let j0 = (gy.floor() as usize).min(self.rows - 1);
        let i1 = (i0 + 1).min(self.cols - 1);
        let j1 = (j0 + 1).min(self.rows - 1);
        let fx = gx - i0 as f32;
        let fy = gy - j0 as f32;
        let n = |i: usize, j: usize| self.d[j * self.cols + i];
        let (a, b, c, e) = (n(i0, j0), n(i1, j0), n(i0, j1), n(i1, j1));
        let mut out = [0.0; 2];
        for k in 0..2 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (e[k] - c[k]) * fx;
            out[k] = top + (bot - top) * fy;
        }
        out
    }

    /// The node index range a brush of `radius` at `(cx, cy)` touches.
    fn nodes_near(&self, cx: f32, cy: f32, radius: f32) -> (usize, usize, usize, usize) {
        let s = self.step as f32;
        let lo = |c: f32, o: i32| (((c - radius - o as f32) / s).floor().max(0.0)) as usize;
        let hi =
            |c: f32, o: i32, n: usize| ((((c + radius - o as f32) / s).ceil()).max(0.0) as usize).min(n - 1);
        (
            lo(cx, self.rect.x),
            hi(cx, self.rect.x, self.cols),
            lo(cy, self.rect.y),
            hi(cy, self.rect.y, self.rows),
        )
    }

    /// One dab of `tool` centred on `(cx, cy)`. `pressure` (0..=1) scales
    /// every brush; `delta` is the pointer movement since the last dab
    /// (used by Push); `rate` (0..=1) is the per-dab amount of the
    /// stationary brushes (twirl angle, pucker/bloat scale, reconstruct and
    /// smooth mix).
    pub fn dab(
        &mut self,
        tool: LiquifyTool,
        (cx, cy): (f32, f32),
        radius: f32,
        pressure: f32,
        rate: f32,
        delta: (f32, f32),
    ) {
        if radius <= 0.0 || pressure <= 0.0 {
            return;
        }
        let (i0, i1, j0, j1) = self.nodes_near(cx, cy, radius);
        if i0 > i1 || j0 > j1 {
            return;
        }
        let mut updates: Vec<(usize, [f32; 2])> = Vec::new();
        for j in j0..=j1 {
            for i in i0..=i1 {
                let (px, py) = self.node_pos(i, j);
                let (rx, ry) = (px - cx, py - cy);
                let w = pressure * falloff((rx * rx + ry * ry).sqrt(), radius);
                if w <= 0.0 {
                    continue;
                }
                let idx = j * self.cols + i;
                let cur = self.d[idx];
                let new = match tool {
                    LiquifyTool::Reconstruct => {
                        let k = 1.0 - (w * rate).min(1.0);
                        [cur[0] * k, cur[1] * k]
                    }
                    LiquifyTool::Smooth => {
                        let s = self.step as f32;
                        let mut sum = [0.0f32; 2];
                        for (ox, oy) in [(-s, 0.0), (s, 0.0), (0.0, -s), (0.0, s)] {
                            let v = self.at(px + ox, py + oy);
                            sum[0] += v[0];
                            sum[1] += v[1];
                        }
                        let k = (w * rate).min(1.0);
                        [
                            cur[0] + (sum[0] / 4.0 - cur[0]) * k,
                            cur[1] + (sum[1] / 4.0 - cur[1]) * k,
                        ]
                    }
                    _ => {
                        // The point whose content moves into this node.
                        let (qx, qy) = match tool {
                            LiquifyTool::Push => (px - w * delta.0, py - w * delta.1),
                            LiquifyTool::TwirlCw | LiquifyTool::TwirlCcw => {
                                let sign = if tool == LiquifyTool::TwirlCw { -1.0 } else { 1.0 };
                                let a = sign * w * rate * TWIRL_PER_DAB;
                                let (sin, cos) = a.sin_cos();
                                (cx + rx * cos - ry * sin, cy + rx * sin + ry * cos)
                            }
                            LiquifyTool::Pucker => {
                                let k = 1.0 + w * rate * SCALE_PER_DAB;
                                (cx + rx * k, cy + ry * k)
                            }
                            LiquifyTool::Bloat => {
                                let k = 1.0 - w * rate * SCALE_PER_DAB;
                                (cx + rx * k, cy + ry * k)
                            }
                            _ => unreachable!(),
                        };
                        let dq = self.at(qx, qy);
                        [dq[0] + (qx - px), dq[1] + (qy - py)]
                    }
                };
                updates.push((idx, new));
            }
        }
        for (idx, v) in updates {
            self.d[idx] = v;
        }
    }

    /// The canvas box every output pixel inside `out` reads from.
    fn source_box(&self, out: Rect) -> Rect {
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        let lo_i = ((out.x - self.rect.x) as f32 / self.step as f32).floor().max(0.0) as usize;
        let hi_i = (((out.right() - self.rect.x) as f32 / self.step as f32)
            .ceil()
            .max(0.0) as usize)
            .min(self.cols - 1);
        let lo_j = ((out.y - self.rect.y) as f32 / self.step as f32).floor().max(0.0) as usize;
        let hi_j = (((out.bottom() - self.rect.y) as f32 / self.step as f32)
            .ceil()
            .max(0.0) as usize)
            .min(self.rows - 1);
        for j in lo_j..=hi_j {
            for i in lo_i..=hi_i {
                let v = self.d[j * self.cols + i];
                x0 = x0.min(v[0]);
                x1 = x1.max(v[0]);
                y0 = y0.min(v[1]);
                y1 = y1.max(v[1]);
            }
        }
        if x0 > x1 {
            return out;
        }
        // Outside the field the offset is zero, so include 0 as well.
        let (x0, x1) = (x0.min(0.0), x1.max(0.0));
        let (y0, y1) = (y0.min(0.0), y1.max(0.0));
        let left = out.x + x0.floor() as i32 - 1;
        let top = out.y + y0.floor() as i32 - 1;
        let right = out.right() + x1.ceil() as i32 + 1;
        let bottom = out.bottom() + y1.ceil() as i32 + 1;
        Rect::new(left, top, (right - left) as u32, (bottom - top) as u32)
    }
}

/// Bilinear sample of `src` (whose top-left sits at canvas `origin`) at
/// canvas point `(x, y)`; transparent outside.
fn sample(src: &Raster, origin: (i32, i32), x: f32, y: f32) -> Rgba {
    let fx0 = x.floor();
    let fy0 = y.floor();
    let (fx, fy) = (x - fx0, y - fy0);
    let (ix, iy) = (fx0 as i32 - origin.0, fy0 as i32 - origin.1);
    let get = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= src.width as i32 || y >= src.height as i32 {
            Rgba::TRANSPARENT
        } else {
            src.get(x as u32, y as u32)
        }
    };
    let (p00, p10, p01, p11) = (get(ix, iy), get(ix + 1, iy), get(ix, iy + 1), get(ix + 1, iy + 1));
    if fx == 0.0 && fy == 0.0 {
        return p00;
    }
    let (w00, w10, w01, w11) = ((1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy);
    Rgba::new(
        p00.r * w00 + p10.r * w10 + p01.r * w01 + p11.r * w11,
        p00.g * w00 + p10.g * w10 + p01.g * w01 + p11.g * w11,
        p00.b * w00 + p10.b * w10 + p01.b * w01 + p11.b * w11,
        p00.a * w00 + p10.a * w10 + p01.a * w01 + p11.a * w11,
    )
}

/// Applies `field` to a layer's pixels. Outside the field the pixels are
/// untouched; inside, each output tile reads only the source box its
/// offsets reach, so memory stays per-tile.
pub fn liquify_store(src: &TileStore, field: &Displacement) -> TileStore {
    if field.is_identity() || field.rect.is_empty() {
        return src.clone();
    }
    let coords = field.rect.tiles();
    let tiles: Vec<_> = coords
        .into_par_iter()
        .map(|c| {
            let out_rect = c.rect().intersect(&field.rect);
            let sbox = field.source_box(out_rect);
            let raster = src.to_raster(sbox);
            let mut tile = src.tile(c).cloned().unwrap_or_else(Tile::new);
            {
                let px = tile.pixels_mut();
                let (ox, oy) = c.origin();
                for y in out_rect.y..out_rect.bottom() {
                    for x in out_rect.x..out_rect.right() {
                        let d = field.at(x as f32, y as f32);
                        let p = sample(&raster, (sbox.x, sbox.y), x as f32 + d[0], y as f32 + d[1]);
                        px[(y - oy) as usize * TILE_SIZE + (x - ox) as usize] = p;
                    }
                }
            }
            (c, tile)
        })
        .collect();
    let mut out = src.clone();
    for (c, t) in tiles {
        out.insert(c, std::sync::Arc::new(t));
    }
    out.prune_blank();
    out
}

/// Applies `field` to a scaled preview: `src` shows the canvas area
/// starting at `origin` at `scale` preview pixels per canvas pixel.
pub fn liquify_preview(src: &Raster, origin: (f32, f32), scale: f32, field: &Displacement) -> Raster {
    let mut out = Raster::new(src.width, src.height);
    let w = src.width as usize;
    out.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let cx = origin.0 + x as f32 / scale;
            let cy = origin.1 + y as f32 / scale;
            let d = field.at(cx, cy);
            let sx = (cx + d[0] - origin.0) * scale;
            let sy = (cy + d[1] - origin.1) * scale;
            *p = sample(src, (0, 0), sx, sy);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn checker(w: u32, h: u32) -> TileStore {
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = ((x / 8 + y / 8) % 2) as f32;
                r.set(x, y, Rgba::new(v, x as f32 / w as f32, y as f32 / h as f32, 1.0));
            }
        }
        TileStore::from_raster(&r, 0, 0)
    }

    #[test]
    fn identity_field_leaves_pixels_untouched() {
        let src = checker(300, 280);
        let field = Displacement::new(Rect::new(0, 0, 300, 280), 1);
        let out = liquify_store(&src, &field);
        let r = Rect::new(0, 0, 300, 280);
        assert_eq!(out.to_raster(r), src.to_raster(r));
    }

    #[test]
    fn a_uniform_field_shifts_content_exactly() {
        let src = checker(300, 280);
        let mut field = Displacement::new(Rect::new(0, 0, 300, 280), 4);
        for v in &mut field.d {
            *v = [-10.0, 3.0];
        }
        let out = liquify_store(&src, &field);
        // Output (x, y) shows the source at (x - 10, y + 3): content moved
        // right by 10 and up by 3, across tile seams too.
        for (x, y) in [(10, 0), (100, 50), (255, 100), (256, 100), (290, 250)] {
            let o = out.get_pixel(x, y);
            let s = src.get_pixel(x - 10, y + 3);
            assert_eq!(o, s, "at ({x}, {y})");
        }
        // Pixels whose source lies off the layer become transparent.
        assert_eq!(out.get_pixel(5, 100).a, 0.0);
    }

    #[test]
    fn push_moves_the_centre_by_the_drag_and_fades_to_the_rim() {
        let mut f = Displacement::new(Rect::new(0, 0, 200, 200), 1);
        f.dab(LiquifyTool::Push, (100.0, 100.0), 40.0, 1.0, 1.0, (6.0, 0.0));
        // Centre: weight 1, the content 6 px to the left arrives here.
        let c = f.at(100.0, 100.0);
        assert!(close(c[0], -6.0) && close(c[1], 0.0), "{c:?}");
        // Half radius: falloff (1 - 0.25)² = 0.5625.
        let h = f.at(100.0, 120.0);
        assert!(close(h[0], -6.0 * 0.5625), "{h:?}");
        // At and beyond the rim nothing moves.
        assert_eq!(f.at(100.0, 140.0), [0.0, 0.0]);
        assert_eq!(f.at(10.0, 10.0), [0.0, 0.0]);
    }

    #[test]
    fn repeated_pushes_advect_instead_of_adding_up_blindly() {
        // Two dabs of 3 px at the same spot move the centre by 6 px.
        let mut f = Displacement::new(Rect::new(0, 0, 200, 200), 1);
        f.dab(LiquifyTool::Push, (100.0, 100.0), 50.0, 1.0, 1.0, (3.0, 0.0));
        f.dab(LiquifyTool::Push, (100.0, 100.0), 50.0, 1.0, 1.0, (3.0, 0.0));
        let c = f.at(100.0, 100.0);
        assert!((c[0] + 6.0).abs() < 0.05, "{c:?}");
    }

    #[test]
    fn reconstruct_at_full_rate_restores_the_centre() {
        let mut f = Displacement::new(Rect::new(0, 0, 100, 100), 1);
        f.dab(LiquifyTool::Push, (50.0, 50.0), 30.0, 1.0, 1.0, (5.0, 5.0));
        assert!(f.at(50.0, 50.0)[0] < -4.9);
        f.dab(LiquifyTool::Reconstruct, (50.0, 50.0), 30.0, 1.0, 1.0, (0.0, 0.0));
        assert_eq!(f.at(50.0, 50.0), [0.0, 0.0]);
        // Off-centre it only eases back: weight (1 - (15/30)²)² = 0.5625.
        let before = -5.0 * 0.5625;
        let after = f.at(65.0, 50.0)[0];
        assert!(close(after, before * (1.0 - 0.5625)), "{after}");
    }

    #[test]
    fn twirl_pucker_and_bloat_move_content_the_right_way() {
        let r = Rect::new(0, 0, 200, 200);
        // Twirl clockwise (y down): content right of the centre comes from
        // above it, so the sample point is rotated counter-clockwise.
        let mut t = Displacement::new(r, 1);
        t.dab(LiquifyTool::TwirlCw, (100.0, 100.0), 60.0, 1.0, 1.0, (0.0, 0.0));
        let a = -TWIRL_PER_DAB * falloff(20.0, 60.0);
        let v = t.at(120.0, 100.0);
        assert!(close(v[0], 20.0 * a.cos() - 20.0), "{v:?}");
        assert!(close(v[1], 20.0 * a.sin()), "{v:?}");
        assert!(v[1] < 0.0, "samples from above");
        // Pucker samples farther out (content shrinks towards the centre).
        let mut p = Displacement::new(r, 1);
        p.dab(LiquifyTool::Pucker, (100.0, 100.0), 60.0, 1.0, 1.0, (0.0, 0.0));
        let w = falloff(20.0, 60.0);
        assert!(close(p.at(120.0, 100.0)[0], 20.0 * w * SCALE_PER_DAB));
        // Bloat samples closer in (content grows).
        let mut b = Displacement::new(r, 1);
        b.dab(LiquifyTool::Bloat, (100.0, 100.0), 60.0, 1.0, 1.0, (0.0, 0.0));
        assert!(close(b.at(120.0, 100.0)[0], -20.0 * w * SCALE_PER_DAB));
    }

    #[test]
    fn coarse_fields_interpolate_between_nodes() {
        let mut f = Displacement::new(Rect::new(0, 0, 64, 64), 8);
        f.d[f.cols + 1] = [8.0, 0.0]; // node (8, 8)
        assert_eq!(f.at(8.0, 8.0), [8.0, 0.0]);
        assert!(close(f.at(12.0, 8.0)[0], 4.0));
        assert!(close(f.at(12.0, 12.0)[0], 2.0));
        assert_eq!(Displacement::step_for(Rect::new(0, 0, 4000, 3000), 4_000_000), 2);
        assert_eq!(Displacement::step_for(Rect::new(0, 0, 1000, 1000), 4_000_000), 1);
    }

    #[test]
    #[ignore = "timing; cargo test --release -p lumenply-render liquify_timing -- --ignored --nocapture"]
    fn liquify_timing() {
        let (w, h) = (4000, 3000);
        let mut r = Raster::new(w, h);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            let v = (i % 251) as f32 / 251.0;
            *p = Rgba::new(v, 0.5, 1.0 - v, 1.0);
        }
        let src = TileStore::from_raster(&r, 0, 0);
        let rect = Rect::new(0, 0, w, h);
        let mut f = Displacement::new(rect, Displacement::step_for(rect, 4_000_000));
        for k in 0..30 {
            f.dab(LiquifyTool::Bloat, (2000.0, 1500.0), 900.0, 1.0, 0.1, (0.0, 0.0));
            f.dab(
                LiquifyTool::Push,
                (1000.0 + k as f32 * 10.0, 800.0),
                300.0,
                1.0,
                1.0,
                (10.0, 0.0),
            );
        }
        let t = std::time::Instant::now();
        let out = liquify_store(&src, &f);
        println!("12 MP liquify bake: {:?} ({} tiles)", t.elapsed(), out.len());
        let small = Raster::new(1400, 1050);
        let t = std::time::Instant::now();
        let _ = liquify_preview(&small, (0.0, 0.0), 0.35, &f);
        println!("1.5 MP preview frame: {:?}", t.elapsed());
    }

    #[test]
    fn the_preview_matches_the_full_render_at_scale_one() {
        let src = checker(120, 90);
        let mut f = Displacement::new(Rect::new(0, 0, 120, 90), 1);
        f.dab(LiquifyTool::Bloat, (60.0, 45.0), 30.0, 1.0, 1.0, (0.0, 0.0));
        let full = liquify_store(&src, &f).to_raster(Rect::new(0, 0, 120, 90));
        let prev = liquify_preview(&src.to_raster(Rect::new(0, 0, 120, 90)), (0.0, 0.0), 1.0, &f);
        let mut worst = 0.0f32;
        for (a, b) in full.pixels.iter().zip(&prev.pixels) {
            worst = worst.max((a.r - b.r).abs()).max((a.a - b.a).abs());
        }
        assert!(worst < 1e-4, "preview drifts from the render by {worst}");
    }
}
