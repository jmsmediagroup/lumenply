//! Photoshop's blend modes beyond the W3C ten handled inline in
//! [`crate::blend_channel`]: the remaining separable modes (burns, dodges,
//! lights, Hard Mix, Exclusion, Subtract, Divide), the non-separable
//! whole-colour modes (Darker/Lighter Color, Hue, Saturation, Color,
//! Luminosity) and Dissolve.
//!
//! Everything here works on straight colour in `[0, 1]` and does not know
//! which encoding it is fed: the compositor passes linear light (ADR 0005),
//! and a future Photoshop-style gamma-blending option can pass encoded
//! values through the same functions.
//!
//! The non-separable modes follow the W3C Compositing and Blending spec
//! (`SetLum`, `SetSat`, `ClipColor`) with Rec. 601 luma weights
//! 0.3 / 0.59 / 0.11, which is what Photoshop uses.
//!
//! Dissolve is not a colour formula: the layer's alpha × opacity becomes a
//! per-pixel hard threshold against [`dissolve_noise`], a fixed hash of the
//! *canvas* pixel coordinates, so the speckle pattern is identical across
//! tiles, zoom levels, undo and re-renders. Its colour maths is Normal's.

use lumenply_doc::BlendMode;
use lumenply_tiles::{Rgba, Tile, TileCoord, TILE_PIXELS, TILE_SIZE};

/// Rec. 601 luma, as Photoshop's non-separable modes use.
#[inline]
pub fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// W3C `ClipColor`: pull an out-of-gamut colour back into `[0, 1]` along
/// the line to its own luma, so the luma is kept.
#[inline]
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 && l - n > 1e-12 {
        let k = l / (l - n);
        out = out.map(|v| l + (v - l) * k);
    }
    if x > 1.0 && x - l > 1e-12 {
        let k = (1.0 - l) / (x - l);
        out = out.map(|v| l + (v - l) * k);
    }
    out
}

/// W3C `SetLum`: shift `c` to luma `l`, then clip.
#[inline]
pub fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

/// Saturation as the W3C spec defines it: max − min.
#[inline]
pub fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// W3C `SetSat`: rescale `c` so max − min is `s`, keeping the order of
/// the channels (min becomes 0).
#[inline]
pub fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    // Indices of the min, mid and max channels.
    let (mut lo, mut mid, mut hi) = (0usize, 1usize, 2usize);
    if c[lo] > c[mid] {
        std::mem::swap(&mut lo, &mut mid);
    }
    if c[mid] > c[hi] {
        std::mem::swap(&mut mid, &mut hi);
    }
    if c[lo] > c[mid] {
        std::mem::swap(&mut lo, &mut mid);
    }
    let mut out = [0.0f32; 3];
    let range = c[hi] - c[lo];
    if range > 0.0 {
        out[mid] = (c[mid] - c[lo]) * s / range;
        out[hi] = s;
    }
    out
}

/// The separable modes [`crate::blend_channel`] hands over (everything
/// past the original ten). For the non-separable modes this is their
/// value when both colours are grey (which is all a single channel can
/// say): Hue, Saturation and Color keep the backdrop, Luminosity takes
/// the source, Darker/Lighter Color pick the darker/lighter value.
/// Dissolve's colour is Normal's.
#[inline]
pub(crate) fn extra_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::ColorBurn => color_burn(cb, cs),
        BlendMode::LinearBurn => (cb + cs - 1.0).max(0.0),
        BlendMode::ColorDodge => color_dodge(cb, cs),
        // Burn with 2s below ½, dodge with 2s − 1 above. At the ends
        // Photoshop lets the source win (s = 0 → 0, s = 1 → 1, whatever
        // the backdrop), unlike plain Color Burn/Dodge where b = 1 / b = 0
        // win; measured on psd-tools' blend-modes/vivid-light.psd.
        BlendMode::VividLight => {
            if cs <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else if cs <= 0.5 {
                color_burn(cb, 2.0 * cs)
            } else {
                color_dodge(cb, 2.0 * cs - 1.0)
            }
        }
        BlendMode::LinearLight => (cb + 2.0 * cs - 1.0).clamp(0.0, 1.0),
        BlendMode::PinLight => {
            if cs <= 0.5 {
                cb.min(2.0 * cs)
            } else {
                cb.max(2.0 * cs - 1.0)
            }
        }
        // 1 where b + s ≥ 1, else 0 — the threshold of Vivid Light with
        // Color Burn/Dodge's own edges, so b = 0 under s = 1 stays 0 and
        // b = 1 over s = 0 stays 1, as Photoshop renders hard-mix.psd.
        BlendMode::HardMix => {
            let on = if cs >= 1.0 {
                cb > 0.0
            } else if cs <= 0.0 {
                cb >= 1.0
            } else {
                cb + cs >= 1.0
            };
            if on {
                1.0
            } else {
                0.0
            }
        }
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        BlendMode::Subtract => (cb - cs).max(0.0),
        BlendMode::Divide => {
            if cs <= 0.0 {
                if cb > 0.0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                (cb / cs).min(1.0)
            }
        }
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color => cb,
        BlendMode::Luminosity => cs,
        BlendMode::DarkerColor => cb.min(cs),
        BlendMode::LighterColor => cb.max(cs),
        // Dissolve, and the W3C ten (handled by blend_channel itself).
        _ => cs,
    }
}

