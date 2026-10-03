//! The way into a model and back out: what colours it sees, resampling to
//! its input size, its tensor layouts, and upscaling its output onto the
//! image.
//!
//! The image arrives premultiplied in linear light (the engine's format).
//! A model sees it composited over white and sRGB-encoded, as the photos
//! it was trained on were; resampling averages in linear light (where
//! light mixes) and the encoding is applied to the small result.

use lumenply_tiles::{Raster, Rgba};
use rayon::prelude::*;

/// sRGB encoding of linear light (both 0..=1; out-of-range input clamps).
#[inline]
pub(crate) fn encode_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v >= 1.0 {
        1.0
    } else if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Inverse of [`encode_srgb`].
#[inline]
pub(crate) fn decode_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// A pixel composited over white: linear RGB.
#[inline]
pub(crate) fn over_white(p: Rgba) -> [f32; 3] {
    let k = 1.0 - p.a.clamp(0.0, 1.0);
    [p.r + k, p.g + k, p.b + k]
}

#[inline]
pub(crate) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// SAM's resize: the longer side becomes `target`, the other keeps the
/// aspect ratio, rounded half up as `ResizeLongestSide` does (never 0).
pub(crate) fn longest_side(w: u32, h: u32, target: u32) -> (u32, u32) {
    let s = target as f64 / w.max(h).max(1) as f64;
    let f = |v: u32| ((v as f64 * s + 0.5) as u32).max(1);
    (f(w), f(h))
}

/// One output sample of a 1-D resampling: the first source index it reads
/// and the weights of that and the following sources (summing to 1).
pub(crate) type Taps = (usize, Vec<f32>);

/// The taps resampling `src` samples to `dst`: shrinking averages the
/// source area each output sample covers (partial pixels by their
/// overlap); enlarging interpolates linearly between pixel centres,
/// clamped at the ends.
pub(crate) fn taps(src: usize, dst: usize) -> Vec<Taps> {
    assert!(src > 0 && dst > 0, "resampling an empty axis");
    if src == dst {
        return (0..dst).map(|i| (i, vec![1.0])).collect();
    }
    let r = src as f64 / dst as f64;
    if r > 1.0 {
        (0..dst)
            .map(|d| {
                let (a, b) = (d as f64 * r, (d + 1) as f64 * r);
                let i0 = a.floor() as usize;
                let i1 = (b.ceil() as usize).min(src);
                let w = (i0..i1)
                    .map(|i| {
                        let lo = (i as f64).max(a);
                        let hi = ((i + 1) as f64).min(b);
                        ((hi - lo).max(0.0) / r) as f32
                    })
                    .collect();
                (i0, w)
            })
            .collect()
    } else {
        (0..dst)
            .map(|d| {
                let u = ((d as f64 + 0.5) * r - 0.5).clamp(0.0, (src - 1) as f64);
                let i0 = u.floor() as usize;
                let t = (u - i0 as f64) as f32;
                if i0 + 1 < src && t > 0.0 {
                    (i0, vec![1.0 - t, t])
                } else {
                    (i0, vec![1.0])
                }
            })
            .collect()
    }
}

/// Resize a `w`×`h` image read through `pixel(x, y)` to `dw`×`dh` with
/// [`taps`] on each axis (rows first, both passes in parallel).
pub(crate) fn resize<F>(w: usize, h: usize, dw: usize, dh: usize, pixel: F) -> Vec<[f32; 3]>
where
    F: Fn(usize, usize) -> [f32; 3] + Sync,
{
    let tx = taps(w, dw);
    let ty = taps(h, dh);
    let mut mid = vec![[0f32; 3]; dw * h];
    mid.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        for (o, (x0, ws)) in row.iter_mut().zip(&tx) {
            let mut acc = [0f32; 3];
            for (k, &wt) in ws.iter().enumerate() {
                let p = pixel(x0 + k, y);
                for c in 0..3 {
                    acc[c] += p[c] * wt;
                }
            }
            *o = acc;
        }
    });
    let mut out = vec![[0f32; 3]; dw * dh];
    out.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        let (y0, ws) = &ty[y];
        for (k, &wt) in ws.iter().enumerate() {
            let src = &mid[(y0 + k) * dw..(y0 + k + 1) * dw];
            for (o, s) in row.iter_mut().zip(src) {
                for c in 0..3 {
                    o[c] += s[c] * wt;
                }
            }
        }
    });
    out
}

/// `img` over white, resized to `dw`×`dh` (linear RGB).
pub(crate) fn resize_raster(img: &Raster, dw: usize, dh: usize) -> Vec<[f32; 3]> {
    let w = img.width as usize;
    resize(w, img.height as usize, dw, dh, |x, y| {
        over_white(img.pixels[y * w + x])
    })
}

/// SAM's pixel mean (sRGB, 0..=255): padding with it reads as zero once
/// the encoder has normalised, exactly as SAM pads after normalising.
pub(crate) const SAM_MEAN: [f32; 3] = [123.675, 116.28, 103.53];

