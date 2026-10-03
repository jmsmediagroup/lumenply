//! Content-Aware Scale: seam carving (Avidan & Shamir 2007) with forward
//! energy (Rubinstein, Shamir & Avidan 2008).
//!
//! A vertical seam is an 8-connected path of one pixel per row. Removing
//! the cheapest seam makes the image one pixel narrower where that shows
//! least. Enlarging finds the k cheapest seams on a copy and duplicates
//! them, each new pixel the average of the seam pixel and its right-hand
//! neighbour (as in the paper), in steps of at most half the width so the
//! same seam is not stretched over and over. Height changes run the same
//! code on the transposed image.
//!
//! **Energy.** Each pixel costs the gradient magnitude of its gamma-encoded
//! luminance (transparency counts as a step too) plus the forward energy:
//! the colour jumps between the pixels a removal makes neighbours. A
//! protect mask adds a huge energy inside it, so seams go round it; "skin
//! tones" adds a large energy to pixels in the usual gamma YCbCr skin box.
//!
//! **Speed.** One dynamic-programming pass is O(w·h). Each pass yields
//! several seams: candidates are traced back from the cheapest bottom-row
//! costs and kept while they stay disjoint (with a one-pixel gap) and cost
//! no more than half again the best one. Energies are computed row-parallel;
//! the cumulative pass keeps only two rows of costs.
//!
//! **Amount** (0..=1) blends with plain scaling as Photoshop does: carving
//! covers that share of the size change and a Lanczos resample
//! ([`crate::resample::resample`]) the rest, so 0 is exactly a plain resample.

use lumenply_doc::adjust::srgb_encode;
use lumenply_tiles::{Raster, Rgba};
use rayon::prelude::*;

use crate::resample::resample;

/// Energy added per fully protected pixel: more than any seam can collect
/// from image content alone, so seams cross protection only when they must.
const PROTECT_ENERGY: f32 = 1.0e5;
/// Energy added per skin-coloured pixel ("Protect skin tones").
const SKIN_ENERGY: f32 = 2.0e2;

/// What Content-Aware Scale does besides the target size.
#[derive(Clone, Copy, Debug, Default)]
pub struct CarveOptions<'a> {
    /// The share of the size change done by carving, 0..=1; the rest is a
    /// plain resample (Photoshop's Amount / 100).
    pub amount: f32,
    /// Per-pixel protection 0..=1, row-major, the size of the source.
    pub protect: Option<&'a [f32]>,
    /// Raise the energy of skin-coloured pixels.
    pub skin: bool,
}

/// Scale `src` to `w × h`, content-aware by `opts.amount`.
pub fn content_aware_scale(src: &Raster, w: u32, h: u32, opts: &CarveOptions) -> Raster {
    content_aware_scale_carry(src, None, w, h, opts).0
}

