//! The Gradient tool's pixel work: a multi-stop gradient drawn along a
//! dragged line onto a pixel layer or a layer mask.
//!
//! Geometry follows Photoshop. With `S` the press point, `E` the release
//! point and `L = |E − S|`, a pixel centre `P` sits at
//!
//! - **Linear**: `(P − S)·û / L` (û the unit drag direction)
//! - **Radial**: `|P − S| / L`
//! - **Angle**: the counter-clockwise (on screen) angle from û, over 360°
//! - **Reflected**: `|(P − S)·û| / L`
//! - **Diamond**: `(|along| + |across|) / L`, so `E` is one corner
//!
//! clamped to 0..1 (and flipped by Reverse). Colours interpolate on
//! gamma-encoded values like every gradient in the document (ADR 0008).
//! Dither adds ±½ of an 8-bit step of position-seeded noise to the
//! encoded colour before it is decoded, so an 8-bit export rounds up or
//! down in the right proportion instead of banding; the noise depends on
//! the pixel position only, so the result is deterministic.

use std::sync::Arc;

use lumenply_doc::adjust::srgb_decode;
use lumenply_doc::{BlendMode, Gradient, GradientStyle, Mask, Selection};
use lumenply_tiles::{Rect, Rgba, Tile, TileCoord, TileStore, TILE_SIZE};
use rayon::prelude::*;

use crate::blend::{blend_pixel_at, dissolve_weight};
use crate::blend_channel;

/// Everything a gradient drag paints with.
#[derive(Clone, Debug, PartialEq)]
pub struct GradientPaint {
    pub gradient: Gradient,
    pub style: GradientStyle,
    /// Press and release points, canvas pixels (continuous coordinates:
    /// pixel `(x, y)` has its centre at `(x + 0.5, y + 0.5)`).
    pub start: (f32, f32),
    pub end: (f32, f32),
    pub reverse: bool,
    /// Use the stops' opacity (Photoshop's "Transparency"); off paints
    /// every stop opaque.
    pub transparency: bool,
    /// Ordered noise against banding in 8-bit exports.
    pub dither: bool,
    /// 0..1.
    pub opacity: f32,
    pub blend: BlendMode,
}

impl GradientPaint {
    /// A Normal, opaque, undithered gradient from `start` to `end`.
    pub fn new(gradient: Gradient, style: GradientStyle, start: (f32, f32), end: (f32, f32)) -> Self {
        GradientPaint {
            gradient,
            style,
            start,
            end,
            reverse: false,
            transparency: true,
            dither: false,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }
    }

    /// Length of the dragged line in pixels.
    pub fn length(&self) -> f32 {
        let (dx, dy) = (self.end.0 - self.start.0, self.end.1 - self.start.1);
        let l = (dx * dx + dy * dy).sqrt();
        if l.is_finite() {
            l
        } else {
            0.0
        }
    }

    /// Where the continuous point `(x, y)` falls along the gradient, 0..1
    /// (Reverse applied).
    pub fn position(&self, x: f32, y: f32) -> f32 {
        let t = ramp_position(self.style, self.start, self.end, x, y);
        if self.reverse {
            1.0 - t
        } else {
            t
        }
    }
}

