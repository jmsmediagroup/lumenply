//! Mosaic, Emboss, Find Edges, Surface Blur, Lens Blur and Dust & Scratches
//! kernels (dispatched from [`crate::filters::filter_raster`]).
//!
//! Like the other filters they read a dense raster whose edges are
//! transparent and whose top-left pixel sits at canvas position `origin`.
//! Filters that judge brightness (emboss, edges, the thresholds of surface
//! blur and dust & scratches) do so on gamma-encoded values, as adjustments
//! do (ADR 0005), so their thresholds match Photoshop's 0–255 levels.

use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_doc::Filter;
use lumenply_tiles::{Raster, Rect, Rgba, TileStore};
use rayon::prelude::*;

/// Straight, gamma-encoded colour and alpha.
#[inline]
fn enc(p: Rgba) -> [f32; 4] {
    if p.a <= 0.0 {
        return [0.0; 4];
    }
    let [r, g, b, a] = p.to_straight();
    [srgb_encode(r), srgb_encode(g), srgb_encode(b), a]
}

#[inline]
fn dec(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba::from_straight(
        srgb_decode(r.clamp(0.0, 1.0)),
        srgb_decode(g.clamp(0.0, 1.0)),
        srgb_decode(b.clamp(0.0, 1.0)),
        a,
    )
}

/// Square cells of `size` px anchored to the canvas origin, so every tile
/// of a live layer cuts the same cells. Each is its pixels' premultiplied
/// mean.
pub(crate) fn mosaic(src: &Raster, size: f32, origin: (i32, i32)) -> Raster {
    let s = Filter::mosaic_cell(size);
    if s <= 1 || src.pixels.is_empty() {
        return src.clone();
    }
    let (w, h) = (src.width as i32, src.height as i32);
    // Cell k spans canvas [k·s, (k+1)·s); in raster coordinates shift by origin.
    let first = |o: i32| o.div_euclid(s);
    let span = |k: i32, o: i32, len: i32| ((k * s - o).max(0), ((k + 1) * s - o).min(len));
    let (cx0, cx1) = (first(origin.0), first(origin.0 + w - 1));
    let (cy0, cy1) = (first(origin.1), first(origin.1 + h - 1));
    let cols = (cx1 - cx0 + 1) as usize;
    let means: Vec<Vec<Rgba>> = (cy0..=cy1)
        .into_par_iter()
        .map(|cy| {
            let (y0, y1) = span(cy, origin.1, h);
            (cx0..=cx1)
                .map(|cx| {
                    let (x0, x1) = span(cx, origin.0, w);
                    let mut sum = [0f64; 4];
                    for y in y0..y1 {
                        let row = &src.pixels[(y * w) as usize..((y + 1) * w) as usize];
                        for p in &row[x0 as usize..x1 as usize] {
                            sum[0] += p.r as f64;
                            sum[1] += p.g as f64;
                            sum[2] += p.b as f64;
                            sum[3] += p.a as f64;
                        }
                    }
                    let n = ((y1 - y0) * (x1 - x0)).max(1) as f64;
                    Rgba::new(
                        (sum[0] / n) as f32,
                        (sum[1] / n) as f32,
                        (sum[2] / n) as f32,
                        (sum[3] / n) as f32,
                    )
                })
                .collect()
        })
        .collect();
    let mut out = src.clone();
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let cells = &means[(first(origin.1 + y as i32) - cy0) as usize];
            for (x, p) in row.iter_mut().enumerate() {
                let k = (first(origin.0 + x as i32) - cx0) as usize;
                *p = cells[k.min(cols - 1)];
            }
        });
    out
}

