//! Content-aware fill: multi-scale PatchMatch inpainting (Barnes et al.
//! 2009, with Wexler et al.'s EM-style voting).
//!
//! The hole is filled coarse to fine. At each pyramid level a nearest-
//! neighbour field (NNF) maps every patch that touches the hole to the most
//! similar patch lying wholly outside it; the hole's pixels are then
//! re-estimated as the similarity-weighted average of what every
//! overlapping patch's match says they should be, and the two steps
//! alternate. Each finer level starts from the coarser field scaled up.
//!
//! Patches compare premultiplied, gamma-encoded colour plus alpha, so
//! distances follow what the eye sees and transparent pixels carry no
//! stale colour. Everything is seeded and every parallel pass works on
//! independent lines, so the result is deterministic.

use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_tiles::{Raster, Rgba};
use rayon::prelude::*;

/// Patch radius: 7×7 patches.
const R: i32 = 3;
const PATCH: i32 = 2 * R + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InpaintError {
    /// There is nothing outside the hole to copy from.
    NoSource,
}

impl std::fmt::Display for InpaintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("nothing outside the selection to sample from")
    }
}

type Px = [f32; 4];

struct Level {
    w: i32,
    h: i32,
    img: Vec<Px>,
    hole: Vec<bool>,
}

impl Level {
    #[inline]
    fn idx(&self, x: i32, y: i32) -> usize {
        (y * self.w + x) as usize
    }
}

/// Fill the pixels of `img` where `hole` is true from the rest of `img`.
/// `img` is premultiplied linear (the engine's pixels); pixels outside the
/// hole come back unchanged. `seed` makes the random search repeatable.
pub fn inpaint(img: &Raster, hole: &[bool], seed: u64) -> Result<Raster, InpaintError> {
    let (w, h) = (img.width as i32, img.height as i32);
    assert_eq!(hole.len(), (w * h) as usize, "hole mask does not match the image");
    if !hole.iter().any(|&b| b) {
        return Ok(img.clone());
    }
    if hole.iter().all(|&b| b) {
        return Err(InpaintError::NoSource);
    }
    let base = Level {
        w,
        h,
        img: img.pixels.par_iter().map(|&p| encode(p)).collect(),
        hole: hole.to_vec(),
    };
    // Halve until the hole is a couple of patches across, keeping enough
    // room around it to sample from.
    let extent = hole_extent(&base);
    let mut levels = vec![base];
    loop {
        let l = levels.last().expect("at least the base level");
        let (nw, nh) = ((l.w + 1) / 2, (l.h + 1) / 2);
        let e = extent >> (levels.len() - 1);
        if e <= 2 * PATCH || nw.min(nh) < 3 * PATCH {
            break;
        }
        levels.push(downsample(l));
    }

    let mut rng = Rng::new(seed);
    let mut nnf: Vec<u32> = Vec::new();
    let mut prev_w = 0;
    let mut prev_h = 0;
    let mut prev_img: Vec<Px> = Vec::new();
    let coarsest = levels.len() - 1;
    for li in (0..levels.len()).rev() {
        let lvl = &mut levels[li];
        let valid = valid_sources(lvl)?;
        let sources: Vec<u32> = (0..valid.len() as u32).filter(|&i| valid[i as usize]).collect();
        let targets = targets_of(lvl);
        if li == coarsest {
            onion_fill(lvl);
            nnf = vec![0; lvl.img.len()];
            for &t in &targets {
                nnf[t] = sources[rng.below(sources.len() as u32) as usize];
            }
        } else {
            // Scale the coarser field and image up.
            let mut up = vec![0u32; lvl.img.len()];
            for &t in &targets {
                let (x, y) = (t as i32 % lvl.w, t as i32 / lvl.w);
                let (cx, cy) = ((x / 2).min(prev_w - 1), (y / 2).min(prev_h - 1));
                let s = nnf[(cy * prev_w + cx) as usize] as i32;
                let (sx, sy) = (s % prev_w, s / prev_w);
                let fx = (2 * sx + (x - 2 * cx)).clamp(R, lvl.w - 1 - R);
                let fy = (2 * sy + (y - 2 * cy)).clamp(R, lvl.h - 1 - R);
                let f = lvl.idx(fx, fy);
                up[t] = if valid[f] {
                    f as u32
                } else {
                    sources[rng.below(sources.len() as u32) as usize]
                };
            }
            for y in 0..lvl.h {
                for x in 0..lvl.w {
                    let i = lvl.idx(x, y);
                    if lvl.hole[i] {
                        let (cx, cy) = ((x / 2).min(prev_w - 1), (y / 2).min(prev_h - 1));
                        lvl.img[i] = prev_img[(cy * prev_w + cx) as usize];
                    }
                }
            }
            nnf = up;
            let cost = costs(lvl, &targets, &nnf);
            vote(lvl, &nnf, &cost);
        }
        let em = if li == coarsest {
            8
        } else if li == 0 {
            3
        } else {
            4
        };
        for it in 0..em {
            let mut cost = costs(lvl, &targets, &nnf);
            let s = rng.next() ^ ((li as u64) << 40) ^ ((it as u64) << 32);
            patchmatch(lvl, &valid, &targets, &mut nnf, &mut cost, s);
            vote(lvl, &nnf, &cost);
        }
        prev_w = lvl.w;
        prev_h = lvl.h;
        prev_img = lvl.img.clone();
    }

    let fine = &levels[0];
    let mut out = img.clone();
    out.pixels
        .par_iter_mut()
        .zip(fine.img.par_iter().zip(fine.hole.par_iter()))
        .for_each(|(o, (f, &is_hole))| {
            if is_hole {
                *o = decode(*f);
            }
        });
    Ok(out)
}

