//! Quick Selection: paint over an object and the selection grows to its
//! edges.
//!
//! Geodesic segmentation (after Bai & Sapiro): the pixels under the brush
//! seed the foreground; pixels whose colour is far from every colour under
//! the brush seed the background. Both fronts spread over the image, a
//! step costing the colour change it crosses (CIELAB), so fronts flow
//! freely through even regions and stall at edges. A pixel belongs to the
//! foreground when that front reaches it more cheaply. The work runs on a
//! reduced copy (≤ `WORK_PIXELS`); the mask is brought back to full size
//! and snapped to the full-resolution edges with a guided filter.

use crate::{Command, EditError, EditResult};
use lumenply_doc::{CombineOp, Document, Mask, Selection};
use lumenply_tiles::{Raster, Rgba};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::commands::SampleSource;

/// Pixel budget of the reduced working image.
const WORK_PIXELS: u64 = 600_000;
/// Colour distance (ΔE) beyond which a pixel is certainly not what the
/// brush covered.
const FAR: f32 = 22.0;
/// Colour distance within which a pixel counts as one of a front's own
/// colours, and the per-step price of each ΔE beyond it.
const SPREAD: f32 = 8.0;
const UNLIKE: f32 = 0.5;
/// Strong edges cost disproportionately: a step of ΔE `e` costs
/// `e + e²/EDGE`, while gentle gradients stay nearly free.
const EDGE: f32 = 8.0;
/// The selection front's price (in ΔE) for travelling one brush radius.
const LENGTH: f32 = 1.0;

/// Grow (or, with `CombineOp::Subtract`, shrink) the selection from a
/// brush stroke.
pub struct QuickSelect {
    pub points: Vec<(f32, f32)>,
    pub radius: f32,
    pub op: CombineOp,
    pub sample: SampleSource,
}

impl Command for QuickSelect {
    fn label(&self) -> String {
        "Quick selection".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.points.is_empty() {
            return Err(EditError::Invalid("quick selection needs a stroke".into()));
        }
        let canvas = doc.canvas();
        let image = match self.sample {
            SampleSource::Merged => lumenply_render::composite_raster(doc),
            SampleSource::Layer(id) => {
                let l = doc.layer(id).ok_or(EditError::NoLayer(id))?;
                let store = l.raster_store().ok_or(EditError::NotPixel(id))?;
                store.to_raster(canvas)
            }
        };
        let points: Vec<(f32, f32)> = self
            .points
            .iter()
            .map(|&(x, y)| (x - canvas.x as f32, y - canvas.y as f32))
            .collect();
        let region = quick_select_mask(&image, &points, self.radius);
        let mut mask = Mask::hide_all();
        for y in 0..image.height {
            for x in 0..image.width {
                let v = region[(y * image.width + x) as usize];
                if v > 0.0 {
                    mask.set_value(canvas.x + x as i32, canvas.y + y as i32, v);
                }
            }
        }
        mask.prune_uniform();
        let shape = Selection::from_mask(&mask);
        let op = match self.op {
            CombineOp::Replace => CombineOp::Replace,
            CombineOp::Subtract => CombineOp::Subtract,
            _ => CombineOp::Union,
        };
        let mut sel = doc.selection.take().unwrap_or_else(Selection::none);
        sel.combine(&shape, op);
        doc.selection = Some(sel).filter(|s| !s.is_empty());
        Ok(())
    }
}

/// Linear premultiplied RGBA to CIELAB (D65), alpha scaled into a fourth
/// channel so transparent areas read as their own region.
fn lab(p: Rgba) -> [f32; 4] {
    let [r, g, b, a] = p.to_straight();
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = 0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b;
    let f = |t: f32| {
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz), a * 100.0]
}

fn dist(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let d: f32 = (0..4).map(|i| (a[i] - b[i]) * (a[i] - b[i])).sum();
    d.sqrt()
}

/// Box-filter `src` down by an integer `factor`.
fn shrink(src: &Raster, factor: u32) -> Raster {
    if factor <= 1 {
        return src.clone();
    }
    let (w, h) = (src.width.div_ceil(factor), src.height.div_ceil(factor));
    let mut out = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 4];
            let mut n = 0.0;
            for sy in y * factor..((y + 1) * factor).min(src.height) {
                for sx in x * factor..((x + 1) * factor).min(src.width) {
                    let p = src.get(sx, sy);
                    acc[0] += p.r;
                    acc[1] += p.g;
                    acc[2] += p.b;
                    acc[3] += p.a;
                    n += 1.0;
                }
            }
            out.set(x, y, Rgba::new(acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n));
        }
    }
    out
}