/// Where `(x, y)` falls on a `style` gradient dragged from `start` to
/// `end`, 0..1 (see the module docs). A zero-length drag reads 0.
pub fn ramp_position(style: GradientStyle, start: (f32, f32), end: (f32, f32), x: f32, y: f32) -> f32 {
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let len = (dx * dx + dy * dy).sqrt();
    if !len.is_finite() || len <= 1e-6 {
        return 0.0;
    }
    let (ux, uy) = (dx / len, dy / len);
    let (px, py) = (x - start.0, y - start.1);
    let along = px * ux + py * uy;
    // Positive to the right of the direction on screen (y points down).
    let across = -px * uy + py * ux;
    let t = match style {
        GradientStyle::Linear => along / len,
        GradientStyle::Reflected => along.abs() / len,
        GradientStyle::Radial => (px * px + py * py).sqrt() / len,
        GradientStyle::Diamond => (along.abs() + across.abs()) / len,
        GradientStyle::Angle => {
            let a = (-across).atan2(along);
            a.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
        }
    };
    if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Interleaved gradient noise (Jimenez 2014): a cheap, blue-ish,
/// position-only pattern in 0..1.
#[inline]
pub fn dither_noise(x: i32, y: i32) -> f32 {
    let f = 0.067_110_56 * x as f64 + 0.005_837_15 * y as f64;
    (52.982_918_9 * f.fract()).fract() as f32
}

/// Entries in the gamma colour table.
const TABLE: usize = 1024;
/// Entries in the sRGB decode table.
const DECODE: usize = 4096;

/// A gradient ready to evaluate per pixel.
struct Prepared<'a> {
    paint: &'a GradientPaint,
    /// Gamma-encoded straight colour + opacity, `TABLE` samples over 0..1.
    table: Vec<[f32; 4]>,
    /// `srgb_decode` sampled at `DECODE + 1` points (linear interpolation
    /// between them is exact to ~1e-7).
    decode: Vec<f32>,
}

impl<'a> Prepared<'a> {
    fn new(paint: &'a GradientPaint) -> Self {
        let mut table = paint.gradient.sample_gamma(TABLE);
        if !paint.transparency {
            for e in &mut table {
                e[3] = 1.0;
            }
        }
        let decode = (0..=DECODE)
            .map(|i| srgb_decode(i as f32 / DECODE as f32))
            .collect();
        Prepared { paint, table, decode }
    }

    #[inline]
    fn decode(&self, v: f32) -> f32 {
        let p = v.clamp(0.0, 1.0) * DECODE as f32;
        let i = (p as usize).min(DECODE - 1);
        let k = p - i as f32;
        self.decode[i] + (self.decode[i + 1] - self.decode[i]) * k
    }

    /// Gamma-encoded straight colour + opacity at pixel `(x, y)`, dithered
    /// when asked.
    #[inline]
    fn gamma_at(&self, x: i32, y: i32) -> [f32; 4] {
        let t = self.paint.position(x as f32 + 0.5, y as f32 + 0.5);
        let p = t * (TABLE - 1) as f32;
        let i = (p as usize).min(TABLE - 2);
        let k = p - i as f32;
        let (a, b) = (self.table[i], self.table[i + 1]);
        let mut c: [f32; 4] = std::array::from_fn(|j| a[j] + (b[j] - a[j]) * k);
        if self.paint.dither {
            let n = (dither_noise(x, y) - 0.5) / 255.0;
            for v in &mut c {
                *v = (*v + n).clamp(0.0, 1.0);
            }
        }
        c
    }

    /// Premultiplied linear colour at pixel `(x, y)`.
    #[inline]
    fn color_at(&self, x: i32, y: i32) -> Rgba {
        let [r, g, b, a] = self.gamma_at(x, y);
        Rgba::from_straight(self.decode(r), self.decode(g), self.decode(b), a)
    }
}

/// Selection coverage for one tile: its pixels, or a constant.
enum Coverage<'a> {
    All,
    Tile(std::borrow::Cow<'a, [Rgba]>),
    Const(f32),
}

impl Coverage<'_> {
    fn of(sel: Option<&Selection>, c: TileCoord) -> Coverage<'_> {
        match sel {
            None => Coverage::All,
            Some(s) => match s.coverage.tiles.tile(c) {
                Some(t) => Coverage::Tile(t.pixels()),
                None => Coverage::Const(s.coverage.default),
            },
        }
    }

    fn is_none(&self) -> bool {
        matches!(self, Coverage::Const(v) if *v <= 0.0)
    }

    #[inline]
    fn at(&self, i: usize) -> f32 {
        match self {
            Coverage::All => 1.0,
            Coverage::Tile(p) => p[i].a,
            Coverage::Const(v) => *v,
        }
    }
}

