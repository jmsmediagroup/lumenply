//! Pixel filters applied to a whole tile store (destructive filters) or to
//! a dense raster (live filter layers in the compositor).
//!
//! Blurs run on premultiplied colour, which is what keeps transparent edges
//! from darkening. Gaussian blur is three box passes, whose result is within
//! a few percent of a true Gaussian and far cheaper.

pub use lumenply_doc::Filter;
use lumenply_doc::{box_radius, sane_radius};
use lumenply_tiles::{Raster, Rect, Rgba, TileStore};
use rayon::prelude::*;

/// Apply a filter to every painted pixel of `store`.
pub fn apply_filter(store: &TileStore, filter: &Filter) -> TileStore {
    let Some(bounds) = store.content_bounds() else {
        return TileStore::new();
    };
    let pad = filter.pad();
    let area = Rect::new(
        bounds.x - pad,
        bounds.y - pad,
        bounds.w + 2 * pad as u32,
        bounds.h + 2 * pad as u32,
    );
    let src = store.to_raster(area);
    TileStore::from_raster(&filter_raster(&src, filter, (area.x, area.y)), area.x, area.y)
}

/// Run a filter over a dense raster (edges treated as transparent).
/// `origin` is the canvas position of the raster's top-left pixel, so
/// position-seeded filters (noise) agree across tiles and re-renders.
pub fn filter_raster(src: &Raster, filter: &Filter, origin: (i32, i32)) -> Raster {
    match filter {
        Filter::GaussianBlur { radius } => gaussian(src, *radius),
        Filter::BoxBlur { radius } => {
            let mut tmp = src.clone();
            box_blur(src, &mut tmp, sane_radius(*radius).round() as i32);
            tmp
        }
        Filter::Sharpen { amount, radius } => {
            let blurred = gaussian(src, *radius);
            let mut out = src.clone();
            for (o, (s, b)) in out
                .pixels
                .iter_mut()
                .zip(src.pixels.iter().zip(blurred.pixels.iter()))
            {
                let k = *amount;
                let a = s.a;
                // Keep premultiplied colour inside [0, a].
                o.r = (s.r + k * (s.r - b.r)).clamp(0.0, a);
                o.g = (s.g + k * (s.g - b.g)).clamp(0.0, a);
                o.b = (s.b + k * (s.b - b.b)).clamp(0.0, a);
                o.a = a;
            }
            out
        }
        Filter::Noise { amount } => noise(src, *amount, origin),
        Filter::MotionBlur { angle, distance } => motion_blur(src, *angle, *distance),
        Filter::Median { radius } => median(src, Filter::median_radius(*radius)),
        Filter::HighPass { radius } => high_pass(src, *radius),
    }
}

/// Monochromatic uniform noise in [−amount, amount], added to the straight
/// colour, seeded by absolute pixel position.
fn noise(src: &Raster, amount: f32, origin: (i32, i32)) -> Raster {
    let amount = if amount.is_finite() {
        amount.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if amount <= 0.0 {
        return src.clone();
    }
    let w = src.width as usize;
    let mut out = src.clone();
    out.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let py = origin.1.wrapping_add(y as i32);
        for (x, p) in row.iter_mut().enumerate() {
            if p.a <= 0.0 {
                continue;
            }
            let px = origin.0.wrapping_add(x as i32);
            let v = (hash2(px, py) - 0.5) * 2.0 * amount;
            let [r, g, b, a] = p.to_straight();
            *p = Rgba::from_straight(
                (r + v).clamp(0.0, 1.0),
                (g + v).clamp(0.0, 1.0),
                (b + v).clamp(0.0, 1.0),
                a,
            );
        }
    });
    out
}

/// Deterministic hash of a pixel position onto [0, 1).
#[inline]
fn hash2(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x85EB_CA6B) ^ (y as u32).wrapping_mul(0xC2B2_AE35);
    h ^= h >> 13;
    h = h.wrapping_mul(0x27D4_EB2F);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Average along a line through each pixel (premultiplied, so edges
/// against transparency stay correct).
fn motion_blur(src: &Raster, angle: f32, distance: f32) -> Raster {
    let distance = sane_radius(distance);
    let n = distance.round() as i32;
    if n < 1 {
        return src.clone();
    }
    let angle = if angle.is_finite() {
        angle.to_radians()
    } else {
        0.0
    };
    let (dx, dy) = (angle.cos(), angle.sin());
    let w = src.width as usize;
    let mut out = src.clone();
    out.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let mut sum = [0f32; 4];
            for i in 0..=n {
                let t = i as f32 - n as f32 / 2.0;
                let s = sample(src, x as f32 + dx * t, y as f32 + dy * t);
                sum[0] += s.r;
                sum[1] += s.g;
                sum[2] += s.b;
                sum[3] += s.a;
            }
            let k = 1.0 / (n + 1) as f32;
            *p = Rgba::new(sum[0] * k, sum[1] * k, sum[2] * k, sum[3] * k);
        }
    });
    out
}