#[inline]
fn color_burn(cb: f32, cs: f32) -> f32 {
    if cb >= 1.0 {
        1.0
    } else if cs <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - cb) / cs).min(1.0)
    }
}

#[inline]
fn color_dodge(cb: f32, cs: f32) -> f32 {
    if cb <= 0.0 {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}

/// The full blend function `B(Cb, Cs)` on a straight RGB colour: the
/// non-separable modes mix whole colours, every other mode runs
/// [`crate::blend_channel`] per channel.
#[inline]
pub fn blend_color(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::DarkerColor => {
            if lum(cs) < lum(cb) {
                cs
            } else {
                cb
            }
        }
        BlendMode::LighterColor => {
            if lum(cs) > lum(cb) {
                cs
            } else {
                cb
            }
        }
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Color => set_lum(cs, lum(cb)),
        BlendMode::Luminosity => set_lum(cb, lum(cs)),
        _ => [
            crate::blend_channel(mode, cb[0], cs[0]),
            crate::blend_channel(mode, cb[1], cs[1]),
            crate::blend_channel(mode, cb[2], cs[2]),
        ],
    }
}

/// Deterministic noise in `[0, 1)` for canvas pixel `(x, y)`: the
/// threshold Dissolve compares coverage against. An integer hash
/// (Wellons' lowbias32) of the coordinates, so it is exact everywhere.
#[inline]
pub fn dissolve_noise(x: i32, y: i32) -> f32 {
    #[inline]
    fn mix(mut h: u32) -> u32 {
        h ^= h >> 16;
        h = h.wrapping_mul(0x7feb_352d);
        h ^= h >> 15;
        h = h.wrapping_mul(0x846c_a68b);
        h ^= h >> 16;
        h
    }
    let h = mix((x as u32).wrapping_mul(0x9e37_79b1) ^ mix(y as u32 ^ 0x5bd1_e995));
    (h >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// Dissolve's hard threshold: coverage `w` becomes 1 where it beats the
/// noise at `(x, y)`, else 0.
#[inline]
pub fn dissolve_weight(w: f32, x: i32, y: i32) -> f32 {
    if w > dissolve_noise(x, y) {
        1.0
    } else {
        0.0
    }
}

/// [`crate::blend_pixel`] at canvas pixel `(x, y)`: the same for every
/// mode but Dissolve, which needs the coordinate.
#[inline]
pub fn blend_pixel_at(backdrop: Rgba, source: Rgba, mode: BlendMode, opacity: f32, x: i32, y: i32) -> Rgba {
    if mode != BlendMode::Dissolve {
        return crate::blend_pixel(backdrop, source, mode, opacity);
    }
    dissolve_pixel(backdrop, source, opacity, x, y)
}

#[inline]
fn dissolve_pixel(backdrop: Rgba, source: Rgba, opacity: f32, x: i32, y: i32) -> Rgba {
    let a = source.a * opacity.min(1.0);
    if a <= 0.0 || dissolve_weight(a, x, y) <= 0.0 {
        return backdrop;
    }
    // A kept pixel is the source's straight colour at full opacity.
    let k = 1.0 / source.a;
    Rgba::new(source.r * k, source.g * k, source.b * k, 1.0)
}

/// [`crate::blend_tile`] for tile `coord`: the same for every mode but
/// Dissolve, which needs canvas coordinates. The compositor calls this
/// for layer blends; the Normal fast path costs one comparison per tile.
pub fn blend_tile_at(dst: &mut Tile, src: &Tile, mode: BlendMode, opacity: f32, coord: TileCoord) {
    if mode != BlendMode::Dissolve {
        return crate::blend_tile(dst, src, mode, opacity);
    }
    let (ox, oy) = coord.origin();
    let s = src.pixels();
    let d = dst.pixels_mut();
    for i in 0..TILE_PIXELS {
        let (x, y) = (ox + (i % TILE_SIZE) as i32, oy + (i / TILE_SIZE) as i32);
        d[i] = dissolve_pixel(d[i], s[i], opacity, x, y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{blend_channel, blend_pixel};

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    fn check(mode: BlendMode, cases: &[(f32, f32, f32)]) {
        for &(cb, cs, want) in cases {
            let got = blend_channel(mode, cb, cs);
            assert!(close(got, want), "{mode:?}({cb}, {cs}) = {got}, want {want}");
        }
    }

    #[test]
    fn burns() {
        // 1 − (1 − b)/s; s = 0 → 0 unless b = 1.
        check(
            BlendMode::ColorBurn,
            &[
                (0.5, 0.5, 0.0),
                (0.8, 0.5, 0.6),
                (0.6, 0.8, 0.5),
                (0.3, 0.2, 0.0),
                (1.0, 0.0, 1.0),
                (0.9, 0.0, 0.0),
                (0.4, 1.0, 0.4),
            ],
        );
        // b + s − 1 clamped at 0.
        check(
            BlendMode::LinearBurn,
            &[(0.5, 0.5, 0.0), (0.8, 0.6, 0.4), (0.3, 0.2, 0.0), (1.0, 1.0, 1.0)],
        );
    }

    #[test]
    fn dodges() {
        // b/(1 − s) clamped; s = 1 → 1 unless b = 0.
        check(
            BlendMode::ColorDodge,
            &[
                (0.25, 0.5, 0.5),
                (0.6, 0.5, 1.0),
                (0.2, 0.75, 0.8),
                (0.0, 1.0, 0.0),
                (0.1, 1.0, 1.0),
                (0.4, 0.0, 0.4),
            ],
        );
    }

    #[test]
    fn lights() {
        // Vivid: burn with 2s below ½, dodge with 2s − 1 above.
        check(
            BlendMode::VividLight,
            &[
                (0.8, 0.25, 0.6), // burn(0.8, 0.5) = 1 − 0.2/0.5
                (0.5, 0.25, 0.0), // burn(0.5, 0.5) = 0
                (0.3, 0.75, 0.6), // dodge(0.3, 0.5) = 0.3/0.5
                (0.6, 0.75, 1.0), // dodge(0.6, 0.5) clamps
                (0.4, 0.5, 0.4),  // burn(0.4, 1) = identity
                (0.9, 0.0, 0.0),  // s = 0 → 0
                (1.0, 0.0, 0.0),  // s = 0 → 0 even over white (Photoshop)
                (0.0, 1.0, 1.0),  // s = 1 → 1 even over black (Photoshop)
            ],
        );
        // Linear light: b + 2s − 1 clamped.
        check(
            BlendMode::LinearLight,
            &[
                (0.5, 0.5, 0.5),
                (0.3, 0.6, 0.5),
                (0.3, 0.1, 0.0),
                (0.8, 0.9, 1.0),
                (0.2, 0.25, 0.0),
            ],
        );
        // Pin light: min(b, 2s) below ½, max(b, 2s − 1) above.
        check(
            BlendMode::PinLight,
            &[
                (0.5, 0.2, 0.4),
                (0.3, 0.2, 0.3),
                (0.3, 0.9, 0.8),
                (0.9, 0.7, 0.9),
                (0.6, 0.5, 0.6),
            ],
        );
    }

    #[test]
    fn hard_mix_is_zero_or_one() {
        check(
            BlendMode::HardMix,
            &[
                (0.5, 0.5, 1.0),
                (0.4, 0.5, 0.0),
                (0.7, 0.4, 1.0),
                (0.0, 0.99, 0.0),
                (0.02, 0.99, 1.0),
                (1.0, 0.0, 1.0), // white backdrop stays white under black
                (0.0, 1.0, 0.0), // black backdrop stays black under white
                (0.6, 0.0, 0.0),
                (0.1, 1.0, 1.0),
            ],
        );
    }

    #[test]
    fn inversion_modes() {
        check(
            BlendMode::Exclusion,
            &[
                (0.5, 0.5, 0.5),
                (0.2, 0.8, 0.68),
                (1.0, 0.3, 0.7),
                (0.0, 0.4, 0.4),
            ],
        );
        check(
            BlendMode::Subtract,
            &[(0.8, 0.3, 0.5), (0.3, 0.8, 0.0), (0.5, 0.0, 0.5)],
        );
        // b/s clamped; s = 0 → 1 if b > 0 else 0.
        check(
            BlendMode::Divide,
            &[
                (0.2, 0.4, 0.5),
                (0.6, 0.3, 1.0),
                (0.3, 0.0, 1.0),
                (0.0, 0.0, 0.0),
                (0.0, 0.5, 0.0),
            ],
        );
    }

    fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    #[test]
    fn non_separable_modes_match_hand_computed_w3c_results() {
        let cb = [0.2, 0.4, 0.6]; // lum 0.362, sat 0.4
        let cs = [0.9, 0.5, 0.1]; // lum 0.576, sat 0.8
        assert!(close(lum(cb), 0.362) && close(lum(cs), 0.576));
        // Hue: SetSat(cs, 0.4) = [0.4, 0.2, 0], lum 0.238; + 0.124.
        let hue = blend_color(BlendMode::Hue, cb, cs);
        assert!(close3(hue, [0.524, 0.324, 0.124]), "{hue:?}");
        // Saturation: SetSat(cb, 0.8) = [0, 0.4, 0.8], lum 0.324; + 0.038.
        let s = blend_color(BlendMode::Saturation, cb, cs);
        assert!(close3(s, [0.038, 0.438, 0.838]), "{s:?}");
        // Color: cs shifted by 0.362 − 0.576 = −0.214 gives
        // [0.686, 0.286, −0.114], out of gamut: ClipColor with L = 0.362,
        // n = −0.114 maps C → L + (C − L)·0.362/0.476.
        let c = blend_color(BlendMode::Color, cb, cs);
        let k = 0.362f32 / 0.476;
        let want = [0.686f32, 0.286, -0.114].map(|v| 0.362 + (v - 0.362) * k);
        assert!(close3(c, want), "{c:?} vs {want:?}");
        assert!(close3(c, [0.6084034, 0.3042017, 0.0]), "{c:?}");
        assert!(close(lum(c), 0.362));
        // Luminosity: cb shifted by +0.214, in gamut.
        let l = blend_color(BlendMode::Luminosity, cb, cs);
        assert!(close3(l, [0.414, 0.614, 0.814]), "{l:?}");
        // Darker / Lighter Color pick a whole colour by luma.
        assert_eq!(blend_color(BlendMode::DarkerColor, cb, cs), cb);
        assert_eq!(blend_color(BlendMode::LighterColor, cb, cs), cs);
        // Pure blue (lum 0.11) is darker than mid grey (lum 0.5) even
        // though its blue channel is higher: no per-channel mixing.
        let blue = [0.0, 0.0, 1.0];
        let grey = [0.5, 0.5, 0.5];
        assert_eq!(blend_color(BlendMode::DarkerColor, grey, blue), blue);
        assert_eq!(blend_color(BlendMode::LighterColor, grey, blue), grey);
        // Luminosity pushing above white clips toward the luma.
        let lw = blend_color(BlendMode::Luminosity, [1.0, 0.0, 0.0], [0.9, 0.9, 0.9]);
        // [1.6, 0.6, 0.6] has lum 0.9, max 1.6: k = 0.1/0.7.
        let k = 0.1f32 / 0.7;
        assert!(
            close3(lw, [0.9 + 0.7 * k, 0.9 - 0.3 * k, 0.9 - 0.3 * k]),
            "{lw:?}"
        );
        // Grey source under Hue keeps the backdrop's luma and goes grey.
        let g = blend_color(BlendMode::Hue, cb, grey);
        assert!(close3(g, [0.362; 3]), "{g:?}");
    }

    #[test]
    fn non_separable_blend_pixel_composites_the_whole_colour() {
        let b = Rgba::from_straight(0.2, 0.4, 0.6, 1.0);
        let s = Rgba::from_straight(0.9, 0.5, 0.1, 1.0);
        let o = blend_pixel(b, s, BlendMode::Luminosity, 1.0).to_straight();
        assert!(
            close3([o[0], o[1], o[2]], [0.414, 0.614, 0.814]) && close(o[3], 1.0),
            "{o:?}"
        );
        // At 50% opacity: halfway between backdrop and result.
        let o = blend_pixel(b, s, BlendMode::Luminosity, 0.5).to_straight();
        assert!(close3([o[0], o[1], o[2]], [0.307, 0.507, 0.707]), "{o:?}");
        // Translucent backdrop: W3C mix of source, backdrop and B.
        let b = Rgba::from_straight(0.2, 0.4, 0.6, 0.5);
        let o = blend_pixel(b, s, BlendMode::DarkerColor, 1.0);
        // co = cs·(1 − ab) + ab·B = 0.5·cs + 0.5·cb, alpha 1.
        assert!(
            close(o.r, 0.55) && close(o.g, 0.45) && close(o.b, 0.35) && close(o.a, 1.0),
            "{o:?}"
        );
    }

    #[test]
    fn dissolve_is_deterministic_and_covers_its_opacity() {
        let red = Rgba::from_straight(1.0, 0.0, 0.0, 1.0);
        let blue = Rgba::from_straight(0.0, 0.0, 1.0, 1.0);
        let coord = TileCoord::new(3, -2);
        let src = Tile::filled(red);
        let render = |opacity: f32| {
            let mut d = Tile::filled(blue);
            blend_tile_at(&mut d, &src, BlendMode::Dissolve, opacity, coord);
            d
        };
        for (opacity, want) in [(0.5f32, 0.5f32), (0.25, 0.25), (0.9, 0.9)] {
            let t = render(opacity);
            let px = t.pixels();
            let kept = px.iter().filter(|p| p.r == 1.0).count();
            let frac = kept as f32 / TILE_PIXELS as f32;
            assert!((frac - want).abs() < 0.02, "opacity {opacity}: {frac}");
            // Every pixel is either fully the source or the backdrop.
            assert!(px.iter().all(|p| *p == red || *p == blue));
            // Re-rendering gives the identical pattern.
            assert_eq!(render(opacity).pixels(), px);
        }
        // Full opacity on opaque pixels is Normal; zero hides the layer.
        assert!(render(1.0).pixels().iter().all(|p| *p == red));
        let mut d = Tile::filled(blue);
        blend_tile_at(&mut d, &src, BlendMode::Dissolve, 0.0, coord);
        assert!(d.pixels().iter().all(|p| *p == blue));
        // Soft alpha is thresholded the same way: 40% alpha, 100% opacity
        // keeps ~40% of pixels at full strength.
        let soft = Tile::filled(Rgba::from_straight(1.0, 0.0, 0.0, 0.4));
        let mut d = Tile::filled(blue);
        blend_tile_at(&mut d, &soft, BlendMode::Dissolve, 1.0, coord);
        let kept = d.pixels().iter().filter(|p| **p == red).count() as f32 / TILE_PIXELS as f32;
        assert!((kept - 0.4).abs() < 0.02, "{kept}");
        // The pattern follows canvas coordinates: pixel (x, y) of tile
        // (3, −2) agrees with blend_pixel_at at the same canvas position.
        let t = render(0.5);
        let (ox, oy) = coord.origin();
        for i in [0usize, 1, 255, 256, 40_000, 65_535] {
            let (x, y) = (ox + (i % TILE_SIZE) as i32, oy + (i / TILE_SIZE) as i32);
            assert_eq!(
                t.pixels()[i],
                blend_pixel_at(blue, red, BlendMode::Dissolve, 0.5, x, y)
            );
        }
        // Noise values pinned so the pattern never silently changes.
        assert!(close(dissolve_noise(0, 0), dissolve_noise(0, 0)));
        let n: Vec<f32> = (0..4).map(|i| dissolve_noise(i, 7)).collect();
        assert!(n.iter().all(|v| (0.0..1.0).contains(v)));
        assert!(n.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn non_dissolve_modes_ignore_the_coordinate() {
        let b = Rgba::from_straight(0.3, 0.5, 0.7, 1.0);
        let s = Rgba::from_straight(0.6, 0.2, 0.9, 0.8);
        for m in BlendMode::ALL {
            if m == BlendMode::Dissolve {
                continue;
            }
            assert_eq!(blend_pixel_at(b, s, m, 0.7, 12, 34), blend_pixel(b, s, m, 0.7));
        }
    }
}