/// Run `f(tile, x, y, index, coverage)` over every pixel of `area` with
/// selection coverage above zero, tile by tile in parallel. Tiles missing
/// from `store` start as `blank`; the edited tiles replace the old ones.
fn for_each_pixel(
    store: &mut TileStore,
    blank: Rgba,
    area: Rect,
    sel: Option<&Selection>,
    f: impl Fn(&mut [Rgba], i32, i32, usize, f32) + Sync,
) {
    let done: Vec<(TileCoord, Tile)> = area
        .tiles()
        .into_par_iter()
        .filter_map(|c| {
            let sub = area.intersect(&c.rect());
            let cov = Coverage::of(sel, c);
            if sub.is_empty() || cov.is_none() {
                return None;
            }
            let mut t = store.tile(c).cloned().unwrap_or_else(|| Tile::filled(blank));
            let (ox, oy) = c.origin();
            let px = t.pixels_mut();
            for y in sub.y..sub.bottom() {
                for x in sub.x..sub.right() {
                    let i = (y - oy) as usize * TILE_SIZE + (x - ox) as usize;
                    let k = cov.at(i);
                    if k > 0.0 {
                        f(px, x, y, i, k);
                    }
                }
            }
            Some((c, t))
        })
        .collect();
    for (c, t) in done {
        store.insert(c, Arc::new(t));
    }
}

/// Draw `paint` onto premultiplied pixels over `area`, blended by the
/// selection's coverage (when given) times the paint's opacity.
pub fn paint_pixels(store: &mut TileStore, area: Rect, sel: Option<&Selection>, paint: &GradientPaint) {
    let prep = Prepared::new(paint);
    let opacity = paint.opacity.clamp(0.0, 1.0);
    for_each_pixel(store, Rgba::TRANSPARENT, area, sel, |px, x, y, i, k| {
        px[i] = blend_pixel_at(px[i], prep.color_at(x, y), paint.blend, opacity * k, x, y);
    });
    store.prune_blank();
}

/// Draw `paint` onto a layer mask: each pixel moves toward the gradient's
/// grey value — Rec. 709 luma of the gamma-encoded colour, so a black →
/// white ramp hides linearly across its length, as in Photoshop — by the
/// stop opacity × paint opacity × selection coverage. Blend modes apply
/// to the grey values.
pub fn paint_mask(mask: &mut Mask, area: Rect, sel: Option<&Selection>, paint: &GradientPaint) {
    let prep = Prepared::new(paint);
    let opacity = paint.opacity.clamp(0.0, 1.0);
    let d = mask.default;
    for_each_pixel(
        &mut mask.tiles,
        Rgba::new(d, d, d, d),
        area,
        sel,
        |px, x, y, i, k| {
            let [r, g, b, a] = prep.gamma_at(x, y);
            let grey = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0);
            let m = px[i].a;
            let target = blend_channel(paint.blend, m, grey);
            let mut w = a * opacity * k;
            if paint.blend == BlendMode::Dissolve {
                w = dissolve_weight(w, x, y);
            }
            let v = (m + (target - m) * w).clamp(0.0, 1.0);
            px[i] = Rgba::new(v, v, v, v);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dither_noise_is_uniform_and_repeatable() {
        let n: Vec<f32> = (0..64).map(|y| dither_noise(7, y)).collect();
        assert!(n.iter().all(|v| (0.0..1.0).contains(v)));
        assert_eq!(n[5], dither_noise(7, 5));
        let mean = n.iter().sum::<f32>() / 64.0;
        assert!((mean - 0.5).abs() < 0.05, "{mean}");
    }

    #[test]
    fn the_decode_table_matches_the_curve() {
        let p = GradientPaint::new(Gradient::default(), GradientStyle::Linear, (0.0, 0.0), (1.0, 0.0));
        let prep = Prepared::new(&p);
        for v in [0.0, 0.02, 0.04045, 0.3, 0.5, 0.731, 1.0] {
            assert!((prep.decode(v) - srgb_decode(v)).abs() < 1e-6, "{v}");
        }
    }
}