/// The MobileSAM encoder's input (`input_image`, HWC `[size, size, 3]`,
/// sRGB 0..=255; the model normalises): the `rw`×`rh` resized image
/// (linear) in the top-left corner, padded right and below with
/// [`SAM_MEAN`].
pub(crate) fn sam_input(small: &[[f32; 3]], rw: usize, rh: usize, size: usize) -> Vec<f32> {
    assert_eq!(small.len(), rw * rh);
    assert!(rw <= size && rh <= size);
    let mut out = Vec::with_capacity(size * size * 3);
    for y in 0..size {
        for x in 0..size {
            if x < rw && y < rh {
                let p = small[y * rw + x];
                out.extend(p.map(|v| encode_srgb(v) * 255.0));
            } else {
                out.extend(SAM_MEAN);
            }
        }
    }
    out
}

/// ImageNet normalisation (BiRefNet's preprocessor_config.json).
pub(crate) const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
pub(crate) const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// NCHW `[1, 3, h, w]` of `(sRGB − mean) / std` for linear pixels.
pub(crate) fn imagenet_nchw(small: &[[f32; 3]]) -> Vec<f32> {
    let n = small.len();
    let mut out = vec![0f32; 3 * n];
    for (i, p) in small.iter().enumerate() {
        for c in 0..3 {
            out[c * n + i] = (encode_srgb(p[c]) - IMAGENET_MEAN[c]) / IMAGENET_STD[c];
        }
    }
    out
}

/// Bilinear samples of `src` (`sw`×`sh`) on a `dw`×`dh` grid. Grid pixel
/// `x` (centre `x + ½`) reads source position `(x + ½)·kx − ½` (pixel
/// centres at whole numbers), clamped to the source; likewise `y` with
/// `ky`. `kx` is source pixels per grid pixel.
pub(crate) fn upsample(
    src: &[f32],
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    kx: f64,
    ky: f64,
) -> Vec<f32> {
    assert_eq!(src.len(), sw * sh);
    let xs = bilinear_axis(dw, sw, kx);
    let ys = bilinear_axis(dh, sh, ky);
    let mut out = vec![0f32; dw * dh];
    out.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        let (y0, y1, ty) = ys[y];
        let (r0, r1) = (&src[y0 * sw..(y0 + 1) * sw], &src[y1 * sw..(y1 + 1) * sw]);
        for (o, &(x0, x1, tx)) in row.iter_mut().zip(&xs) {
            let a = r0[x0] + (r0[x1] - r0[x0]) * tx;
            let b = r1[x0] + (r1[x1] - r1[x0]) * tx;
            *o = a + (b - a) * ty;
        }
    });
    out
}

/// For each of `n` grid pixels, the two source samples (of `len`) it
/// reads and the weight of the second, as [`upsample`] maps them.
pub(crate) fn bilinear_axis(n: usize, len: usize, k: f64) -> Vec<(usize, usize, f32)> {
    (0..n)
        .map(|d| {
            let u = ((d as f64 + 0.5) * k - 0.5).clamp(0.0, (len - 1) as f64);
            let i0 = u.floor() as usize;
            (i0, (i0 + 1).min(len - 1), (u - i0 as f64) as f32)
        })
        .collect()
}

