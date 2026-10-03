//! Camera Raw–style "Basic" develop: white balance shift, exposure,
//! dehaze, highlights/shadows, texture and clarity (local), whites/blacks,
//! contrast, a camera tone curve, vibrance, saturation and a post-crop
//! vignette. Input and output are linear, premultiplied pixels; tone
//! decisions are made on perceptual (sRGB-encoded) luminance and applied as
//! a luminance ratio, so hues hold.
//!
//! The same code develops a camera RAW file ([`develop`]) and runs the
//! Camera Raw Filter on a layer ([`develop_in`]): the local controls read a
//! coverage-weighted blur whose windows are clipped at the raster's edge, so
//! a raster padded by [`Develop::reach`] gives exact results in its
//! interior, tile by tile.

use lumenply_doc::develop::frame_size;
pub use lumenply_doc::Develop;
use lumenply_tiles::{Raster, Rgba};
use rayon::prelude::*;

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

/// `v` box-blurred three times at radius `r` (≈ a Gaussian), every pixel
/// weighted by its coverage `a`, so transparent pixels don't drag the mean.
/// Windows are clipped at the raster's edge and each pass renormalises, so
/// only pixels within `3·r` of the edge see a partial neighbourhood.
fn blur3(v: &[f32], a: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let (mut v, mut a) = (v.to_vec(), a.to_vec());
    for _ in 0..3 {
        rows_pass(&mut v, &mut a, w, r);
    }
    let (mut vt, mut at) = (transpose(&v, w, h), transpose(&a, w, h));
    for _ in 0..3 {
        rows_pass(&mut vt, &mut at, h, r);
    }
    transpose(&vt, h, w)
}

/// One clipped, coverage-weighted box pass along every row of width `w`:
/// `v` becomes the weighted mean over the window, `a` the mean coverage.
/// Window sums come from f64 prefix sums, so a pixel's result doesn't
/// depend on where its row starts (tiles agree with the whole image).
fn rows_pass(v: &mut [f32], a: &mut [f32], w: usize, r: usize) {
    v.par_chunks_mut(w).zip(a.par_chunks_mut(w)).for_each_init(
        || (Vec::with_capacity(w + 1), Vec::with_capacity(w + 1)),
        |(pv, pa): &mut (Vec<f64>, Vec<f64>), (vr, ar)| {
            pv.clear();
            pa.clear();
            let (mut sv, mut sa) = (0.0f64, 0.0f64);
            pv.push(0.0);
            pa.push(0.0);
            for (&x, &k) in vr.iter().zip(ar.iter()) {
                sv += (x * k) as f64;
                sa += k as f64;
                pv.push(sv);
                pa.push(sa);
            }
            for (x, (o, oa)) in vr.iter_mut().zip(ar.iter_mut()).enumerate() {
                let lo = x.saturating_sub(r);
                let hi = (x + r).min(w - 1);
                let wa = pa[hi + 1] - pa[lo];
                *o = if wa > 1e-12 {
                    ((pv[hi + 1] - pv[lo]) / wa) as f32
                } else {
                    0.0
                };
                *oa = (wa / (hi - lo + 1) as f64) as f32;
            }
        },
    );
}

fn transpose(v: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, o) in col.iter_mut().enumerate() {
            *o = v[y * w + x];
        }
    });
    out
}

/// Develops `src` (linear, premultiplied, opaque or not) with `d`, as
/// opening a RAW file does: the local controls and the vignette are
/// measured on the raster itself.
pub fn develop(src: &Raster, d: &Develop) -> Raster {
    develop_in(src, d, (0, 0), [0, 0, src.width as i32, src.height as i32])
}

