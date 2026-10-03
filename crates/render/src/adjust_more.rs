//! Photoshop's destructive Image ▸ Adjustments that are more than a
//! per-pixel curve: Shadows/Highlights (local tone mapping from an
//! edge-aware luminance base), Equalize, Desaturate, Replace Color and
//! Match Color.
//!
//! Tone maths runs on gamma-encoded values (ADR 0005); pixels arrive and
//! leave premultiplied linear. Nothing here knows about selections or
//! layers: `lumenply_core::adjust_cmds` blends results by the selection.

use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_tiles::{Raster, Rect, Rgba, Tile, TileCoord, TileStore, TILE_SIZE};
use rayon::prelude::*;
use std::sync::Arc;

/// Rec. 709 luma weights, applied to gamma-encoded channels.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

#[inline]
fn encode3(c: [f32; 3]) -> [f32; 3] {
    [srgb_encode(c[0]), srgb_encode(c[1]), srgb_encode(c[2])]
}

#[inline]
fn decode3(c: [f32; 3]) -> [f32; 3] {
    [srgb_decode(c[0]), srgb_decode(c[1]), srgb_decode(c[2])]
}

/// Luma of gamma-encoded RGB.
#[inline]
pub fn gamma_luma(g: [f32; 3]) -> f32 {
    LUMA[0] * g[0] + LUMA[1] * g[1] + LUMA[2] * g[2]
}

/// `l + chroma`, with the chroma shortened just enough that every channel
/// stays in `[0, 1]`: the hue (the chroma's direction) and the luma are
/// kept exactly.
#[inline]
fn fit_chroma(l: f32, chroma: [f32; 3]) -> [f32; 3] {
    let l = l.clamp(0.0, 1.0);
    let mut k = 1.0f32;
    for c in chroma {
        if c > 1e-7 {
            k = k.min((1.0 - l) / c);
        } else if c < -1e-7 {
            k = k.min(l / -c);
        }
    }
    let k = k.max(0.0);
    [
        (l + chroma[0] * k).clamp(0.0, 1.0),
        (l + chroma[1] * k).clamp(0.0, 1.0),
        (l + chroma[2] * k).clamp(0.0, 1.0),
    ]
}

/// Map every pixel of every tile of `store` through `f(x, y, pixel)`
/// (canvas coordinates), in parallel by tile. Each tile's pixels are read
/// once (never per pixel), so compact tiles convert once. Tiles outside
/// `limit` are kept as they are (shared, not copied).
pub fn map_store_pixels(
    store: &TileStore,
    limit: Option<Rect>,
    f: impl Fn(i32, i32, Rgba) -> Rgba + Sync,
) -> TileStore {
    let mut out = TileStore::new();
    let mut coords: Vec<TileCoord> = Vec::new();
    for c in store.coords() {
        match (limit, store.tile_arc(c)) {
            (Some(l), Some(t)) if l.intersect(&c.rect()).is_empty() => out.insert(c, t.clone()),
            _ => coords.push(c),
        }
    }
    let tiles: Vec<(TileCoord, Tile)> = coords
        .par_iter()
        .filter_map(|&c| {
            let src = store.tile(c)?;
            let px = src.pixels();
            let (ox, oy) = c.origin();
            let mut t = Tile::new();
            let out = t.pixels_mut();
            for (i, (o, p)) in out.iter_mut().zip(px.iter()).enumerate() {
                let (x, y) = (ox + (i % TILE_SIZE) as i32, oy + (i / TILE_SIZE) as i32);
                *o = f(x, y, *p);
            }
            Some((c, t))
        })
        .collect();
    for (c, t) in tiles {
        out.insert(c, Arc::new(t));
    }
    out.prune_blank();
    out
}

/// The layer's pixels over `area`, with off-canvas pixels the layer
/// doesn't paint repeating the canvas edge (as destructive filters read
/// them, ADR 0009), so a full-canvas photo has no dark rim to tone-map.
fn read_edge_extended(store: &TileStore, area: Rect, canvas: Rect) -> Raster {
    let orig = store.to_raster(area);
    if canvas.is_empty() {
        return orig;
    }
    let mut src = orig.clone();
    let aw = area.w as usize;
    for y in 0..area.h as i32 {
        for x in 0..area.w as i32 {
            let (gx, gy) = (area.x + x, area.y + y);
            let i = y as usize * aw + x as usize;
            if canvas.contains(gx, gy) || orig.pixels[i].a > 0.0 {
                continue;
            }
            let (cx, cy) = (
                gx.clamp(canvas.x, canvas.right() - 1),
                gy.clamp(canvas.y, canvas.bottom() - 1),
            );
            if area.contains(cx, cy) {
                src.pixels[i] = orig.get((cx - area.x) as u32, (cy - area.y) as u32);
            }
        }
    }
    src
}

// ---- Shadows/Highlights ------------------------------------------------------------

/// One side of Shadows/Highlights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneZone {
    /// Strength, 0..1 (Photoshop's 0–100 %).
    pub amount: f32,
    /// Tonal width, 0..1: how far into the midtones the correction reaches.
    pub tone: f32,
    /// Size of the neighbourhood the local tone is measured over, in px.
    pub radius: f32,
}

/// Photoshop's Shadows/Highlights: lifts dark neighbourhoods and pulls
/// down bright ones while keeping local detail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowsHighlights {
    pub shadows: ToneZone,
    pub highlights: ToneZone,
    /// Saturation of the corrected areas, -1..1 (Photoshop's Color, ±100).
    pub color: f32,
    /// Midtone contrast, -1..1 (±100).
    pub midtone: f32,
}