/// Bilinear sample of a dense scalar field, zero outside.
#[inline]
fn bilinear(f: &[f32], w: i32, h: i32, x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |ix: i32, iy: i32| {
        if ix < 0 || iy < 0 || ix >= w || iy >= h {
            0.0
        } else {
            f[(iy * w + ix) as usize]
        }
    };
    let (ix, iy) = (x0 as i32, y0 as i32);
    let top = at(ix, iy) + (at(ix + 1, iy) - at(ix, iy)) * fx;
    let bottom = at(ix, iy + 1) + (at(ix + 1, iy + 1) - at(ix, iy + 1)) * fx;
    top + (bottom - top) * fy
}

/// Grey relief: mid grey plus the brightness step across each pixel along
/// the light direction (`angle` degrees, counter-clockwise from the right,
/// y up as in Photoshop). Alpha is kept.
pub(crate) fn emboss(src: &Raster, angle: f32, height: f32, amount: f32) -> Raster {
    let h = Filter::emboss_height(height);
    let amount = if amount.is_finite() {
        amount.clamp(0.0, 5.0)
    } else {
        0.0
    };
    let a = if angle.is_finite() {
        angle.to_radians()
    } else {
        0.0
    };
    let (dx, dy) = (a.cos() * h, -a.sin() * h);
    let (w, hh) = (src.width as i32, src.height as i32);
    // Gamma-encoded luminance, premultiplied so shape edges emboss too.
    let lum: Vec<f32> = src
        .pixels
        .par_iter()
        .map(|&p| {
            let [r, g, b, a] = enc(p);
            (0.2126 * r + 0.7152 * g + 0.0722 * b) * a
        })
        .collect();
    let mut out = src.clone();
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, p) in row.iter_mut().enumerate() {
                if p.a <= 0.0 {
                    continue;
                }
                let (fx, fy) = (x as f32, y as f32);
                let lit = bilinear(&lum, w, hh, fx + dx, fy + dy);
                let shade = bilinear(&lum, w, hh, fx - dx, fy - dy);
                let v = 0.5 + amount * (lit - shade) * 0.5;
                *p = dec(v, v, v, p.a);
            }
        });
    out
}

/// Sobel gradient magnitude per channel, inverted: white where flat, the
/// channel's complement along its edges. Alpha is kept.
pub(crate) fn find_edges(src: &Raster) -> Raster {
    let (w, h) = (src.width as i32, src.height as i32);
    // Gamma-encoded colour times alpha (edges against transparency count).
    let e: Vec<[f32; 3]> = src
        .pixels
        .par_iter()
        .map(|&p| {
            let [r, g, b, a] = enc(p);
            [r * a, g * a, b * a]
        })
        .collect();
    let at = |x: i32, y: i32| e[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize];
    let mut out = src.clone();
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as i32;
            for (x, p) in row.iter_mut().enumerate() {
                if p.a <= 0.0 {
                    continue;
                }
                let x = x as i32;
                let mut v = [0f32; 3];
                for (c, o) in v.iter_mut().enumerate() {
                    let k = |dx: i32, dy: i32| at(x + dx, y + dy)[c];
                    let gx = k(1, -1) + 2.0 * k(1, 0) + k(1, 1) - k(-1, -1) - 2.0 * k(-1, 0) - k(-1, 1);
                    let gy = k(-1, 1) + 2.0 * k(0, 1) + k(1, 1) - k(-1, -1) - 2.0 * k(0, -1) - k(1, -1);
                    // A full black-to-white step gives gx = 4.
                    *o = 1.0 - ((gx * gx + gy * gy).sqrt() / 4.0).min(1.0);
                }
                *p = dec(v[0], v[1], v[2], p.a);
            }
        });
    out
}