/// Develops `src`, whose top-left pixel sits at canvas position `origin`,
/// measuring the local controls' radii and the vignette on the canvas
/// rectangle `frame` (`[x, y, w, h]`). Pixels at least
/// `d.reach(frame_size(frame))` px inside the raster's edge are exact, so
/// the Camera Raw Filter renders seamlessly from padded tiles.
pub fn develop_in(src: &Raster, d: &Develop, origin: (i32, i32), frame: [i32; 4]) -> Raster {
    let (w, h) = (src.width as usize, src.height as usize);
    let d = &d.sane();
    if d.is_neutral() || w == 0 || h == 0 {
        return src.clone();
    }
    let gains = d.gains();
    // White balance + exposure, straight colour.
    let (rgb, alpha): (Vec<[f32; 3]>, Vec<f32>) = src
        .pixels
        .par_iter()
        .map(|p| {
            let [r, g, b, a] = p.to_straight();
            ([r * gains[0], g * gains[1], b * gains[2]], a)
        })
        .unzip();
    let lum: Vec<f32> = rgb.par_iter().map(|&c| luma(c)).collect();
    let radii = Develop::radii(frame_size(frame));
    let needs_log = d.is_local() || d.clarity != 0.0 || d.texture != 0.0;
    let log: Vec<f32> = if needs_log {
        lum.par_iter().map(|&y| y.max(1e-5).log2()).collect()
    } else {
        Vec::new()
    };
    // Large-scale log luminance (highlights / shadows), the clarity and
    // texture bases, and the haze estimate: the blurred darkest channel.
    let base = d.is_local().then(|| blur3(&log, &alpha, w, h, radii.tone));
    let clarity = (d.clarity != 0.0).then(|| blur3(&log, &alpha, w, h, radii.clarity));
    let texture = (d.texture != 0.0).then(|| blur3(&log, &alpha, w, h, radii.texture));
    let haze = (d.dehaze != 0.0).then(|| {
        let mins: Vec<f32> = rgb
            .par_iter()
            .map(|c| c[0].min(c[1]).min(c[2]).max(0.0))
            .collect();
        blur3(&mins, &alpha, w, h, radii.tone)
    });

    let (hi, sh) = (d.highlights / 100.0, d.shadows / 100.0);
    let dz = d.dehaze / 100.0;
    let vib = d.vibrance / 100.0;
    // Dehaze also lifts saturation a little.
    let sat = (1.0 + d.saturation / 100.0) * (1.0 + 0.25 * dz.max(0.0)) - 1.0;
    let vignette = Vignette::new(d, frame);
    let mut out = Raster::new(src.width, src.height);
    out.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let a = alpha[i];
            if a <= 0.0 {
                *o = Rgba::TRANSPARENT;
                continue;
            }
            let mut c = rgb[i];
            // Dehaze: lift (or lay) a veil as bright as the neighbourhood's
            // darkest channel, never more than this pixel's own.
            if let Some(hz) = &haze {
                let mn = c[0].min(c[1]).min(c[2]).max(0.0);
                if dz > 0.0 {
                    let v = (dz * 0.6 * hz[i].min(mn)).clamp(0.0, 0.95);
                    for ch in &mut c {
                        *ch = ((*ch - v) / (1.0 - v)).max(0.0);
                    }
                } else {
                    let v = -dz * 0.5;
                    for ch in &mut c {
                        *ch = *ch * (1.0 - v) + 0.6 * v;
                    }
                }
            }
            // Highlights / shadows compress or lift the large-scale
            // luminance; texture and clarity scale the detail around their
            // bases (all in stops, applied as one gain so hues hold).
            if needs_log && lum[i] > 0.0 {
                let l = log[i];
                let mut shift = 0.0;
                if let Some(b) = &base {
                    // Weight by how bright / dark the neighbourhood is (in
                    // stops below white): highlights act above ~-2.5 EV,
                    // shadows below.
                    let b = b[i];
                    let bright = ((b + 2.5) / 2.5).clamp(0.0, 1.0);
                    let dark = ((-b - 2.0) / 4.0).clamp(0.0, 1.0);
                    shift += hi * 1.5 * bright + sh * 2.0 * dark;
                }
                if clarity.is_some() || texture.is_some() {
                    // Mid-tones get the most; black and white keep theirs.
                    let e = encode(l.exp2().min(1.0));
                    let mid = (4.0 * e * (1.0 - e)).clamp(0.0, 1.0);
                    if let Some(bc) = &clarity {
                        shift += d.clarity / 100.0 * 0.6 * mid * (l - bc[i]).clamp(-3.0, 3.0);
                    }
                    if let Some(bt) = &texture {
                        shift += d.texture / 100.0 * 0.8 * mid * (l - bt[i]).clamp(-2.0, 2.0);
                    }
                }
                if shift != 0.0 {
                    let k = shift.exp2();
                    for v in &mut c {
                        *v *= k;
                    }
                }
            }
            c = finish(c, d, vib, sat);
            if let Some(v) = &vignette {
                let k = v.gain(origin.0 + x as i32, origin.1 + y as i32);
                for ch in &mut c {
                    *ch *= k;
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
            *o = Rgba::from_straight(c[0].max(0.0), c[1].max(0.0), c[2].max(0.0), a);
        }
    });
    out
}