/// Premultiplied linear → premultiplied gamma-encoded.
fn encode(p: Rgba) -> Px {
    if p.a <= 0.0 {
        return [0.0; 4];
    }
    let [r, g, b, a] = p.to_straight();
    [srgb_encode(r) * a, srgb_encode(g) * a, srgb_encode(b) * a, a]
}

fn decode(p: Px) -> Rgba {
    let a = p[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return Rgba::TRANSPARENT;
    }
    let s = |v: f32| srgb_decode((v / a).clamp(0.0, 1.0));
    Rgba::from_straight(s(p[0]), s(p[1]), s(p[2]), a)
}

/// The larger side of the hole's bounding box.
fn hole_extent(l: &Level) -> i32 {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in 0..l.h {
        for x in 0..l.w {
            if l.hole[l.idx(x, y)] {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    (x1 - x0 + 1).max(y1 - y0 + 1).max(0)
}

/// Half resolution: known colour is the mean of the known children; a
/// pixel is hole if any child is.
fn downsample(l: &Level) -> Level {
    let (w, h) = ((l.w + 1) / 2, (l.h + 1) / 2);
    let mut img = vec![[0.0; 4]; (w * h) as usize];
    let mut hole = vec![false; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0f32; 4];
            let mut all = [0.0f32; 4];
            let (mut n, mut na) = (0.0f32, 0.0f32);
            let mut any_hole = false;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = (2 * x + dx, 2 * y + dy);
                if sx >= l.w || sy >= l.h {
                    continue;
                }
                let i = l.idx(sx, sy);
                let p = l.img[i];
                add_px(&mut all, p, 1.0);
                na += 1.0;
                if l.hole[i] {
                    any_hole = true;
                } else {
                    add_px(&mut sum, p, 1.0);
                    n += 1.0;
                }
            }
            let o = (y * w + x) as usize;
            hole[o] = any_hole;
            img[o] = if n > 0.0 {
                sum.map(|v| v / n)
            } else {
                all.map(|v| v / na)
            };
        }
    }
    Level { w, h, img, hole }
}

