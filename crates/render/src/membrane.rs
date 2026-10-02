//! Membrane (harmonic) interpolation, the solver behind healing: given
//! values on some cells of a grid, fill the rest with the smoothest field
//! that meets them (the discrete Laplace equation), as seamless cloning
//! does with the colour difference along a patch's rim.
//!
//! Solved coarse to fine: each level starts from the coarser solution
//! scaled up and relaxes with over-relaxed Gauss-Seidel sweeps in
//! red-black order. Cells of one colour depend only on the other colour,
//! so each half-sweep runs in parallel over rows and the result does not
//! depend on thread scheduling.

use rayon::prelude::*;

/// Over-relaxation factor.
const OMEGA: f32 = 1.7;
/// Sweeps at every level above the coarsest, and at the coarsest.
const SWEEPS: usize = 80;
const BASE_SWEEPS: usize = 200;

/// Fill the unknown cells of a `w`×`h` grid with the membrane interpolation
/// of the known ones. Cells beyond the grid edge are simply absent (a free
/// boundary). Unknown cells keep their input value when no cell is known.
pub fn membrane(vals: &mut [[f32; 3]], known: &[bool], w: usize, h: usize) {
    assert_eq!(vals.len(), w * h);
    assert_eq!(known.len(), w * h);
    if known.iter().all(|&k| k) || !known.iter().any(|&k| k) {
        return;
    }
    if w * h <= 64 || w < 4 || h < 4 {
        let (mut sum, mut n) = ([0f32; 3], 0f32);
        for (v, _) in vals.iter().zip(known).filter(|(_, &k)| k) {
            for c in 0..3 {
                sum[c] += v[c];
            }
            n += 1.0;
        }
        let mean = [sum[0] / n, sum[1] / n, sum[2] / n];
        for (v, _) in vals.iter_mut().zip(known).filter(|(_, &k)| !k) {
            *v = mean;
        }
        relax(vals, known, w, h, BASE_SWEEPS);
        return;
    }
    // Coarse level: a cell is known when any of its children is, holding
    // their mean.
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut cv = vec![[0f32; 3]; cw * ch];
    let mut ck = vec![false; cw * ch];
    for cy in 0..ch {
        for cx in 0..cw {
            let (mut sum, mut n) = ([0f32; 3], 0f32);
            for (fx, fy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = (2 * cx + fx, 2 * cy + fy);
                if x < w && y < h && known[y * w + x] {
                    for c in 0..3 {
                        sum[c] += vals[y * w + x][c];
                    }
                    n += 1.0;
                }
            }
            if n > 0.0 {
                ck[cy * cw + cx] = true;
                cv[cy * cw + cx] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        }
    }
    membrane(&mut cv, &ck, cw, ch);
    // Scale the coarse field up (bilinear) as the starting point.
    vals.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let fy = ((y as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (ch - 1) as f32);
        let y0 = fy as usize;
        let y1 = (y0 + 1).min(ch - 1);
        let ty = fy - y0 as f32;
        for (x, v) in row.iter_mut().enumerate() {
            if known[y * w + x] {
                continue;
            }
            let fx = ((x as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (cw - 1) as f32);
            let x0 = fx as usize;
            let x1 = (x0 + 1).min(cw - 1);
            let tx = fx - x0 as f32;
            for c in 0..3 {
                let top = cv[y0 * cw + x0][c] * (1.0 - tx) + cv[y0 * cw + x1][c] * tx;
                let bot = cv[y1 * cw + x0][c] * (1.0 - tx) + cv[y1 * cw + x1][c] * tx;
                v[c] = top * (1.0 - ty) + bot * ty;
            }
        }
    });
    relax(vals, known, w, h, SWEEPS);
}

/// Red-black over-relaxed Gauss-Seidel sweeps of the discrete Laplace
/// equation over the unknown cells. The grid is split into its two
/// checkerboard colours (cell (x, y) is colour (x + y) % 2, slot x / 2 of
/// row y), so one colour updates in parallel while reading the other.
fn relax(vals: &mut [[f32; 3]], known: &[bool], w: usize, h: usize, sweeps: usize) {
    let rs = w.div_ceil(2);
    let mut cols: [Vec<[f32; 3]>; 2] = [vec![[0.0; 3]; rs * h], vec![[0.0; 3]; rs * h]];
    // Slots past a row's end stay "known", so they are never updated.
    let mut fixed: [Vec<bool>; 2] = [vec![true; rs * h], vec![true; rs * h]];
    for y in 0..h {
        for x in 0..w {
            let c = (x + y) & 1;
            cols[c][y * rs + x / 2] = vals[y * w + x];
            fixed[c][y * rs + x / 2] = known[y * w + x];
        }
    }
    for _ in 0..sweeps {
        for (c, fix) in fixed.iter().enumerate() {
            let (lo, hi) = cols.split_at_mut(1);
            let (cur, other) = if c == 0 {
                (&mut lo[0], &hi[0])
            } else {
                (&mut hi[0], &lo[0])
            };
            cur.par_chunks_mut(rs).enumerate().for_each(|(y, row)| {
                for (k, v) in row.iter_mut().enumerate() {
                    if fix[y * rs + k] {
                        continue;
                    }
                    let x = 2 * k + ((c + y) & 1);
                    // Neighbours are all the other colour: left and right
                    // in this row, the same column above and below.
                    let mut acc = [0f32; 3];
                    let mut n = 0f32;
                    let mut add = |q: [f32; 3]| {
                        for i in 0..3 {
                            acc[i] += q[i];
                        }
                        n += 1.0;
                    };
                    if x > 0 {
                        add(other[y * rs + (x - 1) / 2]);
                    }
                    if x + 1 < w {
                        // Slot (x + 1) / 2, written as clippy likes it.
                        add(other[y * rs + x.div_ceil(2)]);
                    }
                    if y > 0 {
                        add(other[(y - 1) * rs + x / 2]);
                    }
                    if y + 1 < h {
                        add(other[(y + 1) * rs + x / 2]);
                    }
                    for i in 0..3 {
                        v[i] += OMEGA * (acc[i] / n - v[i]);
                    }
                }
            });
        }
    }
    for y in 0..h {
        for x in 0..w {
            vals[y * w + x] = cols[(x + y) & 1][y * rs + x / 2];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harmonic_error(w: usize, h: usize) -> (f32, Vec<[f32; 3]>) {
        // f = 0.004 x + 0.006 y is harmonic, so knowing it on the border
        // alone pins the whole interior to it.
        let f = |x: usize, y: usize| 0.004 * x as f32 + 0.006 * y as f32;
        let mut vals = vec![[0f32; 3]; w * h];
        let mut known = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    let v = f(x, y);
                    vals[y * w + x] = [v, -v, 0.5];
                    known[y * w + x] = true;
                }
            }
        }
        membrane(&mut vals, &known, w, h);
        let mut worst = 0f32;
        for y in 0..h {
            for x in 0..w {
                let v = vals[y * w + x];
                let e = f(x, y);
                worst = worst
                    .max((v[0] - e).abs())
                    .max((v[1] + e).abs())
                    .max((v[2] - 0.5).abs());
            }
        }
        (worst, vals)
    }

    #[test]
    fn the_membrane_reproduces_a_harmonic_field_inside_a_large_hole() {
        let (w, h) = (120usize, 90usize);
        let (worst, vals) = harmonic_error(w, h);
        // Within half an 8-bit level everywhere.
        assert!(worst < 1.5e-3, "worst error {worst}");
        // The centre: 0.004·60 + 0.006·45 = 0.51.
        assert!((vals[45 * w + 60][0] - 0.51).abs() < 2e-3);
        // Odd sizes split into uneven checkerboard rows and blocks; the
        // coarse levels fit less snugly, but stay under an 8-bit level.
        let (worst, _) = harmonic_error(121, 77);
        assert!(worst < 3.5e-3, "odd size: worst error {worst}");
    }

    #[test]
    fn known_cells_never_move_and_the_result_is_repeatable() {
        let (w, h) = (300usize, 200usize);
        let mut vals = vec![[0f32; 3]; w * h];
        let mut known = vec![false; w * h];
        for (i, (v, k)) in vals.iter_mut().zip(known.iter_mut()).enumerate() {
            if i % 7 == 0 {
                let t = (i % 13) as f32 / 13.0;
                *v = [t, 1.0 - t, t * t];
                *k = true;
            }
        }
        let before = vals.clone();
        let mut a = vals.clone();
        membrane(&mut a, &known, w, h);
        membrane(&mut vals, &known, w, h);
        assert_eq!(a, vals, "bit-identical across runs");
        for i in (0..w * h).filter(|i| known[*i]) {
            assert_eq!(vals[i], before[i]);
        }
    }
}