/// A content key of `img`: blake3 over its size and pixel bits (bands of
/// rows hashed in parallel, then the band hashes in order).
pub(crate) fn content_key(img: &Raster) -> [u8; 32] {
    let band = (img.width as usize * 64).max(1);
    let bands: Vec<[u8; 32]> = img
        .pixels
        .par_chunks(band)
        .map(|chunk| {
            let mut buf = Vec::with_capacity(chunk.len() * 16);
            for p in chunk {
                for v in [p.r, p.g, p.b, p.a] {
                    buf.extend_from_slice(&v.to_le_bytes());
                }
            }
            *blake3::hash(&buf).as_bytes()
        })
        .collect();
    let mut h = blake3::Hasher::new();
    h.update(&img.width.to_le_bytes());
    h.update(&img.height.to_le_bytes());
    for b in &bands {
        h.update(b);
    }
    *h.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn taps_average_areas_when_shrinking_and_interpolate_when_enlarging() {
        assert_eq!(taps(4, 2), vec![(0, vec![0.5, 0.5]), (2, vec![0.5, 0.5])]);
        // 3 → 2: each output covers 1.5 source pixels.
        let t = taps(3, 2);
        assert_eq!((t[0].0, t[1].0), (0, 1));
        assert!(close(t[0].1[0], 2.0 / 3.0) && close(t[0].1[1], 1.0 / 3.0));
        assert!(close(t[1].1[0], 1.0 / 3.0) && close(t[1].1[1], 2.0 / 3.0));
        // 2 → 4: centres at −¼ (clamped), ¼, ¾, 1¼ (clamped).
        assert_eq!(
            taps(2, 4),
            vec![
                (0, vec![1.0]),
                (0, vec![0.75, 0.25]),
                (0, vec![0.25, 0.75]),
                (1, vec![1.0])
            ]
        );
        assert_eq!(taps(1, 3), vec![(0, vec![1.0]); 3]);
        assert_eq!(taps(5, 5)[3], (3, vec![1.0]));
    }

    #[test]
    fn resize_gives_the_area_means() {
        // A 3×2 ramp: value = x + 10·y in every channel.
        let px = |x: usize, y: usize| [(x + 10 * y) as f32; 3];
        // To 2×1: rows average to 5 + x; columns as taps(3, 2).
        let out = resize(3, 2, 2, 1, px);
        assert!(close(out[0][0], 5.0 + 1.0 / 3.0), "{:?}", out);
        assert!(close(out[1][2], 5.0 + 5.0 / 3.0), "{:?}", out);
        // Identity keeps every value.
        let same = resize(3, 2, 3, 2, px);
        assert_eq!(same[4], [11.0; 3]);
        // Enlarging 2×1 → 4×1.
        let up = resize(2, 1, 4, 1, |x, _| [x as f32 * 8.0; 3]);
        assert_eq!(
            up.iter().map(|p| p[1]).collect::<Vec<_>>(),
            vec![0.0, 2.0, 6.0, 8.0]
        );
    }

    #[test]
    fn longest_side_matches_sam() {
        // 1205 · 1024 / 1800 = 685.51 → 686.
        assert_eq!(longest_side(1800, 1205, 1024), (1024, 686));
        assert_eq!(longest_side(1205, 1800, 1024), (686, 1024));
        assert_eq!(longest_side(500, 500, 1024), (1024, 1024));
        assert_eq!(longest_side(3000, 10, 1024), (1024, 3));
        assert_eq!(longest_side(5000, 1, 1024), (1024, 1));
    }

    #[test]
    fn colours_are_composited_over_white_and_encoded() {
        assert_eq!(over_white(Rgba::new(0.2, 0.1, 0.0, 1.0)), [0.2, 0.1, 0.0]);
        assert_eq!(over_white(Rgba::new(0.1, 0.0, 0.0, 0.5)), [0.6, 0.5, 0.5]);
        assert_eq!(over_white(Rgba::TRANSPARENT), [1.0; 3]);
        // Linear 0.214041 is sRGB ½.
        assert!(close(encode_srgb(0.214_041_14), 0.5));
        assert!(close(decode_srgb(0.5), 0.214_041_14));
        assert_eq!(encode_srgb(2.0), 1.0);
        assert_eq!(sigmoid(0.0), 0.5);
        assert!(close(sigmoid(2.0), 0.880_797));
    }

    #[test]
    fn sam_input_is_hwc_0_255_padded_with_the_mean() {
        // A 2×1 image (black, white) in a 3×3 frame.
        let t = sam_input(&[[0.0; 3], [1.0; 3]], 2, 1, 3);
        assert_eq!(t.len(), 27);
        assert_eq!(&t[0..3], &[0.0, 0.0, 0.0]);
        assert_eq!(&t[3..6], &[255.0, 255.0, 255.0]);
        assert_eq!(&t[6..9], &SAM_MEAN, "right of the image");
        assert_eq!(&t[9..12], &SAM_MEAN, "below the image");
        assert_eq!(&t[24..27], &SAM_MEAN);
    }

    #[test]
    fn imagenet_input_is_planar_and_normalised() {
        // Two pixels: sRGB ½ grey, then white.
        let t = imagenet_nchw(&[[0.214_041_14; 3], [1.0; 3]]);
        assert_eq!(t.len(), 6);
        // Plane R: (0.5 − 0.485) / 0.229, (1 − 0.485) / 0.229.
        assert!(close(t[0], 0.065_502), "{}", t[0]);
        assert!(close(t[1], 2.248_908), "{}", t[1]);
        // Plane B starts at 2·n: (0.5 − 0.406) / 0.225.
        assert!(close(t[4], 0.417_778), "{}", t[4]);
    }

    #[test]
    fn upsample_maps_pixel_centres() {
        // 2 → 4 (half a source pixel per output pixel).
        let up = upsample(&[0.0, 1.0], 2, 1, 4, 1, 0.5, 1.0);
        assert_eq!(up, vec![0.0, 0.25, 0.75, 1.0]);
        // 2×2 → 3×3 at k = ⅔: centres at −⅙ (clamped), ½, 7⁄6 (clamped).
        let up = upsample(&[0.0, 1.0, 2.0, 3.0], 2, 2, 3, 3, 2.0 / 3.0, 2.0 / 3.0);
        assert!(close(up[4], 1.5), "{up:?}");
        assert_eq!(up[0], 0.0);
        assert_eq!(up[8], 3.0);
        assert!(close(up[1], 0.5));
    }

    #[test]
    fn the_content_key_follows_every_pixel_and_the_size() {
        let a = Raster::filled(300, 70, Rgba::new(0.1, 0.2, 0.3, 1.0));
        let mut b = a.clone();
        assert_eq!(content_key(&a), content_key(&b));
        b.set(299, 69, Rgba::new(0.1, 0.2, 0.3001, 1.0));
        assert_ne!(content_key(&a), content_key(&b));
        let c = Raster::filled(70, 300, Rgba::new(0.1, 0.2, 0.3, 1.0));
        assert_ne!(content_key(&a), content_key(&c), "same pixels, other shape");
    }
}