/// A few representative colours of `samples` (k-means, k ≤ 4).
fn palette(samples: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let k = samples.len().min(4);
    if k == 0 {
        return Vec::new();
    }
    // Deterministic spread-out start: farthest-point seeding.
    let mut centres = vec![samples[0]];
    while centres.len() < k {
        let next = samples
            .iter()
            .max_by(|a, b| {
                let da = centres.iter().map(|c| dist(a, c)).fold(f32::MAX, f32::min);
                let db = centres.iter().map(|c| dist(b, c)).fold(f32::MAX, f32::min);
                da.total_cmp(&db)
            })
            .copied()
            .unwrap();
        centres.push(next);
    }
    for _ in 0..8 {
        let mut sum = vec![[0.0f32; 4]; k];
        let mut count = vec![0usize; k];
        for s in samples {
            let i = (0..k)
                .min_by(|&i, &j| dist(s, &centres[i]).total_cmp(&dist(s, &centres[j])))
                .unwrap();
            for c in 0..4 {
                sum[i][c] += s[c];
            }
            count[i] += 1;
        }
        for i in 0..k {
            if count[i] > 0 {
                for c in 0..4 {
                    centres[i][c] = sum[i][c] / count[i] as f32;
                }
            }
        }
    }
    centres
}

/// Geodesic distance from `seeds` over a 4-connected grid whose step cost
/// is the colour change, plus `extra` for entering each pixel (how unlike
/// the front's own colours it is), plus a tiny length term.
fn geodesic(lab: &[[f32; 4]], w: usize, h: usize, seeds: &[usize], extra: &[f32]) -> Vec<f32> {
    let mut d = vec![f32::INFINITY; w * h];
    let mut heap = BinaryHeap::new();
    for &s in seeds {
        d[s] = 0.0;
        heap.push(Reverse((0u32, s as u32)));
    }
    while let Some(Reverse((dk, i))) = heap.pop() {
        let i = i as usize;
        let di = f32::from_bits(dk);
        if di > d[i] {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let mut relax = |j: usize| {
            let e = dist(&lab[i], &lab[j]);
            let nd = di + e + e * e / EDGE + 0.05 + extra[j];
            if nd < d[j] {
                d[j] = nd;
                heap.push(Reverse((nd.to_bits(), j as u32)));
            }
        };
        if x > 0 {
            relax(i - 1);
        }
        if x + 1 < w {
            relax(i + 1);
        }
        if y > 0 {
            relax(i - w);
        }
        if y + 1 < h {
            relax(i + w);
        }
    }
    d
}

/// Mean of `v` over a (2r+1)² box, clamped at the borders (O(1) per pixel
/// through running sums).
fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        let row = &v[y * w..(y + 1) * w];
        let mut acc: f32 = row[..=r.min(w - 1)].iter().sum();
        for x in 0..w {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            tmp[y * w + x] = acc / (hi - lo + 1) as f32;
            if x + r + 1 < w {
                acc += row[x + r + 1];
            }
            if x >= r {
                acc -= row[x - r];
            }
        }
    }
    let mut out = vec![0.0f32; w * h];
    for x in 0..w {
        let mut acc = 0.0f32;
        for y in 0..=r.min(h - 1) {
            acc += tmp[y * w + x];
        }
        for y in 0..h {
            let lo = y.saturating_sub(r);
            let hi = (y + r).min(h - 1);
            out[y * w + x] = acc / (hi - lo + 1) as f32;
            if y + r + 1 < h {
                acc += tmp[(y + r + 1) * w + x];
            }
            if y >= r {
                acc -= tmp[(y - r) * w + x];
            }
        }
    }
    out
}

