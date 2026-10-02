//! Camera Raw–style "Basic" develop: white balance shift, exposure,
//! highlights/shadows (local), whites/blacks, contrast, a camera tone
//! curve, vibrance and saturation. Input and output are linear,
//! premultiplied pixels; tone decisions are made on perceptual
//! (sRGB-encoded) luminance and applied as a luminance ratio, so hues hold.

use lumenply_tiles::{Raster, Rgba};
use rayon::prelude::*;

/// The Basic panel. Every slider is -100..=100 except `exposure` (EV,
/// -5..=5); all zero (and no tone curve) leaves pixels untouched.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Develop {
    pub temperature: f32,
    pub tint: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub vibrance: f32,
    pub saturation: f32,
    /// A gentle film-like S curve, as raw converters apply by default so a
    /// linear develop doesn't look flat.
    pub tone_curve: bool,
}

impl Default for Develop {
    /// What opening a raw file starts with: neutral sliders, tone curve on.
    fn default() -> Self {
        Develop {
            tone_curve: true,
            ..Develop::NEUTRAL
        }
    }
}

impl Develop {
    /// Every control at rest: the identity.
    pub const NEUTRAL: Develop = Develop {
        temperature: 0.0,
        tint: 0.0,
        exposure: 0.0,
        contrast: 0.0,
        highlights: 0.0,
        shadows: 0.0,
        whites: 0.0,
        blacks: 0.0,
        vibrance: 0.0,
        saturation: 0.0,
        tone_curve: false,
    };

    /// White balance and exposure as per-channel linear gains. The
    /// temperature/tint shift keeps a neutral grey's luminance.
    fn gains(&self) -> [f32; 3] {
        let t = self.temperature / 100.0;
        let g = self.tint / 100.0;
        let mut k = [(0.45 * t).exp2(), (-0.3 * g).exp2(), (-0.45 * t).exp2()];
        let y = 0.2126 * k[0] + 0.7152 * k[1] + 0.0722 * k[2];
        let e = self.exposure.exp2();
        for c in &mut k {
            *c = *c / y * e;
        }
        k
    }

    fn is_local(&self) -> bool {
        self.highlights != 0.0 || self.shadows != 0.0
    }
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn decode(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The global tone curve on perceptual luminance `x` (0..=1 nominal).
fn tone(x: f32, d: &Develop) -> f32 {
    let mut x = x;
    // Blacks / whites move the ends (±25% of the range at full travel).
    let b = -d.blacks / 100.0 * 0.25;
    let w = 1.0 - d.whites / 100.0 * 0.25;
    if w > b {
        x = (x - b) / (w - b);
    }
    if d.tone_curve {
        // A soft S that keeps black, mid-grey and white in place: slope
        // 1.3 through the mid-tones, 0.7 at the ends.
        let y = x.clamp(0.0, 1.0);
        x += -0.3 * (y * std::f32::consts::TAU).sin() / std::f32::consts::TAU;
    }
    if d.contrast != 0.0 {
        let k = d.contrast / 100.0;
        // Contrast as a power pivoting on mid-grey, symmetric in both
        // directions.
        let s = (1.0 + k).max(0.05);
        let y = x.clamp(0.0, 1.0);
        let curved = if y < 0.5 {
            0.5 * (2.0 * y).powf(s)
        } else {
            1.0 - 0.5 * (2.0 * (1.0 - y)).powf(s)
        };
        x = curved + (x - y);
    }
    x
}

/// Base (large-scale) log luminance for highlights/shadows: the image's
/// log2 luminance box-blurred three times (≈ Gaussian) at radius `r`.
fn base_layer(lum: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut v: Vec<f32> = lum.iter().map(|&y| (y.max(1e-5)).log2()).collect();
    for _ in 0..3 {
        v = box_blur(&v, w, h, r);
    }
    v
}

fn box_blur(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &v[y * w..(y + 1) * w];
        let mut acc: f32 = row[..=r.min(w - 1)].iter().sum();
        for (x, o) in out.iter_mut().enumerate() {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            *o = acc / (hi - lo + 1) as f32;
            if x + r + 1 < w {
                acc += row[x + r + 1];
            }
            if x >= r {
                acc -= row[x - r];
            }
        }
    });
    let mut out = vec![0.0f32; w * h];
    let cols: Vec<Vec<f32>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let mut col = vec![0.0f32; h];
            let mut acc: f32 = (0..=r.min(h - 1)).map(|y| tmp[y * w + x]).sum();
            for (y, c) in col.iter_mut().enumerate() {
                let lo = y.saturating_sub(r);
                let hi = (y + r).min(h - 1);
                *c = acc / (hi - lo + 1) as f32;
                if y + r + 1 < h {
                    acc += tmp[(y + r + 1) * w + x];
                }
                if y >= r {
                    acc -= tmp[(y - r) * w + x];
                }
            }
            col
        })
        .collect();
    for (x, col) in cols.into_iter().enumerate() {
        for (y, c) in col.into_iter().enumerate() {
            out[y * w + x] = c;
        }
    }
    out
}

