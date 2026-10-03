//! Fitting an upscaled model mask to the full-resolution image.
//!
//! A model sees the image at 1024 pixels (MobileSAM's mask is a further 4×
//! coarser), so its mask, upscaled, has edges a few pixels off and as soft
//! as the upscale. [`refine`] re-fits a band around the mask's ½ contour
//! to the image with Select and Mask's engine (`lumenply_core::refine`):
//! either He et al.'s colour-guided filter with the full-resolution image
//! as the guide (then Select and Mask's Contrast), or Select and Mask's
//! colour-sampling matting, which also moves an edge that is off by up to
//! the band onto the image's edge. Outside the band the mask is unchanged.
//! Selections ([`RefineOptions::selection`]) use the matting: the filter
//! only aligns a transition with an image edge, keeping each side's mean
//! of the mask, so an edge a cell off stays partly wrong.
//!
//! A soft matte whose model saw the image at a known size refines better
//! still with `guided_upsample`, the guided filter's coefficients fitted
//! at the model's resolution and evaluated at the image's; `Matter` uses
//! it.
//!
//! The guide is the image as the model saw it: over white, sRGB-encoded
//! (edges in shadows count as much as edges in highlights), at 8 bits.

use lumenply_core::refine::{
    box_mean, edge_matte, global_refine, guided_filter_band, refine_band, solve3, RefineParams,
};
use lumenply_tiles::{Raster, Rgba};
use rayon::prelude::*;

use crate::prep::{bilinear_axis, decode_srgb, encode_srgb, over_white};
use crate::Matte;

/// How a mask is fitted to the image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefineOptions {
    /// Half-width, in image pixels, of the band around the mask's ½
    /// contour that is re-fitted; 0 returns the mask unchanged.
    pub radius: f32,
    /// The guided filter's window radius in image pixels.
    pub window: u32,
    /// The guided filter's ε: colour variance (sRGB, 0..=1 units squared)
    /// below which a window counts as flat and the mask is averaged there.
    pub eps: f32,
    /// Select and Mask's Contrast after the filter, 0..=100 %: steepens
    /// the soft transition the filter leaves (selections), 0 keeps a
    /// matte's partial coverage.
    pub contrast: f32,
    /// Run Select and Mask's colour-sampling matting in the band first:
    /// pulls an edge up to `radius` off onto the image and gives hair
    /// partial coverage, at some risk where both sides share a colour.
    pub matting: bool,
}

impl RefineOptions {
    /// No refinement.
    pub const OFF: RefineOptions = RefineOptions {
        radius: 0.0,
        window: 0,
        eps: 0.0,
        contrast: 0.0,
        matting: false,
    };

    /// For a selection mask whose model grid cell spans `cell` image
    /// pixels (MobileSAM: 4 × image pixels per 1024-frame pixel).
    ///
    /// Colour matting in a band of 1.5 cells: an upscaled MobileSAM edge is
    /// off by up to a cell or so (it cut 3–6 px of rock off the ridges of
    /// the demo photo, and took sky above the summit), which the guided
    /// filter alone only softens; the matting puts it on the image's edge.
    pub fn selection(cell: f32) -> RefineOptions {
        let cell = if cell.is_finite() { cell.max(0.0) } else { 1.0 };
        RefineOptions {
            radius: (1.5 * cell).max(4.0),
            window: ((cell * 0.5).round() as u32).max(2),
            eps: 1e-3,
            contrast: 0.0,
            matting: true,
        }
    }

    /// For a soft matte at `scale` image pixels per model pixel
    /// (BiRefNet: the image's longer side over 768, or 1024 at high
    /// detail). [`crate::Matter`] fits it with the guided filter as an
    /// upsampler, in windows of two model pixels; `radius` is the band
    /// [`refine`] would use.
    pub fn matte(scale: f32) -> RefineOptions {
        let scale = if scale.is_finite() { scale.max(0.0) } else { 1.0 };
        RefineOptions {
            radius: (2.0 * scale).max(2.0),
            window: ((2.0 * scale).round() as u32).max(2),
            eps: 1e-4,
            contrast: 0.0,
            matting: false,
        }
    }
}

