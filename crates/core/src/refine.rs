//! Select and Mask (Photoshop's refine edge): fit a selection's edge to the
//! image, so hair, fur and soft focus get partial coverage.
//!
//! The selection's 50 % contour, widened by `radius` either side (plus
//! whatever the Refine Edge brush painted), is the uncertain band; inside
//! and outside it the selection stays as it was. Each band pixel's coverage
//! comes from local colour models (sampling matting): the sure foreground
//! and background pixels just beyond the band around it are clustered into
//! a few colours each, and the pixel takes the foreground/background pair
//! that explains its colour best, `I = αF + (1 − α)B`, in linear light
//! (where optical mixing happens). A colour-guided filter then settles the
//! matte onto the image's local colour lines. Smart Radius narrows the
//! band where the edge turns out hard. Smooth, Feather, Contrast and Shift
//! Edge follow, in Photoshop's order; Decontaminate Colours replaces edge
//! colours with the estimated foreground colour for the new-layer outputs.
//!
//! [`refine_matte`] works on dense buffers so the workspace can run it on a
//! reduced preview; [`RefineSelection`] runs it at full resolution as one
//! undo step.

use std::sync::Arc;

use lumenply_doc::{Document, Layer, LayerId, Mask, Selection};
use lumenply_tiles::{Raster, Rect, Rgba, TileStore, TILE_SIZE};
use rayon::prelude::*;

use crate::commands::SampleSource;
use crate::{Command, EditError, EditResult};

/// Side of a matting cell in pixels; each builds its own colour models.
const CELL: usize = 16;
/// Colour clusters per side (foreground, background) and cell.
const CLUSTERS: usize = 4;
/// Samples per side and cell fed to the clustering.
const SAMPLES: usize = 600;
/// Shift Edge never moves the edge further than this (document pixels).
pub const MAX_SHIFT: f32 = 64.0;
/// "No such pixel" for the distance transforms (squared pixels).
const FAR: f64 = 1.0e12;

/// The workspace's sliders. Pixel amounts are in document pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefineParams {
    /// Half-width of the uncertain band around the selection edge, 0..=250.
    pub radius: f32,
    /// Narrow the band where the edge is hard.
    pub smart_radius: bool,
    /// Rounds off jaggies, 0..=100.
    pub smooth: f32,
    /// Blur of the result, 0..=250 px.
    pub feather: f32,
    /// Sharpens the transition, 0..=100 %.
    pub contrast: f32,
    /// Moves the edge out (+) or in (−), −100..=100 % of radius + feather
    /// (at least 4 px, at most [`MAX_SHIFT`]).
    pub shift_edge: f32,
    /// Replace edge colours with the estimated foreground (new-layer outputs).
    pub decontaminate: bool,
    /// How much of the decontaminated colour to use, 0..=100 %.
    pub decontam_amount: f32,
}

impl Default for RefineParams {
    fn default() -> Self {
        RefineParams {
            radius: 0.0,
            smart_radius: false,
            smooth: 0.0,
            feather: 0.0,
            contrast: 0.0,
            shift_edge: 0.0,
            decontaminate: false,
            decontam_amount: 100.0,
        }
    }
}

impl RefineParams {
    /// Every value finite and inside its range.
    pub fn sanitized(&self) -> RefineParams {
        let c = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { 0.0 };
        RefineParams {
            radius: c(self.radius, 0.0, 250.0),
            smart_radius: self.smart_radius,
            smooth: c(self.smooth, 0.0, 100.0),
            feather: c(self.feather, 0.0, 250.0),
            contrast: c(self.contrast, 0.0, 100.0),
            shift_edge: c(self.shift_edge, -100.0, 100.0),
            decontaminate: self.decontaminate,
            decontam_amount: c(self.decontam_amount, 0.0, 100.0),
        }
    }

    /// Shift Edge in document pixels (signed).
    pub fn shift_px(&self) -> f32 {
        let reach = (self.radius + self.feather).max(4.0);
        (self.shift_edge / 100.0 * reach).clamp(-MAX_SHIFT, MAX_SHIFT)
    }

    /// How far (document pixels) the result can differ from the
    /// selection beyond its edge: the work area's margin.
    pub fn reach(&self) -> i32 {
        let p = self.sanitized();
        let feather = 3.0 * lumenply_doc::box_radius(p.feather) as f32;
        let smooth = 3.0 * lumenply_doc::box_radius(smooth_px(p.smooth)) as f32;
        (3.0 * p.radius + feather + smooth + p.shift_px().abs() + 8.0).ceil() as i32
    }

    /// Decontamination is on with a non-zero amount.
    pub fn decontaminates(&self) -> bool {
        self.decontaminate && self.decontam_amount > 0.0
    }
}

/// Smooth's blur size in pixels (100 → 8 px).
fn smooth_px(smooth: f32) -> f32 {
    smooth * 0.08
}

/// One dab of the Refine Edge brush, in document pixels. `erase` restores
/// the original edge under it instead of adding it to the band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefineDab {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub erase: bool,
}

/// Where the refined selection goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RefineOutput {
    #[default]
    Selection,
    LayerMask,
    NewLayer,
    NewLayerWithMask,
}

impl RefineOutput {
    pub const ALL: [RefineOutput; 4] = [
        RefineOutput::Selection,
        RefineOutput::LayerMask,
        RefineOutput::NewLayer,
        RefineOutput::NewLayerWithMask,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RefineOutput::Selection => "Selection",
            RefineOutput::LayerMask => "Layer mask",
            RefineOutput::NewLayer => "New layer",
            RefineOutput::NewLayerWithMask => "New layer with mask",
        }
    }

    /// Writes pixels (so decontamination can apply).
    pub fn makes_layer(self) -> bool {
        matches!(self, RefineOutput::NewLayer | RefineOutput::NewLayerWithMask)
    }
}

/// Paint the brush dabs into a band override for a `w`×`h` buffer whose
/// top-left is document point `origin`, at `scale` buffer pixels per
/// document pixel: 1 adds to the band, −1 removes, 0 leaves it.
pub fn paint_band(dabs: &[RefineDab], w: usize, h: usize, origin: (f32, f32), scale: f32) -> Vec<i8> {
    let mut out = vec![0i8; w * h];
    for d in dabs {
        let (cx, cy) = ((d.x - origin.0) * scale, (d.y - origin.1) * scale);
        let r = (d.radius * scale).max(0.5);
        let (x0, x1) = ((cx - r).floor().max(0.0), (cx + r).ceil().min(w as f32));
        let (y0, y1) = ((cy - r).floor().max(0.0), (cy + r).ceil().min(h as f32));
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        let v = if d.erase { -1 } else { 1 };
        for y in y0 as usize..y1 as usize {
            for x in x0 as usize..x1 as usize {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                if dx * dx + dy * dy <= r * r {
                    out[y * w + x] = v;
                }
            }
        }
    }
    out
}

/// The uncertain band Edge Detection works in (for the workspace's Show
/// Edge view): within `radius` of the selection edge, plus the brush.
pub fn refine_band(coverage: &[f32], band: Option<&[i8]>, w: usize, h: usize, radius: f32) -> Vec<bool> {
    let inside: Vec<bool> = coverage.iter().map(|&c| c >= 0.5).collect();
    let mut unknown = vec![false; w * h];
    if radius > 0.0 {
        let sd = signed_distance(&inside, w, h);
        for (u, d) in unknown.iter_mut().zip(&sd) {
            *u = d.abs() < radius;
        }
    }
    if let Some(b) = band {
        for (u, &v) in unknown.iter_mut().zip(b) {
            match v {
                1 => *u = true,
                -1 => *u = false,
                _ => {}
            }
        }
    }
    unknown
}

/// The refined coverage of a `image`-sized buffer.
///
/// `coverage` is the selection over the same pixels, `band` the brush's
/// override (see [`paint_band`]), `scale` the buffer's pixels per document
/// pixel (1 at full size, less on a preview).
pub fn refine_matte(
    image: &Raster,
    coverage: &[f32],
    band: Option<&[i8]>,
    p: &RefineParams,
    scale: f32,
) -> Vec<f32> {
    let matte = edge_matte(image, coverage, band, p, scale);
    global_refine(matte, image.width as usize, image.height as usize, p, scale)
}