/// Tone (global curve on perceptual luminance, applied as a ratio) and
/// colour (vibrance, saturation in perceptual space) of one straight pixel.
fn finish(mut c: [f32; 3], d: &Develop, vib: f32, sat: f32) -> [f32; 3] {
    let y = luma(c);
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
    c
}

/// The post-crop vignette: an exposure falloff towards the corners of the
/// frame, starting at a distance set by the midpoint.
struct Vignette {
    centre: (f32, f32),
    half: (f32, f32),
    start: f32,
    stops: f32,
}

impl Vignette {
    fn new(d: &Develop, frame: [i32; 4]) -> Option<Vignette> {
        if d.vignette == 0.0 {
            return None;
        }
        let (fw, fh) = (frame[2].max(1) as f32, frame[3].max(1) as f32);
        Some(Vignette {
            centre: (frame[0] as f32 + fw / 2.0, frame[1] as f32 + fh / 2.0),
            half: (fw / 2.0, fh / 2.0),
            start: d.vignette_midpoint / 100.0 * 0.8,
            stops: d.vignette / 100.0 * 2.0,
        })
    }

    /// Linear gain at canvas pixel `(x, y)`: 1 inside the midpoint, up to
    /// 2 stops darker (or lighter) at the frame's corners.
    fn gain(&self, x: i32, y: i32) -> f32 {
        let nx = (x as f32 + 0.5 - self.centre.0) / self.half.0;
        let ny = (y as f32 + 0.5 - self.centre.1) / self.half.1;
        let dist = (nx * nx + ny * ny).sqrt() / std::f32::consts::SQRT_2;
        let t = ((dist - self.start) / (1.0 - self.start)).clamp(0.0, 1.0);
        let t = t * t * (3.0 - 2.0 * t);
        (self.stops * t).exp2()
    }
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