/// [`content_aware_scale`] that also carries a per-pixel channel (a layer
/// mask) through exactly the same seams and resampling, so it stays
/// registered with the pixels.
pub fn content_aware_scale_carry(
    src: &Raster,
    carry: Option<&[f32]>,
    w: u32,
    h: u32,
    opts: &CarveOptions,
) -> (Raster, Option<Vec<f32>>) {
    let (w, h) = (w.max(1), h.max(1));
    let n = src.width as usize * src.height as usize;
    if n == 0 {
        return (
            Raster::new(w, h),
            carry.map(|_| vec![0.0; w as usize * h as usize]),
        );
    }
    if let Some(p) = opts.protect {
        assert_eq!(p.len(), n, "protect mask does not match the image");
    }
    if let Some(c) = carry {
        assert_eq!(c.len(), n, "carried channel does not match the image");
    }
    let amount = if opts.amount.is_finite() {
        opts.amount.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let cw = carve_target(src.width, w, amount);
    let ch = carve_target(src.height, h, amount);
    let (raster, carried) = if (cw, ch) == (src.width, src.height) {
        (src.clone(), carry.map(|c| c.to_vec()))
    } else {
        let mut wk = Work::new(src, carry, opts);
        if cw != src.width {
            wk = resize_cols(wk, cw as usize);
        }
        if ch != src.height {
            wk = resize_cols(wk.transposed(), ch as usize).transposed();
        }
        let r = Raster {
            width: wk.w as u32,
            height: wk.h as u32,
            pixels: wk.px,
        };
        (r, wk.carry)
    };
    if (raster.width, raster.height) == (w, h) {
        return (raster, carried);
    }
    let out = resample(&raster, w, h);
    let carried = carried.map(|c| resample_channel(&c, raster.width, raster.height, w, h));
    (out, carried)
}

/// The size carving takes `from` to on the way to `to`.
fn carve_target(from: u32, to: u32, amount: f32) -> u32 {
    let d = to as f32 - from as f32;
    ((from as f32 + d * amount).round() as i64).max(1) as u32
}

/// Resample a single channel through the colour resampler.
fn resample_channel(c: &[f32], w: u32, h: u32, nw: u32, nh: u32) -> Vec<f32> {
    let r = Raster {
        width: w,
        height: h,
        pixels: c.iter().map(|&v| Rgba::new(v, v, v, 1.0)).collect(),
    };
    resample(&r, nw, nh)
        .pixels
        .iter()
        .map(|p| p.r.clamp(0.0, 1.0))
        .collect()
}

/// The image being carved, row-major with stride `w`. Every array is cut
/// and grown along the same seams. Empty arrays are skipped.
#[derive(Clone)]
struct Work {
    w: usize,
    h: usize,
    px: Vec<Rgba>,
    /// Gamma luminance (plus a step for transparency): what energy reads.
    gray: Vec<f32>,
    /// Fixed extra energy: protection and skin.
    weight: Vec<f32>,
    carry: Option<Vec<f32>>,
    /// Original column of each pixel, while searching seams to insert.
    idx: Vec<u32>,
}

/// Skin test on gamma-encoded, unpremultiplied 0..=1 RGB: the common
/// Cb 77–127, Cr 133–173 box (Chai & Ngan) on 8-bit YCbCr, not too dark.
pub fn is_skin(r: f32, g: f32, b: f32) -> bool {
    let (r, g, b) = (r * 255.0, g * 255.0, b * 255.0);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = 128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b;
    let cr = 128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    y > 40.0 && (77.0..=127.0).contains(&cb) && (133.0..=173.0).contains(&cr)
}

impl Work {
    fn new(src: &Raster, carry: Option<&[f32]>, opts: &CarveOptions) -> Work {
        let (w, h) = (src.width as usize, src.height as usize);
        let mut gray = vec![0.0f32; w * h];
        let mut weight = vec![0.0f32; w * h];
        gray.par_chunks_mut(w)
            .zip(weight.par_chunks_mut(w))
            .enumerate()
            .for_each(|(y, (g, wt))| {
                for x in 0..w {
                    let i = y * w + x;
                    let p = src.pixels[i];
                    let lum = 0.2126 * p.r + 0.7152 * p.g + 0.0722 * p.b;
                    g[x] = srgb_encode(lum) + 0.5 * (1.0 - p.a.clamp(0.0, 1.0));
                    let mut e = opts
                        .protect
                        .map_or(0.0, |m| m[i].clamp(0.0, 1.0) * PROTECT_ENERGY);
                    if opts.skin && p.a > 0.5 {
                        let k = 1.0 / p.a;
                        if is_skin(srgb_encode(p.r * k), srgb_encode(p.g * k), srgb_encode(p.b * k)) {
                            e += SKIN_ENERGY;
                        }
                    }
                    wt[x] = e;
                }
            });
        Work {
            w,
            h,
            px: src.pixels.clone(),
            gray,
            weight,
            carry: carry.map(|c| c.to_vec()),
            idx: Vec::new(),
        }
    }

    fn transposed(self) -> Work {
        let (w, h) = (self.w, self.h);
        Work {
            w: h,
            h: w,
            px: transpose(&self.px, w, h),
            gray: transpose(&self.gray, w, h),
            weight: transpose(&self.weight, w, h),
            carry: self.carry.map(|c| transpose(&c, w, h)),
            idx: transpose(&self.idx, w, h),
        }
    }
}

fn transpose<T: Copy + Send + Sync>(v: &[T], w: usize, h: usize) -> Vec<T> {
    if v.is_empty() {
        return Vec::new();
    }
    // Output row x is input column x.
    let mut out = vec![v[0]; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, o) in col.iter_mut().enumerate() {
            *o = v[y * w + x];
        }
    });
    out
}