/// Patch centres a match may point at: the whole patch inside the image
/// and clear of the hole. When the hole leaves no such patch, any centre
/// outside the hole will do.
fn valid_sources(l: &Level) -> Result<Vec<bool>, InpaintError> {
    let (w, h) = (l.w as usize, l.h as usize);
    let iw = w + 1;
    let mut sum = vec![0u32; iw * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += l.hole[y * w + x] as u32;
            sum[(y + 1) * iw + x + 1] = sum[y * iw + x + 1] + row;
        }
    }
    let mut valid = vec![false; w * h];
    let mut any = false;
    for y in R..l.h - R {
        for x in R..l.w - R {
            let (x0, y0, x1, y1) = (
                (x - R) as usize,
                (y - R) as usize,
                (x + R + 1) as usize,
                (y + R + 1) as usize,
            );
            let n = sum[y1 * iw + x1] + sum[y0 * iw + x0] - sum[y0 * iw + x1] - sum[y1 * iw + x0];
            if n == 0 {
                valid[l.idx(x, y)] = true;
                any = true;
            }
        }
    }
    if !any {
        for y in R..l.h - R {
            for x in R..l.w - R {
                let i = l.idx(x, y);
                valid[i] = !l.hole[i];
                any |= valid[i];
            }
        }
    }
    if any {
        Ok(valid)
    } else {
        Err(InpaintError::NoSource)
    }
}

/// Patch centres whose patch touches the hole: the hole grown by R.
fn targets_of(l: &Level) -> Vec<usize> {
    let mut is = vec![false; l.img.len()];
    for y in 0..l.h {
        for x in 0..l.w {
            if !l.hole[l.idx(x, y)] {
                continue;
            }
            for ty in (y - R).max(0)..=(y + R).min(l.h - 1) {
                for tx in (x - R).max(0)..=(x + R).min(l.w - 1) {
                    is[(ty * l.w + tx) as usize] = true;
                }
            }
        }
    }
    (0..is.len()).filter(|&i| is[i]).collect()
}

/// A smooth first guess for the coarsest hole: peel it from the outside
/// in, each ring the mean of its known neighbours.
fn onion_fill(l: &mut Level) {
    let mut known: Vec<bool> = l.hole.iter().map(|&b| !b).collect();
    loop {
        let mut ring = Vec::new();
        for y in 0..l.h {
            for x in 0..l.w {
                let i = l.idx(x, y);
                if known[i] {
                    continue;
                }
                let mut sum = [0.0f32; 4];
                let mut n = 0.0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < 0 || ny < 0 || nx >= l.w || ny >= l.h {
                            continue;
                        }
                        let j = l.idx(nx, ny);
                        if known[j] {
                            add_px(&mut sum, l.img[j], 1.0);
                            n += 1.0;
                        }
                    }
                }
                if n > 0.0 {
                    ring.push((i, sum.map(|v| v / n)));
                }
            }
        }
        if ring.is_empty() {
            return;
        }
        for (i, p) in ring {
            l.img[i] = p;
            known[i] = true;
        }
    }
}

/// Sum of squared differences between the patch at target centre `t` and
/// source centre `s`, giving up once it passes `best`. Target pixels past
/// the image edge repeat the edge; source patches are always inside.
#[inline]
fn dist(l: &Level, t: usize, s: usize, best: f32) -> f32 {
    let w = l.w;
    let (tx, ty) = (t as i32 % w, t as i32 / w);
    let (sx, sy) = (s as i32 % w, s as i32 / w);
    let inside = tx >= R && ty >= R && tx < l.w - R && ty < l.h - R;
    let mut d = 0.0f32;
    for dy in -R..=R {
        let srow = ((sy + dy) * w + sx) as isize;
        if inside {
            let trow = ((ty + dy) * w + tx) as isize;
            for dx in -R..=R {
                let a = l.img[(trow + dx as isize) as usize];
                let b = l.img[(srow + dx as isize) as usize];
                d += sq(a, b);
            }
        } else {
            let yy = (ty + dy).clamp(0, l.h - 1);
            for dx in -R..=R {
                let xx = (tx + dx).clamp(0, l.w - 1);
                let a = l.img[(yy * w + xx) as usize];
                let b = l.img[(srow + dx as isize) as usize];
                d += sq(a, b);
            }
        }
        if d >= best {
            return d;
        }
    }
    d
}