/// Develops `src` (linear, premultiplied, opaque or not) with `d`.
pub fn develop(src: &Raster, d: &Develop) -> Raster {
    let (w, h) = (src.width as usize, src.height as usize);
    if *d == Develop::NEUTRAL || w == 0 || h == 0 {
        return src.clone();
    }
    let gains = d.gains();
    // White balance + exposure, straight colour.
    let mut rgb: Vec<[f32; 3]> = src
        .pixels
        .par_iter()
        .map(|p| {
            let [r, g, b, _] = p.to_straight();
            [r * gains[0], g * gains[1], b * gains[2]]
        })
        .collect();
    // Highlights / shadows: compress or lift the large-scale luminance
    // (log2 base), keeping local detail.
    if d.is_local() {
        let lum: Vec<f32> = rgb.iter().map(|&c| luma(c)).collect();
        let radius = ((w.max(h) as f32) * 0.02).round().max(1.0) as usize;
        let base = base_layer(&lum, w, h, radius);
        let (hi, sh) = (d.highlights / 100.0, d.shadows / 100.0);
        rgb.par_iter_mut()
            .zip(base.par_iter())
            .zip(lum.par_iter())
            .for_each(|((c, &b), &y)| {
                if y <= 0.0 {
                    return;
                }
                // Weight by how bright / dark the neighbourhood is (in stops
                // below white): highlights act above ~-2.5 EV, shadows below.
                let bright = ((b + 2.5) / 2.5).clamp(0.0, 1.0);
                let dark = ((-b - 2.0) / 4.0).clamp(0.0, 1.0);
                let shift = hi * 1.5 * bright + sh * 2.0 * dark;
                let k = shift.exp2();
                for v in c.iter_mut() {
                    *v *= k;
                }
            });
    }
    // Tone (global curve on perceptual luminance, applied as a ratio) and
    // colour (vibrance, saturation in perceptual space).
    let (vib, sat) = (d.vibrance / 100.0, d.saturation / 100.0);
    let mut out = Raster::new(src.width, src.height);
    out.pixels
        .par_iter_mut()
        .zip(rgb.par_iter())
        .zip(src.pixels.par_iter())
        .for_each(|((o, c), s)| {
            let y = luma(*c);
            let mut c = *c;
            if y > 0.0 {
                let pe = encode(y);
                let pt = tone(pe, d);
                let k = decode(pt.max(0.0)) / y;
                for v in &mut c {
                    *v *= k;
                }
            }
            if vib != 0.0 || sat != 0.0 {
                let e = [
                    encode(c[0].max(0.0)),
                    encode(c[1].max(0.0)),
                    encode(c[2].max(0.0)),
                ];
                let l = luma(e);
                let mx = e[0].max(e[1]).max(e[2]);
                let mn = e[0].min(e[1]).min(e[2]);
                let chroma = mx - mn;
                // Vibrance favours the muted colours.
                let amount = (1.0 + sat) * (1.0 + vib * (1.0 - chroma).clamp(0.0, 1.0));
                for (ci, ev) in c.iter_mut().zip(e) {
                    *ci = decode((l + (ev - l) * amount).max(0.0));
                }
            }
            // Over-range channels roll towards white instead of shifting
            // hue as they clip.
            let m = c[0].max(c[1]).max(c[2]);
            if m > 1.0 {
                let y = luma(c).min(1.0);
                let t = ((m - 1.0) / m).clamp(0.0, 1.0);
                for v in &mut c {
                    *v = (*v / m) * (1.0 - t) + y * t;
                }
            }
            *o = Rgba::from_straight(c[0].max(0.0), c[1].max(0.0), c[2].max(0.0), s.a);
        });
    out
}