impl Default for ShadowsHighlights {
    /// Photoshop's defaults: shadows 35 %, tone 50 %, 30 px; highlights
    /// off; color +20; midtone 0.
    fn default() -> Self {
        ShadowsHighlights {
            shadows: ToneZone {
                amount: 0.35,
                tone: 0.5,
                radius: 30.0,
            },
            highlights: ToneZone {
                amount: 0.0,
                tone: 0.5,
                radius: 30.0,
            },
            color: 0.2,
            midtone: 0.0,
        }
    }
}

/// Edge-stop of the guided-filter base, in squared gamma units: local
/// variation below ~0.12 is smoothed away, edges stronger than that are
/// kept, so the correction follows a mountain's outline instead of
/// bleeding across it (no halos).
const SH_EPS: f64 = 0.015;
/// Toe of the tone curves: keeps their slope finite at black, so lifted
/// shadows don't amplify the noise floor without bound.
const SH_TOE: f32 = 0.02;
/// How strongly a full-weight zone bends its curve (exponent 1 / (1 + 2w)).
const SH_GAIN: f32 = 2.0;

fn sane_radius(r: f32) -> usize {
    if r.is_finite() {
        r.round().clamp(1.0, 2500.0) as usize
    } else {
        1
    }
}

impl ShadowsHighlights {
    /// True when the settings change nothing.
    pub fn is_identity(&self) -> bool {
        self.shadows.amount <= 0.0 && self.highlights.amount <= 0.0 && self.midtone == 0.0
    }

    /// How far (px) a pixel's result depends on its neighbours: the guided
    /// filter is two box passes of the larger active radius.
    pub fn reach(&self) -> i32 {
        let mut r = 0;
        if self.shadows.amount > 0.0 {
            r = r.max(sane_radius(self.shadows.radius));
        }
        if self.highlights.amount > 0.0 {
            r = r.max(sane_radius(self.highlights.radius));
        }
        2 * r as i32
    }
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The shadow lift for weight `w` (0..1): a toe'd power curve that keeps
/// black at black and white at white and is the identity at `w = 0`.
#[inline]
pub fn lift_curve(l: f32, w: f32) -> f32 {
    if w <= 0.0 {
        return l;
    }
    let g = 1.0 / (1.0 + SH_GAIN * w);
    let t = SH_TOE;
    let lo = t.powf(g);
    (((l.clamp(0.0, 1.0) + t).powf(g) - lo) / ((1.0 + t).powf(g) - lo)).clamp(0.0, 1.0)
}

/// Weight of the shadow correction at base tone `b`.
#[inline]
fn shadow_weight(z: &ToneZone, b: f32) -> f32 {
    if z.amount <= 0.0 {
        return 0.0;
    }
    z.amount.clamp(0.0, 1.0) * (1.0 - smoothstep(0.0, z.tone.clamp(0.01, 1.0), b))
}

/// Weight of the highlight correction at base tone `b`.
#[inline]
fn highlight_weight(z: &ToneZone, b: f32) -> f32 {
    if z.amount <= 0.0 {
        return 0.0;
    }
    z.amount.clamp(0.0, 1.0) * smoothstep(1.0 - z.tone.clamp(0.01, 1.0), 1.0, b)
}

/// Box sums (window `2r+1`, clipped at the edges) of a `w`×`h` plane.
/// Stored as f32, summed in f64, so a sum doesn't depend on where the
/// running window started (tiles and the whole image agree).
fn box_sum(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &v[y * w..(y + 1) * w];
        let mut acc: f64 = row[..=r.min(w - 1)].iter().map(|&x| x as f64).sum();
        for (x, o) in out.iter_mut().enumerate() {
            *o = acc as f32;
            if x + r + 1 < w {
                acc += row[x + r + 1] as f64;
            }
            if x >= r {
                acc -= row[x - r] as f64;
            }
        }
    });
    // Vertical: independent column strips, each a running sum down the rows.
    let mut out = vec![0.0f32; w * h];
    const STRIP: usize = 64;
    let strips: Vec<(usize, Vec<f32>)> = (0..w.div_ceil(STRIP))
        .into_par_iter()
        .map(|s| {
            let x0 = s * STRIP;
            let sw = STRIP.min(w - x0);
            let mut col = vec![0.0f32; sw * h];
            let mut acc = vec![0.0f64; sw];
            for y in 0..=r.min(h - 1) {
                for (a, t) in acc.iter_mut().zip(&tmp[y * w + x0..y * w + x0 + sw]) {
                    *a += *t as f64;
                }
            }
            for y in 0..h {
                for (c, a) in col[y * sw..(y + 1) * sw].iter_mut().zip(&acc) {
                    *c = *a as f32;
                }
                if y + r + 1 < h {
                    let base = (y + r + 1) * w + x0;
                    for (a, t) in acc.iter_mut().zip(&tmp[base..base + sw]) {
                        *a += *t as f64;
                    }
                }
                if y >= r {
                    let base = (y - r) * w + x0;
                    for (a, t) in acc.iter_mut().zip(&tmp[base..base + sw]) {
                        *a -= *t as f64;
                    }
                }
            }
            (x0, col)
        })
        .collect();
    drop(tmp);
    for (x0, col) in strips {
        let sw = col.len() / h;
        for y in 0..h {
            out[y * w + x0..y * w + x0 + sw].copy_from_slice(&col[y * sw..(y + 1) * sw]);
        }
    }
    out
}