#[inline]
fn add_px(acc: &mut Px, p: Px, k: f32) {
    for (a, v) in acc.iter_mut().zip(p) {
        *a += v * k;
    }
}

#[inline]
fn sq(a: Px, b: Px) -> f32 {
    let (r, g, bl, al) = (a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]);
    r * r + g * g + bl * bl + al * al
}

fn costs(l: &Level, targets: &[usize], nnf: &[u32]) -> Vec<f32> {
    let mut cost = vec![f32::INFINITY; l.img.len()];
    let vals: Vec<(usize, f32)> = targets
        .par_iter()
        .map(|&t| (t, dist(l, t, nnf[t] as usize, f32::INFINITY)))
        .collect();
    for (t, c) in vals {
        cost[t] = c;
    }
    cost
}

/// One PatchMatch round: propagation and random search sweeping rows
/// forward, columns forward, rows backward, columns backward. Each sweep
/// runs its lines in parallel; a line only reads its own updated entries,
/// so the outcome does not depend on scheduling.
fn patchmatch(l: &Level, valid: &[bool], targets: &[usize], nnf: &mut [u32], cost: &mut [f32], seed: u64) {
    let mut is_target = vec![false; l.img.len()];
    for &t in targets {
        is_target[t] = true;
    }
    let (w, h) = (l.w as usize, l.h as usize);
    let search = l.w.max(l.h);
    for (pass, (vertical, dir)) in [(false, 1i32), (true, 1), (false, -1), (true, -1)]
        .into_iter()
        .enumerate()
    {
        let lines = if vertical { w } else { h };
        let len = if vertical { h } else { w };
        let step = if vertical { w } else { 1 };
        let results: Vec<(usize, Vec<u32>, Vec<f32>)> = (0..lines)
            .into_par_iter()
            .filter_map(|line| {
                let at = |k: usize| if vertical { k * w + line } else { line * w + k };
                if !(0..len).any(|k| is_target[at(k)]) {
                    return None;
                }
                let mut f: Vec<u32> = (0..len).map(|k| nnf[at(k)]).collect();
                let mut c: Vec<f32> = (0..len).map(|k| cost[at(k)]).collect();
                let mut rng =
                    Rng::new(seed ^ ((pass as u64) << 56) ^ (line as u64).wrapping_mul(0x9E37_79B9));
                let order: Box<dyn Iterator<Item = usize>> = if dir > 0 {
                    Box::new(0..len)
                } else {
                    Box::new((0..len).rev())
                };
                for k in order {
                    let t = at(k);
                    if !is_target[t] {
                        continue;
                    }
                    // Propagation: the previous neighbour's match, shifted.
                    let prev = k as i32 - dir;
                    if prev >= 0 && (prev as usize) < len && is_target[at(prev as usize)] {
                        let cand = f[prev as usize] as i64 + (dir as i64) * step as i64;
                        if cand >= 0 && (cand as usize) < valid.len() && valid[cand as usize] {
                            // A shift along x must stay on the same row.
                            let same_line = vertical || (cand as usize) / w == f[prev as usize] as usize / w;
                            if same_line && cand as u32 != f[k] {
                                let d = dist(l, t, cand as usize, c[k]);
                                if d < c[k] {
                                    c[k] = d;
                                    f[k] = cand as u32;
                                }
                            }
                        }
                    }
                    // Random search in shrinking windows around the best.
                    let mut radius = search;
                    while radius >= 1 {
                        let cur = f[k] as i32;
                        let (cx, cy) = (cur % l.w, cur / l.w);
                        let nx = (cx + rng.range(radius)).clamp(R, l.w - 1 - R);
                        let ny = (cy + rng.range(radius)).clamp(R, l.h - 1 - R);
                        let cand = (ny * l.w + nx) as usize;
                        if valid[cand] && cand as u32 != f[k] {
                            let d = dist(l, t, cand, c[k]);
                            if d < c[k] {
                                c[k] = d;
                                f[k] = cand as u32;
                            }
                        }
                        radius /= 2;
                    }
                }
                Some((line, f, c))
            })
            .collect();
        for (line, f, c) in results {
            for k in 0..len {
                let i = if vertical { k * w + line } else { line * w + k };
                nnf[i] = f[k];
                cost[i] = c[k];
            }
        }
    }
}