/// Edge-preserving blur: within a disc of `radius`, each neighbour weighs
/// max(0, 1 − |Δ| / (2.5 · threshold)) per channel (Photoshop's surface
/// blur), times its alpha. Large radii sample the disc on a coarser grid
/// so the cost stays near 300 taps a pixel. Alpha is kept.
pub(crate) fn surface_blur(src: &Raster, radius: f32, threshold: f32) -> Raster {
    let r = Filter::surface_radius(radius);
    let t = if threshold.is_finite() {
        threshold.clamp(0.0, 255.0) / 255.0
    } else {
        0.0
    };
    if t <= 0.0 {
        return src.clone();
    }
    let inv = 1.0 / (2.5 * t);
    let step = ((r as f32 / 10.0).round() as i32).max(1);
    let taps: Vec<(i32, i32)> = (-r..=r)
        .step_by(step as usize)
        .flat_map(|dy| (-r..=r).step_by(step as usize).map(move |dx| (dx, dy)))
        .filter(|(dx, dy)| dx * dx + dy * dy <= r * r)
        .collect();
    let (w, h) = (src.width as i32, src.height as i32);
    let e: Vec<[f32; 4]> = src.pixels.par_iter().map(|&p| enc(p)).collect();
    let mut out = src.clone();
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as i32;
            for (x, p) in row.iter_mut().enumerate() {
                if p.a <= 0.0 {
                    continue;
                }
                let x = x as i32;
                let c = e[(y * w + x) as usize];
                let mut sum = [0f32; 3];
                let mut wsum = [0f32; 3];
                for &(dx, dy) in &taps {
                    let (sx, sy) = (x + dx, y + dy);
                    if sx < 0 || sy < 0 || sx >= w || sy >= h {
                        continue;
                    }
                    let q = e[(sy * w + sx) as usize];
                    if q[3] <= 0.0 {
                        continue;
                    }
                    for k in 0..3 {
                        let wt = (1.0 - (q[k] - c[k]).abs() * inv).max(0.0) * q[3];
                        sum[k] += wt * q[k];
                        wsum[k] += wt;
                    }
                }
                let ch = |k: usize| if wsum[k] > 0.0 { sum[k] / wsum[k] } else { c[k] };
                *p = dec(ch(0), ch(1), ch(2), p.a);
            }
        });
    out
}

/// Disc-kernel blur on premultiplied colour, bright pixels boosted first
/// (by up to 7× at full `highlights`) so they bloom into bokeh discs, then
/// clipped. Rows of the disc are summed with per-row prefix sums: O(radius)
/// per pixel. Outside the raster counts as transparent, as for the other
/// blurs.
pub(crate) fn lens_blur(src: &Raster, radius: f32, highlights: f32) -> Raster {
    let r = Filter::lens_radius(radius);
    if r < 1 {
        return src.clone();
    }
    let boost = if highlights.is_finite() {
        highlights.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (w, h) = (src.width as usize, src.height as usize);
    let energy: Vec<[f32; 4]> = src
        .pixels
        .par_iter()
        .map(|p| {
            let mut gain = 1.0;
            if boost > 0.0 && p.a > 0.0 {
                let m = (p.r.max(p.g).max(p.b) / p.a).min(1.0);
                let k = ((m - 0.6) / 0.4).clamp(0.0, 1.0);
                gain += 6.0 * boost * k * k;
            }
            [p.r * gain, p.g * gain, p.b * gain, p.a]
        })
        .collect();
    let half: Vec<usize> = (-r..=r)
        .map(|dy| (((r * r - dy * dy) as f32).sqrt().floor()) as usize)
        .collect();
    let area: f32 = half.iter().map(|&k| (2 * k + 1) as f32).sum();
    let mut out = src.clone();
    out.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let mut acc = vec![[0f32; 4]; w];
        let mut prefix = vec![[0f64; 4]; w + 1];
        for (j, &k) in half.iter().enumerate() {
            let sy = y as i64 + j as i64 - r as i64;
            if sy < 0 || sy >= h as i64 {
                continue;
            }
            let srow = &energy[sy as usize * w..(sy as usize + 1) * w];
            for (x, v) in srow.iter().enumerate() {
                for c in 0..4 {
                    prefix[x + 1][c] = prefix[x][c] + v[c] as f64;
                }
            }
            for (x, a) in acc.iter_mut().enumerate() {
                let (x0, x1) = (x.saturating_sub(k), (x + k + 1).min(w));
                for c in 0..4 {
                    a[c] += (prefix[x1][c] - prefix[x0][c]) as f32;
                }
            }
        }
        for (p, a) in row.iter_mut().zip(&acc) {
            let al = (a[3] / area).clamp(0.0, 1.0);
            *p = Rgba::new(
                (a[0] / area).clamp(0.0, al),
                (a[1] / area).clamp(0.0, al),
                (a[2] / area).clamp(0.0, al),
                al,
            );
        }
    });
    out
}