/// Bilinear sample of a raster, transparent outside.
#[inline]
fn sample(src: &Raster, x: f32, y: f32) -> Rgba {
    let x0 = x.floor();
    let y0 = y.floor();
    let (fx, fy) = (x - x0, y - y0);
    let at = |ix: i32, iy: i32| -> Rgba {
        if ix < 0 || iy < 0 || ix >= src.width as i32 || iy >= src.height as i32 {
            Rgba::TRANSPARENT
        } else {
            src.get(ix as u32, iy as u32)
        }
    };
    let (ix, iy) = (x0 as i32, y0 as i32);
    let (p00, p10, p01, p11) = (at(ix, iy), at(ix + 1, iy), at(ix, iy + 1), at(ix + 1, iy + 1));
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    Rgba::new(
        lerp(lerp(p00.r, p10.r, fx), lerp(p01.r, p11.r, fx), fy),
        lerp(lerp(p00.g, p10.g, fx), lerp(p01.g, p11.g, fx), fy),
        lerp(lerp(p00.b, p10.b, fx), lerp(p01.b, p11.b, fx), fy),
        lerp(lerp(p00.a, p10.a, fx), lerp(p01.a, p11.a, fx), fy),
    )
}

/// Per-channel median over a square window (premultiplied channels).
fn median(src: &Raster, r: i32) -> Raster {
    let (w, h) = (src.width as i32, src.height as i32);
    let wu = src.width as usize;
    let mut out = src.clone();
    out.pixels.par_chunks_mut(wu).enumerate().for_each(|(y, row)| {
        let y = y as i32;
        let mut win: Vec<f32> = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);
        for (x, p) in row.iter_mut().enumerate() {
            let x = x as i32;
            let mut channel = |pick: fn(&Rgba) -> f32| -> f32 {
                win.clear();
                for sy in (y - r).max(0)..=(y + r).min(h - 1) {
                    for sx in (x - r).max(0)..=(x + r).min(w - 1) {
                        win.push(pick(&src.pixels[sy as usize * wu + sx as usize]));
                    }
                }
                let mid = win.len() / 2;
                *win.select_nth_unstable_by(mid, f32::total_cmp).1
            };
            *p = Rgba::new(
                channel(|p| p.r),
                channel(|p| p.g),
                channel(|p| p.b),
                channel(|p| p.a),
            );
        }
    });
    out
}

/// Mid grey plus the detail the blur removes. Transparent pixels stay
/// transparent.
fn high_pass(src: &Raster, radius: f32) -> Raster {
    let blurred = gaussian(src, radius);
    let mut out = src.clone();
    out.pixels
        .par_iter_mut()
        .zip(blurred.pixels.par_iter())
        .for_each(|(p, b)| {
            if p.a <= 0.0 {
                *p = Rgba::TRANSPARENT;
                return;
            }
            let [sr, sg, sb, a] = p.to_straight();
            let [br, bg, bb, ba] = b.to_straight();
            let g = |s: f32, bl: f32, w: f32| (0.5 + s - bl * w).clamp(0.0, 1.0);
            // Weight the blurred term by its own coverage so edges against
            // transparency don't darken.
            let w = if ba > 0.0 { 1.0 } else { 0.0 };
            *p = Rgba::from_straight(g(sr, br, w), g(sg, bg, w), g(sb, bb, w), a);
        });
    out
}

fn gaussian(src: &Raster, radius: f32) -> Raster {
    let r = box_radius(radius);
    let mut a = src.clone();
    let mut b = Raster::new(src.width, src.height);
    for _ in 0..3 {
        box_blur(&a, &mut b, r);
        std::mem::swap(&mut a, &mut b);
    }
    a
}

/// Separable box blur with transparent edges (`src` → `dst`).
///
/// Both passes run row-parallel: the vertical pass works on a transposed
/// copy so it is a horizontal pass too. The per-pixel math is identical to
/// the serial version, so results are bit-exact.
fn box_blur(src: &Raster, dst: &mut Raster, r: i32) {
    if src.width == 0 || src.height == 0 {
        return;
    }
    let mut tmp = Raster::new(src.width, src.height);
    blur_rows(src, &mut tmp, r);
    let t = transpose(&tmp);
    let mut t2 = Raster::new(t.width, t.height);
    blur_rows(&t, &mut t2, r);
    *dst = transpose(&t2);
}