/// Re-estimate every hole pixel as the weighted mean of what each patch
/// covering it says, through that patch's match. Closer matches weigh
/// more: exp(−d / 2σ) with σ the 75th percentile of the match distances.
fn vote(l: &mut Level, nnf: &[u32], cost: &[f32]) {
    let mut finite: Vec<f32> = cost.iter().copied().filter(|c| c.is_finite()).collect();
    let sigma = if finite.is_empty() {
        1.0
    } else {
        let k = (finite.len() * 3 / 4).min(finite.len() - 1);
        *finite.select_nth_unstable_by(k, f32::total_cmp).1
    }
    .max(1e-6);
    let (w, h) = (l.w, l.h);
    let src = &l.img;
    let hole = &l.hole;
    let new: Vec<(usize, Px)> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            (0..w).filter_map(move |x| {
                let q = (y * w + x) as usize;
                if !hole[q] {
                    return None;
                }
                let mut acc = [0.0f32; 4];
                let mut wsum = 0.0f32;
                for dy in -R..=R {
                    let py = y + dy;
                    if py < 0 || py >= h {
                        continue;
                    }
                    for dx in -R..=R {
                        let px = x + dx;
                        if px < 0 || px >= w {
                            continue;
                        }
                        let p = (py * w + px) as usize;
                        let c = cost[p];
                        if !c.is_finite() {
                            continue;
                        }
                        let s = nnf[p] as i32;
                        let (sx, sy) = (s % w - dx, s / w - dy);
                        let v = src[(sy * w + sx) as usize];
                        let wt = (-c / (2.0 * sigma)).exp().max(1e-8);
                        add_px(&mut acc, v, wt);
                        wsum += wt;
                    }
                }
                (wsum > 0.0).then(|| (q, acc.map(|v| v / wsum)))
            })
        })
        .collect();
    for (q, p) in new {
        l.img[q] = p;
    }
}

