//! High-quality image resampling for export: separable Lanczos-3 on
//! premultiplied linear pixels (no gamma darkening, no fringes at
//! transparent edges). Downscaling widens the kernel by the scale factor,
//! so it filters like an area average instead of aliasing.

use lumenply_tiles::{Raster, Rgba};
use rayon::prelude::*;

const LOBES: f32 = 3.0;

fn sinc(x: f32) -> f32 {
    if x.abs() < 1e-6 {
        1.0
    } else {
        let p = std::f32::consts::PI * x;
        p.sin() / p
    }
}

fn lanczos(x: f32) -> f32 {
    if x.abs() >= LOBES {
        0.0
    } else {
        sinc(x) * sinc(x / LOBES)
    }
}

/// For each output index, the first source index and normalised weights.
fn weights(src: u32, dst: u32) -> Vec<(usize, Vec<f32>)> {
    let scale = src as f32 / dst as f32;
    let support = LOBES * scale.max(1.0);
    let stretch = scale.max(1.0);
    (0..dst)
        .map(|i| {
            let centre = (i as f32 + 0.5) * scale - 0.5;
            let lo = (centre - support).floor().max(0.0) as usize;
            let hi = ((centre + support).ceil() as usize).min(src as usize - 1);
            let mut w: Vec<f32> = (lo..=hi)
                .map(|j| lanczos((j as f32 - centre) / stretch))
                .collect();
            let sum: f32 = w.iter().sum();
            if sum.abs() > 1e-8 {
                for v in &mut w {
                    *v /= sum;
                }
            }
            (lo, w)
        })
        .collect()
}

fn clamp_px(p: [f32; 4]) -> Rgba {
    // Lanczos rings slightly below zero next to hard edges.
    Rgba::new(p[0].max(0.0), p[1].max(0.0), p[2].max(0.0), p[3].clamp(0.0, 1.0))
}

/// Resamples `src` to `w × h`.
pub fn resample(src: &Raster, w: u32, h: u32) -> Raster {
    let (w, h) = (w.max(1), h.max(1));
    if src.width == w && src.height == h {
        return src.clone();
    }
    if src.width == 0 || src.height == 0 {
        return Raster::new(w, h);
    }
    let sw = src.width as usize;
    // Horizontal pass: src.height rows of `w`.
    let wx = weights(src.width, w);
    let mut tmp = vec![[0.0f32; 4]; w as usize * src.height as usize];
    tmp.par_chunks_mut(w as usize).enumerate().for_each(|(y, row)| {
        let line = &src.pixels[y * sw..(y + 1) * sw];
        for (x, out) in row.iter_mut().enumerate() {
            let (lo, ref ws) = wx[x];
            let mut acc = [0.0f32; 4];
            for (k, wt) in ws.iter().enumerate() {
                let p = line[lo + k];
                acc[0] += p.r * wt;
                acc[1] += p.g * wt;
                acc[2] += p.b * wt;
                acc[3] += p.a * wt;
            }
            *out = acc;
        }
    });
    // Vertical pass.
    let wy = weights(src.height, h);
    let mut out = Raster::new(w, h);
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let (lo, ref ws) = wy[y];
            for (x, o) in row.iter_mut().enumerate() {
                let mut acc = [0.0f32; 4];
                for (k, wt) in ws.iter().enumerate() {
                    let p = tmp[(lo + k) * w as usize + x];
                    for c in 0..4 {
                        acc[c] += p[c] * wt;
                    }
                }
                *o = clamp_px(acc);
            }
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32, p: Rgba) -> Raster {
        Raster::filled(w, h, p)
    }

    #[test]
    fn same_size_is_the_identity() {
        let mut r = Raster::new(7, 5);
        for (i, p) in r.pixels.iter_mut().enumerate() {
            let v = i as f32 / 35.0;
            *p = Rgba::new(v, v * 0.5, 0.2, 1.0);
        }
        assert_eq!(resample(&r, 7, 5), r);
    }

    #[test]
    fn a_flat_image_stays_flat_at_any_size() {
        let c = Rgba::new(0.3, 0.2, 0.1, 1.0);
        for (w, h) in [(13, 9), (400, 300), (1, 1)] {
            let out = resample(&flat(100, 70, c), w, h);
            for p in &out.pixels {
                assert!(
                    (p.r - 0.3).abs() < 1e-4 && (p.a - 1.0).abs() < 1e-4,
                    "{p:?} at {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn halving_a_fine_checker_averages_in_linear_light() {
        // 1-px checker of 0 and 1: any reduction must land on linear 0.5
        // (gamma-space resizing would give the darker 0.21).
        let mut r = Raster::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + y) % 2) as f32;
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let out = resample(&r, 16, 16);
        for p in &out.pixels[16 * 4..16 * 12] {
            assert!((p.r - 0.5).abs() < 0.02, "{p:?}");
        }
    }

    #[test]
    fn transparent_surroundings_leave_no_dark_fringe() {
        // A white square on transparency: premultiplied resampling keeps
        // the edge pixels white when unpremultiplied.
        let mut r = Raster::new(40, 40);
        for y in 10..30 {
            for x in 10..30 {
                r.set(x, y, Rgba::new(1.0, 1.0, 1.0, 1.0));
            }
        }
        let out = resample(&r, 17, 17);
        for p in &out.pixels {
            if p.a > 0.05 {
                let [cr, ..] = p.to_straight();
                assert!(cr > 0.97, "edge colour stays white: {p:?}");
            }
        }
    }
}