/// Edge-aware local tone: a guided filter of `lum` guided by itself,
/// weighted by `alpha` (transparent pixels and the area past the raster
/// count for nothing). Radius `r`; depends on pixels up to `2r` away.
pub fn tone_base(lum: &[f32], alpha: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let n = w * h;
    let wt: Vec<f32> = alpha.iter().map(|&a| a.clamp(0.0, 1.0)).collect();
    let sw = box_sum(&wt, w, h, r);
    let (si, sii) = {
        let wi: Vec<f32> = (0..n).map(|i| wt[i] * lum[i]).collect();
        let si = box_sum(&wi, w, h, r);
        let wii: Vec<f32> = (0..n).map(|i| wi[i] * lum[i]).collect();
        drop(wi);
        (si, box_sum(&wii, w, h, r))
    };
    // Per-window linear model base = a·I + b (in f64: the variance is a
    // small difference of large sums).
    let mut wa = vec![0.0f32; n];
    let mut wb = vec![0.0f32; n];
    wa.par_iter_mut()
        .zip(wb.par_iter_mut())
        .enumerate()
        .for_each(|(i, (oa, ob))| {
            let s = sw[i] as f64;
            if s <= 1e-9 {
                return;
            }
            let m = si[i] as f64 / s;
            let var = (sii[i] as f64 / s - m * m).max(0.0);
            let a = var / (var + SH_EPS);
            let b = (1.0 - a) * m;
            *oa = (wt[i] as f64 * a) as f32;
            *ob = (wt[i] as f64 * b) as f32;
        });
    drop((si, sii, wt));
    // Every weighted pixel's own window holds its weight, so the second
    // pass normalises by the same window sums `sw`.
    let sa = box_sum(&wa, w, h, r);
    drop(wa);
    let sb = box_sum(&wb, w, h, r);
    drop(wb);
    (0..n)
        .into_par_iter()
        .map(|i| {
            let s = sw[i] as f64;
            if s <= 1e-9 {
                return lum[i];
            }
            ((sa[i] as f64 / s) * lum[i] as f64 + sb[i] as f64 / s) as f32
        })
        .collect()
}

/// Shadows/Highlights over a dense raster. Pixels within
/// [`ShadowsHighlights::reach`] of the raster's border see the border
/// (past it counts as transparent), so callers pad by the reach.
pub fn shadows_highlights(src: &Raster, p: &ShadowsHighlights) -> Raster {
    let (w, h) = (src.width as usize, src.height as usize);
    if p.is_identity() || w == 0 || h == 0 {
        return src.clone();
    }
    // Straight gamma colour and its luma.
    let gam: Vec<[f32; 3]> = src
        .pixels
        .par_iter()
        .map(|q| {
            let s = q.to_straight();
            encode3([s[0], s[1], s[2]])
        })
        .collect();
    let lum: Vec<f32> = gam.iter().map(|&g| gamma_luma(g)).collect();
    let alpha: Vec<f32> = src.pixels.iter().map(|q| q.a).collect();
    let (rs, rh) = (sane_radius(p.shadows.radius), sane_radius(p.highlights.radius));
    let sh_on = p.shadows.amount > 0.0;
    let hi_on = p.highlights.amount > 0.0;
    let base_s = sh_on.then(|| tone_base(&lum, &alpha, w, h, rs));
    // The same radius shares one base.
    let own_h = (hi_on && !(sh_on && rh == rs)).then(|| tone_base(&lum, &alpha, w, h, rh));
    let base_h: Option<&Vec<f32>> = match (hi_on, &own_h) {
        (false, _) => None,
        (true, Some(b)) => Some(b),
        (true, None) => base_s.as_ref(),
    };
    let mut out = src.clone();
    out.pixels.par_iter_mut().enumerate().for_each(|(i, o)| {
        if o.a <= 0.0 {
            return;
        }
        let l = lum[i];
        let ws = base_s.as_ref().map_or(0.0, |b| shadow_weight(&p.shadows, b[i]));
        let wh = base_h.map_or(0.0, |b| highlight_weight(&p.highlights, b[i]));
        let l1 = lift_curve(l, ws);
        let l2 = 1.0 - lift_curve(1.0 - l1, wh);
        let m = p.midtone.clamp(-1.0, 1.0);
        let l3 = (l2 + m * (l2 - 0.5) * 2.0 * l2 * (1.0 - l2)).clamp(0.0, 1.0);
        // Colour: chroma follows the tone change (a lift by 3× scales the
        // chroma by 3^(0.5 + color)), so lifted shadows keep their colour
        // instead of greying out; untouched pixels keep theirs exactly.
        let ks = if l > 1e-4 { (l1 / l).min(8.0) } else { 1.0 };
        let kh = if l1 < 1.0 - 1e-4 {
            ((1.0 - l2) / (1.0 - l1)).min(8.0)
        } else {
            1.0
        };
        let k0 = ks * kh;
        let s = if k0 != 1.0 {
            k0.powf(0.5 + p.color.clamp(-1.0, 1.0))
        } else {
            1.0
        };
        let g = gam[i];
        let rgb = fit_chroma(l3, [(g[0] - l) * s, (g[1] - l) * s, (g[2] - l) * s]);
        let lin = decode3(rgb);
        *o = Rgba::from_straight(lin[0], lin[1], lin[2], o.a);
    });
    out
}

/// Shadows/Highlights of a layer's pixels: the result over the layer's
/// content bounds, read with the canvas edge repeated past the canvas.
pub fn shadows_highlights_layer(
    store: &TileStore,
    p: &ShadowsHighlights,
    canvas: Rect,
) -> Option<(Rect, Raster)> {
    let bounds = store.content_bounds()?;
    let pad = p.reach();
    let area = Rect::new(
        bounds.x - pad,
        bounds.y - pad,
        bounds.w + 2 * pad as u32,
        bounds.h + 2 * pad as u32,
    );
    let src = read_edge_extended(store, area, canvas);
    let full = shadows_highlights(&src, p);
    let mut out = Raster::new(bounds.w, bounds.h);
    for y in 0..bounds.h {
        let s = (y as i32 + pad) as usize * area.w as usize + pad as usize;
        let d = y as usize * bounds.w as usize;
        out.pixels[d..d + bounds.w as usize].copy_from_slice(&full.pixels[s..s + bounds.w as usize]);
    }
    Some((bounds, out))
}

// ---- Equalize --------------------------------------------------------------------