/// The image as the refinement sees it: over white, sRGB-encoded, 8 bits
/// per channel (3 bytes a pixel, so an embedding can keep it).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Guide {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<[u8; 3]>,
}

/// Linear → 8-bit sRGB through a table fine enough (2¹⁴ steps) that the
/// steep start of the curve rounds like the exact function, ±1.
fn srgb8_table() -> &'static [u8] {
    static T: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        (0..=(1 << 14))
            .map(|i| (encode_srgb(i as f32 / (1 << 14) as f32) * 255.0 + 0.5) as u8)
            .collect()
    })
}

#[inline]
fn to_srgb8(table: &[u8], v: f32) -> u8 {
    table[((v.clamp(0.0, 1.0) * (1 << 14) as f32) + 0.5) as usize]
}

impl Guide {
    pub(crate) fn new(img: &Raster) -> Guide {
        let table = srgb8_table();
        let rgb = img
            .pixels
            .par_iter()
            .map(|&p| over_white(p).map(|v| to_srgb8(table, v)))
            .collect();
        Guide {
            w: img.width as usize,
            h: img.height as usize,
            rgb,
        }
    }
}

/// Fit `matte` (as large as `image`) to the image's edges (see the module
/// documentation).
pub fn refine(image: &Raster, matte: &Matte, opts: RefineOptions) -> Matte {
    assert_eq!(
        (image.width, image.height),
        (matte.width, matte.height),
        "the matte does not match the image"
    );
    if opts.radius <= 0.0 || !opts.radius.is_finite() {
        return matte.clone();
    }
    refine_guided(&Guide::new(image), matte, opts)
}

/// [`refine`] against a prepared guide.
pub(crate) fn refine_guided(guide: &Guide, matte: &Matte, opts: RefineOptions) -> Matte {
    let (w, h) = (guide.w, guide.h);
    assert_eq!((w, h), (matte.width as usize, matte.height as usize));
    if opts.radius <= 0.0 || !opts.radius.is_finite() || w == 0 || h == 0 {
        return matte.clone();
    }
    let band = refine_band(&matte.alpha, None, w, h, opts.radius);
    // The band's bounds, plus what the filter windows read around it.
    let rows: Vec<Option<(usize, usize)>> = band
        .par_chunks(w)
        .map(|row| {
            let first = row.iter().position(|&b| b)?;
            let last = row.iter().rposition(|&b| b)?;
            Some((first, last + 1))
        })
        .collect();
    let Some(y0) = rows.iter().position(Option::is_some) else {
        return matte.clone();
    };
    let y1 = rows.iter().rposition(Option::is_some).expect("y0 exists") + 1;
    let (x0, x1) = rows
        .iter()
        .flatten()
        .fold((usize::MAX, 0), |(a, b), &(f, l)| (a.min(f), b.max(l)));
    let halo = 2 * opts.window as usize
        + 2
        + if opts.matting {
            opts.radius.ceil() as usize
        } else {
            0
        };
    let (cx0, cx1) = (x0.saturating_sub(halo), (x1 + halo).min(w));
    let (cy0, cy1) = (y0.saturating_sub(halo), (y1 + halo).min(h));
    let (cw, ch) = (cx1 - cx0, cy1 - cy0);
    let crop = |i: usize| (cy0 + i / cw) * w + cx0 + i % cw;
    let p: Vec<f32> = (0..cw * ch).map(|i| matte.alpha[crop(i)]).collect();
    let unknown: Vec<bool> = (0..cw * ch).map(|i| band[crop(i)]).collect();

    let mut q = if opts.matting {
        let lut: Vec<f32> = (0..256).map(|v| decode_srgb(v as f32 / 255.0)).collect();
        let pixels = (0..cw * ch)
            .map(|i| {
                let c = guide.rgb[crop(i)];
                Rgba::new(lut[c[0] as usize], lut[c[1] as usize], lut[c[2] as usize], 1.0)
            })
            .collect();
        let img = Raster {
            width: cw as u32,
            height: ch as u32,
            pixels,
        };
        let params = RefineParams {
            radius: opts.radius,
            ..RefineParams::default()
        };
        edge_matte(&img, &p, None, &params, 1.0)
    } else {
        let g: Vec<[f32; 4]> = (0..cw * ch)
            .map(|i| {
                let c = guide.rgb[crop(i)];
                [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]
            })
            .collect();
        let r = (opts.window as usize).max(1);
        guided_filter_band(&g, &p, &unknown, cw, ch, r, opts.eps.max(1e-8))
    };
    for v in &mut q {
        *v = v.clamp(0.0, 1.0);
    }
    if opts.contrast > 0.0 {
        let params = RefineParams {
            contrast: opts.contrast,
            ..RefineParams::default()
        };
        q = global_refine(q, cw, ch, &params, 1.0);
    }
    let mut alpha = matte.alpha.clone();
    for (i, (&v, &u)) in q.iter().zip(&unknown).enumerate() {
        if u {
            alpha[crop(i)] = if v < 0.5 / 255.0 {
                0.0
            } else if v > 1.0 - 0.5 / 255.0 {
                1.0
            } else {
                v
            };
        }
    }
    Matte {
        width: matte.width,
        height: matte.height,
        alpha,
    }
}