fn sane_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// Edge detection alone (Radius, Smart Radius and the brush): the matte
/// before the global refinements. The workspace keeps it while only
/// those change.
pub fn edge_matte(
    image: &Raster,
    coverage: &[f32],
    band: Option<&[i8]>,
    p: &RefineParams,
    scale: f32,
) -> Vec<f32> {
    let p = p.sanitized();
    let (w, h) = (image.width as usize, image.height as usize);
    assert_eq!(coverage.len(), w * h, "coverage does not match the image");
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let scale = sane_scale(scale);
    let tri = Trimap::new(coverage, band, w, h, p.radius * scale);
    let mut alpha = coverage.to_vec();
    if tri.any_unknown {
        let colours: Vec<[f32; 4]> = image.pixels.iter().map(|q| [q.r, q.g, q.b, q.a]).collect();
        let est = sample_matte(&colours, &tri, coverage, w, h);
        let r_gf = ((2.0 * scale).round() as usize).clamp(1, 4);
        let guided = guided_filter_band(&colours, &est, &tri.unknown, w, h, r_gf, GF_EPS);
        for (i, a) in alpha.iter_mut().enumerate() {
            if tri.unknown[i] {
                let v = guided[i].clamp(0.0, 1.0);
                *a = if v < 0.03 {
                    0.0
                } else if v > 0.97 {
                    1.0
                } else {
                    v
                };
            }
        }
        if p.smart_radius && p.radius * scale > 2.0 {
            smart_radius(&mut alpha, &tri.unknown, w, h, p.radius * scale);
        }
    }
    alpha
}

/// The global refinements (Smooth, Feather, Contrast, Shift Edge) of a
/// `w`×`h` matte, in Photoshop's order.
pub fn global_refine(mut alpha: Vec<f32>, w: usize, h: usize, p: &RefineParams, scale: f32) -> Vec<f32> {
    let p = p.sanitized();
    let scale = sane_scale(scale);
    if w == 0 || h == 0 {
        return alpha;
    }
    let rs = smooth_px(p.smooth) * scale;
    if rs > 0.0 {
        smooth(&mut alpha, w, h, rs);
    }
    if p.feather * scale > 0.0 {
        let b = lumenply_doc::box_radius(p.feather * scale) as usize;
        for _ in 0..3 {
            alpha = box_mean(&alpha, w, h, b);
        }
    }
    if p.contrast > 0.0 {
        let k = p.contrast / 100.0;
        for a in &mut alpha {
            *a = contrast(*a, k);
        }
    }
    let d = p.shift_px() * scale;
    if d.abs() > 1e-3 {
        alpha = shift_edge(&alpha, w, h, d);
    }
    alpha
}

/// Apply Contrast `k` (0..=1) to one coverage value.
fn contrast(a: f32, k: f32) -> f32 {
    if k >= 0.999 {
        if a >= 0.5 {
            1.0
        } else {
            0.0
        }
    } else {
        (0.5 + (a - 0.5) / (1.0 - k)).clamp(0.0, 1.0)
    }
}

/// The uncertain band and the sure regions either side of it.
struct Trimap {
    /// Selected (coverage ≥ ½).
    inside: Vec<bool>,
    unknown: Vec<bool>,
    any_unknown: bool,
    /// Distance to the nearest sure foreground / background pixel.
    to_fg: Vec<f32>,
    to_bg: Vec<f32>,
    /// Sure pixels close enough to the band to sample its colours.
    ring: Vec<bool>,
}

impl Trimap {
    fn new(coverage: &[f32], band: Option<&[i8]>, w: usize, h: usize, radius: f32) -> Trimap {
        let inside: Vec<bool> = coverage.iter().map(|&c| c >= 0.5).collect();
        let n = w * h;
        let unknown = refine_band(coverage, band, w, h, radius);
        let any_unknown = unknown.iter().any(|&u| u);
        if !any_unknown {
            return Trimap {
                inside,
                unknown,
                any_unknown,
                to_fg: Vec::new(),
                to_bg: Vec::new(),
                ring: Vec::new(),
            };
        }
        let fg: Vec<bool> = (0..n).map(|i| !unknown[i] && inside[i]).collect();
        let bg: Vec<bool> = (0..n).map(|i| !unknown[i] && !inside[i]).collect();
        let to_fg: Vec<f32> = edt_sq(&fg, w, h).into_iter().map(f32::sqrt).collect();
        let to_bg: Vec<f32> = edt_sq(&bg, w, h).into_iter().map(f32::sqrt).collect();
        let to_unknown = edt_sq(&unknown, w, h);
        let reach = radius.max(4.0);
        let ring: Vec<bool> = (0..n)
            .map(|i| !unknown[i] && to_unknown[i] <= reach * reach)
            .collect();
        Trimap {
            inside,
            unknown,
            any_unknown,
            to_fg,
            to_bg,
            ring,
        }
    }
}

/// Signed distance to the edge of `inside`: positive inside (half a pixel
/// on an edge pixel), negative outside.
fn signed_distance(inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    let outside: Vec<bool> = inside.iter().map(|&b| !b).collect();
    let to_out = edt_sq(&outside, w, h);
    let to_in = edt_sq(inside, w, h);
    (0..w * h)
        .map(|i| {
            if inside[i] {
                to_out[i].sqrt() - 0.5
            } else {
                -(to_in[i].sqrt() - 0.5)
            }
        })
        .collect()
}

/// Squared Euclidean distance from every pixel to the nearest `set` pixel
/// (Felzenszwalb & Huttenlocher), [`FAR`] where there is none.
fn edt_sq(set: &[bool], w: usize, h: usize) -> Vec<f32> {
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let f: Vec<f64> = set[y * w..(y + 1) * w]
            .iter()
            .map(|&b| if b { 0.0 } else { FAR })
            .collect();
        edt_line(&f, out);
    });
    let t = transpose(&rows, w, h);
    let mut cols = vec![0f32; w * h];
    cols.par_chunks_mut(h).enumerate().for_each(|(x, out)| {
        let f: Vec<f64> = t[x * h..(x + 1) * h].iter().map(|&v| v as f64).collect();
        edt_line(&f, out);
    });
    transpose(&cols, h, w)
}

/// One line of the distance transform: lower envelope of parabolas.
fn edt_line(f: &[f64], out: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut v = vec![0usize; n];
    let mut z = vec![0f64; n + 1];
    let mut k = 0usize;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        let fq = f[q] + (q * q) as f64;
        loop {
            let p = v[k];
            let s = (fq - (f[p] + (p * p) as f64)) / (2.0 * (q - p) as f64);
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
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        let d = q as f64 - p as f64;
        *o = (d * d + f[p]).min(FAR) as f32;
    }
}

/// `src` is `w`×`h` row-major; the result is `h`×`w`.
fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, row)| {
        for (y, o) in row.iter_mut().enumerate() {
            *o = src[y * w + x];
        }
    });
    out
}

/// Horizontal mean over `[x − r, x + r]` clipped to the row.
fn hmean(v: &[f32], w: usize, out: &mut [f32], r: usize) {
    let mut acc: f64 = v[..=r.min(w - 1)].iter().map(|&x| x as f64).sum();
    for x in 0..w {
        let lo = x.saturating_sub(r);
        let hi = (x + r).min(w - 1);
        out[x] = (acc / (hi - lo + 1) as f64) as f32;
        if x + r + 1 < w {
            acc += v[x + r + 1] as f64;
        }
        if x >= r {
            acc -= v[x - r] as f64;
        }
    }
}

/// Mean over a (2r+1)² box clipped to the buffer, in parallel.
fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut a = vec![0f32; w * h];
    a.par_chunks_mut(w)
        .zip(v.par_chunks(w))
        .for_each(|(o, row)| hmean(row, w, o, r));
    let t = transpose(&a, w, h);
    let mut b = vec![0f32; w * h];
    b.par_chunks_mut(h)
        .zip(t.par_chunks(h))
        .for_each(|(o, col)| hmean(col, h, o, r));
    transpose(&b, h, w)
}