/// Levels of the Equalize histogram (8-bit levels, as Photoshop).
pub const EQ_LEVELS: usize = 256;

/// Add a straight-alpha pixel's gamma luma to a 256-level histogram with
/// weight `w` (alpha × selection coverage).
#[inline]
pub fn histogram_add(hist: &mut [f64; EQ_LEVELS], p: Rgba, w: f32) {
    if p.a <= 0.0 || w <= 0.0 {
        return;
    }
    let s = p.to_straight();
    let l = gamma_luma(encode3([s[0], s[1], s[2]]));
    let k = (l.clamp(0.0, 1.0) * (EQ_LEVELS - 1) as f32).round() as usize;
    hist[k.min(EQ_LEVELS - 1)] += w as f64;
}

/// Histogram equalisation curve: level `k` maps to
/// `(cdf(k) − cdf_min) / (total − cdf_min)`, so the darkest occupied level
/// becomes black and the brightest white. `None` when there is fewer than
/// two occupied levels (nothing to spread).
pub fn equalize_curve(hist: &[f64; EQ_LEVELS]) -> Option<[f32; EQ_LEVELS]> {
    let total: f64 = hist.iter().sum();
    let first = hist.iter().position(|&c| c > 0.0)?;
    let cmin = hist[first];
    if total - cmin <= 1e-9 {
        return None;
    }
    let mut out = [0.0f32; EQ_LEVELS];
    let mut acc = 0.0f64;
    for (o, &c) in out.iter_mut().zip(hist) {
        acc += c;
        *o = ((acc - cmin) / (total - cmin)).clamp(0.0, 1.0) as f32;
    }
    Some(out)
}

/// Equalize one premultiplied pixel: its gamma luma goes through `curve`
/// (linear between levels); the chroma scales with the luma (at most 4×),
/// so colours keep their saturation and exactly their hue.
pub fn equalize_pixel(p: Rgba, curve: &[f32; EQ_LEVELS]) -> Rgba {
    if p.a <= 0.0 {
        return p;
    }
    let s = p.to_straight();
    let g = encode3([s[0], s[1], s[2]]);
    let l = gamma_luma(g);
    let x = l.clamp(0.0, 1.0) * (EQ_LEVELS - 1) as f32;
    let i = (x as usize).min(EQ_LEVELS - 2);
    let t = x - i as f32;
    let l2 = curve[i] + (curve[i + 1] - curve[i]) * t;
    let k = (l2 / l.max(1e-4)).clamp(0.0, 4.0);
    let rgb = decode3(fit_chroma(l2, [(g[0] - l) * k, (g[1] - l) * k, (g[2] - l) * k]));
    Rgba::from_straight(rgb[0], rgb[1], rgb[2], p.a)
}

// ---- Desaturate ------------------------------------------------------------------

/// Photoshop's Desaturate (Shift+Cmd+U): Hue/Saturation at saturation
/// −100, i.e. every channel becomes the HSL lightness `(max + min) / 2` of
/// the gamma-encoded colour.
pub fn desaturate_pixel(p: Rgba) -> Rgba {
    if p.a <= 0.0 {
        return p;
    }
    let s = p.to_straight();
    let g = encode3([s[0], s[1], s[2]]);
    let v = srgb_decode((g[0].max(g[1]).max(g[2]) + g[0].min(g[1]).min(g[2])) * 0.5);
    Rgba::from_straight(v, v, v, p.a)
}

// ---- Replace Color ---------------------------------------------------------------

/// Select ▸ Color Range's matching model: straight linear RGB, largest
/// channel difference, full weight up to half the tolerance and a linear
/// ramp to zero at the tolerance (soft edges).
#[inline]
pub fn color_range_weight(rgb: [f32; 3], target: [f32; 3], tolerance: f32) -> f32 {
    let tol = tolerance.clamp(0.002, 1.0);
    let ramp = (tol * 0.5).max(1e-4);
    let d = (rgb[0] - target[0])
        .abs()
        .max((rgb[1] - target[1]).abs())
        .max((rgb[2] - target[2]).abs());
    ((tol - d) / ramp).clamp(0.0, 1.0)
}

/// Image ▸ Adjustments ▸ Replace Color.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplaceColor {
    /// The colour to match, straight linear RGB.
    pub color: [f32; 3],
    /// More colours to match (Photoshop's "add to sample" eyedropper).
    pub added: Vec<[f32; 3]>,
    /// Colours to leave alone ("subtract from sample"): they cut the mask
    /// by their own soft match.
    pub removed: Vec<[f32; 3]>,
    /// Fuzziness as a Color Range tolerance, 0..1.
    pub fuzziness: f32,
    /// Hue shift in degrees, saturation and lightness −1..1 (Hue/Saturation).
    pub hue: f32,
    pub saturation: f32,
    pub lightness: f32,
}

impl ReplaceColor {
    /// The Hue/Saturation adjustment applied where the colour matches.
    pub fn adjustment(&self) -> lumenply_doc::Adjustment {
        lumenply_doc::Adjustment::HueSaturation {
            hue: self.hue,
            saturation: self.saturation,
            lightness: self.lightness,
            colorize: false,
        }
    }

    /// How much a premultiplied pixel is selected, 0..1.
    pub fn weight(&self, p: Rgba) -> f32 {
        if p.a <= 0.0 {
            return 0.0;
        }
        let s = p.to_straight();
        let c = [s[0], s[1], s[2]];
        let tol = self.fuzziness;
        let w = self
            .added
            .iter()
            .fold(color_range_weight(c, self.color, tol), |m, &a| {
                m.max(color_range_weight(c, a, tol))
            });
        if w <= 0.0 {
            return 0.0;
        }
        let cut = self
            .removed
            .iter()
            .fold(0.0f32, |m, &r| m.max(color_range_weight(c, r, tol)));
        w * (1.0 - cut)
    }
}