/// Running-sum box blur of each row, rows in parallel.
fn blur_rows(src: &Raster, dst: &mut Raster, r: i32) {
    let w = src.width as i32;
    let n = (2 * r + 1) as f32;
    let wu = src.width as usize;
    dst.pixels
        .par_chunks_mut(wu)
        .zip(src.pixels.par_chunks(wu))
        .for_each(|(drow, srow)| {
            let mut sum = [0f32; 4];
            let at = |x: i32| -> Rgba {
                if x < 0 || x >= w {
                    Rgba::TRANSPARENT
                } else {
                    srow[x as usize]
                }
            };
            for x in -r..=r {
                add(&mut sum, at(x), 1.0);
            }
            for x in 0..w {
                drow[x as usize] = Rgba::new(sum[0] / n, sum[1] / n, sum[2] / n, sum[3] / n);
                add(&mut sum, at(x + r + 1), 1.0);
                add(&mut sum, at(x - r), -1.0);
            }
        });
}

fn transpose(src: &Raster) -> Raster {
    let (w, h) = (src.width as usize, src.height as usize);
    let mut out = Raster::new(src.height, src.width);
    out.pixels.par_chunks_mut(h).enumerate().for_each(|(x, row)| {
        for (y, p) in row.iter_mut().enumerate() {
            *p = src.pixels[y * w + x];
        }
    });
    out
}