/// Colour-line edge refinement: in the band where `mask` is uncertain,
/// a pixel's coverage is where its colour falls between the mean colours
/// of the sure-foreground and sure-background pixels around it (within
/// `r`). Full colour, so edges between equally light colours stay sharp,
/// and mixed edge pixels get matching partial coverage.
fn refine(labs: &[[f32; 4]], mask: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let sure_f: Vec<f32> = mask.iter().map(|&v| if v >= 0.98 { 1.0 } else { 0.0 }).collect();
    let sure_b: Vec<f32> = mask.iter().map(|&v| if v <= 0.02 { 1.0 } else { 0.0 }).collect();
    let nf = box_mean(&sure_f, w, h, r);
    let nb = box_mean(&sure_b, w, h, r);
    let mut f_mean = Vec::with_capacity(4);
    let mut b_mean = Vec::with_capacity(4);
    for c in 0..4 {
        let fv: Vec<f32> = labs.iter().zip(&sure_f).map(|(l, k)| l[c] * k).collect();
        let bv: Vec<f32> = labs.iter().zip(&sure_b).map(|(l, k)| l[c] * k).collect();
        f_mean.push(box_mean(&fv, w, h, r));
        b_mean.push(box_mean(&bv, w, h, r));
    }
    (0..w * h)
        .map(|i| {
            let v = mask[i];
            if v >= 0.98 || v <= 0.02 || nf[i] <= 0.0 || nb[i] <= 0.0 {
                return v.round();
            }
            let mut fb = [0.0f32; 4];
            let mut cb = [0.0f32; 4];
            for c in 0..4 {
                let fc = f_mean[c][i] / nf[i];
                let bc = b_mean[c][i] / nb[i];
                fb[c] = fc - bc;
                cb[c] = labs[i][c] - bc;
            }
            let len2: f32 = fb.iter().map(|x| x * x).sum();
            if len2 < 1.0 {
                return v;
            }
            let t = (0..4).map(|c| cb[c] * fb[c]).sum::<f32>() / len2;
            let t = t.clamp(0.0, 1.0);
            if t < 0.03 {
                0.0
            } else if t > 0.97 {
                1.0
            } else {
                t
            }
        })
        .collect()
}