// ---- Match Color -----------------------------------------------------------------

const D65: [f32; 3] = [0.950_47, 1.0, 1.088_83];

fn lab_f(t: f32) -> f32 {
    const D: f32 = 6.0 / 29.0;
    if t > D * D * D {
        t.cbrt()
    } else {
        t / (3.0 * D * D) + 4.0 / 29.0
    }
}

fn lab_finv(t: f32) -> f32 {
    const D: f32 = 6.0 / 29.0;
    if t > D {
        t * t * t
    } else {
        3.0 * D * D * (t - 4.0 / 29.0)
    }
}

/// CIE L*a*b* (D65) of straight linear sRGB.
pub fn rgb_to_lab(c: [f32; 3]) -> [f32; 3] {
    let x = 0.412_456_4 * c[0] + 0.357_576_1 * c[1] + 0.180_437_5 * c[2];
    let y = 0.212_672_9 * c[0] + 0.715_152_2 * c[1] + 0.072_175 * c[2];
    let z = 0.019_333_9 * c[0] + 0.119_192 * c[1] + 0.950_304_1 * c[2];
    let (fx, fy, fz) = (lab_f(x / D65[0]), lab_f(y / D65[1]), lab_f(z / D65[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Straight linear sRGB of CIE L*a*b* (D65); may leave `[0, 1]`.
pub fn lab_to_rgb(lab: [f32; 3]) -> [f32; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let (x, y, z) = (
        lab_finv(fx) * D65[0],
        lab_finv(fy) * D65[1],
        lab_finv(fz) * D65[2],
    );
    [
        3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z,
        -0.969_266 * x + 1.876_010_8 * y + 0.041_556 * z,
        0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z,
    ]
}

/// Mean and standard deviation of L*, a* and b*.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabStats {
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

/// Running sums for [`LabStats`], weighted (alpha × coverage).
#[derive(Clone, Copy, Debug, Default)]
pub struct LabAccum {
    w: f64,
    s: [f64; 3],
    ss: [f64; 3],
}

impl LabAccum {
    pub fn add(&mut self, p: Rgba, weight: f32) {
        let w = (p.a * weight) as f64;
        if w <= 0.0 {
            return;
        }
        let st = p.to_straight();
        let lab = rgb_to_lab([st[0], st[1], st[2]]);
        self.w += w;
        for ((s, ss), v) in self.s.iter_mut().zip(self.ss.iter_mut()).zip(lab) {
            let v = v as f64;
            *s += w * v;
            *ss += w * v * v;
        }
    }

    pub fn merge(mut self, o: LabAccum) -> LabAccum {
        self.w += o.w;
        for c in 0..3 {
            self.s[c] += o.s[c];
            self.ss[c] += o.ss[c];
        }
        self
    }

    pub fn stats(&self) -> Option<LabStats> {
        if self.w <= 1e-9 {
            return None;
        }
        let mut mean = [0.0f32; 3];
        let mut std = [0.0f32; 3];
        for c in 0..3 {
            let m = self.s[c] / self.w;
            mean[c] = m as f32;
            std[c] = (self.ss[c] / self.w - m * m).max(0.0).sqrt() as f32;
        }
        Some(LabStats { mean, std })
    }
}

/// Lab statistics of a store's pixels (all of them, weighted by alpha).
pub fn store_lab_stats(store: &TileStore, weight: impl Fn(i32, i32) -> f32 + Sync) -> Option<LabStats> {
    let coords: Vec<TileCoord> = store.coords().collect();
    coords
        .par_iter()
        .map(|&c| {
            let mut acc = LabAccum::default();
            if let Some(t) = store.tile(c) {
                let px = t.pixels();
                let (ox, oy) = c.origin();
                for (i, p) in px.iter().enumerate() {
                    if p.a > 0.0 {
                        let (x, y) = (ox + (i % TILE_SIZE) as i32, oy + (i / TILE_SIZE) as i32);
                        acc.add(*p, weight(x, y));
                    }
                }
            }
            acc
        })
        .reduce(LabAccum::default, LabAccum::merge)
        .stats()
}

/// Lab statistics of a dense raster (weighted by alpha).
pub fn raster_lab_stats(r: &Raster) -> Option<LabStats> {
    r.pixels
        .par_chunks(4096)
        .map(|ch| {
            let mut acc = LabAccum::default();
            for p in ch {
                acc.add(*p, 1.0);
            }
            acc
        })
        .reduce(LabAccum::default, LabAccum::merge)
        .stats()
}

/// Image ▸ Adjustments ▸ Match Color: move the target's Lab statistics
/// onto the source's (Reinhard et al. 2001, in L*a*b*).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchColor {
    /// The source's statistics; `None` matches the target to itself (only
    /// the sliders and Neutralize act).
    pub source: Option<LabStats>,
    /// Brightness of the result, 0.01..2 (Photoshop's Luminance 1–200, 100 = as matched).
    pub luminance: f32,
    /// Colourfulness, 0..2 (Color Intensity 0–200).
    pub intensity: f32,
    /// Blend back towards the original, 0..1.
    pub fade: f32,
    /// Remove the colour cast: the result's mean a*, b* become neutral.
    pub neutralize: bool,
}

impl Default for MatchColor {
    fn default() -> Self {
        MatchColor {
            source: None,
            luminance: 1.0,
            intensity: 1.0,
            fade: 0.0,
            neutralize: false,
        }
    }
}

impl MatchColor {
    /// Match one premultiplied pixel given the target's statistics.
    pub fn apply(&self, target: &LabStats, p: Rgba) -> Rgba {
        if p.a <= 0.0 {
            return p;
        }
        let st = p.to_straight();
        let orig = [st[0], st[1], st[2]];
        let lab = rgb_to_lab(orig);
        let src = self.source.unwrap_or(*target);
        // A floor on the target's spread keeps a flat target from blowing up.
        const MIN_STD: [f32; 3] = [1.0, 1.0, 1.0];
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let mut m = src.mean[c];
            if self.neutralize && c > 0 {
                m = 0.0;
            }
            let k = src.std[c] / target.std[c].max(MIN_STD[c]);
            out[c] = (lab[c] - target.mean[c]) * k + m;
        }
        out[0] = (out[0] * self.luminance.clamp(0.01, 2.0)).clamp(0.0, 100.0);
        let ci = self.intensity.clamp(0.0, 2.0);
        out[1] *= ci;
        out[2] *= ci;
        let rgb = lab_to_rgb(out);
        let f = self.fade.clamp(0.0, 1.0);
        let mix = |a: f32, b: f32| (a.clamp(0.0, 1.0) * (1.0 - f) + b * f).max(0.0);
        Rgba::from_straight(
            mix(rgb[0], orig[0]),
            mix(rgb[1], orig[1]),
            mix(rgb[2], orig[2]),
            p.a,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(g: f32) -> Rgba {
        let v = srgb_decode(g);
        Rgba::new(v, v, v, 1.0)
    }

    fn gamma_of(p: Rgba) -> [f32; 3] {
        let s = p.to_straight();
        encode3([s[0], s[1], s[2]])
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    /// A deterministic, textured test image with a hard edge.
    fn scene(w: u32, h: u32) -> Raster {
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let n = ((x * 7919 + y * 104_729) % 97) as f32 / 97.0;
                let base = if x * 3 < w * 2 { 0.12 } else { 0.85 };
                let g = (base + (n - 0.5) * 0.08 + y as f32 / h as f32 * 0.05).clamp(0.0, 1.0);
                let c = [
                    srgb_decode(g),
                    srgb_decode(g * 0.9),
                    srgb_decode((g * 1.1).min(1.0)),
                ];
                r.set(x, y, Rgba::from_straight(c[0], c[1], c[2], 1.0));
            }
        }
        r
    }

    #[test]
    fn shadows_highlights_amount_zero_is_identity() {
        let src = scene(90, 70);
        let p = ShadowsHighlights {
            shadows: ToneZone {
                amount: 0.0,
                tone: 0.5,
                radius: 10.0,
            },
            color: 0.5,
            ..Default::default()
        };
        assert!(p.is_identity());
        assert_eq!(shadows_highlights(&src, &p), src);
        assert_eq!(lift_curve(0.37, 0.0), 0.37);
    }

    #[test]
    fn shadows_highlights_tiled_equals_whole() {
        let src = scene(300, 200);
        let p = ShadowsHighlights {
            shadows: ToneZone {
                amount: 0.8,
                tone: 0.6,
                radius: 9.0,
            },
            highlights: ToneZone {
                amount: 0.5,
                tone: 0.4,
                radius: 6.0,
            },
            color: 0.3,
            midtone: 0.2,
        };
        let whole = shadows_highlights(&src, &p);
        let pad = p.reach();
        assert_eq!(pad, 18);
        let t = 64i32;
        let mut worst = 0.0f32;
        for ty in (0..200).step_by(t as usize) {
            for tx in (0..300).step_by(t as usize) {
                // The tile plus its reach, clipped to the raster.
                let x0 = (tx - pad).max(0);
                let y0 = (ty - pad).max(0);
                let x1 = (tx + t + pad).min(300);
                let y1 = (ty + t + pad).min(200);
                let mut sub = Raster::new((x1 - x0) as u32, (y1 - y0) as u32);
                for y in y0..y1 {
                    for x in x0..x1 {
                        sub.set((x - x0) as u32, (y - y0) as u32, src.get(x as u32, y as u32));
                    }
                }
                let out = shadows_highlights(&sub, &p);
                for y in ty..(ty + t).min(200) {
                    for x in tx..(tx + t).min(300) {
                        let a = out.get((x - x0) as u32, (y - y0) as u32);
                        let b = whole.get(x as u32, y as u32);
                        for (u, v) in [(a.r, b.r), (a.g, b.g), (a.b, b.b), (a.a, b.a)] {
                            worst = worst.max((u - v).abs());
                        }
                    }
                }
            }
        }
        assert!(worst <= 1e-4, "tiles differ from the whole by {worst}");
    }

    #[test]
    fn shadows_lift_a_dark_ramp_by_the_expected_amounts() {
        // A horizontal gamma ramp 0..0.3: inside a linear ramp the guided
        // base equals the pixel's own tone, so each pixel follows the
        // curve exactly.
        let w = 81u32;
        let mut src = Raster::new(w, 9);
        for y in 0..9 {
            for x in 0..w {
                src.set(x, y, grey(x as f32 * 0.005));
            }
        }
        let p = ShadowsHighlights {
            shadows: ToneZone {
                amount: 1.0,
                tone: 0.5,
                radius: 2.0,
            },
            highlights: ToneZone {
                amount: 0.0,
                tone: 0.5,
                radius: 2.0,
            },
            color: 0.0,
            midtone: 0.0,
        };
        let out = shadows_highlights(&src, &p);
        // Expected gamma values (from the curve formula, computed offline):
        // tone 0.05 -> 0.18929, 0.10 -> 0.29131, 0.20 -> 0.40541, 0.30 -> 0.45192.
        for (x, want) in [(10u32, 0.18929f32), (20, 0.29131), (40, 0.40541), (60, 0.45192)] {
            let g = gamma_of(out.get(x, 4));
            assert!(close(g[0], want, 2e-3), "x={x}: got {} want {want}", g[0]);
            assert!(
                close(g[1], g[0], 1e-5) && close(g[2], g[0], 1e-5),
                "grey stays grey"
            );
        }
    }

    #[test]
    fn shadows_leave_bright_areas_and_highlights_leave_dark_ones() {
        let mut src = Raster::new(40, 40);
        for y in 0..40 {
            for x in 0..40 {
                src.set(x, y, if x < 20 { grey(0.1) } else { grey(0.9) });
            }
        }
        let p = ShadowsHighlights {
            shadows: ToneZone {
                amount: 1.0,
                tone: 0.5,
                radius: 4.0,
            },
            ..Default::default()
        };
        let out = shadows_highlights(&src, &p);
        // Flat 0.1 (far from the edge) lifts by the full curve; 0.9 stays.
        let dark = gamma_of(out.get(5, 20))[0];
        let bright = gamma_of(out.get(35, 20))[0];
        assert!(close(dark, 0.29131, 2e-3), "dark {dark}");
        assert!(close(bright, 0.9, 1e-4), "bright {bright}");
        // Right at the edge the edge-aware base keeps the dark side lifted
        // (no dark halo) and the bright side untouched (no light halo).
        let at_edge_dark = gamma_of(out.get(19, 20))[0];
        let at_edge_bright = gamma_of(out.get(20, 20))[0];
        assert!(at_edge_dark > 0.26, "dark side next to the edge {at_edge_dark}");
        assert!(
            at_edge_bright > 0.89,
            "bright side next to the edge {at_edge_bright}"
        );
        // Highlights only: 0.9 comes down, 0.1 stays.
        let p = ShadowsHighlights {
            shadows: ToneZone {
                amount: 0.0,
                ..p.shadows
            },
            highlights: ToneZone {
                amount: 1.0,
                tone: 0.5,
                radius: 4.0,
            },
            ..p
        };
        let out = shadows_highlights(&src, &p);
        assert!(close(gamma_of(out.get(35, 20))[0], 1.0 - 0.29131, 2e-3));
        assert!(close(gamma_of(out.get(5, 20))[0], 0.1, 1e-4));
    }

    #[test]
    fn lifted_shadows_keep_their_colour() {
        // Flat gamma (0.15, 0.10, 0.05): luma 0.10702 lifts to 0.30246 and
        // the chroma scales by (0.30246 / 0.10702)^(0.5 + color).
        let c = decode3([0.15, 0.1, 0.05]);
        let src = Raster::filled(24, 24, Rgba::from_straight(c[0], c[1], c[2], 1.0));
        for (color, want) in [
            (0.0f32, [0.37471f32, 0.29065, 0.2066]),
            (0.2, [0.3914, 0.28793, 0.18446]),
        ] {
            let p = ShadowsHighlights {
                shadows: ToneZone {
                    amount: 1.0,
                    tone: 0.5,
                    radius: 3.0,
                },
                color,
                ..Default::default()
            };
            let g = gamma_of(shadows_highlights(&src, &p).get(12, 12));
            for i in 0..3 {
                assert!(close(g[i], want[i], 1e-3), "color {color}: {g:?} want {want:?}");
            }
        }
    }

    #[test]
    fn equalize_spreads_levels_by_their_cumulative_count() {
        let mut hist = [0.0f64; EQ_LEVELS];
        for g in [0.2f32, 0.2, 0.4, 0.8] {
            histogram_add(&mut hist, grey(g), 1.0);
        }
        let curve = equalize_curve(&hist).unwrap();
        // cdf: 0.2 -> 2, 0.4 -> 3, 0.8 -> 4; min 2, total 4.
        assert_eq!(curve[51], 0.0);
        assert_eq!(curve[102], 0.5);
        assert_eq!(curve[204], 1.0);
        let px = |g: f32| gamma_of(equalize_pixel(grey(g), &curve))[0];
        assert!(close(px(0.2), 0.0, 1e-4));
        assert!(close(px(0.4), 0.5, 1e-4));
        assert!(close(px(0.8), 1.0, 1e-4));
        // A single level has nothing to spread.
        let mut flat = [0.0f64; EQ_LEVELS];
        histogram_add(&mut flat, grey(0.5), 3.0);
        assert!(equalize_curve(&flat).is_none());
    }

    #[test]
    fn equalize_keeps_hue() {
        let mut hist = [0.0f64; EQ_LEVELS];
        for g in [0.1f32, 0.3, 0.5, 0.9] {
            histogram_add(&mut hist, grey(g), 1.0);
        }
        let curve = equalize_curve(&hist).unwrap();
        // Gamma (0.4, 0.3, 0.2): luma 0.31404 -> level 80.08, between 0.3
        // (level 77, cdf 2 -> 1/3) and 0.5 (level 128): the curve is 1/3.
        let c = [srgb_decode(0.4), srgb_decode(0.3), srgb_decode(0.2)];
        let out = gamma_of(equalize_pixel(Rgba::from_straight(c[0], c[1], c[2], 1.0), &curve));
        let l = gamma_luma(out);
        let want_l = 1.0 / 3.0;
        assert_eq!((curve[80], curve[81]), (1.0 / 3.0, 1.0 / 3.0));
        assert!(close(l, want_l, 1e-3), "luma {l} want {want_l}");
        // The chroma scales with the luma: by (1/3) / 0.31404 = 1.0614,
        // so the hue (the chroma's direction) is exactly kept.
        let lin = 0.2126 * 0.4 + 0.7152 * 0.3 + 0.0722 * 0.2;
        let k = (1.0 / 3.0) / lin;
        assert!(close(k, 1.0614, 1e-3));
        for (o, i) in out.iter().zip([0.4f32, 0.3, 0.2]) {
            assert!(close(o - l, (i - lin) * k, 1e-3), "chroma {o} vs {i}");
        }
    }

    #[test]
    fn desaturate_is_hsl_lightness_in_gamma() {
        // Pure red -> 0.5; (0.8, 0.4, 0.2) -> (0.8 + 0.2) / 2 = 0.5;
        // (0.6, 0.6, 0.2) -> 0.4.
        for (c, want) in [
            ([1.0f32, 0.0, 0.0], 0.5f32),
            ([0.8, 0.4, 0.2], 0.5),
            ([0.6, 0.6, 0.2], 0.4),
        ] {
            let lin = decode3(c);
            let p = Rgba::from_straight(lin[0], lin[1], lin[2], 0.5);
            let out = desaturate_pixel(p);
            assert!(close(out.a, 0.5, 1e-6));
            let g = gamma_of(out);
            for v in g {
                assert!(close(v, want, 1e-4), "{c:?}: {v} want {want}");
            }
            // The same as Hue/Saturation at −100.
            let hs = lumenply_doc::Adjustment::HueSaturation {
                hue: 0.0,
                saturation: -1.0,
                lightness: 0.0,
                colorize: false,
            }
            .compile()
            .apply(lin);
            assert!(close(srgb_encode(hs[0]), want, 2e-3));
        }
    }

    #[test]
    fn replace_color_weights_like_color_range() {
        let mut rc = ReplaceColor {
            color: [0.8, 0.1, 0.1],
            added: Vec::new(),
            removed: Vec::new(),
            fuzziness: 0.2,
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0,
        };
        let px = |c: [f32; 3]| Rgba::from_straight(c[0], c[1], c[2], 1.0);
        assert_eq!(rc.weight(px([0.8, 0.1, 0.1])), 1.0);
        assert_eq!(rc.weight(px([0.88, 0.1, 0.1])), 1.0); // within half the tolerance
        assert!(close(rc.weight(px([0.95, 0.1, 0.1])), 0.5, 1e-5)); // halfway down the ramp
        assert_eq!(rc.weight(px([0.8, 0.5, 0.1])), 0.0);
        assert_eq!(rc.weight(Rgba::TRANSPARENT), 0.0);
        // Added samples widen the match; removed ones cut it by their own
        // soft weight (0.15 from the removed colour: half the ramp).
        rc.added.push([0.8, 0.5, 0.1]);
        assert_eq!(rc.weight(px([0.8, 0.5, 0.1])), 1.0);
        rc.removed.push([0.95, 0.25, 0.1]);
        assert!(close(rc.weight(px([0.8, 0.1, 0.1])), 0.5, 1e-5));
        assert_eq!(rc.weight(px([0.95, 0.25, 0.1])), 0.0);
        // Hue +120 turns gamma red into gamma green.
        let red = decode3([1.0, 0.0, 0.0]);
        let out = rc.adjustment().compile().apply(red);
        let g = encode3(out);
        assert!(
            close(g[0], 0.0, 2e-3) && close(g[1], 1.0, 2e-3) && close(g[2], 0.0, 2e-3),
            "{g:?}"
        );
    }

    #[test]
    fn lab_round_trips_and_hits_reference_values() {
        // sRGB white is L 100, a 0, b 0; linear 0.2140 grey (sRGB 0.5) is L 53.39.
        let w = rgb_to_lab([1.0, 1.0, 1.0]);
        assert!(close(w[0], 100.0, 0.01) && close(w[1], 0.0, 0.02) && close(w[2], 0.0, 0.02));
        let g = rgb_to_lab([srgb_decode(0.5); 3]);
        assert!(close(g[0], 53.39, 0.02), "{g:?}");
        // Pure red: L 53.24, a 80.09, b 67.20.
        let r = rgb_to_lab([1.0, 0.0, 0.0]);
        assert!(
            close(r[0], 53.24, 0.05) && close(r[1], 80.09, 0.1) && close(r[2], 67.20, 0.1),
            "{r:?}"
        );
        let c = [0.3, 0.6, 0.1];
        let back = lab_to_rgb(rgb_to_lab(c));
        for i in 0..3 {
            assert!(close(back[i], c[i], 1e-4));
        }
    }

    #[test]
    fn match_color_moves_mean_and_spread_onto_the_source() {
        // Two neutral target pixels at L* 40 and 60 (mean 50, std 10);
        // source mean 50, std 20 -> the results land at L* 30 and 70.
        let px = |l: f32| {
            let c = lab_to_rgb([l, 0.0, 0.0]);
            Rgba::from_straight(c[0], c[1], c[2], 1.0)
        };
        let mut acc = LabAccum::default();
        acc.add(px(40.0), 1.0);
        acc.add(px(60.0), 1.0);
        let target = acc.stats().unwrap();
        assert!(close(target.mean[0], 50.0, 0.01) && close(target.std[0], 10.0, 0.01));
        let m = MatchColor {
            source: Some(LabStats {
                mean: [50.0, 10.0, -5.0],
                std: [20.0, 4.0, 4.0],
            }),
            ..Default::default()
        };
        let lab = |p: Rgba| {
            let s = p.to_straight();
            rgb_to_lab([s[0], s[1], s[2]])
        };
        let a = lab(m.apply(&target, px(40.0)));
        let b = lab(m.apply(&target, px(60.0)));
        assert!(close(a[0], 30.0, 0.05) && close(b[0], 70.0, 0.05), "{a:?} {b:?}");
        // A flat target channel (a*, std 0) only moves to the source mean.
        assert!(close(a[1], 10.0, 0.1) && close(a[2], -5.0, 0.1), "{a:?}");
        // Neutralize puts the mean cast back at zero; fade 1 is the original.
        let n = MatchColor {
            neutralize: true,
            ..m
        };
        let a = lab(n.apply(&target, px(40.0)));
        assert!(close(a[1], 0.0, 0.1) && close(a[2], 0.0, 0.1), "{a:?}");
        let f = MatchColor { fade: 1.0, ..m };
        let o = f.apply(&target, px(40.0));
        assert!(close(lab(o)[0], 40.0, 0.01));
        // No source: matched to itself, the identity.
        let id = MatchColor::default();
        assert!(close(lab(id.apply(&target, px(60.0)))[0], 60.0, 0.01));
    }
}