/// The same mean, single-threaded (used inside parallel strips).
fn box_mean_seq(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut a = vec![0f32; w * h];
    for (o, row) in a.chunks_mut(w).zip(v.chunks(w)) {
        hmean(row, w, o, r);
    }
    let mut col = vec![0f32; h];
    let mut res = vec![0f32; h];
    let mut out = vec![0f32; w * h];
    for x in 0..w {
        for y in 0..h {
            col[y] = a[y * w + x];
        }
        hmean(&col, h, &mut res, r);
        for y in 0..h {
            out[y * w + x] = res[y];
        }
    }
    out
}

fn dist2(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    (0..4).map(|c| (a[c] - b[c]) * (a[c] - b[c])).sum()
}

/// A few representative colours of `samples` with their spread (RMS
/// distance of the members to their centre): k-means from a
/// farthest-point start, so it is deterministic.
fn clusters(samples: &[[f32; 4]]) -> Vec<([f32; 4], f32)> {
    let k = samples.len().min(CLUSTERS);
    if k == 0 {
        return Vec::new();
    }
    let mut centres = vec![samples[0]];
    while centres.len() < k {
        let (far, d) = samples
            .iter()
            .map(|s| (s, centres.iter().map(|c| dist2(s, c)).fold(f32::MAX, f32::min)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("non-empty");
        if d <= 1e-10 {
            break; // fewer distinct colours than clusters
        }
        centres.push(*far);
    }
    let k = centres.len();
    let mut spread = vec![0f32; k];
    for _ in 0..6 {
        let mut sum = vec![[0f32; 4]; k];
        let mut err = vec![0f32; k];
        let mut count = vec![0usize; k];
        for s in samples {
            let (j, d) = (0..k)
                .map(|j| (j, dist2(s, &centres[j])))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .expect("k > 0");
            for c in 0..4 {
                sum[j][c] += s[c];
            }
            err[j] += d;
            count[j] += 1;
        }
        for j in 0..k {
            if count[j] > 0 {
                for c in 0..4 {
                    centres[j][c] = sum[j][c] / count[j] as f32;
                }
                spread[j] = (err[j] / count[j] as f32).sqrt();
            }
        }
    }
    centres.into_iter().zip(spread).collect()
}

/// Coverage estimate from the colour models of each cell, for every
/// unknown pixel and for the sure ones in the sampling ring (so the guided
/// filter sees a matte consistent with the colours around the band, even
/// where the Refine Edge brush cut across a wrong edge); the rest keep
/// their coverage.
fn sample_matte(colours: &[[f32; 4]], tri: &Trimap, coverage: &[f32], w: usize, h: usize) -> Vec<f32> {
    let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let cells: Vec<(usize, usize)> = (0..ch)
        .flat_map(|cy| (0..cw).map(move |cx| (cx, cy)))
        .filter(|&(cx, cy)| {
            (cy * CELL..((cy + 1) * CELL).min(h)).any(|y| {
                (cx * CELL..((cx + 1) * CELL).min(w)).any(|x| tri.unknown[y * w + x] || tri.ring[y * w + x])
            })
        })
        .collect();
    let results: Vec<Vec<(usize, f32)>> = cells
        .par_iter()
        .map(|&(cx, cy)| {
            let (x0, x1) = (cx * CELL, ((cx + 1) * CELL).min(w));
            let (y0, y1) = (cy * CELL, ((cy + 1) * CELL).min(h));
            // Reach far enough to find both sides from every pixel here.
            let mut m = 0f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = y * w + x;
                    if tri.unknown[i] || tri.ring[i] {
                        m = m.max(tri.to_fg[i].min(1e5)).max(tri.to_bg[i].min(1e5));
                    }
                }
            }
            let m = (m.ceil() as usize + 2 + CELL / 2).min(w.max(h));
            let (gx0, gx1) = (x0.saturating_sub(m), (x1 + m).min(w));
            let (gy0, gy1) = (y0.saturating_sub(m), (y1 + m).min(h));
            let area = (gx1 - gx0) * (gy1 - gy0);
            let stride = ((area as f32 / (4.0 * SAMPLES as f32)).sqrt().floor() as usize).max(1);
            let (mut fs, mut bs) = (Vec::new(), Vec::new());
            for y in (gy0..gy1).step_by(stride) {
                for x in (gx0..gx1).step_by(stride) {
                    let i = y * w + x;
                    if tri.ring[i] {
                        if tri.inside[i] {
                            fs.push(colours[i]);
                        } else {
                            bs.push(colours[i]);
                        }
                    }
                }
            }
            let thin = |v: Vec<[f32; 4]>| -> Vec<[f32; 4]> {
                let step = v.len().div_ceil(SAMPLES).max(1);
                v.into_iter().step_by(step).collect()
            };
            let fc = clusters(&thin(fs));
            let bc = clusters(&thin(bs));
            let mut out = Vec::new();
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = y * w + x;
                    if !tri.unknown[i] && !tri.ring[i] {
                        continue;
                    }
                    let a = if fc.is_empty() || bc.is_empty() {
                        coverage[i]
                    } else {
                        best_alpha(&colours[i], &fc, &bc)
                    };
                    out.push((i, a));
                }
            }
            out
        })
        .collect();
    let mut est = coverage.to_vec();
    for cell in results {
        for (i, a) in cell {
            est[i] = a;
        }
    }
    est
}

/// The coverage of colour `c` under the foreground/background pair that
/// explains it best.
fn best_alpha(c: &[f32; 4], fc: &[([f32; 4], f32)], bc: &[([f32; 4], f32)]) -> f32 {
    let near = |set: &[([f32; 4], f32)]| {
        set.iter()
            .map(|(m, s)| (dist2(c, m).sqrt(), *s))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .expect("non-empty")
    };
    let (df, sf) = near(fc);
    let (db, sb) = near(bc);
    // Within one side's own spread but not the other's: that side's
    // colour, plain noise rather than a mixture. (Both happen where the
    // sure regions disagree, e.g. a brushed area cut across a wrong edge;
    // the pairs below sort that out.)
    const TOL: f32 = 1e-3;
    let (in_f, in_b) = (df <= sf.max(TOL), db <= sb.max(TOL));
    if in_f && !in_b {
        return 1.0;
    }
    if in_b && !in_f {
        return 0.0;
    }
    let nearer = if df < db { 1.0 } else { 0.0 };
    let mut best = (f32::MAX, nearer);
    for (f, _) in fc {
        for (b, _) in bc {
            let mut fb = [0f32; 4];
            let mut cb = [0f32; 4];
            for k in 0..4 {
                fb[k] = f[k] - b[k];
                cb[k] = c[k] - b[k];
            }
            let len2: f32 = fb.iter().map(|v| v * v).sum();
            if len2 < 1e-6 {
                continue;
            }
            let a = ((0..4).map(|k| cb[k] * fb[k]).sum::<f32>() / len2).clamp(0.0, 1.0);
            let resid: f32 = (0..4).map(|k| (cb[k] - a * fb[k]).powi(2)).sum();
            // Distortion relative to how far apart the pair is (robust
            // matting's confidence), so a well-separated pair wins ties.
            let cost = resid / (len2 + 1e-4);
            if cost < best.0 {
                best = (cost, a);
            }
        }
    }
    // A colour no pair explains (off every mixing line by more than a
    // third of the pair's separation) is not a mixture: a colour the
    // models missed. It goes to the side it is nearer, rather than to a
    // meaningless projection, and the guided filter smooths the call.
    if best.0 > UNEXPLAINED * UNEXPLAINED {
        return nearer;
    }
    best.1
}

/// Relative distance from the best mixing line beyond which a colour
/// counts as unexplained (see [`best_alpha`]).
const UNEXPLAINED: f32 = 0.35;

/// The guided filter's ε (linear-light colour variance): below it a
/// window counts as flat and the matte is averaged there.
const GF_EPS: f32 = 4e-4;

/// A filtered strip: first row, column span, values.
type StripOut = (usize, usize, usize, Vec<f32>);

/// Rows per guided-filter strip.
const STRIP: usize = 96;