/// Rows of `src` (stride `w`) picked by `idx` (stride `nw`).
fn gather<T: Copy + Send + Sync>(src: &[T], w: usize, idx: &[u32], nw: usize) -> Vec<T> {
    let mut out = vec![src[0]; idx.len()];
    out.par_chunks_mut(nw).enumerate().for_each(|(y, o)| {
        let row = &src[y * w..(y + 1) * w];
        for (v, &i) in o.iter_mut().zip(&idx[y * nw..(y + 1) * nw]) {
            *v = row[i as usize];
        }
    });
    out
}

/// Carve (or grow) the width to `to` columns.
fn resize_cols(wk: Work, to: usize) -> Work {
    let to = to.max(1);
    if to < wk.w {
        // Cut only what energy reads plus each pixel's original column;
        // gather the pixels (and the carried channel) once at the end.
        let (w0, h) = (wk.w, wk.h);
        let mut wk = wk;
        let px = std::mem::take(&mut wk.px);
        let carry = wk.carry.take();
        wk.idx = (0..h).flat_map(|_| 0..w0 as u32).collect();
        let mut out = shrink(wk, w0 - to, &mut |_, _| {});
        let idx = std::mem::take(&mut out.idx);
        out.px = gather(&px, w0, &idx, out.w);
        out.carry = carry.map(|c| gather(&c, w0, &idx, out.w));
        out
    } else if to > wk.w {
        let n = to - wk.w;
        enlarge(wk, n)
    } else {
        wk
    }
}

/// Remove `n` vertical seams. `found` sees each pass's seams (in the
/// coordinates of the image they were found in) before they are cut.
fn shrink(mut wk: Work, n: usize, found: &mut dyn FnMut(&Work, &[Vec<u32>])) -> Work {
    let mut left = n.min(wk.w.saturating_sub(1));
    let mut scratch = Scratch::default();
    while left > 0 {
        // A batch of at most a twentieth of the width keeps later seams in
        // a pass close to what a fresh pass would choose.
        let k_max = left.min((wk.w / 20).max(1));
        let seams = find_seams(&wk, k_max, &mut scratch);
        found(&wk, &seams);
        wk = cut(&wk, &seams);
        left -= seams.len();
    }
    wk
}

/// Insert `n` vertical seams, in steps of at most half the width.
fn enlarge(mut wk: Work, n: usize) -> Work {
    let mut left = n;
    while left > 0 {
        let step = left.min((wk.w / 2).max(1));
        // Search on a copy that only carries what energy needs, plus the
        // original column of every pixel.
        let probe = Work {
            w: wk.w,
            h: wk.h,
            px: Vec::new(),
            gray: wk.gray.clone(),
            weight: wk.weight.clone(),
            carry: None,
            idx: (0..wk.h).flat_map(|_| 0..wk.w as u32).collect(),
        };
        let mut dup = vec![Vec::with_capacity(step); wk.h];
        if step >= probe.w {
            // Too narrow to search: duplicate every column.
            for (y, d) in dup.iter_mut().enumerate() {
                d.extend(
                    probe.idx[y * probe.w..(y + 1) * probe.w]
                        .iter()
                        .copied()
                        .take(step),
                );
            }
        } else {
            let _rest = shrink(probe, step, &mut |p, seams| {
                for seam in seams {
                    for (y, &x) in seam.iter().enumerate() {
                        dup[y].push(p.idx[y * p.w + x as usize]);
                    }
                }
            });
        }
        for d in &mut dup {
            d.sort_unstable();
        }
        wk = grow(&wk, &dup);
        left -= step;
    }
    wk
}

/// Buffers reused by every pass of one carve.
#[derive(Default)]
struct Scratch {
    base: Vec<f32>,
    dl: Vec<f32>,
    dr: Vec<f32>,
    dir: Vec<i8>,
    used: Vec<bool>,
}