/// He and Sun's fast guided filter used as an upsampler: the local linear
/// model `q = a·I + b` is fitted to a model's matte `p` against the image
/// as the model saw it (`low`, `lw`×`lh`, sRGB 0..=1) in windows of radius
/// `r`, the coefficients averaged over the windows as usual, and then
/// evaluated at every pixel of the full-resolution `guide`, coefficients
/// interpolated bilinearly. The matte's edges land where the
/// full-resolution colours change: no blur from the upscale, and no tail
/// beyond the model's own transition. Clamped to 0..=1.
pub(crate) fn guided_upsample(
    low: &[[f32; 3]],
    p: &[f32],
    lw: usize,
    lh: usize,
    r: usize,
    eps: f32,
    guide: &Guide,
) -> Vec<f32> {
    let n = lw * lh;
    assert_eq!((low.len(), p.len()), (n, n));
    let mean = |f: &(dyn Fn(usize) -> f32 + Sync)| -> Vec<f32> {
        let v: Vec<f32> = (0..n).into_par_iter().map(f).collect();
        box_mean(&v, lw, lh, r)
    };
    let mi = [0, 1, 2].map(|c| mean(&|i| low[i][c]));
    let mp = mean(&|i| p[i]);
    let mip = [0, 1, 2].map(|c| mean(&|i| low[i][c] * p[i]));
    let mii = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)].map(|(a, b)| mean(&|i| low[i][a] * low[i][b]));
    let coef: Vec<[f32; 4]> = (0..n)
        .into_par_iter()
        .map(|j| {
            let m = [mi[0][j], mi[1][j], mi[2][j]];
            let cov = [0, 1, 2].map(|c| mip[c][j] - m[c] * mp[j]);
            let v = |k: usize, a: usize, b: usize| mii[k][j] - m[a] * m[b];
            let s = [
                [v(0, 0, 0) + eps, v(1, 0, 1), v(2, 0, 2)],
                [v(1, 0, 1), v(3, 1, 1) + eps, v(4, 1, 2)],
                [v(2, 0, 2), v(4, 1, 2), v(5, 2, 2) + eps],
            ];
            let a = solve3(s, cov);
            [a[0], a[1], a[2], mp[j] - a[0] * m[0] - a[1] * m[1] - a[2] * m[2]]
        })
        .collect();
    let ab = [0, 1, 2, 3].map(|k| mean(&|i| coef[i][k]));
    let (w, h) = (guide.w, guide.h);
    let xs = bilinear_axis(w, lw, lw as f64 / w as f64);
    let ys = bilinear_axis(h, lh, lh as f64 / h as f64);
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (y0, y1, ty) = ys[y];
        for (x, (o, &(x0, x1, tx))) in row.iter_mut().zip(&xs).enumerate() {
            let at = |k: usize| {
                let m = &ab[k];
                let a = m[y0 * lw + x0] + (m[y0 * lw + x1] - m[y0 * lw + x0]) * tx;
                let b = m[y1 * lw + x0] + (m[y1 * lw + x1] - m[y1 * lw + x0]) * tx;
                a + (b - a) * ty
            };
            let c = guide.rgb[y * w + x];
            let q = at(0) * c[0] as f32 / 255.0
                + at(1) * c[1] as f32 / 255.0
                + at(2) * c[2] as f32 / 255.0
                + at(3);
            *o = q.clamp(0.0, 1.0);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 3] = [0.8, 0.05, 0.03];
    const BLUE: [f32; 3] = [0.03, 0.08, 0.7];

    /// `w`×`h`, red left of `edge`, blue from it on.
    fn two_tone(w: u32, h: u32, edge: u32) -> Raster {
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let c = if x < edge { RED } else { BLUE };
                r.set(x, y, Rgba::new(c[0], c[1], c[2], 1.0));
            }
        }
        r
    }

    /// A blurry mask: 1 left of `from`, falling linearly to 0 at `to`.
    fn ramp(w: u32, h: u32, from: f32, to: f32) -> Matte {
        let alpha = (0..w * h)
            .map(|i| {
                let x = (i % w) as f32 + 0.5;
                ((to - x) / (to - from)).clamp(0.0, 1.0)
            })
            .collect();
        Matte {
            width: w,
            height: h,
            alpha,
        }
    }

    fn row(m: &Matte, y: u32, xs: std::ops::Range<u32>) -> Vec<f32> {
        xs.map(|x| (m.get(x, y) * 100.0).round() / 100.0).collect()
    }

    #[test]
    fn a_hard_edge_in_the_guide_pulls_a_blurry_mask_onto_it() {
        // Image edge at x = 30 (red | blue). The mask falls linearly from 1
        // at x = 28 to 0 at x = 36, its ½ point at x = 32: an upscaled
        // model mask, blurry and 2 px off.
        let img = two_tone(64, 24, 30);
        let mask = ramp(64, 24, 28.0, 36.0);
        assert_eq!(row(&mask, 12, 28..34), vec![0.94, 0.81, 0.69, 0.56, 0.44, 0.31]);
        let opts = RefineOptions {
            radius: 8.0,
            window: 3,
            eps: 1e-3,
            contrast: 0.0,
            matting: false,
        };
        // The filter alone puts the transition on the image edge: a step
        // from 0.92 to 0.5 between x = 29 and 30 where the mask had 0.81 |
        // 0.69. Each side keeps its local mean of the mask, so the blue side
        // stays partly covered.
        let soft = refine(&img, &mask, opts);
        assert_eq!(
            row(&soft, 12, 26..34),
            vec![0.97, 0.96, 0.94, 0.92, 0.5, 0.44, 0.37, 0.3]
        );
        // A wider window averages more of the true background into the
        // blue side and Contrast steepens it: within 2 px of the edge the
        // blue side is nearly clear, beyond it clear; the red side is full.
        let pulled = refine(
            &img,
            &mask,
            RefineOptions {
                window: 8,
                contrast: 50.0,
                ..opts
            },
        );
        for y in [0, 12, 23] {
            assert_eq!(
                row(&pulled, y, 26..34),
                vec![1.0, 1.0, 1.0, 1.0, 0.14, 0.07, 0.0, 0.0],
                "row {y}"
            );
        }
        // Outside the band (8 px around x = 32) nothing changes.
        assert_eq!(pulled.get(23, 12), mask.get(23, 12));
        assert_eq!(pulled.get(41, 12), mask.get(41, 12));
        // Without an edge in the guide (a flat image) the filter only
        // averages: the ½ point stays between x = 31 and 32.
        let flat = refine(
            &Raster::filled(64, 24, Rgba::new(0.2, 0.2, 0.2, 1.0)),
            &mask,
            opts,
        );
        assert!((flat.get(31, 12) + flat.get(32, 12) - 1.0).abs() < 0.02);
        assert!(flat.get(31, 12) > 0.5 && flat.get(32, 12) < 0.5);
    }

    #[test]
    fn matting_snaps_an_edge_further_off() {
        // 5 px off: beyond what the filter pulls, within the matting band.
        let img = two_tone(64, 24, 28);
        let mask = ramp(64, 24, 31.0, 35.0);
        let m = refine(
            &img,
            &mask,
            RefineOptions {
                radius: 8.0,
                window: 2,
                eps: 1e-3,
                contrast: 0.0,
                matting: true,
            },
        );
        assert_eq!(row(&m, 12, 25..31), vec![1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn off_and_empty_masks_come_back_unchanged() {
        let img = two_tone(16, 8, 8);
        let mask = ramp(16, 8, 6.0, 10.0);
        assert_eq!(refine(&img, &mask, RefineOptions::OFF), mask);
        let none = Matte {
            width: 16,
            height: 8,
            alpha: vec![0.0; 128],
        };
        assert_eq!(refine(&img, &none, RefineOptions::selection(8.0)), none);
    }

    #[test]
    fn presets_scale_with_the_upscale() {
        let s = RefineOptions::selection(7.0);
        assert_eq!((s.radius, s.window, s.matting), (10.5, 4, true));
        let small = RefineOptions::selection(1.0);
        assert_eq!((small.radius, small.window), (4.0, 2));
        let m = RefineOptions::matte(4.0);
        assert_eq!((m.radius, m.window, m.contrast), (8.0, 8, 0.0));
        assert_eq!(RefineOptions::matte(0.5).radius, 2.0);
    }

    #[test]
    fn guided_upsampling_puts_a_low_resolution_matte_on_full_resolution_edges() {
        // Full resolution 64×32 with its edge at x = 30; the model saw it at
        // 16×8, where pixel 7 (x = 28..32) is half red, half blue, and its
        // matte there is ½. A bilinear upscale smears that over x = 26..34.
        let img = two_tone(64, 32, 30);
        let low = crate::prep::resize_raster(&img, 16, 8)
            .into_iter()
            .map(|c| c.map(encode_srgb))
            .collect::<Vec<_>>();
        let p: Vec<f32> = (0..16 * 8)
            .map(|i| match i % 16 {
                0..=6 => 1.0,
                7 => 0.5,
                _ => 0.0,
            })
            .collect();
        let smeared = Matte::new(64, 32, crate::prep::upsample(&p, 16, 8, 64, 32, 0.25, 0.25));
        assert_eq!(
            row(&smeared, 16, 26..36),
            vec![0.94, 0.81, 0.69, 0.56, 0.44, 0.31, 0.19, 0.06, 0.0, 0.0]
        );
        let q = Matte::new(
            64,
            32,
            guided_upsample(&low, &p, 16, 8, 2, 1e-4, &Guide::new(&img)),
        );
        // Exactly on the image's edge: covered red, clear blue (within the
        // filter's ε: 0.98 | 0.01 next to the edge).
        let step = [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        for y in [0, 16, 31] {
            let got = row(&q, y, 26..36);
            assert!(
                got.iter().zip(step).all(|(g, s)| (g - s).abs() <= 0.025),
                "row {y}: {got:?}"
            );
        }
        assert_eq!((q.get(0, 0), q.get(63, 31)), (1.0, 0.0));
    }

    #[test]
    fn the_guide_is_srgb_over_white() {
        let mut img = Raster::new(2, 1);
        img.set(0, 0, Rgba::new(0.214_041_14, 0.0, 1.0, 1.0));
        let g = Guide::new(&img);
        assert_eq!(g.rgb, vec![[128, 0, 255], [255, 255, 255]]);
    }
}