/// He et al.'s colour-guided filter of `p`, computed only around the
/// unknown pixels (strips in parallel) and returned for every pixel
/// (`p` elsewhere).
fn guided_filter_band(
    guide: &[[f32; 4]],
    p: &[f32],
    unknown: &[bool],
    w: usize,
    h: usize,
    r: usize,
    eps: f32,
) -> Vec<f32> {
    let halo = 2 * r + 2;
    let strips: Vec<(usize, usize)> = (0..h.div_ceil(STRIP))
        .map(|s| (s * STRIP, ((s + 1) * STRIP).min(h)))
        .collect();
    let results: Vec<Option<StripOut>> = strips
        .par_iter()
        .map(|&(y0, y1)| {
            // Columns that hold unknown pixels in this strip.
            let mut xs = (usize::MAX, 0usize);
            for y in y0..y1 {
                for x in 0..w {
                    if unknown[y * w + x] {
                        xs.0 = xs.0.min(x);
                        xs.1 = xs.1.max(x + 1);
                    }
                }
            }
            if xs.0 >= xs.1 {
                return None;
            }
            let (ax0, ax1) = (xs.0.saturating_sub(halo), (xs.1 + halo).min(w));
            let (ay0, ay1) = (y0.saturating_sub(halo), (y1 + halo).min(h));
            let (sw, sh) = (ax1 - ax0, ay1 - ay0);
            let at = |x: usize, y: usize| (ay0 + y) * w + ax0 + x;
            let n = sw * sh;
            let mut chan: Vec<Vec<f32>> = vec![vec![0f32; n]; 13];
            for y in 0..sh {
                for x in 0..sw {
                    let i = at(x, y);
                    let g = guide[i];
                    let j = y * sw + x;
                    let pv = p[i];
                    chan[0][j] = g[0];
                    chan[1][j] = g[1];
                    chan[2][j] = g[2];
                    chan[3][j] = pv;
                    chan[4][j] = g[0] * pv;
                    chan[5][j] = g[1] * pv;
                    chan[6][j] = g[2] * pv;
                    chan[7][j] = g[0] * g[0];
                    chan[8][j] = g[0] * g[1];
                    chan[9][j] = g[0] * g[2];
                    chan[10][j] = g[1] * g[1];
                    chan[11][j] = g[1] * g[2];
                    chan[12][j] = g[2] * g[2];
                }
            }
            let m: Vec<Vec<f32>> = chan.iter().map(|c| box_mean_seq(c, sw, sh, r)).collect();
            let mut ab: Vec<Vec<f32>> = vec![vec![0f32; n]; 4];
            for j in 0..n {
                let mi = [m[0][j], m[1][j], m[2][j]];
                let mp = m[3][j];
                let cov = [m[4][j] - mi[0] * mp, m[5][j] - mi[1] * mp, m[6][j] - mi[2] * mp];
                let s = [
                    [
                        m[7][j] - mi[0] * mi[0] + eps,
                        m[8][j] - mi[0] * mi[1],
                        m[9][j] - mi[0] * mi[2],
                    ],
                    [
                        m[8][j] - mi[0] * mi[1],
                        m[10][j] - mi[1] * mi[1] + eps,
                        m[11][j] - mi[1] * mi[2],
                    ],
                    [
                        m[9][j] - mi[0] * mi[2],
                        m[11][j] - mi[1] * mi[2],
                        m[12][j] - mi[2] * mi[2] + eps,
                    ],
                ];
                let a = solve3(s, cov);
                ab[0][j] = a[0];
                ab[1][j] = a[1];
                ab[2][j] = a[2];
                ab[3][j] = mp - a[0] * mi[0] - a[1] * mi[1] - a[2] * mi[2];
            }
            let mab: Vec<Vec<f32>> = ab.iter().map(|c| box_mean_seq(c, sw, sh, r)).collect();
            // The strip's own rows, its unknown columns.
            let (ox0, ox1) = (xs.0, xs.1);
            let mut out = Vec::with_capacity((y1 - y0) * (ox1 - ox0));
            for y in y0..y1 {
                for x in ox0..ox1 {
                    let i = y * w + x;
                    let j = (y - ay0) * sw + (x - ax0);
                    let g = guide[i];
                    out.push(mab[0][j] * g[0] + mab[1][j] * g[1] + mab[2][j] * g[2] + mab[3][j]);
                }
            }
            Some((y0, ox0, ox1, out))
        })
        .collect();
    let mut q = p.to_vec();
    for (y0, x0, x1, vals) in results.into_iter().flatten() {
        let sw = x1 - x0;
        for (k, v) in vals.into_iter().enumerate() {
            let i = (y0 + k / sw) * w + x0 + k % sw;
            if unknown[i] {
                q[i] = v;
            }
        }
    }
    q
}

/// Solve the symmetric 3×3 system `s · a = b` (Cramer; `s` is positive
/// definite thanks to the filter's ε).
fn solve3(s: [[f32; 3]; 3], b: [f32; 3]) -> [f32; 3] {
    let det = |m: [[f32; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(s);
    if d.abs() < 1e-30 {
        return [0.0; 3];
    }
    let mut a = [0f32; 3];
    for (c, ac) in a.iter_mut().enumerate() {
        let mut m = s;
        for r in 0..3 {
            m[r][c] = b[r];
        }
        *ac = det(m) / d;
    }
    a
}

/// Smart Radius: where the matte's transition is narrow (a hard edge),
/// pixels further than a little past it from its 50 % contour go back to
/// plain inside/outside; soft edges keep the full band.
fn smart_radius(alpha: &mut [f32], unknown: &[bool], w: usize, h: usize, radius: f32) {
    let partial: Vec<f32> = alpha
        .iter()
        .zip(unknown)
        .map(|(&a, &u)| if u && a > 0.0 && a < 1.0 { 1.0 } else { 0.0 })
        .collect();
    let r = ((radius / 2.0).round() as usize).max(2);
    // Partial pixels per window row ≈ how wide the transition is.
    let width = box_mean(&partial, w, h, r);
    let inside: Vec<bool> = alpha.iter().map(|&a| a >= 0.5).collect();
    let sd = signed_distance(&inside, w, h);
    for i in 0..w * h {
        if !unknown[i] {
            continue;
        }
        let local = (1.5 * width[i] * (2 * r + 1) as f32 + 1.0).clamp(1.5, radius);
        if sd[i].abs() > local {
            alpha[i] = if inside[i] { 1.0 } else { 0.0 };
        }
    }
}

/// Smooth: blur, then steepen back so a straight edge keeps its place
/// and sharpness while jaggies and specks smaller than the blur go.
fn smooth(alpha: &mut [f32], w: usize, h: usize, px: f32) {
    let b = lumenply_doc::box_radius(px) as usize;
    let mut blurred = alpha.to_vec();
    for _ in 0..3 {
        blurred = box_mean(&blurred, w, h, b);
    }
    // The three-pass kernel's centre tap is k0 = (3b² + 3b + 1)/(2b + 1)³,
    // so a blurred straight step reads ½ ± k0/2 either side of the edge:
    // a gain of 1.5/k0 puts both back at 0 and 1, and fills notches and
    // drops specks once the blur has averaged them below ½.
    let k0 = (3 * b * b + 3 * b + 1) as f32 / ((2 * b + 1).pow(3)) as f32;
    let gain = 1.5 / k0;
    for (a, v) in alpha.iter_mut().zip(blurred) {
        *a = (0.5 + (v - 0.5) * gain).clamp(0.0, 1.0);
    }
}

/// Grey-scale dilation (d > 0) or erosion (d < 0) by a disc of radius |d|,
/// fractional radii blended between the neighbouring whole ones. Windows
/// clip at the buffer edge, so nothing grows in from or shrinks away from
/// it.
fn shift_edge(alpha: &[f32], w: usize, h: usize, d: f32) -> Vec<f32> {
    let grow = d > 0.0;
    let src: Vec<f32> = if grow {
        alpha.to_vec()
    } else {
        alpha.iter().map(|a| 1.0 - a).collect()
    };
    let r = d.abs();
    let (lo, frac) = (r.floor() as usize, r.fract());
    let a = dilate_disc(&src, w, h, lo);
    let out: Vec<f32> = if frac > 1e-3 {
        let b = dilate_disc(&src, w, h, lo + 1);
        a.iter().zip(&b).map(|(x, y)| x + (y - x) * frac).collect()
    } else {
        a
    };
    if grow {
        out
    } else {
        out.into_iter().map(|v| 1.0 - v).collect()
    }
}

/// Maximum over a disc of radius `r` (the pixel centres within `r + ½`).
fn dilate_disc(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 {
        return src.to_vec();
    }
    let rr = (r as f32 + 0.5) * (r as f32 + 0.5);
    let half: Vec<usize> = (0..=r)
        .map(|dy| (rr - (dy * dy) as f32).max(0.0).sqrt().floor() as usize)
        .map(|k| k.min(r))
        .collect();
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, orow)| {
        let mut tmp = vec![0f32; w];
        for dy in -(r as isize)..=(r as isize) {
            let sy = y as isize + dy;
            if sy < 0 || sy >= h as isize {
                continue;
            }
            let row = &src[sy as usize * w..(sy as usize + 1) * w];
            sliding_max(row, half[dy.unsigned_abs()], &mut tmp);
            for (o, t) in orow.iter_mut().zip(&tmp) {
                *o = o.max(*t);
            }
        }
    });
    out
}

