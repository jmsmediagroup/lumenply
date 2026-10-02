//! Pixel filters applied to a whole tile store (destructive filters) or to
//! a dense raster (live filter layers in the compositor).
//!
//! Blurs run on premultiplied colour, which is what keeps transparent edges
//! from darkening. Gaussian blur is three box passes, whose result is within
//! a few percent of a true Gaussian and far cheaper.

pub use nge_doc::Filter;
use nge_doc::{box_radius, sane_radius};
use nge_tiles::{Raster, Rect, Rgba, TileStore};

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
    TileStore::from_raster(&filter_raster(&src, filter), area.x, area.y)
}

/// Run a filter over a dense raster (edges treated as transparent).
pub fn filter_raster(src: &Raster, filter: &Filter) -> Raster {
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
    }
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
fn box_blur(src: &Raster, dst: &mut Raster, r: i32) {
    let (w, h) = (src.width as i32, src.height as i32);
    let n = (2 * r + 1) as f32;
    let mut tmp = Raster::new(src.width, src.height);
    // Horizontal pass.
    for y in 0..h {
        let mut sum = [0f32; 4];
        let at = |x: i32| -> Rgba {
            if x < 0 || x >= w {
                Rgba::TRANSPARENT
            } else {
                src.get(x as u32, y as u32)
            }
        };
        for x in -r..=r {
            add(&mut sum, at(x), 1.0);
        }
        for x in 0..w {
            tmp.set(
                x as u32,
                y as u32,
                Rgba::new(sum[0] / n, sum[1] / n, sum[2] / n, sum[3] / n),
            );
            add(&mut sum, at(x + r + 1), 1.0);
            add(&mut sum, at(x - r), -1.0);
        }
    }
    // Vertical pass.
    for x in 0..w {
        let mut sum = [0f32; 4];
        let at = |y: i32| -> Rgba {
            if y < 0 || y >= h {
                Rgba::TRANSPARENT
            } else {
                tmp.get(x as u32, y as u32)
            }
        };
        for y in -r..=r {
            add(&mut sum, at(y), 1.0);
        }
        for y in 0..h {
            dst.set(
                x as u32,
                y as u32,
                Rgba::new(sum[0] / n, sum[1] / n, sum[2] / n, sum[3] / n),
            );
            add(&mut sum, at(y + r + 1), 1.0);
            add(&mut sum, at(y - r), -1.0);
        }
    }
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
