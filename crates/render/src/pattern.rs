//! Pattern helpers for the compositor and the built-in pattern set.
//!
//! Pattern fills render through [`crate::fill`] like the other fills (the
//! sampler lives in `lumenply_doc::pattern`); the Pattern Overlay effect is
//! painted by the compositor beside the colour and gradient overlays.

use lumenply_doc::adjust::srgb_decode;
use lumenply_doc::Pattern;
use lumenply_tiles::{Raster, Rgba};

/// Straight colour and alpha of a premultiplied pixel.
#[inline]
pub fn straight(p: Rgba) -> ([f32; 3], f32) {
    if p.a <= 0.0 {
        return ([0.0; 3], 0.0);
    }
    ([p.r / p.a, p.g / p.a, p.b / p.a], p.a)
}

/// Ids of the built-in patterns start with this, so documents that use one
/// find it again in any build.
pub const BUILTIN_PREFIX: &str = "lumenply-builtin-";

fn srgb(r: f32, g: f32, b: f32) -> Rgba {
    Rgba::new(srgb_decode(r), srgb_decode(g), srgb_decode(b), 1.0)
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba::new(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a + (b.a - a.a) * t,
    )
}

fn generate(w: u32, h: u32, f: impl Fn(u32, u32) -> Rgba) -> Raster {
    let mut r = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            r.set(x, y, f(x, y));
        }
    }
    r
}

/// Deterministic hash noise in 0..1.
fn hash2(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x27d4_eb2d) ^ y.wrapping_mul(0x1656_67b1) ^ seed.wrapping_mul(0x9e37_79b9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 65535.0
}

/// Smooth periodic value noise (period `p` cells across `size` pixels).
fn tile_noise(x: u32, y: u32, size: u32, cells: u32, seed: u32) -> f32 {
    let fx = x as f32 * cells as f32 / size as f32;
    let fy = y as f32 * cells as f32 / size as f32;
    let (x0, y0) = (fx.floor() as u32, fy.floor() as u32);
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let at = |i: u32, j: u32| hash2(i % cells, j % cells, seed);
    let a = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * s(tx);
    let b = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * s(tx);
    a + (b - a) * s(ty)
}