/// Maximum over `[x − k, x + k]` clipped to the row (monotonic deque).
fn sliding_max(row: &[f32], k: usize, out: &mut [f32]) {
    let n = row.len();
    let mut q: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut next = 0usize;
    for (x, o) in out.iter_mut().enumerate().take(n) {
        let hi = (x + k).min(n - 1);
        while next <= hi {
            while q.back().is_some_and(|&b| row[b] <= row[next]) {
                q.pop_back();
            }
            q.push_back(next);
            next += 1;
        }
        let lo = x.saturating_sub(k);
        while q.front().is_some_and(|&f| f < lo) {
            q.pop_front();
        }
        *o = row[*q.front().expect("window holds x")];
    }
}

/// Decontaminate Colours: edge pixels (partly covered, or in the band)
/// move toward the foreground colour estimated from the matte, `I = αF +
/// (1 − α)B` solved with the mean background colour around each pixel.
/// `matte` is the coverage before the global refinements (the optical
/// mix), `alpha` the final coverage. Returns the new premultiplied colours.
pub fn decontaminate(
    image: &Raster,
    coverage: &[f32],
    matte: &[f32],
    alpha: &[f32],
    band_radius: f32,
    amount: f32,
) -> Vec<Rgba> {
    let (w, h) = (image.width as usize, image.height as usize);
    let n = w * h;
    let k = (amount / 100.0).clamp(0.0, 1.0);
    let sure_f: Vec<f32> = matte
        .iter()
        .map(|&a| if a >= 0.999 { 1.0 } else { 0.0 })
        .collect();
    let sure_b: Vec<f32> = matte
        .iter()
        .map(|&a| if a <= 0.001 { 1.0 } else { 0.0 })
        .collect();
    let r = (2.0 * band_radius).ceil() as usize + 4;
    let wf = box_mean(&sure_f, w, h, r);
    let wb = box_mean(&sure_b, w, h, r);
    let mut fm = Vec::with_capacity(4);
    let mut bm = Vec::with_capacity(4);
    for c in 0..4 {
        let ch = |p: &Rgba| [p.r, p.g, p.b, p.a][c];
        let fv: Vec<f32> = image.pixels.iter().zip(&sure_f).map(|(p, s)| ch(p) * s).collect();
        let bv: Vec<f32> = image.pixels.iter().zip(&sure_b).map(|(p, s)| ch(p) * s).collect();
        fm.push(box_mean(&fv, w, h, r));
        bm.push(box_mean(&bv, w, h, r));
    }
    (0..n)
        .into_par_iter()
        .map(|i| {
            let p = image.pixels[i];
            let a = alpha[i];
            let m = matte[i];
            let edge = a > 0.0 && (a < 1.0 || (m > 0.0 && m < 1.0) || coverage[i] < 1.0);
            if !edge || k == 0.0 || m >= 0.999 {
                return p;
            }
            let c = [p.r, p.g, p.b, p.a];
            let fbar = (wf[i] > 1e-6).then(|| [0, 1, 2, 3].map(|ch| fm[ch][i] / wf[i]));
            let bbar = (wb[i] > 1e-6).then(|| [0, 1, 2, 3].map(|ch| bm[ch][i] / wb[i]));
            let solved = bbar.filter(|_| m >= 0.1).map(|b| {
                let mut f = [0f32; 4];
                for ch in 0..4 {
                    f[ch] = (b[ch] + (c[ch] - b[ch]) / m).clamp(0.0, 1.0);
                }
                // Premultiplied: colour never exceeds coverage.
                for ch in 0..3 {
                    f[ch] = f[ch].min(f[3]);
                }
                f
            });
            let f = match (solved, fbar) {
                (Some(s), Some(fb)) => {
                    // Trust the solve where the pixel is mostly foreground.
                    let t = ((m - 0.1) / 0.3).clamp(0.0, 1.0);
                    [0, 1, 2, 3].map(|ch| fb[ch] + (s[ch] - fb[ch]) * t)
                }
                (Some(s), None) => s,
                (None, Some(fb)) => fb,
                (None, None) => c,
            };
            Rgba::new(
                c[0] + (f[0] - c[0]) * k,
                c[1] + (f[1] - c[1]) * k,
                c[2] + (f[2] - c[2]) * k,
                c[3] + (f[3] - c[3]) * k,
            )
        })
        .collect()
}

/// Select ▸ Select and Mask… applied: refine the active selection's edge
/// against the image and send the result to `output` — one undo step.
pub struct RefineSelection {
    pub params: RefineParams,
    pub brush: Vec<RefineDab>,
    pub output: RefineOutput,
    /// The layer a mask or new layer is made from (the active layer).
    pub layer: Option<LayerId>,
    /// The pixels the edge is fitted to.
    pub sample: SampleSource,
}

/// The canvas area a refine of `sel` can change.
pub fn refine_area(doc: &Document, sel: &Selection, params: &RefineParams, brush: &[RefineDab]) -> Rect {
    let canvas = doc.canvas();
    if sel.coverage.default > 0.0 {
        return canvas;
    }
    let mut r = sel.tight_bounds(canvas);
    for d in brush {
        let k = d.radius.ceil() as i32 + 1;
        let dab = Rect::new(d.x as i32 - k, d.y as i32 - k, 2 * k as u32 + 1, 2 * k as u32 + 1);
        r = if r.is_empty() { dab } else { r.union(&dab) };
    }
    if r.is_empty() {
        return r;
    }
    let m = params.reach();
    Rect::new(r.x - m, r.y - m, r.w + 2 * m as u32, r.h + 2 * m as u32).intersect(&canvas)
}