/// A one-click exposure / whites / blacks suggestion (Camera Raw's Auto):
/// mid-grey median, 0.1% clipping at each end.
pub fn auto_develop(src: &Raster) -> Develop {
    let mut lum: Vec<f32> = src
        .pixels
        .iter()
        .step_by((src.pixels.len() / 200_000).max(1))
        .map(|p| {
            let [r, g, b, _] = p.to_straight();
            luma([r, g, b])
        })
        .collect();
    if lum.is_empty() {
        return Develop::default();
    }
    lum.sort_by(f32::total_cmp);
    let at = |q: f32| lum[((lum.len() - 1) as f32 * q) as usize];
    let median = at(0.5).max(1e-4);
    // Aim the median at 18% grey, gently (half the way in stops).
    let exposure = ((0.18 / median).log2() * 0.5).clamp(-3.0, 3.0);
    let gain = exposure.exp2();
    let lo = encode(at(0.001) * gain);
    let hi = encode((at(0.999) * gain).min(1.0));
    Develop {
        exposure,
        blacks: (-(lo / 0.25) * 100.0).clamp(-100.0, 0.0) * 0.5,
        whites: (((1.0 - hi) / 0.25) * 100.0).clamp(0.0, 100.0) * 0.5,
        ..Develop::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(v: [f32; 3], w: u32, h: u32) -> Raster {
        let mut r = Raster::new(w, h);
        for p in &mut r.pixels {
            *p = Rgba::new(v[0], v[1], v[2], 1.0);
        }
        r
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn neutral_settings_are_the_identity() {
        let r = flat([0.2, 0.4, 0.1], 8, 8);
        assert_eq!(develop(&r, &Develop::NEUTRAL), r);
    }

    #[test]
    fn exposure_scales_linear_light_by_powers_of_two() {
        let r = flat([0.1, 0.05, 0.02], 4, 4);
        let d = Develop {
            exposure: 1.0,
            ..Develop::NEUTRAL
        };
        let p = develop(&r, &d).pixels[0];
        assert!(
            close(p.r, 0.2, 1e-5) && close(p.g, 0.1, 1e-5) && close(p.b, 0.04, 1e-5),
            "{p:?}"
        );
    }

    #[test]
    fn white_balance_warms_and_keeps_grey_luminance() {
        let grey = flat([0.18, 0.18, 0.18], 4, 4);
        let warm = develop(
            &grey,
            &Develop {
                temperature: 50.0,
                ..Develop::NEUTRAL
            },
        )
        .pixels[0];
        assert!(warm.r > warm.g && warm.g > warm.b, "{warm:?}");
        assert!(close(luma([warm.r, warm.g, warm.b]), 0.18, 1e-4));
        let green = develop(
            &grey,
            &Develop {
                tint: -50.0,
                ..Develop::NEUTRAL
            },
        )
        .pixels[0];
        assert!(green.g > green.r && green.g > green.b, "{green:?}");
    }

    #[test]
    fn saturation_minus_100_is_grey() {
        let r = flat([0.5, 0.1, 0.05], 4, 4);
        let p = develop(
            &r,
            &Develop {
                saturation: -100.0,
                ..Develop::NEUTRAL
            },
        )
        .pixels[0];
        assert!(close(p.r, p.g, 1e-5) && close(p.g, p.b, 1e-5), "{p:?}");
    }

    #[test]
    fn vibrance_lifts_muted_colours_more_than_vivid_ones() {
        let gain = |c: [f32; 3]| {
            let r = flat(c, 2, 2);
            let p = develop(
                &r,
                &Develop {
                    vibrance: 60.0,
                    ..Develop::NEUTRAL
                },
            )
            .pixels[0];
            let before = encode(c[0]) - encode(c[2]);
            let after = encode(p.r) - encode(p.b);
            after / before
        };
        assert!(gain([0.25, 0.2, 0.17]) > gain([0.8, 0.05, 0.01]));
    }

    #[test]
    fn highlights_pull_bright_areas_and_shadows_lift_dark_ones() {
        // Left half dark, right half bright.
        let mut r = Raster::new(200, 100);
        for y in 0..100 {
            for x in 0..200 {
                let v = if x < 100 { 0.01 } else { 0.8 };
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let hi = develop(
            &r,
            &Develop {
                highlights: -100.0,
                ..Develop::NEUTRAL
            },
        );
        assert!(
            hi.get(190, 50).r < 0.6,
            "bright pulled down: {}",
            hi.get(190, 50).r
        );
        assert!(
            close(hi.get(10, 50).r, 0.01, 2e-3),
            "dark untouched: {}",
            hi.get(10, 50).r
        );
        let sh = develop(
            &r,
            &Develop {
                shadows: 100.0,
                ..Develop::NEUTRAL
            },
        );
        assert!(sh.get(10, 50).r > 0.03, "dark lifted: {}", sh.get(10, 50).r);
        assert!(
            close(sh.get(190, 50).r, 0.8, 0.02),
            "bright untouched: {}",
            sh.get(190, 50).r
        );
    }

    #[test]
    fn contrast_spreads_tones_around_mid_grey() {
        let dark = decode(0.3);
        let light = decode(0.7);
        let mut r = Raster::new(2, 1);
        r.set(0, 0, Rgba::new(dark, dark, dark, 1.0));
        r.set(1, 0, Rgba::new(light, light, light, 1.0));
        let out = develop(
            &r,
            &Develop {
                contrast: 50.0,
                ..Develop::NEUTRAL
            },
        );
        assert!(encode(out.get(0, 0).r) < 0.3 && encode(out.get(1, 0).r) > 0.7);
        // Mid-grey is the pivot.
        let mid = flat([decode(0.5); 3], 1, 1);
        let m = develop(
            &mid,
            &Develop {
                contrast: 50.0,
                ..Develop::NEUTRAL
            },
        );
        assert!(close(encode(m.pixels[0].r), 0.5, 1e-4));
    }

    #[test]
    fn over_range_colour_rolls_to_white_without_exceeding_it() {
        let r = flat([0.9, 0.5, 0.2], 2, 2);
        let p = develop(
            &r,
            &Develop {
                exposure: 2.0,
                ..Develop::NEUTRAL
            },
        )
        .pixels[0];
        assert!(p.r <= 1.0 && p.g <= 1.0 && p.b <= 1.0, "{p:?}");
        assert!(p.r >= p.g && p.g >= p.b, "hue order kept: {p:?}");
    }

    #[test]
    fn auto_brings_a_dark_image_up() {
        let r = flat([0.02, 0.02, 0.02], 10, 10);
        let d = auto_develop(&r);
        assert!(d.exposure > 1.0, "{d:?}");
        assert!(d.tone_curve);
    }
}