/// xorshift64*: small, fast, and the same everywhere.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u32) -> u32 {
        (((self.next() >> 32) * n as u64) >> 32) as u32
    }

    /// Uniform in [−r, r].
    fn range(&mut self, r: i32) -> i32 {
        self.below(2 * r as u32 + 1) as i32 - r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(v: f32) -> Rgba {
        Rgba::new(v, v, v, 1.0)
    }

    fn hole_rect(w: u32, h: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<bool> {
        let mut m = vec![false; (w * h) as usize];
        for y in y0..y1 {
            for x in x0..x1 {
                m[(y * w + x) as usize] = true;
            }
        }
        m
    }

    /// Blank the hole so a fill that did nothing can't pass.
    fn punch(img: &Raster, hole: &[bool]) -> Raster {
        let mut out = img.clone();
        for (p, &hl) in out.pixels.iter_mut().zip(hole) {
            if hl {
                *p = Rgba::new(1.0, 0.0, 1.0, 1.0);
            }
        }
        out
    }

    fn max_err(a: &Raster, b: &Raster, hole: &[bool]) -> f32 {
        a.pixels
            .iter()
            .zip(&b.pixels)
            .zip(hole)
            .filter(|(_, &hl)| hl)
            .map(|((p, q), _)| (p.r - q.r).abs().max((p.g - q.g).abs()).max((p.b - q.b).abs()))
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_flat_image_fills_flat() {
        let truth = Raster::filled(64, 48, Rgba::from_straight(0.3, 0.5, 0.2, 1.0));
        let hole = hole_rect(64, 48, 20, 10, 44, 38);
        let out = inpaint(&punch(&truth, &hole), &hole, 1).unwrap();
        assert!(max_err(&out, &truth, &hole) < 1e-4);
        assert_eq!(out.get(0, 0), truth.get(0, 0), "outside untouched");
    }

    #[test]
    fn stripes_continue_across_the_hole() {
        // Vertical stripes, period 8: four white columns, four dark.
        let mut truth = Raster::new(96, 64);
        for y in 0..64 {
            for x in 0..96 {
                truth.set(x, y, grey(if x % 8 < 4 { 0.9 } else { 0.05 }));
            }
        }
        let hole = hole_rect(96, 64, 34, 18, 58, 44); // 24×26, off-phase on both sides
        let out = inpaint(&punch(&truth, &hole), &hole, 7).unwrap();
        let e = max_err(&out, &truth, &hole);
        assert!(e < 0.02, "stripes reconstructed within 0.02, got {e}");
        // Spot checks in the middle of the hole: column 45 is white
        // (45 % 8 = 5 → dark), column 41 (41 % 8 = 1) white.
        assert!((out.get(41, 30).r - 0.9).abs() < 0.02);
        assert!((out.get(45, 30).r - 0.05).abs() < 0.02);
    }

    #[test]
    fn horizontal_stripes_and_a_hole_on_the_edge() {
        let mut truth = Raster::new(80, 80);
        for y in 0..80 {
            for x in 0..80 {
                truth.set(
                    x,
                    y,
                    Rgba::from_straight(0.8, if y % 6 < 3 { 0.7 } else { 0.1 }, 0.2, 1.0),
                );
            }
        }
        let hole = hole_rect(80, 80, 60, 30, 80, 50); // touches the right edge
        let out = inpaint(&punch(&truth, &hole), &hole, 3).unwrap();
        let e = max_err(&out, &truth, &hole);
        assert!(e < 0.02, "edge hole reconstructed within 0.02, got {e}");
    }

    #[test]
    fn deterministic_and_errors_without_a_source() {
        let mut img = Raster::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x * 13 + y * 7) % 17) as f32 / 17.0;
                img.set(x, y, grey(v));
            }
        }
        let hole = hole_rect(64, 64, 20, 20, 40, 40);
        let a = inpaint(&img, &hole, 42).unwrap();
        let b = inpaint(&img, &hole, 42).unwrap();
        assert_eq!(a, b, "same seed, same fill");
        assert_eq!(
            inpaint(&img, &vec![true; 64 * 64], 1),
            Err(InpaintError::NoSource)
        );
        assert_eq!(inpaint(&img, &vec![false; 64 * 64], 1).unwrap(), img);
    }

    #[test]
    #[ignore = "timing; cargo test --release -p lumenply-render inpaint_timing -- --ignored --nocapture"]
    fn inpaint_timing() {
        let (w, h) = (2400u32, 1600u32);
        let mut img = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = (((x / 7) ^ (y / 5)) % 13) as f32 / 13.0;
                img.set(x, y, Rgba::from_straight(v, 0.5 * v + 0.2, 1.0 - v, 1.0));
            }
        }
        for (hw, hh) in [(120u32, 120u32), (300, 240), (500, 400)] {
            let hole = hole_rect(w, h, 1000, 600, 1000 + hw, 600 + hh);
            let t = std::time::Instant::now();
            let _ = inpaint(&img, &hole, 1).unwrap();
            println!("{hw}×{hh} hole in a {w}×{h} image: {:?}", t.elapsed());
        }
    }
}