/// The patterns every install has, so the picker is never empty. All are
/// seamless (their edges continue across the repeat).
pub fn builtin_patterns() -> Vec<Pattern> {
    let id = |k: &str| format!("{BUILTIN_PREFIX}{k}");
    let light = srgb(0.93, 0.93, 0.93);
    let dark = srgb(0.62, 0.62, 0.62);
    let ink = srgb(0.16, 0.18, 0.22);
    let paper = srgb(0.97, 0.96, 0.92);
    vec![
        Pattern::new(
            id("checker"),
            "Checkerboard",
            generate(16, 16, |x, y| if (x / 8 + y / 8) % 2 == 0 { light } else { dark }),
        ),
        Pattern::new(
            id("stripes"),
            "Diagonal Stripes",
            generate(16, 16, |x, y| if (x + y) % 16 < 6 { ink } else { paper }),
        ),
        Pattern::new(
            id("dots"),
            "Polka Dots",
            generate(24, 24, |x, y| {
                // Anti-aliased dot of radius 6 in the cell centre.
                let (dx, dy) = (x as f32 + 0.5 - 12.0, y as f32 + 0.5 - 12.0);
                let d = (dx * dx + dy * dy).sqrt();
                mix(ink, paper, (d - 5.5).clamp(0.0, 1.0))
            }),
        ),
        Pattern::new(
            id("grid"),
            "Grid",
            generate(16, 16, |x, y| {
                if x == 0 || y == 0 {
                    srgb(0.55, 0.65, 0.8)
                } else {
                    paper
                }
            }),
        ),
        Pattern::new(
            id("noise"),
            "Noise",
            generate(64, 64, |x, y| {
                let v = 0.35 + 0.4 * hash2(x, y, 7);
                srgb(v, v, v)
            }),
        ),
        Pattern::new(
            id("wood"),
            "Wood",
            generate(128, 128, |x, y| {
                // Rings along y bent by low-frequency noise, plus fine grain.
                let warp = tile_noise(x, y, 128, 4, 3) * 3.0;
                let ring = ((y as f32 / 128.0 * 8.0 + warp) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                let grain = hash2(x / 3, y, 11) * 0.08;
                let t = (ring * 0.7 + grain).clamp(0.0, 1.0);
                mix(srgb(0.55, 0.36, 0.2), srgb(0.78, 0.58, 0.36), t)
            }),
        ),
        Pattern::new(
            id("bricks"),
            "Bricks",
            generate(32, 16, |x, y| {
                let row = y / 8;
                let xx = (x + if row % 2 == 1 { 8 } else { 0 }) % 32;
                let mortar = y % 8 == 7 || xx % 16 == 15;
                if mortar {
                    srgb(0.82, 0.8, 0.76)
                } else {
                    let v = 0.85 + 0.15 * hash2(xx / 16 + row * 7, row, 5);
                    srgb(0.66 * v, 0.27 * v, 0.2 * v)
                }
            }),
        ),
        Pattern::new(
            id("clouds"),
            "Clouds",
            generate(128, 128, |x, y| {
                let v = 0.5 * tile_noise(x, y, 128, 4, 1)
                    + 0.3 * tile_noise(x, y, 128, 8, 2)
                    + 0.2 * tile_noise(x, y, 128, 16, 3);
                mix(srgb(0.36, 0.55, 0.85), srgb(0.97, 0.97, 1.0), v.clamp(0.0, 1.0))
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::render_fill;
    use lumenply_doc::{BlendMode, Document, Fill, Layer, PatternOverlayFx};
    use lumenply_tiles::Rect;

    fn two_by_two() -> Pattern {
        // Red, green / blue, transparent.
        let mut r = Raster::new(2, 2);
        r.set(0, 0, Rgba::new(1.0, 0.0, 0.0, 1.0));
        r.set(1, 0, Rgba::new(0.0, 1.0, 0.0, 1.0));
        r.set(0, 1, Rgba::new(0.0, 0.0, 1.0, 1.0));
        Pattern::new("t", "Test", r)
    }

    #[test]
    fn pattern_fills_tile_across_tile_boundaries() {
        let p = two_by_two();
        let s = render_fill(&Fill::pattern(p.reference()), Rect::new(0, 0, 300, 4), true);
        // Period 2 everywhere, including across the 256-px tile seam.
        assert_eq!(s.get_pixel(0, 0), Rgba::new(1.0, 0.0, 0.0, 1.0));
        assert_eq!(s.get_pixel(255, 0), Rgba::new(0.0, 1.0, 0.0, 1.0));
        assert_eq!(s.get_pixel(256, 0), Rgba::new(1.0, 0.0, 0.0, 1.0));
        assert_eq!(s.get_pixel(256, 1), Rgba::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(s.get_pixel(299, 3).a, 0.0, "the transparent pixel stays clear");
        assert_eq!(s.get_pixel(300, 0).a, 0.0, "nothing past the canvas");
    }

    #[test]
    fn pattern_overlay_paints_inside_the_coverage_at_its_opacity() {
        let p = two_by_two();
        let mut doc = Document::new(4, 4);
        doc.patterns.push(p.clone());
        let id = doc.alloc_id();
        let mut l = Layer::pixel(id, "L");
        // Opaque white 2×2 at (0, 0).
        for y in 0..2 {
            for x in 0..2 {
                l.pixels_mut().unwrap().set_pixel(x, y, Rgba::WHITE);
            }
        }
        let mut fx = PatternOverlayFx::new(p.reference());
        fx.opacity = 0.5;
        fx.blend = BlendMode::Normal;
        l.effects.pattern_overlay = Some(fx);
        doc.add_layer(l);
        let out = crate::composite_raster(&doc);
        // (0, 0): half red over white → (1, 0.5, 0.5).
        let a = out.get(0, 0);
        assert!(
            (a.r - 1.0).abs() < 1e-3 && (a.g - 0.5).abs() < 1e-3 && (a.b - 0.5).abs() < 1e-3,
            "{a:?}"
        );
        // (1, 1): the pattern is transparent there → white.
        let b = out.get(1, 1);
        assert!((b.r - 1.0).abs() < 1e-3 && (b.g - 1.0).abs() < 1e-3, "{b:?}");
        // Outside the coverage nothing is painted.
        assert_eq!(out.get(3, 3).a, 0.0);
    }

    #[test]
    fn builtins_are_seamless_small_and_uniquely_named() {
        let all = builtin_patterns();
        assert_eq!(all.len(), 8);
        let mut ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        ids.dedup();
        assert_eq!(ids.len(), 8);
        for p in &all {
            assert!(p.id.starts_with(BUILTIN_PREFIX));
            assert!(p.width() <= 128 && p.height() <= 128);
            assert!(p.image.pixels.iter().all(|q| q.a == 1.0), "{} is opaque", p.name);
        }
        // Checkerboard: 8-px squares, light at the origin.
        let c = &all[0].image;
        assert!(c.get(0, 0).r > c.get(8, 0).r);
        assert_eq!(c.get(0, 0), c.get(8, 8));
    }
}