/// Coverage (0..=1, row-major over `image`) of the region a brush stroke
/// along `points` (image pixels) with `radius` picks out.
pub fn quick_select_mask(image: &Raster, points: &[(f32, f32)], radius: f32) -> Vec<f32> {
    let (fw, fh) = (image.width as usize, image.height as usize);
    if fw == 0 || fh == 0 {
        return Vec::new();
    }
    let px = fw as u64 * fh as u64;
    let mut factor = 1u32;
    while px / (factor as u64 * factor as u64) > WORK_PIXELS {
        factor += 1;
    }
    let small = shrink(image, factor);
    let (w, h) = (small.width as usize, small.height as usize);
    let labs: Vec<[f32; 4]> = small.pixels.iter().map(|&p| lab(p)).collect();

    // Foreground seeds: everything under the brush.
    let f = factor as f32;
    let r = (radius / f).max(0.75);
    let mut fg = vec![false; w * h];
    for pair in points
        .windows(2)
        .chain(std::iter::once(&[points[0], points[0]][..]))
    {
        let (a, b) = ((pair[0].0 / f, pair[0].1 / f), (pair[1].0 / f, pair[1].1 / f));
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let steps = (len / (r * 0.5).max(0.5)).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let (cx, cy) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            let (x0, x1) = (
                (cx - r).floor().max(0.0) as usize,
                ((cx + r).ceil() as usize).min(w - 1),
            );
            let (y0, y1) = (
                (cy - r).floor().max(0.0) as usize,
                ((cy + r).ceil() as usize).min(h - 1),
            );
            if cx + r < 0.0 || cy + r < 0.0 || x0 > x1 || y0 > y1 {
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                    if dx * dx + dy * dy <= r * r {
                        fg[y * w + x] = true;
                    }
                }
            }
        }
    }
    let fg_seeds: Vec<usize> = (0..w * h).filter(|&i| fg[i]).collect();
    if fg_seeds.is_empty() {
        return vec![0.0; fw * fh];
    }

    // Background seeds: colours unlike anything under the brush.
    let stride = (fg_seeds.len() / 2000).max(1);
    let samples: Vec<[f32; 4]> = fg_seeds.iter().step_by(stride).map(|&i| labs[i]).collect();
    let centres = palette(&samples);
    let bg_seeds: Vec<usize> = (0..w * h)
        .filter(|&i| !fg[i] && centres.iter().map(|c| dist(&labs[i], c)).fold(f32::MAX, f32::min) > FAR)
        .collect();

    let small_mask: Vec<f32> = if bg_seeds.is_empty() {
        vec![1.0; w * h]
    } else {
        // The selection's front also pays for entering colours unlike the
        // stroke's (beyond their natural spread), so it cannot drift
        // through slow gradients; the background's does not, which keeps
        // ambiguous colours out. Like Photoshop's tool this errs small:
        // another stroke grows it, a leak would need subtracting.
        // It also pays for distance in brush radii: a small brush claims
        // nearby ground, a big one reaches far.
        let travel = LENGTH / r;
        let fg_extra: Vec<f32> = labs
            .iter()
            .map(|l| {
                let c = centres.iter().map(|c| dist(l, c)).fold(f32::MAX, f32::min);
                UNLIKE * (c - SPREAD).max(0.0) + travel
            })
            .collect();
        let d_fg = geodesic(&labs, w, h, &fg_seeds, &fg_extra);
        let d_bg = geodesic(&labs, w, h, &bg_seeds, &vec![0.0; w * h]);
        (0..w * h)
            .map(|i| if d_fg[i] <= d_bg[i] { 1.0 } else { 0.0 })
            .collect()
    };

    if factor == 1 {
        // Already full resolution: refine a one-pixel band either side of
        // the label edge into anti-aliased, colour-true coverage.
        let band = box_mean(&small_mask, w, h, 1);
        return refine(&labs, &band, w, h, 3);
    }
    // Back to full size (bilinear: the uncertain band is about one working
    // pixel wide), then settle the band against full-resolution colour.
    let mut up = vec![0.0f32; fw * fh];
    for y in 0..fh {
        let sy = ((y as f32 + 0.5) / f - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (sy.floor() as usize, sy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..fw {
            let sx = ((x as f32 + 0.5) / f - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (sx.floor() as usize, sx.fract());
            let x1 = (x0 + 1).min(w - 1);
            let top = small_mask[y0 * w + x0] * (1.0 - tx) + small_mask[y0 * w + x1] * tx;
            let bot = small_mask[y1 * w + x0] * (1.0 - tx) + small_mask[y1 * w + x1] * tx;
            up[y * fw + x] = top * (1.0 - ty) + bot * ty;
        }
    }
    let full_labs: Vec<[f32; 4]> = image.pixels.iter().map(|&p| lab(p)).collect();
    refine(&full_labs, &up, fw, fh, factor as usize * 2 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;

    /// Left half red, right half blue, with a little noise so regions are
    /// not perfectly flat.
    fn two_halves(w: u32, h: u32) -> Raster {
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let n = ((x * 7 + y * 13) % 5) as f32 * 0.01;
                let p = if x < w / 2 {
                    Rgba::new(0.8 + n, 0.1, 0.1, 1.0)
                } else {
                    Rgba::new(0.1, 0.2, 0.8 + n, 1.0)
                };
                r.set(x, y, p);
            }
        }
        r
    }

    #[test]
    fn a_stroke_inside_one_region_selects_exactly_that_region() {
        let img = two_halves(120, 80);
        let m = quick_select_mask(&img, &[(20.0, 40.0), (30.0, 45.0)], 5.0);
        let at = |x: usize, y: usize| m[y * 120 + x];
        for y in [0, 20, 40, 79] {
            for x in [0, 30, 58, 59] {
                assert!(at(x, y) > 0.95, "({x}, {y}) is red: {}", at(x, y));
            }
            for x in [60, 61, 90, 119] {
                assert!(at(x, y) < 0.05, "({x}, {y}) is blue: {}", at(x, y));
            }
        }
    }

    #[test]
    fn large_images_are_segmented_reduced_and_snapped_back_to_full_size() {
        // 1200 × 900 > the working budget: the edge must still land on the
        // full-resolution seam within a pixel or two.
        let img = two_halves(1200, 900);
        let m = quick_select_mask(&img, &[(900.0, 300.0), (950.0, 600.0)], 40.0);
        let at = |x: usize, y: usize| m[y * 1200 + x];
        assert!(at(1199, 450) > 0.95 && at(605, 450) > 0.95);
        assert!(at(0, 450) < 0.05 && at(594, 450) < 0.05);
        assert!(at(597, 450) < 0.5 && at(602, 450) > 0.5, "edge at the seam");
    }

    /// Visual check on a real photo: `QS_IMAGE=photo.jpg QS_OUT=dir cargo
    /// test --release -p lumenply-core quick_select_on_a_photo -- --ignored`.
    #[test]
    #[ignore = "writes overlays for a visual check"]
    fn quick_select_on_a_photo() {
        let (Ok(path), Ok(out)) = (std::env::var("QS_IMAGE"), std::env::var("QS_OUT")) else {
            return;
        };
        let img = image::open(&path).unwrap().to_rgba8();
        let (w, h) = img.dimensions();
        let mut r = Raster::new(w, h);
        for (i, p) in img.pixels().enumerate() {
            let c = |v: u8| ((v as f32 / 255.0 + 0.055) / 1.055).powf(2.4);
            r.pixels[i] = Rgba::new(c(p[0]), c(p[1]), c(p[2]), 1.0);
        }
        let (fw, fh) = (w as f32, h as f32);
        type Stroke = (&'static str, Vec<(f32, f32)>, f32);
        let strokes: [Stroke; 3] = [
            (
                "sky",
                vec![
                    (fw * 0.15, fh * 0.15),
                    (fw * 0.5, fh * 0.1),
                    (fw * 0.85, fh * 0.2),
                ],
                fw * 0.02,
            ),
            (
                "peak",
                vec![(fw * 0.2, fh * 0.45), (fw * 0.22, fh * 0.5)],
                fw * 0.01,
            ),
            ("lake", vec![(fw * 0.36, fh * 0.79)], fw * 0.008),
        ];
        for (name, pts, rad) in strokes {
            let t = std::time::Instant::now();
            let m = quick_select_mask(&r, &pts, rad);
            println!("{name}: {:?}", t.elapsed());
            let mut o = img.clone();
            for (i, p) in o.pixels_mut().enumerate() {
                let k = m[i];
                // Unselected areas tinted red, as in a quick mask.
                p[0] = (p[0] as f32 * k + (p[0] as f32 * 0.4 + 150.0) * (1.0 - k)) as u8;
                p[1] = (p[1] as f32 * (0.4 + 0.6 * k)) as u8;
                p[2] = (p[2] as f32 * (0.4 + 0.6 * k)) as u8;
            }
            o.save(format!("{out}/qs-{name}.png")).unwrap();
        }
    }

    #[test]
    fn an_even_image_is_selected_whole() {
        let mut img = Raster::new(40, 30);
        for p in &mut img.pixels {
            *p = Rgba::new(0.3, 0.3, 0.3, 1.0);
        }
        let m = quick_select_mask(&img, &[(5.0, 5.0)], 3.0);
        assert!(m.iter().all(|&v| v == 1.0));
    }

    #[test]
    fn the_command_adds_and_subtracts() {
        let mut doc = Document::new(120, 80);
        let id = doc.add_pixel_layer("halves");
        let img = two_halves(120, 80);
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() =
            lumenply_tiles::TileStore::from_raster(&img, 0, 0);
        let mut ed = Editor::new(doc);
        ed.execute(&QuickSelect {
            points: vec![(20.0, 40.0)],
            radius: 4.0,
            op: CombineOp::Union,
            sample: SampleSource::Layer(id),
        })
        .unwrap();
        let sel = |ed: &Editor, x, y| ed.doc().selection.as_ref().map_or(0.0, |s| s.value(x, y));
        assert!(sel(&ed, 10, 10) > 0.95 && sel(&ed, 100, 10) < 0.05);
        // Adding the blue half selects everything.
        ed.execute(&QuickSelect {
            points: vec![(100.0, 40.0)],
            radius: 4.0,
            op: CombineOp::Union,
            sample: SampleSource::Layer(id),
        })
        .unwrap();
        assert!(sel(&ed, 10, 10) > 0.95 && sel(&ed, 100, 10) > 0.95);
        // Subtracting red leaves blue.
        ed.execute(&QuickSelect {
            points: vec![(20.0, 40.0)],
            radius: 4.0,
            op: CombineOp::Subtract,
            sample: SampleSource::Layer(id),
        })
        .unwrap();
        assert!(sel(&ed, 10, 10) < 0.05 && sel(&ed, 100, 10) > 0.95);
        assert_eq!(ed.history().last(), Some(&"Quick selection"));
    }
}
