//! Shared field helpers for layer effects: a chamfer distance transform,
//! growing (spread / choke) a coverage field, and the stroke ring for each
//! stroke position. The effect passes themselves live in `lib.rs`.

use lumenply_doc::StrokeAlign;

/// Distance (in pixels, 3-4 chamfer) from every cell to the nearest cell
/// where `seed` holds. Cells with no seed anywhere get a huge value.
pub(crate) fn chamfer(seed: impl Fn(usize) -> bool, w: usize, h: usize) -> Vec<f32> {
    const BIG: f32 = 1e6;
    let mut d: Vec<f32> = (0..w * h).map(|i| if seed(i) { 0.0 } else { BIG }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut best = d[i];
            if x > 0 {
                best = best.min(d[i - 1] + 3.0);
            }
            if y > 0 {
                best = best.min(d[i - w] + 3.0);
                if x > 0 {
                    best = best.min(d[i - w - 1] + 4.0);
                }
                if x + 1 < w {
                    best = best.min(d[i - w + 1] + 4.0);
                }
            }
            d[i] = best;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut best = d[i];
            if x + 1 < w {
                best = best.min(d[i + 1] + 3.0);
            }
            if y + 1 < h {
                best = best.min(d[i + w] + 3.0);
                if x > 0 {
                    best = best.min(d[i + w - 1] + 4.0);
                }
                if x + 1 < w {
                    best = best.min(d[i + w + 1] + 4.0);
                }
            }
            d[i] = best;
        }
    }
    for v in &mut d {
        *v /= 3.0;
    }
    d
}

/// `field` (0..1) grown outward by `px` pixels with an antialiased edge:
/// Photoshop's spread (and, on the inverse coverage, choke).
pub(crate) fn grow(field: &[f32], w: usize, h: usize, px: f32) -> Vec<f32> {
    let px = lumenply_doc::sane_radius(px);
    if px <= 0.0 {
        return field.to_vec();
    }
    let d = chamfer(|i| field[i] >= 0.5, w, h);
    field
        .iter()
        .zip(&d)
        .map(|(&a, &dist)| a.max((px + 1.0 - dist).clamp(0.0, 1.0)))
        .collect()
}

/// Stroke coverage (0..1) per cell of a `w × h` window for a stroke `size`
/// px wide at `position` around the coverage `cov`.
pub(crate) fn stroke_ring(cov: &[f32], w: usize, h: usize, size: f32, position: StrokeAlign) -> Vec<f32> {
    let (outer, inner) = match position {
        StrokeAlign::Outside => (size, 0.0),
        StrokeAlign::Inside => (0.0, size),
        StrokeAlign::Center => (size / 2.0, size / 2.0),
    };
    let mut ring = vec![0f32; w * h];
    if outer > 0.0 {
        // Outside: grows from the edge into the transparent area.
        let d = chamfer(|i| cov[i] >= 0.5, w, h);
        for (i, r) in ring.iter_mut().enumerate() {
            if d[i] > 0.0 {
                *r = (outer + 1.0 - d[i]).clamp(0.0, 1.0);
            }
        }
    }
    if inner > 0.0 {
        // Inside: eats into the coverage from its edge, antialiased by
        // the coverage itself.
        let d = chamfer(|i| cov[i] < 0.5, w, h);
        for (i, r) in ring.iter_mut().enumerate() {
            if cov[i] > 0.0 {
                let a = (inner + 1.0 - d[i]).clamp(0.0, 1.0) * cov[i];
                *r = r.max(a);
            }
        }
    }
    ring
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chamfer_measures_straight_and_diagonal_steps() {
        // One seed in the middle of a 5×5 window.
        let d = chamfer(|i| i == 12, 5, 5);
        assert_eq!(d[12], 0.0);
        assert_eq!(d[13], 1.0); // one step right
        assert_eq!(d[14], 2.0);
        assert!((d[18] - 4.0 / 3.0).abs() < 1e-6); // one diagonal step
    }

    #[test]
    fn grow_extends_the_coverage_by_the_spread() {
        // A 1-px column of coverage at x = 2 in a 7×1 strip.
        let mut f = vec![0f32; 7];
        f[2] = 1.0;
        // A pixel k px from the seed spans [k − 1, k] px past its edge, so
        // a 1.5-px spread covers neighbours fully and the next ones half.
        let g = grow(&f, 7, 1, 1.5);
        assert_eq!(g, vec![0.5, 1.0, 1.0, 1.0, 0.5, 0.0, 0.0]);
        assert_eq!(grow(&f, 7, 1, 0.0), f);
    }

    #[test]
    fn stroke_ring_sits_outside_inside_or_centred() {
        // Coverage on x = 3..=6 of a 10-px strip.
        let cov: Vec<f32> = (0..10)
            .map(|x| if (3..=6).contains(&x) { 1.0 } else { 0.0 })
            .collect();
        let out = stroke_ring(&cov, 10, 1, 2.0, StrokeAlign::Outside);
        assert_eq!(out, vec![0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0]);
        let ins = stroke_ring(&cov, 10, 1, 1.5, StrokeAlign::Inside);
        assert_eq!(ins, vec![0.0, 0.0, 0.0, 1.0, 0.5, 0.5, 1.0, 0.0, 0.0, 0.0]);
        let mid = stroke_ring(&cov, 10, 1, 2.0, StrokeAlign::Center);
        assert_eq!(mid, vec![0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0]);
    }
}