impl Command for RefineSelection {
    fn label(&self) -> String {
        match self.output {
            RefineOutput::Selection => "Select and mask",
            RefineOutput::LayerMask => "Select and mask to layer mask",
            RefineOutput::NewLayer => "Select and mask to new layer",
            RefineOutput::NewLayerWithMask => "Select and mask to new layer with mask",
        }
        .into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        (self.output == RefineOutput::LayerMask)
            .then_some(self.layer)
            .flatten()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc
            .selection
            .clone()
            .ok_or_else(|| EditError::Invalid("nothing is selected".into()))?;
        let params = self.params.sanitized();
        let canvas = doc.canvas();
        let roi = refine_area(doc, &sel, &params, &self.brush);
        // The layer the output is made from, checked before any work.
        let source = match self.output {
            RefineOutput::Selection => None,
            _ => {
                let id = self
                    .layer
                    .ok_or_else(|| EditError::Invalid("select a layer first".into()))?;
                let l = doc.layer(id).ok_or(EditError::NoLayer(id))?;
                if self.output.makes_layer() && l.pixels().is_none() {
                    return Err(EditError::NotPixel(id));
                }
                Some(id)
            }
        };
        let mut coverage = sel.coverage.clone();
        let mut colours: Option<(Rect, Vec<Rgba>)> = None;
        if !roi.is_empty() {
            let image = match self.sample {
                SampleSource::Merged => {
                    let full = lumenply_render::composite_raster(doc);
                    crop_raster(&full, canvas, roi)
                }
                SampleSource::Layer(id) => doc
                    .layer(id)
                    .ok_or(EditError::NoLayer(id))?
                    .raster_store()
                    .ok_or(EditError::NotPixel(id))?
                    .to_raster(roi),
            };
            let cov = sel.coverage.to_dense(roi);
            let (w, h) = (roi.w as usize, roi.h as usize);
            let band = (!self.brush.is_empty())
                .then(|| paint_band(&self.brush, w, h, (roi.x as f32, roi.y as f32), 1.0));
            let matte = edge_matte(&image, &cov, band.as_deref(), &params, 1.0);
            let alpha = global_refine(matte.clone(), w, h, &params, 1.0);
            if self.output.makes_layer() && params.decontaminates() {
                // Colours come from the layer the new one copies, fitted
                // with the matte before the global refinements.
                let id = source.expect("checked above");
                let layer_px = doc
                    .layer(id)
                    .and_then(|l| l.pixels())
                    .ok_or(EditError::NotPixel(id))?
                    .to_raster(roi);
                let fixed = decontaminate(
                    &layer_px,
                    &cov,
                    &matte,
                    &alpha,
                    params.radius,
                    params.decontam_amount,
                );
                colours = Some((roi, fixed));
            }
            coverage.set_dense(roi, &alpha);
        }
        coverage.enabled = true;
        let refined = Selection { coverage };
        match self.output {
            RefineOutput::Selection => {
                doc.selection = Some(refined).filter(|s| !s.is_empty());
            }
            RefineOutput::LayerMask => {
                let id = source.expect("checked above");
                let l = doc.layer_mut(id).ok_or(EditError::NoLayer(id))?;
                l.mask = Some(refined.to_mask());
                doc.selection = None;
            }
            RefineOutput::NewLayer | RefineOutput::NewLayerWithMask => {
                let id = source.expect("checked above");
                let src = doc.layer(id).ok_or(EditError::NoLayer(id))?;
                let store = src.pixels().ok_or(EditError::NotPixel(id))?;
                let name = format!("{} copy", src.name);
                let with_mask = self.output == RefineOutput::NewLayerWithMask;
                let mask = refined.to_mask();
                let mut pixels = copy_pixels(store, colours.as_ref(), (!with_mask).then_some(&mask));
                pixels.compact();
                let new_id = doc.alloc_id();
                let mut l = Layer::pixel(new_id, name);
                *l.pixels_mut().expect("pixel layer") = pixels;
                if with_mask {
                    l.mask = Some(mask);
                }
                // As in Photoshop, the source is hidden under its refined copy.
                if let Some(s) = doc.layer_mut(id) {
                    s.visible = false;
                }
                let list = doc.siblings_mut(id).ok_or(EditError::NoLayer(id))?;
                let at = list.iter().position(|x| x.id == id).expect("in its siblings");
                list.insert(at + 1, l);
                doc.selection = None;
            }
        }
        Ok(())
    }
}

/// The part of a canvas-sized raster inside `roi`.
fn crop_raster(full: &Raster, canvas: Rect, roi: Rect) -> Raster {
    let mut out = Raster::new(roi.w, roi.h);
    let (ox, oy) = ((roi.x - canvas.x) as u32, (roi.y - canvas.y) as u32);
    for y in 0..roi.h {
        let s = ((oy + y) * full.width + ox) as usize;
        let d = (y * roi.w) as usize;
        out.pixels[d..d + roi.w as usize].copy_from_slice(&full.pixels[s..s + roi.w as usize]);
    }
    out
}