/// Up to `k_max` cheapest disjoint vertical seams (x per row), cheapest
/// first. Always at least one.
fn find_seams(wk: &Work, k_max: usize, s: &mut Scratch) -> Vec<Vec<u32>> {
    let (w, h) = (wk.w, wk.h);
    // Per pixel: base = own energy + the cost of the new horizontal
    // neighbours (forward energy C_U); dl, dr = the extra vertical cost of
    // arriving from the upper left / right (C_L - C_U, C_R - C_U).
    for v in [&mut s.base, &mut s.dl, &mut s.dr] {
        v.resize(w * h, 0.0);
    }
    s.dir.resize(w * h, 0);
    s.used.clear();
    s.used.resize(w * h, false);
    let (base, dl, dr, dir, used) = (&mut s.base, &mut s.dl, &mut s.dr, &mut s.dir, &mut s.used);
    base.par_chunks_mut(w)
        .zip(dl.par_chunks_mut(w))
        .zip(dr.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, ((b, l), r))| {
            let g = &wk.gray[y * w..(y + 1) * w];
            let up = (y > 0).then(|| &wk.gray[(y - 1) * w..y * w]);
            let dn = (y + 1 < h).then(|| &wk.gray[(y + 1) * w..(y + 2) * w]);
            let wt = &wk.weight[y * w..(y + 1) * w];
            for x in 0..w {
                let c = g[x];
                let gl = if x > 0 { g[x - 1] } else { c };
                let gr = if x + 1 < w { g[x + 1] } else { c };
                let gu = up.map_or(c, |u| u[x]);
                let gd = dn.map_or(c, |d| d[x]);
                let cu = (gr - gl).abs();
                let grad = 0.5 * ((gr - gl).abs() + (gd - gu).abs());
                b[x] = wt[x] + cu + grad;
                l[x] = (gu - gl).abs();
                r[x] = (gu - gr).abs();
            }
        });
    // Cumulative minimum, top to bottom; `dir` remembers the step up.
    let mut prev: Vec<f64> = base[..w].iter().map(|&v| v as f64).collect();
    let mut cur = vec![0f64; w];
    for y in 1..h {
        let o = y * w;
        let (b, l, r) = (&base[o..o + w], &dl[o..o + w], &dr[o..o + w]);
        let d = &mut dir[o..o + w];
        for x in 0..w {
            let mut best = prev[x];
            let mut k = 0i8;
            if x > 0 {
                let v = prev[x - 1] + l[x] as f64;
                if v < best {
                    best = v;
                    k = -1;
                }
            }
            if x + 1 < w {
                let v = prev[x + 1] + r[x] as f64;
                if v < best {
                    best = v;
                    k = 1;
                }
            }
            cur[x] = best + b[x] as f64;
            d[x] = k;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let mut order: Vec<u32> = (0..w as u32).collect();
    order.sort_by(|&a, &b| prev[a as usize].total_cmp(&prev[b as usize]).then(a.cmp(&b)));
    let best = prev[order[0] as usize];
    // Later seams in a batch must be nearly as cheap as the first; the
    // absolute slack lets flat areas give up many seams at once.
    let limit = best * 1.5 + 0.05 * h as f64;
    let mut seams: Vec<Vec<u32>> = Vec::new();
    let mut path = vec![0u32; h];
    for &x0 in &order {
        if seams.len() >= k_max || (!seams.is_empty() && prev[x0 as usize] > limit) {
            break;
        }
        let mut x = x0 as usize;
        let mut ok = true;
        for y in (0..h).rev() {
            let row = y * w;
            if used[row + x] || (x > 0 && used[row + x - 1]) || (x + 1 < w && used[row + x + 1]) {
                ok = false;
                break;
            }
            path[y] = x as u32;
            if y > 0 {
                x = (x as i32 + dir[row + x] as i32) as usize;
            }
        }
        if !ok {
            continue;
        }
        for (y, &x) in path.iter().enumerate() {
            used[y * w + x as usize] = true;
        }
        seams.push(path.clone());
    }
    seams
}

/// Per row, the sorted columns the seams pass through.
fn rows_of(seams: &[Vec<u32>], h: usize) -> Vec<Vec<u32>> {
    let mut rows = vec![Vec::with_capacity(seams.len()); h];
    for s in seams {
        for (y, &x) in s.iter().enumerate() {
            rows[y].push(x);
        }
    }
    for r in &mut rows {
        r.sort_unstable();
    }
    rows
}

/// Remove the seams from every array.
fn cut(wk: &Work, seams: &[Vec<u32>]) -> Work {
    let rows = rows_of(seams, wk.h);
    let nw = wk.w - seams.len();
    let f = |v: &[_]| compact(v, wk.w, nw, &rows);
    Work {
        w: nw,
        h: wk.h,
        px: compact(&wk.px, wk.w, nw, &rows),
        gray: f(&wk.gray),
        weight: f(&wk.weight),
        carry: wk.carry.as_ref().map(|c| f(c)),
        idx: compact(&wk.idx, wk.w, nw, &rows),
    }
}

fn compact<T: Copy + Send + Sync>(src: &[T], w: usize, nw: usize, rows: &[Vec<u32>]) -> Vec<T> {
    if src.is_empty() {
        return Vec::new();
    }
    let mut out = vec![src[0]; nw * rows.len()];
    out.par_chunks_mut(nw)
        .zip(rows.par_iter())
        .enumerate()
        .for_each(|(y, (o, c))| {
            let row = &src[y * w..(y + 1) * w];
            let (mut j, mut n) = (0, 0);
            for (x, &v) in row.iter().enumerate() {
                if j < c.len() && c[j] as usize == x {
                    j += 1;
                    continue;
                }
                o[n] = v;
                n += 1;
            }
        });
    out
}

/// Insert, after each listed column of each row, the average of that
/// pixel and its right-hand neighbour.
fn grow(wk: &Work, dup: &[Vec<u32>]) -> Work {
    let k = dup.first().map_or(0, |d| d.len());
    let nw = wk.w + k;
    let mix_px = |a: Rgba, b: Rgba| {
        Rgba::new(
            (a.r + b.r) * 0.5,
            (a.g + b.g) * 0.5,
            (a.b + b.b) * 0.5,
            (a.a + b.a) * 0.5,
        )
    };
    let mix_f = |a: f32, b: f32| (a + b) * 0.5;
    Work {
        w: nw,
        h: wk.h,
        px: expand(&wk.px, wk.w, dup, mix_px),
        gray: expand(&wk.gray, wk.w, dup, mix_f),
        weight: expand(&wk.weight, wk.w, dup, mix_f),
        carry: wk.carry.as_ref().map(|c| expand(c, wk.w, dup, mix_f)),
        idx: Vec::new(),
    }
}

fn expand<T: Copy + Send + Sync>(
    src: &[T],
    w: usize,
    dup: &[Vec<u32>],
    mix: impl Fn(T, T) -> T + Sync,
) -> Vec<T> {
    if src.is_empty() {
        return Vec::new();
    }
    // Every row gains the same number of pixels.
    let nw = w + dup.first().map_or(0, |d| d.len());
    let mut out = vec![src[0]; nw * dup.len()];
    out.par_chunks_mut(nw)
        .zip(dup.par_iter())
        .enumerate()
        .for_each(|(y, (o, d))| {
            let row = &src[y * w..(y + 1) * w];
            let (mut j, mut n) = (0, 0);
            for (x, &v) in row.iter().enumerate() {
                o[n] = v;
                n += 1;
                while j < d.len() && d[j] as usize == x {
                    o[n] = mix(v, row[(x + 1).min(w - 1)]);
                    n += 1;
                    j += 1;
                }
            }
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise in 0..1.
    fn noise(i: u32) -> f32 {
        let mut x = i.wrapping_mul(0x9E37_79B9) ^ 0x5EED;
        x ^= x >> 15;
        x = x.wrapping_mul(0x85EB_CA6B);
        x ^= x >> 13;
        (x & 0xFFFF) as f32 / 65535.0
    }

    fn noisy_px(x: u32, y: u32) -> Rgba {
        let v = noise(y * 7919 + x);
        Rgba::new(v, noise(y * 104_729 + x * 31 + 1), 1.0 - v, 1.0)
    }

    /// Flat grey left of `split`, high-contrast noise from `split` on.
    fn flat_then_detail(w: u32, h: u32, split: u32) -> Raster {
        let mut r = Raster::filled(w, h, Rgba::new(0.4, 0.4, 0.4, 1.0));
        for y in 0..h {
            for x in split..w {
                r.set(x, y, noisy_px(x, y));
            }
        }
        r
    }

    fn full(amount: f32) -> CarveOptions<'static> {
        CarveOptions {
            amount,
            ..Default::default()
        }
    }

    #[test]
    fn shrinking_removes_seams_from_the_flat_part_only() {
        // 120 × 60: flat 0..60, detailed 60..120. Down to 90 wide.
        let src = flat_then_detail(120, 60, 60);
        let out = content_aware_scale(&src, 90, 60, &full(1.0));
        assert_eq!((out.width, out.height), (90, 60));
        // The detailed half is bit-identical, 30 px to the left.
        for y in 0..60 {
            for x in 60..120 {
                assert_eq!(out.get(x - 30, y), src.get(x, y), "object pixel ({x}, {y})");
            }
        }
        // And the flat part is still flat.
        for y in 0..60 {
            for x in 0..30 {
                assert_eq!(out.get(x, y), Rgba::new(0.4, 0.4, 0.4, 1.0));
            }
        }
    }

    #[test]
    fn shrinking_the_height_runs_on_the_transposed_image() {
        // Detail on top (rows 0..40), flat below. 70 → 50 rows.
        let mut src = Raster::filled(50, 70, Rgba::new(0.2, 0.5, 0.7, 1.0));
        for y in 0..40 {
            for x in 0..50 {
                src.set(x, y, noisy_px(x, y));
            }
        }
        let out = content_aware_scale(&src, 50, 50, &full(1.0));
        assert_eq!((out.width, out.height), (50, 50));
        for y in 0..40 {
            for x in 0..50 {
                assert_eq!(out.get(x, y), src.get(x, y), "({x}, {y})");
            }
        }
        assert_eq!(out.get(25, 45), Rgba::new(0.2, 0.5, 0.7, 1.0));
    }

    #[test]
    fn a_protected_band_comes_through_untouched() {
        // Noise everywhere: unprotected, seams cut through the band too.
        let (w, h) = (100u32, 40u32);
        let mut src = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                src.set(x, y, noisy_px(x, y));
            }
        }
        let band = 20..50u32;
        let mut protect = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in band.clone() {
                protect[(y * w + x) as usize] = 1.0;
            }
        }
        // Where does the band's first pixel of each row land? It must be
        // the same column in every row, with the whole band intact.
        let band_shift = |out: &Raster| -> Option<u32> {
            let mut shift = None;
            for y in 0..h {
                let s = (0..=band.start)
                    .find(|&s| band.clone().all(|x| x >= s && out.get(x - s, y) == src.get(x, y)))?;
                if *shift.get_or_insert(s) != s {
                    return None;
                }
            }
            shift
        };
        let opts = CarveOptions {
            amount: 1.0,
            protect: Some(&protect),
            skin: false,
        };
        let out = content_aware_scale(&src, 70, h, &opts);
        assert_eq!((out.width, out.height), (70, h));
        let s = band_shift(&out).expect("the protected band is intact");
        assert!(s <= 20, "at most the 20 columns left of the band go: {s}");
        let plain = content_aware_scale(&src, 70, h, &full(1.0));
        assert_eq!(band_shift(&plain), None, "without protection seams cross it");
    }

    #[test]
    fn insertion_enlarges_to_the_exact_size_and_keeps_the_detail() {
        let src = flat_then_detail(80, 30, 40);
        for (w, h) in [(100, 30), (80, 41), (130, 45), (200, 30)] {
            let out = content_aware_scale(&src, w, h, &full(1.0));
            assert_eq!((out.width, out.height), (w, h));
        }
        // 80 → 100: twenty averaged seams go into the flat part; the
        // detail moves right by exactly 20 px and is bit-identical.
        let out = content_aware_scale(&src, 100, 30, &full(1.0));
        for y in 0..30 {
            for x in 40..80 {
                assert_eq!(out.get(x + 20, y), src.get(x, y), "({x}, {y})");
            }
            for x in 0..60 {
                assert_eq!(out.get(x, y), Rgba::new(0.4, 0.4, 0.4, 1.0), "flat ({x}, {y})");
            }
        }
    }

    #[test]
    fn inserted_pixels_average_their_neighbours() {
        // A horizontal ramp 0, 0.1, ... 0.9: each new pixel is the mean of
        // the seam pixel and its right neighbour (the edge pixel itself at
        // the right edge), so the row stays sorted, keeps every original
        // value, and only gains midpoints or edge copies.
        let mut src = Raster::new(10, 3);
        for y in 0..3 {
            for x in 0..10 {
                let v = x as f32 / 10.0;
                src.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let out = content_aware_scale(&src, 13, 3, &full(1.0));
        let row: Vec<f32> = (0..13).map(|x| out.get(x, 1).r).collect();
        assert!(row.windows(2).all(|p| p[0] <= p[1]), "{row:?}");
        for x in 0..10 {
            let v = x as f32 / 10.0;
            assert!(row.iter().any(|r| (r - v).abs() < 1e-6), "{v} kept: {row:?}");
        }
        for v in &row {
            assert!(
                ((v * 20.0).round() - v * 20.0).abs() < 1e-4,
                "{v} is a step or midpoint"
            );
        }
        // This ramp's seams: one midpoint each between 0.7–0.8 and
        // 0.8–0.9, and a copy of the right edge.
        let want = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.75, 0.8, 0.85, 0.9, 0.9];
        for (r, e) in row.iter().zip(want) {
            assert!((r - e).abs() < 1e-6, "{row:?}");
        }
    }

    #[test]
    fn amount_zero_is_a_plain_resample_and_amount_blends() {
        let src = flat_then_detail(64, 40, 30);
        let out = content_aware_scale(&src, 41, 29, &full(0.0));
        let plain = resample(&src, 41, 29);
        let worst = out
            .pixels
            .iter()
            .zip(&plain.pixels)
            .map(|(a, b)| (a.r - b.r).abs().max((a.g - b.g).abs()).max((a.a - b.a).abs()))
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "{worst}");
        // Half: carve 64 → 52 (half of 23 columns, rounded), then resample.
        assert_eq!(carve_target(64, 41, 0.5), 53);
        assert_eq!(carve_target(64, 41, 1.0), 41);
        assert_eq!(carve_target(64, 100, 0.25), 73);
        let half = content_aware_scale(&src, 41, 40, &full(0.5));
        assert_eq!((half.width, half.height), (41, 40));
    }

    #[test]
    fn a_carried_channel_follows_the_seams() {
        // Carry the detail mask: it must come out exactly where the
        // detail went.
        let src = flat_then_detail(60, 20, 30);
        let carry: Vec<f32> = (0..60 * 20)
            .map(|i| if i % 60 >= 30 { 1.0 } else { 0.0 })
            .collect();
        let (out, c) = content_aware_scale_carry(&src, Some(&carry), 45, 20, &full(1.0));
        let c = c.unwrap();
        assert_eq!(c.len(), 45 * 20);
        for y in 0..20u32 {
            for x in 0..45u32 {
                let detail = x >= 15;
                assert_eq!(
                    c[(y * 45 + x) as usize],
                    if detail { 1.0 } else { 0.0 },
                    "({x}, {y})"
                );
                if detail {
                    assert_eq!(out.get(x, y), src.get(x + 15, y));
                }
            }
        }
    }

    #[test]
    fn skin_tones_are_detected_in_the_gamma_ycbcr_box() {
        assert!(is_skin(0.88, 0.67, 0.55), "light skin");
        assert!(is_skin(0.55, 0.36, 0.25), "darker skin");
        assert!(!is_skin(0.2, 0.4, 0.9), "sky blue");
        assert!(!is_skin(0.3, 0.6, 0.2), "grass");
        assert!(!is_skin(0.5, 0.5, 0.5), "grey");
        // Skin protection steers seams away from a skin patch in noise-free
        // flat surroundings of a different colour.
        let mut src = Raster::filled(60, 20, Rgba::new(0.05, 0.2, 0.4, 1.0));
        let skin = Rgba::new(
            lumenply_doc::adjust::srgb_decode(0.88),
            lumenply_doc::adjust::srgb_decode(0.67),
            lumenply_doc::adjust::srgb_decode(0.55),
            1.0,
        );
        for y in 0..20 {
            for x in 0..30 {
                src.set(x, y, skin);
            }
        }
        let opts = CarveOptions {
            amount: 1.0,
            protect: None,
            skin: true,
        };
        let out = content_aware_scale(&src, 40, 20, &opts);
        let skin_cols = (0..40).filter(|&x| out.get(x, 10) == skin).count();
        assert_eq!(skin_cols, 30, "the skin patch keeps all 30 columns");
    }

    #[test]
    fn degenerate_sizes_still_answer() {
        let src = flat_then_detail(8, 6, 4);
        let one = content_aware_scale(&src, 1, 1, &full(1.0));
        assert_eq!((one.width, one.height), (1, 1));
        let wide = content_aware_scale(&src, 40, 6, &full(1.0));
        assert_eq!((wide.width, wide.height), (40, 6));
        let empty = content_aware_scale(&Raster::new(0, 0), 5, 4, &full(1.0));
        assert_eq!((empty.width, empty.height), (5, 4));
    }
}