/// Median where the pixel stands out from it by more than `threshold`
/// levels in any channel (gamma-encoded), the pixel itself elsewhere.
pub(crate) fn dust_scratches(src: &Raster, radius: f32, threshold: f32) -> Raster {
    let med = crate::filters::median(src, Filter::median_radius(radius));
    let t = if threshold.is_finite() {
        threshold.clamp(0.0, 255.0) / 255.0
    } else {
        0.0
    };
    let mut out = src.clone();
    out.pixels
        .par_iter_mut()
        .zip(med.pixels.par_iter())
        .for_each(|(p, m)| {
            let (a, b) = (enc(*p), enc(*m));
            let d = (0..4).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max);
            if d > t {
                *p = *m;
            }
        });
    out
}

/// A destructive filter that reads like a live one at the canvas border:
/// off-canvas pixels the layer doesn't paint repeat the canvas edge, so a
/// blur of a full-canvas photo doesn't fade to transparent there (and a
/// mosaic's edge cells stay opaque). Off-canvas pixels the layer didn't
/// have stay empty afterwards; layer content past the canvas is filtered
/// as it is.
pub fn apply_filter_in_canvas(store: &TileStore, filter: &Filter, canvas: Rect) -> TileStore {
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
    let orig = store.to_raster(area);
    let mut src = orig.clone();
    let off_canvas = |x: i32, y: i32| !canvas.contains(x, y);
    if !canvas.is_empty() {
        let aw = area.w as usize;
        for y in 0..area.h as i32 {
            for x in 0..area.w as i32 {
                let (gx, gy) = (area.x + x, area.y + y);
                let i = y as usize * aw + x as usize;
                if !off_canvas(gx, gy) || orig.pixels[i].a > 0.0 {
                    continue;
                }
                let (cx, cy) = (
                    gx.clamp(canvas.x, canvas.right() - 1),
                    gy.clamp(canvas.y, canvas.bottom() - 1),
                );
                if area.contains(cx, cy) {
                    src.pixels[i] = orig.get((cx - area.x) as u32, (cy - area.y) as u32);
                }
            }
        }
    }
    let mut out = crate::filters::filter_raster(&src, filter, (area.x, area.y));
    if !canvas.is_empty() {
        let aw = area.w as usize;
        for (i, p) in out.pixels.iter_mut().enumerate() {
            let (gx, gy) = (area.x + (i % aw) as i32, area.y + (i / aw) as i32);
            if off_canvas(gx, gy) && orig.pixels[i].a <= 0.0 {
                *p = Rgba::TRANSPARENT;
            }
        }
    }
    TileStore::from_raster(&out, area.x, area.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::filter_raster;

    fn grey(v: f32) -> Rgba {
        Rgba::new(v, v, v, 1.0)
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn mosaic_cells_are_means_anchored_to_the_canvas() {
        // A horizontal ramp: column x has value x / 100.
        let mut src = Raster::new(40, 8);
        for y in 0..8 {
            for x in 0..40 {
                src.set(x, y, grey(x as f32 / 100.0));
            }
        }
        let out = filter_raster(&src, &Filter::Mosaic { size: 10.0 }, (0, 0));
        // Cell 0 covers x 0..10: mean 0.045; cell 2 covers 20..30: 0.245.
        assert!(close(out.get(0, 0).r, 0.045, 1e-6) && close(out.get(9, 7).r, 0.045, 1e-6));
        assert!(close(out.get(25, 3).r, 0.245, 1e-6));
        // Shifted origin (a tile further right): cells stay on the canvas
        // grid, so canvas x = 20 still starts a cell.
        let shifted = filter_raster(&src, &Filter::Mosaic { size: 10.0 }, (5, 0));
        // Raster x 15..25 is canvas 20..30, the mean of raster 15..25.
        assert!(
            close(shifted.get(15, 0).r, 0.195, 1e-6),
            "{}",
            shifted.get(15, 0).r
        );
        assert!(close(shifted.get(14, 0).r, shifted.get(10, 0).r, 1e-6));
        assert!(!close(shifted.get(14, 0).r, shifted.get(15, 0).r, 1e-3));
    }

    #[test]
    fn emboss_is_mid_grey_on_flat_areas_and_lifts_lit_edges() {
        let mut src = Raster::filled(40, 20, grey(0.2));
        for y in 0..20 {
            for x in 20..40 {
                src.set(x, y, grey(0.8));
            }
        }
        let f = Filter::Emboss {
            angle: 0.0,
            height: 2.0,
            amount: 1.0,
        };
        let out = filter_raster(&src, &f, (0, 0));
        let mid = srgb_decode(0.5);
        assert!(close(out.get(5, 10).r, mid, 1e-4), "flat → 50 % grey");
        assert!(close(out.get(30, 10).r, mid, 1e-4));
        // Light from the right (0°): the step up at x = 20 faces it.
        let lit = out.get(20, 10).r;
        let step = srgb_encode(0.8) - srgb_encode(0.2);
        assert!(close(lit, srgb_decode(0.5 + 0.5 * step), 1e-3), "{lit}");
        assert!(close(out.get(20, 10).a, 1.0, 1e-6));
        // From the left the same edge falls into shadow.
        let f = Filter::Emboss {
            angle: 180.0,
            height: 2.0,
            amount: 1.0,
        };
        let back = filter_raster(&src, &f, (0, 0));
        assert!(close(back.get(20, 10).r, srgb_decode(0.5 - 0.5 * step), 1e-3));
    }

    #[test]
    fn find_edges_whitens_flat_areas_and_darkens_steps() {
        let mut src = Raster::filled(20, 10, grey(0.5));
        for y in 0..10 {
            for x in 10..20 {
                src.set(x, y, Rgba::new(0.0, 0.0, 0.0, 1.0));
            }
        }
        let out = filter_raster(&src, &Filter::FindEdges, (0, 0));
        assert!(close(out.get(3, 5).r, 1.0, 1e-5), "flat → white");
        assert!(close(out.get(15, 5).g, 1.0, 1e-5));
        // On the step from encoded 0.735 to 0: gx = 4 × 0.735 → 1 − 0.735.
        let e = srgb_encode(0.5);
        assert!(close(out.get(9, 5).r, srgb_decode(1.0 - e), 1e-4));
        assert!(close(out.get(10, 5).r, srgb_decode(1.0 - e), 1e-4));
    }

    #[test]
    fn surface_blur_smooths_noise_but_keeps_edges() {
        let mut src = Raster::new(40, 20);
        for y in 0..20 {
            for x in 0..40 {
                let base = if x < 20 { 0.2 } else { 0.7 };
                let n = if (x + y) % 2 == 0 { 0.01 } else { -0.01 };
                src.set(x, y, grey(base + n));
            }
        }
        let f = Filter::SurfaceBlur {
            radius: 3.0,
            threshold: 20.0,
        };
        let out = filter_raster(&src, &f, (0, 0));
        // The ±0.01 checker (a few levels) averages out...
        assert!(close(out.get(10, 10).r, 0.2, 0.004), "{}", out.get(10, 10).r);
        assert!(close(out.get(30, 10).r, 0.7, 0.006), "{}", out.get(30, 10).r);
        // ...but the 0.2 → 0.7 step (≈ 90 levels) is far over the threshold.
        assert!(close(out.get(19, 10).r, 0.2, 0.004));
        assert!(close(out.get(20, 10).r, 0.7, 0.006));
        assert!(close(out.get(20, 10).a, 1.0, 1e-6));
        // A zero threshold changes nothing.
        let none = Filter::SurfaceBlur {
            radius: 3.0,
            threshold: 0.0,
        };
        assert_eq!(filter_raster(&src, &none, (0, 0)), src);
    }

    #[test]
    fn lens_blur_spreads_a_point_into_a_flat_disc() {
        let mut src = Raster::filled(41, 41, Rgba::new(0.0, 0.0, 0.0, 1.0));
        src.set(20, 20, grey(1.0));
        let f = Filter::LensBlur {
            radius: 5.0,
            highlights: 0.0,
        };
        let out = filter_raster(&src, &f, (0, 0));
        // Disc rows for r = 5 have half-widths 0, 3, 4, 4, 4, 5, 4, 4, 4, 3, 0:
        // 2 · (1 + 7 + 9 + 9 + 9) + 11 = 81 pixels.
        let area = 81.0;
        assert!(close(out.get(20, 20).r, 1.0 / area, 1e-6));
        assert!(close(out.get(25, 20).r, 1.0 / area, 1e-6), "rim inside");
        assert!(close(out.get(24, 24).r, 0.0, 1e-6), "corner outside the disc");
        assert!(close(out.get(26, 20).r, 0.0, 1e-6));
        assert!(close(out.get(10, 10).a, 1.0, 1e-6));
        // Highlights boost a bright point 7×.
        let f = Filter::LensBlur {
            radius: 5.0,
            highlights: 1.0,
        };
        let boosted = filter_raster(&src, &f, (0, 0));
        assert!(close(boosted.get(20, 20).r, 7.0 / area, 1e-5));
    }

    #[test]
    fn dust_and_scratches_removes_specks_over_the_threshold_only() {
        let mut src = Raster::filled(20, 20, grey(0.3));
        src.set(5, 5, grey(1.0)); // a speck
        src.set(12, 12, grey(0.31)); // a tiny variation under the threshold
        let f = Filter::DustScratches {
            radius: 2.0,
            threshold: 10.0,
        };
        let out = filter_raster(&src, &f, (0, 0));
        assert!(close(out.get(5, 5).r, 0.3, 1e-6), "speck removed");
        assert!(
            close(out.get(12, 12).r, 0.31, 1e-6),
            "detail under the threshold kept"
        );
    }

    #[test]
    fn destructive_filters_repeat_the_canvas_edge() {
        let canvas = Rect::new(0, 0, 64, 64);
        let store = TileStore::from_raster(&Raster::filled(64, 64, grey(0.5)), 0, 0);
        let f = Filter::GaussianBlur { radius: 6.0 };
        let fresh = apply_filter_in_canvas(&store, &f, canvas);
        assert!(close(fresh.get_pixel(0, 0).a, 1.0, 1e-5), "corner stays opaque");
        assert!(close(fresh.get_pixel(0, 0).r, 0.5, 1e-5));
        assert!(
            fresh.get_pixel(-1, 10).is_transparent(),
            "nothing painted off canvas"
        );
        // The plain path fades at the same corner.
        assert!(crate::filters::apply_filter(&store, &f).get_pixel(0, 0).a < 0.9);
        // Mosaic edge cells (64 = 4·15 + 4) stay opaque too.
        let m = apply_filter_in_canvas(&store, &Filter::Mosaic { size: 15.0 }, canvas);
        assert!(close(m.get_pixel(63, 63).a, 1.0, 1e-5));
    }

    #[test]
    #[ignore = "timing; cargo test --release -p lumenply-render new_filter_timing -- --ignored --nocapture"]
    fn new_filter_timing() {
        let (w, h) = (2400u32, 1600u32);
        let mut src = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = (((x / 7) ^ (y / 5)) % 13) as f32 / 13.0;
                src.set(x, y, Rgba::from_straight(v, 0.5 * v + 0.2, 1.0 - v, 1.0));
            }
        }
        for f in [
            Filter::Mosaic { size: 16.0 },
            Filter::Emboss {
                angle: 135.0,
                height: 3.0,
                amount: 1.0,
            },
            Filter::FindEdges,
            Filter::SurfaceBlur {
                radius: 5.0,
                threshold: 15.0,
            },
            Filter::SurfaceBlur {
                radius: 40.0,
                threshold: 15.0,
            },
            Filter::LensBlur {
                radius: 10.0,
                highlights: 0.5,
            },
            Filter::LensBlur {
                radius: 60.0,
                highlights: 0.5,
            },
            Filter::DustScratches {
                radius: 2.0,
                threshold: 10.0,
            },
            Filter::DustScratches {
                radius: 8.0,
                threshold: 10.0,
            },
        ] {
            let t = std::time::Instant::now();
            let _ = filter_raster(&src, &f, (0, 0));
            println!("{:?} on {w}×{h}: {:?}", f, t.elapsed());
        }
    }

    /// The live path renders a filter layer tile by tile from padded
    /// pieces; a reach (`pad`) that is too small shows as seams. Compare
    /// the tiled composite with one pass over the whole edge-clamped canvas.
    #[test]
    fn new_filters_render_seamlessly_across_tiles() {
        let (w, h) = (600u32, 300u32);
        let mut doc = lumenply_doc::Document::new(w, h);
        let id = doc.add_pixel_layer("base");
        let mut base = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = ((x * 37 + y * 11) % 97) as f32 / 97.0;
                let edge = if (x / 40 + y / 30) % 2 == 0 { 0.9 } else { 0.1 };
                base.set(x, y, Rgba::new(v * 0.5 + edge * 0.5, edge, 1.0 - v, 1.0));
            }
        }
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = TileStore::from_raster(&base, 0, 0);
        let filters = [
            Filter::Mosaic { size: 23.0 },
            Filter::Emboss {
                angle: 30.0,
                height: 3.5,
                amount: 2.0,
            },
            Filter::FindEdges,
            Filter::SurfaceBlur {
                radius: 6.0,
                threshold: 40.0,
            },
            Filter::LensBlur {
                radius: 9.0,
                highlights: 0.7,
            },
            Filter::DustScratches {
                radius: 3.0,
                threshold: 8.0,
            },
        ];
        for f in filters {
            let mut d = doc.clone();
            d.add_filter(f.clone());
            let tiled = crate::composite_raster(&d);
            // Reference: the whole canvas, padded by repeating its edge.
            let p = f.pad().max(1);
            let mut padded = Raster::new(w + 2 * p as u32, h + 2 * p as u32);
            for y in 0..padded.height as i32 {
                for x in 0..padded.width as i32 {
                    let sx = (x - p).clamp(0, w as i32 - 1) as u32;
                    let sy = (y - p).clamp(0, h as i32 - 1) as u32;
                    padded.set(x as u32, y as u32, base.get(sx, sy));
                }
            }
            let whole = filter_raster(&padded, &f, (-p, -p));
            let mut worst = 0.0f32;
            for y in 0..h {
                for x in 0..w {
                    let a = tiled.get(x, y);
                    let b = whole.get(x + p as u32, y + p as u32);
                    worst = worst
                        .max((a.r - b.r).abs())
                        .max((a.g - b.g).abs())
                        .max((a.b - b.b).abs())
                        .max((a.a - b.a).abs());
                }
            }
            assert!(
                worst < 1e-4,
                "{}: tiles differ from the whole by {worst}",
                f.name()
            );
        }
    }
}