/// A copy of `store` with the decontaminated colours pasted in and, when
/// given, every pixel scaled by `mask` (tile by tile).
fn copy_pixels(store: &TileStore, colours: Option<&(Rect, Vec<Rgba>)>, mask: Option<&Mask>) -> TileStore {
    let mut out = TileStore::new();
    for c in store.coords() {
        let Some(tile) = store.tile(c) else { continue };
        let rect = c.rect();
        let cov = mask.map(|m| m.to_dense(rect));
        if cov.as_ref().is_some_and(|v| v.iter().all(|&a| a <= 0.0)) {
            continue;
        }
        let mut t = tile.clone();
        let (ox, oy) = c.origin();
        let px = t.pixels_mut();
        if let Some((roi, cols)) = colours {
            let sub = roi.intersect(&rect);
            for y in sub.y..sub.bottom() {
                for x in sub.x..sub.right() {
                    let src = ((y - roi.y) as u32 * roi.w + (x - roi.x) as u32) as usize;
                    px[(y - oy) as usize * TILE_SIZE + (x - ox) as usize] = cols[src];
                }
            }
        }
        if let Some(cov) = &cov {
            for (p, &a) in px.iter_mut().zip(cov) {
                if a < 1.0 {
                    *p = p.scale(a);
                }
            }
        }
        out.insert(c, Arc::new(t));
    }
    out.prune_blank();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddPixelLayer, SetSelection};
    use crate::Editor;

    const F: [f32; 3] = [0.8, 0.2, 0.1];
    const B: [f32; 3] = [0.1, 0.25, 0.8];

    /// `w`×`h`: foreground colour on the left, background on the right,
    /// mixed by `alpha(x)` (the true coverage).
    fn mix_image(w: u32, h: u32, alpha: impl Fn(u32) -> f32) -> Raster {
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let a = alpha(x);
                let c = |k: usize| B[k] + (F[k] - B[k]) * a;
                r.set(x, y, Rgba::new(c(0), c(1), c(2), 1.0));
            }
        }
        r
    }

    /// Coverage of a selection holding the columns left of `edge`.
    fn left_of(w: u32, h: u32, edge: u32) -> Vec<f32> {
        (0..w * h).map(|i| if i % w < edge { 1.0 } else { 0.0 }).collect()
    }

    fn row(m: &[f32], w: u32, y: u32, xs: std::ops::Range<u32>) -> Vec<f32> {
        xs.map(|x| (m[(y * w + x) as usize] * 1000.0).round() / 1000.0)
            .collect()
    }

    fn radius(r: f32) -> RefineParams {
        RefineParams {
            radius: r,
            ..RefineParams::default()
        }
    }

    #[test]
    fn a_hard_step_edge_snaps_to_the_image_and_stays_sharp() {
        // True edge at x = 30; the selection stops 4 px short of it.
        let img = mix_image(64, 32, |x| if x < 30 { 1.0 } else { 0.0 });
        let m = refine_matte(&img, &left_of(64, 32, 26), None, &radius(8.0), 1.0);
        for y in [0, 16, 31] {
            assert_eq!(
                row(&m, 64, y, 24..36),
                vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                "row {y}"
            );
        }
        // Beyond the band nothing changes.
        assert_eq!(m[16 * 64 + 2], 1.0);
        assert_eq!(m[16 * 64 + 60], 0.0);
    }

    #[test]
    fn a_soft_edge_gets_its_true_partial_coverage() {
        // An 8 px blend from x = 28 to 36: pixel x is (36 − x − ½) / 8 covered.
        let truth = |x: u32| ((36.0 - x as f32 - 0.5) / 8.0).clamp(0.0, 1.0);
        let img = mix_image(72, 24, truth);
        let m = refine_matte(&img, &left_of(72, 24, 32), None, &radius(10.0), 1.0);
        for x in [28, 29, 30, 31, 32, 33, 34, 35] {
            let got = m[12 * 72 + x as usize];
            assert!((got - truth(x)).abs() < 0.02, "x {x}: {got} vs {}", truth(x));
        }
        assert_eq!(m[12 * 72 + 27], 1.0);
        assert_eq!(m[12 * 72 + 36], 0.0);
        // Selection-only refinements (radius 0) leave it as it was.
        let same = refine_matte(&img, &left_of(72, 24, 32), None, &radius(0.0), 1.0);
        assert_eq!(same, left_of(72, 24, 32));
    }

    #[test]
    fn shift_edge_moves_the_half_contour_by_the_expected_pixels() {
        let img = mix_image(64, 16, |x| if x < 30 { 1.0 } else { 0.0 });
        let cov = left_of(64, 16, 30);
        // Radius 8: +50 % moves the edge 4 px out, −25 % 2 px in.
        let out = RefineParams {
            shift_edge: 50.0,
            ..radius(8.0)
        };
        assert_eq!(out.shift_px(), 4.0);
        let m = refine_matte(&img, &cov, None, &out, 1.0);
        assert_eq!(row(&m, 64, 8, 31..36), vec![1.0, 1.0, 1.0, 0.0, 0.0]);
        let inward = RefineParams {
            shift_edge: -25.0,
            ..radius(8.0)
        };
        let m = refine_matte(&img, &cov, None, &inward, 1.0);
        assert_eq!(row(&m, 64, 8, 26..30), vec![1.0, 1.0, 0.0, 0.0]);
        // Half a pixel blends the neighbouring whole shifts.
        let half = RefineParams {
            shift_edge: 12.5,
            ..RefineParams::default()
        };
        assert_eq!(half.shift_px(), 0.5);
        let m = refine_matte(&img, &cov, None, &half, 1.0);
        assert_eq!(row(&m, 64, 8, 29..32), vec![1.0, 0.5, 0.0]);
        // Shift is capped.
        let far = RefineParams {
            radius: 250.0,
            shift_edge: 100.0,
            ..RefineParams::default()
        };
        assert_eq!(far.shift_px(), MAX_SHIFT);
    }

    #[test]
    fn feather_contrast_and_smooth_shape_the_transition() {
        let img = mix_image(64, 32, |_| 1.0);
        let cov = left_of(64, 32, 30);
        // Feather is symmetric about the edge: the two pixels either side
        // of it sum to one, and the edge spreads over several pixels.
        let f = RefineParams {
            feather: 4.0,
            ..RefineParams::default()
        };
        let m = refine_matte(&img, &cov, None, &f, 1.0);
        // 4 px → three box passes of radius 2, whose kernel is c(k)/125 with
        // c(0) = 19: the pixel inside the edge holds ½ + 19/250 = 0.576, the
        // one outside 0.424, and x = 26 (three in) 1 − 10/125 = 0.92.
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(close(m[16 * 64 + 29], 0.576), "{}", m[16 * 64 + 29]);
        assert!(close(m[16 * 64 + 30], 0.424), "{}", m[16 * 64 + 30]);
        assert!(close(m[16 * 64 + 26], 0.92), "{}", m[16 * 64 + 26]);
        // Contrast 50 % doubles the distance from ½.
        assert!(close(contrast(0.6, 0.5), 0.7));
        assert!(close(contrast(0.3, 0.5), 0.1));
        assert_eq!(contrast(0.5, 0.5), 0.5);
        assert_eq!(contrast(0.51, 1.0), 1.0);
        let fc = RefineParams { contrast: 100.0, ..f };
        let m = refine_matte(&img, &cov, None, &fc, 1.0);
        assert_eq!(row(&m, 64, 16, 27..33), vec![1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
        // Smooth fills a one-pixel notch and drops a speck, keeping the
        // straight edge where it was.
        let mut notched = cov.clone();
        notched[16 * 64 + 29] = 0.0;
        notched[5 * 64 + 50] = 1.0;
        let s = RefineParams {
            smooth: 50.0,
            ..RefineParams::default()
        };
        let m = refine_matte(&img, &notched, None, &s, 1.0);
        assert_eq!(m[16 * 64 + 29], 1.0, "notch filled");
        assert_eq!(m[5 * 64 + 50], 0.0, "speck gone");
        assert_eq!(row(&m, 64, 8, 27..33), vec![1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn smart_radius_keeps_a_hard_edge_tight_and_a_soft_one_soft() {
        // Rows 0..20 hard at x = 40; rows 20..40 a 12 px blend centred there.
        let mut img = Raster::new(96, 40);
        let soft = |x: u32| ((46.0 - x as f32 - 0.5) / 12.0).clamp(0.0, 1.0);
        for y in 0..40 {
            for x in 0..96 {
                let a = if y < 20 { (x < 40) as u32 as f32 } else { soft(x) };
                let c = |k: usize| B[k] + (F[k] - B[k]) * a;
                // A little noise in the sure areas.
                let n = ((x * 7 + y * 13) % 5) as f32 * 0.004;
                img.set(x, y, Rgba::new(c(0) + n, c(1), c(2) - n, 1.0));
            }
        }
        let p = RefineParams {
            radius: 16.0,
            smart_radius: true,
            ..RefineParams::default()
        };
        let m = refine_matte(&img, &left_of(96, 40, 37), None, &p, 1.0);
        let partial = |y: u32| {
            (0..96)
                .filter(|&x| m[(y * 96 + x) as usize] > 0.0 && m[(y * 96 + x) as usize] < 1.0)
                .count()
        };
        assert!(partial(10) <= 1, "hard row: {:?}", row(&m, 96, 10, 34..46));
        assert_eq!(m[10 * 96 + 39], 1.0);
        assert_eq!(m[10 * 96 + 40], 0.0);
        let got = m[30 * 96 + 40];
        assert!((got - soft(40)).abs() < 0.04, "soft row keeps its blend: {got}");
        assert!(partial(30) >= 9, "{:?}", row(&m, 96, 30, 34..52));
    }

    #[test]
    fn the_brush_refines_only_where_it_paints() {
        let img = mix_image(64, 32, |x| if x < 30 { 1.0 } else { 0.0 });
        let cov = left_of(64, 32, 26);
        // No radius: only the dab (centred on the edge at row 8) is matted.
        let dabs = [RefineDab {
            x: 28.0,
            y: 8.0,
            radius: 5.0,
            erase: false,
        }];
        let band = paint_band(&dabs, 64, 32, (0.0, 0.0), 1.0);
        let m = refine_matte(&img, &cov, Some(&band), &radius(0.0), 1.0);
        assert_eq!(row(&m, 64, 8, 25..31), vec![1.0, 1.0, 1.0, 1.0, 1.0, 0.0]);
        assert_eq!(row(&m, 64, 24, 25..31), vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        // Erasing inside the band restores the original edge there.
        let erase = [RefineDab {
            x: 28.0,
            y: 8.0,
            radius: 5.0,
            erase: true,
        }];
        let band = paint_band(&erase, 64, 32, (0.0, 0.0), 1.0);
        let m = refine_matte(&img, &cov, Some(&band), &radius(8.0), 1.0);
        assert_eq!(row(&m, 64, 8, 25..31), vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(row(&m, 64, 24, 25..31), vec![1.0, 1.0, 1.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn a_preview_scale_shrinks_the_pixel_amounts() {
        // The same scene at half size with half the radius gives the same
        // edge, half as far in.
        let img = mix_image(32, 16, |x| if x < 15 { 1.0 } else { 0.0 });
        let m = refine_matte(&img, &left_of(32, 16, 13), None, &radius(8.0), 0.5);
        assert_eq!(row(&m, 32, 8, 12..18), vec![1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
    }

    fn editor_with(img: Raster, sel: Selection) -> (Editor, LayerId) {
        let (w, h) = (img.width, img.height);
        let mut ed = Editor::new(Document::new(w, h));
        ed.execute(&AddPixelLayer::from_raster("Photo", img, 0, 0))
            .unwrap();
        let id = ed.doc().layers()[0].id;
        ed.execute(&SetSelection { selection: Some(sel) }).unwrap();
        (ed, id)
    }

    #[test]
    fn the_command_refines_the_selection_as_one_step() {
        let img = mix_image(300, 40, |x| if x < 130 { 1.0 } else { 0.0 });
        let (mut ed, id) = editor_with(img, Selection::rect(Rect::new(0, 0, 126, 40)));
        let steps = ed.history().len();
        let cmd = RefineSelection {
            params: radius(8.0),
            brush: Vec::new(),
            output: RefineOutput::Selection,
            layer: Some(id),
            sample: SampleSource::Merged,
        };
        ed.execute(&cmd).unwrap();
        assert_eq!(ed.history().len(), steps + 1);
        assert_eq!(ed.history().last(), Some(&"Select and mask"));
        let s = ed.doc().selection.as_ref().unwrap();
        let cols: Vec<f32> = (124..134).map(|x| s.value(x, 20)).collect();
        assert_eq!(cols, vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(s.value(299, 20), 0.0);
        ed.undo();
        assert_eq!(ed.doc().selection.as_ref().unwrap().value(128, 20), 0.0);
        // Nothing selected: an error, not a step.
        ed.execute(&SetSelection { selection: None }).unwrap();
        assert!(ed.execute(&cmd).is_err());
    }

    #[test]
    fn layer_mask_output_masks_the_layer_and_deselects() {
        let img = mix_image(64, 32, |x| if x < 30 { 1.0 } else { 0.0 });
        let (mut ed, id) = editor_with(img, Selection::rect(Rect::new(0, 0, 26, 32)));
        ed.execute(&RefineSelection {
            params: radius(8.0),
            brush: Vec::new(),
            output: RefineOutput::LayerMask,
            layer: Some(id),
            sample: SampleSource::Layer(id),
        })
        .unwrap();
        let doc = ed.doc();
        assert!(doc.selection.is_none());
        let mask = doc.layer(id).unwrap().mask.as_ref().expect("masked");
        let cols: Vec<f32> = (27..33).map(|x| mask.value(x, 10)).collect();
        assert_eq!(cols, vec![1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
        assert_eq!(ed.history().last(), Some(&"Select and mask to layer mask"));
    }

    #[test]
    fn new_layer_outputs_copy_the_cut_out_and_hide_the_source() {
        let truth = |x: u32| ((36.0 - x as f32 - 0.5) / 8.0).clamp(0.0, 1.0);
        let img = mix_image(72, 24, truth);
        let (mut ed, id) = editor_with(img, Selection::rect(Rect::new(0, 0, 32, 24)));
        let base = RefineSelection {
            params: radius(10.0),
            brush: Vec::new(),
            output: RefineOutput::NewLayer,
            layer: Some(id),
            sample: SampleSource::Layer(id),
        };
        ed.execute(&base).unwrap();
        let doc = ed.doc();
        assert_eq!(doc.layers().len(), 2);
        let (src, copy) = (&doc.layers()[0], &doc.layers()[1]);
        assert!(!src.visible, "the source is hidden");
        assert_eq!(copy.name, "Photo copy");
        assert!(copy.mask.is_none());
        let px = copy.pixels().unwrap();
        // x = 32 is 0.4375 covered: its pixel keeps that much of the mixed
        // colour (16-bit storage, hence the tolerance).
        let p = px.get_pixel(32, 12);
        let a = truth(32);
        let mixed = B[0] + (F[0] - B[0]) * a;
        assert!((p.a - a).abs() < 0.02 && (p.r - mixed * a).abs() < 0.02, "{p:?}");
        assert_eq!(px.get_pixel(50, 12).a, 0.0);
        assert!((px.get_pixel(5, 12).r - F[0]).abs() < 1e-3);
        ed.undo();
        assert_eq!(ed.doc().layers().len(), 1);
        assert!(ed.doc().layers()[0].visible);

        // With a mask and decontamination: the pixels stay whole, the mask
        // holds the coverage, and the edge pixel turns foreground red.
        ed.execute(&RefineSelection {
            params: RefineParams {
                decontaminate: true,
                decontam_amount: 100.0,
                ..radius(10.0)
            },
            output: RefineOutput::NewLayerWithMask,
            ..base
        })
        .unwrap();
        let copy = &ed.doc().layers()[1];
        let mask = copy.mask.as_ref().expect("masked copy");
        assert!((mask.value(32, 12) - a).abs() < 0.02);
        let p = copy.pixels().unwrap().get_pixel(32, 12);
        assert!(
            (p.r - F[0]).abs() < 0.02 && (p.b - F[2]).abs() < 0.02,
            "decontaminated: {p:?}"
        );
        assert_eq!(p.a, 1.0);
        assert_eq!(
            ed.history().last(),
            Some(&"Select and mask to new layer with mask")
        );
    }

    #[test]
    fn distance_transform_is_exact() {
        let mut set = vec![false; 35];
        set[2 * 7 + 3] = true;
        let d = edt_sq(&set, 7, 5);
        assert_eq!(d[2 * 7 + 3], 0.0);
        assert_eq!(d[2 * 7 + 6], 9.0);
        assert_eq!(d[4 * 7 + 5], 8.0);
        assert_eq!(d[0], 13.0);
        assert_eq!(edt_sq(&[false; 4], 2, 2)[0], FAR as f32);
    }

    #[test]
    fn disc_dilation_rounds_corners() {
        let mut src = vec![0f32; 81];
        src[4 * 9 + 4] = 1.0;
        let d = dilate_disc(&src, 9, 9, 2);
        // Centres within 2.5 px: (±2, ±1) yes, (±2, ±2) no.
        assert_eq!(d[4 * 9 + 6], 1.0);
        assert_eq!(d[5 * 9 + 6], 1.0);
        assert_eq!(d[6 * 9 + 6], 0.0);
        assert_eq!(d[4 * 9 + 7], 0.0);
        let total: f32 = d.iter().sum();
        assert_eq!(total, 21.0);
    }

    /// Visual check on a real photo: `RF_IMAGE=photo.jpg RF_OUT=dir cargo
    /// test --release -p lumenply-core refine_on_a_photo -- --ignored`.
    #[test]
    #[ignore = "writes images for a visual check"]
    fn refine_on_a_photo() {
        let (Ok(path), Ok(out)) = (std::env::var("RF_IMAGE"), std::env::var("RF_OUT")) else {
            return;
        };
        let img = image::open(&path).unwrap().to_rgba8();
        let (w, h) = img.dimensions();
        let mut r = Raster::new(w, h);
        for (i, p) in img.pixels().enumerate() {
            let c = |v: u8| ((v as f32 / 255.0 + 0.055) / 1.055).powf(2.4);
            r.pixels[i] = Rgba::new(c(p[0]), c(p[1]), c(p[2]), 1.0);
        }
        // A rough polygon under the ridge line of the demo photo.
        let (fw, fh) = (w as f32, h as f32);
        let ridge: Vec<(f32, f32)> = [
            (0.0, 0.50),
            (0.04, 0.48),
            (0.14, 0.43),
            (0.18, 0.425),
            (0.25, 0.47),
            (0.32, 0.52),
            (0.45, 0.545),
            (0.50, 0.525),
            (0.56, 0.545),
            (0.65, 0.53),
            (0.75, 0.545),
            (0.85, 0.515),
            (0.93, 0.495),
            (1.0, 0.51),
        ]
        .iter()
        .map(|&(x, y)| (x * fw, y * fh))
        .chain([(fw, fh), (0.0, fh)])
        .collect();
        let sel = Selection::polygon(&ridge);
        let cov = sel.coverage.to_dense(Rect::new(0, 0, w, h));
        let radius: f32 = std::env::var("RF_RADIUS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20.0);
        let p = RefineParams {
            radius,
            smart_radius: std::env::var("RF_SMART").is_ok(),
            ..RefineParams::default()
        };
        let t = std::time::Instant::now();
        let m = refine_matte(&r, &cov, None, &p, 1.0);
        println!("refine {}×{} radius {radius}: {:?}", w, h, t.elapsed());
        for (name, cov) in [("before", &cov), ("after", &m)] {
            let mut o = img.clone();
            for (i, px) in o.pixels_mut().enumerate() {
                let k = cov[i];
                px[0] = (px[0] as f32 * k + (px[0] as f32 * 0.4 + 150.0) * (1.0 - k)) as u8;
                px[1] = (px[1] as f32 * (0.4 + 0.6 * k)) as u8;
                px[2] = (px[2] as f32 * (0.4 + 0.6 * k)) as u8;
            }
            o.save(format!("{out}/rf-{name}.png")).unwrap();
            let mask = image::GrayImage::from_fn(w, h, |x, y| {
                image::Luma([(cov[(y * w + x) as usize] * 255.0) as u8])
            });
            mask.save(format!("{out}/rf-{name}-mask.png")).unwrap();
        }
    }
}