    /// A 100×20 grey step: 0.05 left of x = 50, 0.2 from there. On a
    /// 100 px frame clarity and texture blur at radius 1, whose triple box
    /// puts 17/27 of its weight on the near side of a step: the first
    /// bright pixel's base is (10·log2 0.05 + 17·log2 0.2) / 27.
    fn step() -> Raster {
        let mut r = Raster::new(100, 20);
        for y in 0..20 {
            for x in 0..100 {
                let v = if x < 50 { 0.05 } else { 0.2 };
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        r
    }

    #[test]
    fn clarity_steepens_edges_and_leaves_flat_areas() {
        let d = Develop {
            clarity: 100.0,
            ..Develop::NEUTRAL
        };
        let out = develop(&step(), &d);
        // Detail 10/27 · 2 stops, mid-tone weight 0.99904 at 0.2:
        // 0.2 · 2^(0.6 · 0.99904 · 0.74074) = 0.27208.
        assert!(close(out.get(50, 10).r, 0.27208, 2e-4), "{:?}", out.get(50, 10));
        // The dark side of the edge goes darker: 0.05 · 2^(−0.6 · 0.6127 · 0.74074).
        assert!(close(out.get(49, 10).r, 0.03974, 2e-4), "{:?}", out.get(49, 10));
        // Away from the edge nothing changes.
        assert!(close(out.get(10, 10).r, 0.05, 1e-4), "{:?}", out.get(10, 10));
        assert!(close(out.get(90, 10).r, 0.2, 1e-4), "{:?}", out.get(90, 10));
        let flat_out = develop(&flat([0.2, 0.2, 0.2], 40, 40), &d);
        assert!(close(flat_out.get(20, 20).r, 0.2, 1e-4));
    }

    #[test]
    fn negative_texture_softens_edges() {
        let d = Develop {
            texture: -100.0,
            ..Develop::NEUTRAL
        };
        let out = develop(&step(), &d);
        // 0.2 · 2^(−0.8 · 0.99904 · 0.74074) = 0.13268.
        assert!(close(out.get(50, 10).r, 0.13268, 2e-4), "{:?}", out.get(50, 10));
        assert!(close(out.get(90, 10).r, 0.2, 1e-4));
    }

    #[test]
    fn dehaze_lifts_or_lays_a_veil() {
        let grey = flat([0.2, 0.2, 0.2], 40, 40);
        let clear = develop(
            &grey,
            &Develop {
                dehaze: 100.0,
                ..Develop::NEUTRAL
            },
        );
        // Veil 0.6 · 0.2 = 0.12 removed: (0.2 − 0.12) / (1 − 0.12).
        assert!(
            close(clear.get(20, 20).r, 0.090909, 1e-4),
            "{:?}",
            clear.get(20, 20)
        );
        let hazy = develop(
            &grey,
            &Develop {
                dehaze: -100.0,
                ..Develop::NEUTRAL
            },
        );
        // Half the way to a 0.6 veil.
        assert!(close(hazy.get(20, 20).r, 0.4, 1e-4), "{:?}", hazy.get(20, 20));
        // Colour gets more saturated as the veil comes off.
        let c = flat([0.4, 0.25, 0.15], 40, 40);
        let p = develop(
            &c,
            &Develop {
                dehaze: 50.0,
                ..Develop::NEUTRAL
            },
        )
        .get(20, 20);
        assert!(p.r / p.b > 0.4 / 0.15 + 0.5, "{p:?}");
    }

    #[test]
    fn vignette_darkens_the_corners_from_the_midpoint() {
        let grey = flat([0.5, 0.5, 0.5], 100, 100);
        let d = Develop {
            vignette: -100.0,
            ..Develop::NEUTRAL
        };
        let out = develop(&grey, &d);
        assert!(close(out.get(49, 49).r, 0.5, 1e-4), "centre untouched");
        // Corner pixel: distance 0.99, past the 0.4 start by 0.59 / 0.6,
        // smoothstep 0.99918, two stops down: 0.5 · 2^(−1.99835) = 0.12514.
        assert!(close(out.get(0, 0).r, 0.12514, 1e-4), "{:?}", out.get(0, 0));
        assert!(close(out.get(99, 99).r, 0.12514, 1e-4));
        // Inside the midpoint (distance 0.3 < 0.4) nothing changes.
        assert!(close(out.get(29, 49).r, 0.5, 1e-4), "{:?}", out.get(29, 49));
        // The filter measures on its frame: the same frame placed 100 px to
        // the right puts the same corner at canvas x = 100.
        let moved = develop_in(&grey, &d, (100, 0), [100, 0, 100, 100]);
        assert_eq!(moved.get(0, 0), out.get(0, 0));
    }

    #[test]
    fn neutral_filter_settings_leave_pixels_unchanged() {
        let mut r = Raster::new(64, 48);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            let a = if i % 7 == 0 {
                0.0
            } else {
                0.3 + (i % 5) as f32 * 0.15
            };
            *p = Rgba::from_straight(
                (i % 13) as f32 / 13.0,
                (i % 17) as f32 / 17.0,
                (i % 3) as f32 / 3.0,
                a,
            );
        }
        let out = develop_in(&r, &Develop::NEUTRAL, (10, 20), [0, 0, 800, 600]);
        for (a, b) in out.pixels.iter().zip(&r.pixels) {
            assert!(
                (a.r - b.r).abs() < 1e-4 && (a.g - b.g).abs() < 1e-4 && (a.b - b.b).abs() < 1e-4,
                "{a:?} vs {b:?}"
            );
            assert!((a.a - b.a).abs() < 1e-6);
        }
        // A develop that does change pixels keeps transparent ones empty.
        let lifted = develop_in(
            &r,
            &Develop {
                shadows: 50.0,
                ..Develop::NEUTRAL
            },
            (0, 0),
            [0, 0, 64, 48],
        );
        assert_eq!(lifted.pixels[0], Rgba::TRANSPARENT);
    }
}