#[inline]
fn add(sum: &mut [f32; 4], p: Rgba, k: f32) {
    sum[0] += p.r * k;
    sum[1] += p.g * k;
    sum[2] += p.b * k;
    sum[3] += p.a * k;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_deterministic_bounded_and_position_seeded() {
        let src = Raster::filled(64, 64, Rgba::from_straight(0.5, 0.5, 0.5, 1.0));
        let a = filter_raster(&src, &Filter::Noise { amount: 0.2 }, (100, 100));
        let b = filter_raster(&src, &Filter::Noise { amount: 0.2 }, (100, 100));
        assert_eq!(a.pixels, b.pixels, "same origin, same noise");
        let c = filter_raster(&src, &Filter::Noise { amount: 0.2 }, (101, 100));
        assert_ne!(a.pixels, c.pixels, "different origin, different noise");

        let mut mean = 0.0;
        for (p, s) in a.pixels.iter().zip(src.pixels.iter()) {
            let d = p.to_straight()[0] - s.to_straight()[0];
            assert!(d.abs() <= 0.2 + 1e-3, "bounded: {d}");
            assert!((p.a - s.a).abs() < 1e-6, "alpha untouched");
            mean += d;
        }
        mean /= a.pixels.len() as f32;
        assert!(mean.abs() < 0.01, "roughly zero-mean: {mean}");

        // Tiles agree: the overlapping pixel of two offset renders matches.
        let shifted = filter_raster(&src, &Filter::Noise { amount: 0.2 }, (110, 100));
        assert_eq!(a.get(30, 5), shifted.get(20, 5));

        let zero = filter_raster(&src, &Filter::Noise { amount: 0.0 }, (0, 0));
        assert_eq!(zero.pixels, src.pixels, "amount 0 is identity");
    }

    #[test]
    fn motion_blur_smears_along_its_angle_only() {
        let mut src = Raster::new(64, 64);
        for y in 0..64 {
            for x in 30..34 {
                src.set(x, y, Rgba::WHITE);
            }
        }
        let f = Filter::MotionBlur {
            angle: 0.0,
            distance: 20.0,
        };
        let out = filter_raster(&src, &f, (0, 0));
        assert!(out.get(22, 32).a > 0.05, "smeared left");
        assert!(out.get(41, 32).a > 0.05, "smeared right");
        assert!(out.get(32, 32).a < 1.0, "centre thinned");
        // Vertical motion leaves a vertical bar intact (edges aside).
        let f = Filter::MotionBlur {
            angle: 90.0,
            distance: 20.0,
        };
        let v = filter_raster(&src, &f, (0, 0));
        assert!(v.get(22, 32).a < 1e-3, "no horizontal spread");
        assert!((v.get(32, 32).a - 1.0).abs() < 1e-3, "bar interior unchanged");
    }

    #[test]
    fn median_removes_salt_but_keeps_edges() {
        let mut src = Raster::filled(32, 32, Rgba::from_straight(0.2, 0.2, 0.2, 1.0));
        src.set(16, 16, Rgba::WHITE); // salt
        for y in 0..32 {
            for x in 0..8 {
                src.set(x, y, Rgba::from_straight(0.9, 0.9, 0.9, 1.0));
            }
        }
        let out = filter_raster(&src, &Filter::Median { radius: 2.0 }, (0, 0));
        let p = out.get(16, 16).to_straight();
        assert!((p[0] - 0.2).abs() < 0.01, "salt removed: {p:?}");
        let edge_in = out.get(6, 16).to_straight();
        let edge_out = out.get(9, 16).to_straight();
        assert!((edge_in[0] - 0.9).abs() < 0.01, "edge kept: {edge_in:?}");
        assert!((edge_out[0] - 0.2).abs() < 0.01, "edge kept: {edge_out:?}");
    }

    #[test]
    fn high_pass_flattens_flat_areas_to_mid_grey() {
        let src = Raster::filled(64, 64, Rgba::from_straight(0.8, 0.3, 0.1, 1.0));
        let out = filter_raster(&src, &Filter::HighPass { radius: 4.0 }, (0, 0));
        let p = out.get(32, 32).to_straight();
        assert!(
            (p[0] - 0.5).abs() < 0.02 && (p[1] - 0.5).abs() < 0.02 && (p[2] - 0.5).abs() < 0.02,
            "flat input becomes mid grey: {p:?}"
        );
        assert!((p[3] - 1.0).abs() < 1e-5);
        // Transparent stays transparent.
        let mut src = Raster::new(16, 16);
        src.set(8, 8, Rgba::WHITE);
        let out = filter_raster(&src, &Filter::HighPass { radius: 2.0 }, (0, 0));
        assert_eq!(out.get(1, 1), Rgba::TRANSPARENT);
    }

    /// Degenerate radii (negative, NaN, infinite) must neither panic nor
    /// size absurd paddings; they act as a no-op blur.
    #[test]
    fn degenerate_radii_are_harmless() {
        assert_eq!(Filter::BoxBlur { radius: -5.0 }.pad(), 0);
        assert_eq!(Filter::BoxBlur { radius: f32::NAN }.pad(), 0);
        assert_eq!(
            Filter::GaussianBlur {
                radius: f32::NEG_INFINITY
            }
            .pad(),
            3
        );
        assert!(
            Filter::GaussianBlur {
                radius: f32::INFINITY
            }
            .pad()
                <= 2000
        );
        assert!(box_radius(f32::NAN) >= 1);

        let mut r = Raster::new(8, 8);
        r.set(4, 4, Rgba::WHITE);
        let src = TileStore::from_raster(&r, 0, 0);
        for f in [
            Filter::BoxBlur { radius: -5.0 },
            Filter::BoxBlur { radius: f32::NAN },
            Filter::Sharpen {
                amount: 1.0,
                radius: -1.0,
            },
        ] {
            let out = apply_filter(&src, &f);
            let p = out.get_pixel(4, 4);
            assert!(p.a.is_finite(), "{}: alpha {}", f.name(), p.a);
        }
    }

    fn total_alpha(s: &TileStore) -> f32 {
        let b = s.content_bounds().unwrap();
        let mut a = 0.0;
        for y in b.y..b.bottom() {
            for x in b.x..b.right() {
                a += s.get_pixel(x, y).a;
            }
        }
        a
    }

    #[test]
    fn blur_spreads_but_conserves_coverage() {
        let mut r = Raster::new(40, 40);
        for y in 15..25 {
            for x in 15..25 {
                r.set(x, y, Rgba::from_straight(1.0, 0.0, 0.0, 1.0));
            }
        }
        let src = TileStore::from_raster(&r, 0, 0);
        let out = apply_filter(&src, &Filter::GaussianBlur { radius: 4.0 });
        let centre = out.get_pixel(20, 20);
        assert!(centre.a > 0.9 && centre.a < 1.0 + 1e-4, "{centre:?}");
        let edge = out.get_pixel(14, 20);
        assert!(edge.a > 0.05 && edge.a < 0.6, "blur reaches outside: {edge:?}");
        assert!(out.get_pixel(0, 20).is_transparent());
        // Colour stays pure red (premultiplied blur does not darken edges).
        let [er, eg, _, _] = edge.to_straight();
        assert!((er - 1.0).abs() < 1e-4 && eg.abs() < 1e-4, "{edge:?}");
        assert!((total_alpha(&out) - total_alpha(&src)).abs() / total_alpha(&src) < 0.01);
    }

    #[test]
    fn sharpen_increases_local_contrast() {
        let mut r = Raster::new(40, 10);
        for x in 0..40 {
            let v = if x < 20 { 0.3 } else { 0.7 };
            for y in 0..10 {
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let src = TileStore::from_raster(&r, 0, 0);
        let out = apply_filter(
            &src,
            &Filter::Sharpen {
                amount: 1.0,
                radius: 2.0,
            },
        );
        assert!(out.get_pixel(19, 5).r < 0.3, "dark side gets darker at the edge");
        assert!(
            out.get_pixel(20, 5).r > 0.7,
            "bright side gets brighter at the edge"
        );
        assert!((out.get_pixel(5, 5).r - 0.3).abs() < 1e-3, "flat areas unchanged");
        assert!(out.get_pixel(20, 5).r <= 1.0);
    }

    #[test]
    fn empty_store_stays_empty() {
        assert!(apply_filter(&TileStore::new(), &Filter::BoxBlur { radius: 3.0 }).is_empty());
    }
}
