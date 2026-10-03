//! CMYK soft proofing (View ▸ Proof Colors): how an sRGB colour would look
//! printed on U.S. Web Coated (SWOP) v2, Photoshop's default working CMYK.
//!
//! Each colour is separated into inks and printed back through the same
//! Yule–Nielsen Neugebauer model the PSD reader uses for CMYK files
//! (`psd::color_modes::SWOP_PRIMARIES`):
//!
//! - **Black generation**: medium GCR on the grey component
//!   `1 − max(r, g, b)` (gamma-encoded), so light colours print without
//!   black and shadows lean on it.
//! - **Separation**: C, M and Y solve for the target colour with K fixed,
//!   by projected Levenberg–Marquardt on CIELAB error, so a colour the inks
//!   cannot reach lands on the nearest printable one (relative
//!   colorimetric), within a 300 % total ink limit.
//! - **Black point compensation**: the darkest printable black maps to
//!   display black on the way back (Photoshop's default proof setup:
//!   Relative Colorimetric, BPC on, paper and black ink not simulated), so
//!   neutrals keep their tone and only out-of-gamut colours dull.
//!
//! The separation is a small optimisation per colour, so the display path
//! uses [`ProofLut`]: a 33³ grid on gamma-encoded RGB, built once and read
//! with tetrahedral interpolation (exact along the grey axis).

use std::sync::OnceLock;

use crate::psd::SWOP_PRIMARIES;

/// The profile the proof simulates, for the UI.
pub const PROOF_PROFILE: &str = "U.S. Web Coated (SWOP) v2";

/// Total ink limit of U.S. Web Coated (SWOP) v2, as a fraction (300 %).
pub const TOTAL_INK_LIMIT: f64 = 3.0;

/// Grid points per axis of the display lookup.
pub const LUT_SIZE: usize = 33;

/// Gamut warning threshold: CIELAB distance between a colour and its
/// nearest printable colour above which View ▸ Gamut Warning greys it.
pub const GAMUT_DE: f32 = 3.0;

/// Square roots of the SWOP Neugebauer primaries, in f64 for the solver.
fn sqrt_primaries() -> &'static [[f64; 3]; 16] {
    static P: OnceLock<[[f64; 3]; 16]> = OnceLock::new();
    P.get_or_init(|| SWOP_PRIMARIES.map(|p| p.map(|v| (v as f64).sqrt())))
}

/// Inks (C, M, Y, K, 0..1) → linear sRGB, the same model as the PSD
/// reader's `swop_approx`.
pub fn print_linear(ink: [f64; 4]) -> [f64; 3] {
    let ink = ink.map(|v| v.clamp(0.0, 1.0));
    let mut acc = [0.0f64; 3];
    for (idx, prim) in sqrt_primaries().iter().enumerate() {
        let mut w = 1.0;
        for (ch, &v) in ink.iter().enumerate() {
            let on = idx >> (3 - ch) & 1 == 1;
            w *= if on { v } else { 1.0 - v };
        }
        for c in 0..3 {
            acc[c] += w * prim[c];
        }
    }
    acc.map(|v| v * v)
}

fn decode(v: f64) -> f64 {
    crate::srgb_to_linear_f(v as f32) as f64
}

fn encode(v: f64) -> f64 {
    crate::linear_to_srgb_f(v as f32) as f64
}

/// Linear sRGB → Oklab, scaled ×100 so distances read like CIELAB ΔE.
/// Oklab rather than CIELAB because its hues stay put as chroma drops:
/// nearest-colour clipping in CIELAB turns saturated blue purple.
fn lab(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb;
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        100.0 * (0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s),
        100.0 * (1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s),
        100.0 * (0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s),
    ]
}

/// Medium GCR black generation on gamma-encoded RGB: none below a 25 %
/// grey component, then rising to solid black.
pub fn black_generation(rgb: [f64; 3]) -> f64 {
    let grey = 1.0 - rgb.iter().fold(0.0f64, |a, &v| a.max(v));
    ((grey - 0.25) / 0.75).clamp(0.0, 1.0).powf(1.5)
}

/// Keep C, M, Y within 0..1 and the four inks within the total ink limit.
fn project(cmy: [f64; 3], k: f64) -> [f64; 3] {
    let cmy = cmy.map(|v| v.clamp(0.0, 1.0));
    let room = (TOTAL_INK_LIMIT - k).max(0.0);
    let sum: f64 = cmy.iter().sum();
    if sum > room {
        cmy.map(|v| v * room / sum)
    } else {
        cmy
    }
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// C, M, Y for a target CIELAB colour with black fixed at `k`: projected
/// Levenberg–Marquardt with a forward-difference Jacobian.
fn solve_cmy(target: [f64; 3], k: f64, start: [f64; 3]) -> [f64; 3] {
    let resid = |x: [f64; 3]| {
        let l = lab(print_linear([x[0], x[1], x[2], k]));
        [l[0] - target[0], l[1] - target[1], l[2] - target[2]]
    };
    let mut x = project(start, k);
    let mut r = resid(x);
    let mut err = r.iter().map(|v| v * v).sum::<f64>();
    let mut lambda = 1e-3;
    for _ in 0..60 {
        if err < 1e-8 {
            break;
        }
        let mut j = [[0.0f64; 3]; 3];
        for v in 0..3 {
            let h = if x[v] > 0.5 { -1e-6 } else { 1e-6 };
            let mut xp = x;
            xp[v] += h;
            let rp = resid(xp);
            for c in 0..3 {
                j[c][v] = (rp[c] - r[c]) / h;
            }
        }
        // Normal equations: (JᵀJ + λ·diag(JᵀJ)) δ = −Jᵀr.
        let mut a = [[0.0f64; 3]; 3];
        let mut g = [0.0f64; 3];
        for p in 0..3 {
            for q in 0..3 {
                a[p][q] = (0..3).map(|c| j[c][p] * j[c][q]).sum();
            }
            g[p] = -(0..3).map(|c| j[c][p] * r[c]).sum::<f64>();
        }
        let mut improved = false;
        for _ in 0..8 {
            let mut m = a;
            for d in 0..3 {
                m[d][d] += lambda * a[d][d].max(1e-9);
            }
            let Some(delta) = solve3(m, g) else {
                lambda *= 10.0;
                continue;
            };
            let cand = project([x[0] + delta[0], x[1] + delta[1], x[2] + delta[2]], k);
            let rc = resid(cand);
            let ec = rc.iter().map(|v| v * v).sum::<f64>();
            if ec < err {
                let moved = dist2(cand, x);
                x = cand;
                r = rc;
                err = ec;
                lambda = (lambda * 0.3).max(1e-9);
                improved = moved > 1e-16;
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    x
}

/// Cramer's rule for a 3×3 system.
fn solve3(m: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-30 {
        return None;
    }
    let mut out = [0.0; 3];
    for (col, o) in out.iter_mut().enumerate() {
        let mut mc = m;
        for row in 0..3 {
            mc[row][col] = b[row];
        }
        *o = det(mc) / d;
    }
    Some(out)
}

/// Luminance of the darkest printable black (300 % rich black), the
/// black point that compensation maps to display black.
pub fn black_point() -> f64 {
    static BP: OnceLock<f64> = OnceLock::new();
    *BP.get_or_init(|| {
        let cmy = solve_cmy([0.0, 0.0, 0.0], 1.0, [1.0, 1.0, 1.0]);
        let o = print_linear([cmy[0], cmy[1], cmy[2], 1.0]);
        0.212_672_9 * o[0] + 0.715_152_2 * o[1] + 0.072_175 * o[2]
    })
}

/// One colour's proof: the CMYK separation, the colour it prints as
/// (gamma-encoded sRGB, after black point compensation) and how far the
/// print is from the requested colour (CIELAB ΔE, 0 when in gamut).
#[derive(Clone, Copy, Debug)]
pub struct Proofed {
    pub cmyk: [f64; 4],
    pub rgb: [f64; 3],
    pub delta_e: f64,
}

/// Proof one gamma-encoded sRGB colour (0..1 per channel) directly.
pub fn proof(rgb: [f64; 3]) -> Proofed {
    let bp = black_point();
    let target = rgb.map(|v| decode(v.clamp(0.0, 1.0)) * (1.0 - bp) + bp);
    let lab_t = lab(target);
    let k = black_generation(rgb);
    // Out-of-gamut colours can have two nearly-as-close prints (lighter or
    // more saturated), so solve from the naive separation (each ink takes
    // what black left) and from the best point of a coarse 6³ scan, and
    // keep the closer: neighbouring colours then pick the same branch.
    let naive = rgb.map(|v| ((1.0 - v.clamp(0.0, 1.0)) - k).max(0.0) / (1.0 - k).max(1e-6));
    let err = |x: [f64; 3]| dist2(lab(print_linear([x[0], x[1], x[2], k])), lab_t);
    let mut scan = (f64::INFINITY, naive);
    for i in 0..216 {
        let x = project([i / 36, i / 6 % 6, i % 6].map(|v| v as f64 / 5.0), k);
        let e = err(x);
        if e < scan.0 {
            scan = (e, x);
        }
    }
    let a = solve_cmy(lab_t, k, naive);
    let b = solve_cmy(lab_t, k, scan.1);
    let cmy = if err(b) < err(a) - 1e-9 { b } else { a };
    let cmyk = [cmy[0], cmy[1], cmy[2], k];
    let printed = print_linear(cmyk);
    let delta_e = dist2(lab(printed), lab_t).sqrt();
    let rgb = printed.map(|v| encode(((v - bp) / (1.0 - bp)).clamp(0.0, 1.0)));
    Proofed { cmyk, rgb, delta_e }
}

/// The proof as a 3D lookup on gamma-encoded RGB: per grid point the
/// proofed colour (encoded, 0..1) and its gamut distance.
pub struct ProofLut {
    n: usize,
    nodes: Vec<[f32; 4]>,
}

impl ProofLut {
    /// Build an `n`³ lookup (n ≥ 2), spread over the available cores.
    pub fn build(n: usize) -> Self {
        let n = n.max(2);
        let step = 1.0 / (n - 1) as f64;
        let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(n);
        let mut nodes = vec![[0.0f32; 4]; n * n * n];
        // Index = (r·n + g)·n + b: each red slab is contiguous.
        let per = n.div_ceil(threads);
        std::thread::scope(|s| {
            for (t, chunk) in nodes.chunks_mut(per * n * n).enumerate() {
                s.spawn(move || {
                    for (i, node) in chunk.iter_mut().enumerate() {
                        let idx = t * per * n * n + i;
                        let (r, g, b) = (idx / (n * n), idx / n % n, idx % n);
                        let p = proof([r as f64 * step, g as f64 * step, b as f64 * step]);
                        *node = [
                            p.rgb[0] as f32,
                            p.rgb[1] as f32,
                            p.rgb[2] as f32,
                            p.delta_e as f32,
                        ];
                    }
                });
            }
        });
        Self { n, nodes }
    }

    /// The shared U.S. Web Coated (SWOP) display lookup, built on first use.
    pub fn swop() -> &'static ProofLut {
        static LUT: OnceLock<ProofLut> = OnceLock::new();
        LUT.get_or_init(|| ProofLut::build(LUT_SIZE))
    }

    /// Tetrahedral interpolation at gamma-encoded RGB (0..1): the proofed
    /// colour (encoded, 0..1) and the gamut distance.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 4] {
        let n = self.n;
        let s = (n - 1) as f32;
        let mut base = [0usize; 3];
        let mut f = [0.0f32; 3];
        for c in 0..3 {
            let v = rgb[c].clamp(0.0, 1.0) * s;
            let i = (v.floor() as usize).min(n - 2);
            base[c] = i;
            f[c] = v - i as f32;
        }
        let at = |dr: usize, dg: usize, db: usize| -> [f32; 4] {
            self.nodes[((base[0] + dr) * n + base[1] + dg) * n + base[2] + db]
        };
        let (fr, fg, fb) = (f[0], f[1], f[2]);
        // The six tetrahedra of the cube, by the order of the fractions.
        let (w, p1, p2) = if fr >= fg {
            if fg >= fb {
                ([1.0 - fr, fr - fg, fg - fb, fb], at(1, 0, 0), at(1, 1, 0))
            } else if fr >= fb {
                ([1.0 - fr, fr - fb, fb - fg, fg], at(1, 0, 0), at(1, 0, 1))
            } else {
                ([1.0 - fb, fb - fr, fr - fg, fg], at(0, 0, 1), at(1, 0, 1))
            }
        } else if fb >= fg {
            ([1.0 - fb, fb - fg, fg - fr, fr], at(0, 0, 1), at(0, 1, 1))
        } else if fb >= fr {
            ([1.0 - fg, fg - fb, fb - fr, fr], at(0, 1, 0), at(0, 1, 1))
        } else {
            ([1.0 - fg, fg - fr, fr - fb, fb], at(0, 1, 0), at(1, 1, 0))
        };
        let (p0, p3) = (at(0, 0, 0), at(1, 1, 1));
        std::array::from_fn(|c| w[0] * p0[c] + w[1] * p1[c] + w[2] * p2[c] + w[3] * p3[c])
    }

    /// Proof an 8-bit gamma-encoded colour. With `warn`, colours further
    /// than [`GAMUT_DE`] from their print show as Photoshop's gamut
    /// warning grey instead.
    pub fn apply(&self, rgb: [u8; 3], warn: bool) -> [u8; 3] {
        let s = self.sample(rgb.map(|v| v as f32 / 255.0));
        if warn && s[3] > GAMUT_DE {
            return [128, 128, 128];
        }
        [s[0], s[1], s[2]].map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(p: Proofed) -> [u8; 3] {
        p.rgb.map(|v| (v * 255.0 + 0.5) as u8)
    }

    fn rgb8(c: [u8; 3]) -> [f64; 3] {
        c.map(|v| v as f64 / 255.0)
    }

    #[test]
    fn print_model_matches_the_psd_reader() {
        // Paper white, solid black ink and the 300 % rich black.
        assert_eq!(print_linear([0.0; 4]), [1.0, 1.0, 1.0]);
        let k = print_linear([0.0, 0.0, 0.0, 1.0]);
        assert!((k[0] - 0.01678).abs() < 1e-6, "{k:?}");
        // The model's 300 % rich black is far darker than the profile's
        // (its overprint primaries are fitted on 8-bit sRGB): Y ≈ 0.0008.
        let bp = black_point();
        assert!((bp - 0.0008).abs() < 0.0001, "black point {bp}");
    }

    #[test]
    fn white_and_black_map_to_display_white_and_black() {
        assert_eq!(bytes(proof([1.0; 3])), [255, 255, 255]);
        let black = proof([0.0; 3]);
        assert!(bytes(black).iter().all(|&v| v <= 3), "{:?}", bytes(black));
        // Black separates to solid K with 200 % of colour under it.
        assert_eq!(black.cmyk[3], 1.0);
        let total: f64 = black.cmyk.iter().sum();
        assert!(total <= TOTAL_INK_LIMIT + 1e-9, "{:?}", black.cmyk);
    }

    #[test]
    fn neutral_greys_stay_neutral() {
        for v in (16..=240).step_by(16) {
            let p = proof(rgb8([v as u8; 3]));
            let out = bytes(p);
            for c in out {
                assert!((c as i32 - v).abs() <= 1, "grey {v} → {out:?} ({:?})", p.cmyk);
            }
            assert!(p.delta_e < 0.5, "grey {v} ΔE {}", p.delta_e);
        }
    }

    #[test]
    fn saturated_blue_dulls_to_the_swop_blue() {
        // sRGB blue is far outside SWOP: it prints as solid cyan with 82 %
        // magenta, no yellow or black, and shows as a duller (35, 75, 165)
        // (Photoshop's own proof is close: about (41, 71, 157)).
        let p = proof([0.0, 0.0, 1.0]);
        let out = bytes(p);
        let want = [35, 75, 165];
        for c in 0..3 {
            assert!((out[c] as i32 - want[c]).abs() <= 1, "{out:?}");
        }
        assert!(
            p.cmyk[0] > 0.99 && (p.cmyk[1] - 0.82).abs() < 0.02,
            "{:?}",
            p.cmyk
        );
        assert!(p.cmyk[2] < 0.01 && p.cmyk[3] == 0.0, "{:?}", p.cmyk);
        assert!(p.delta_e > 10.0, "{}", p.delta_e);
        // Pure green and cyan dull too; yellow, inside the gamut's
        // shoulder, hardly moves.
        assert!(proof([0.0, 1.0, 0.0]).delta_e > 10.0);
        assert!(proof([0.0, 1.0, 1.0]).delta_e > 10.0);
        assert!(proof([1.0, 1.0, 0.0]).delta_e < 4.0);
    }

    #[test]
    fn a_skin_tone_barely_moves() {
        let src = [224u8, 172, 140];
        let p = proof(rgb8(src));
        let out = bytes(p);
        for c in 0..3 {
            assert!((out[c] as i32 - src[c] as i32).abs() <= 2, "{src:?} → {out:?}");
        }
        assert!(p.delta_e < 0.5);
    }

    #[test]
    fn lut_matches_the_direct_proof() {
        let lut = ProofLut::build(9);
        // At grid points the lookup is the direct proof.
        for &(r, g, b) in &[(0, 0, 0), (8, 0, 0), (2, 5, 7), (8, 8, 8), (4, 4, 4)] {
            let rgb = [r, g, b].map(|v: i32| v as f64 / 8.0);
            let d = proof(rgb).rgb;
            let s = lut.sample(rgb.map(|v| v as f32));
            for c in 0..3 {
                assert!(
                    (s[c] as f64 - d[c]).abs() < 1.0 / 255.0,
                    "{rgb:?}: {s:?} vs {d:?}"
                );
            }
        }
    }

    #[test]
    fn display_lut_stays_within_a_fraction_of_a_level_on_average() {
        // Between grid points the 33³ lookup interpolates; over random
        // colours it is off by 0.18 levels on average (worst cases sit on
        // the gamut boundary, where the mapping folds).
        let lut = ProofLut::swop();
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) % 256) as u8
        };
        let (mut sum, n) = (0.0, 1000);
        for _ in 0..n {
            let c = [rnd(), rnd(), rnd()];
            let d = proof(rgb8(c)).rgb;
            let s = lut.sample(c.map(|v| v as f32 / 255.0));
            sum += (0..3)
                .map(|i| (s[i] as f64 - d[i]).abs() * 255.0)
                .fold(0.0, f64::max);
        }
        assert!(sum / (n as f64) < 0.3, "mean {}", sum / n as f64);
        // 8-bit apply: greys and the skin tone pass through.
        assert_eq!(lut.apply([128, 128, 128], false), [128, 128, 128]);
        assert_eq!(lut.apply([255, 255, 255], true), [255, 255, 255]);
        let skin = lut.apply([224, 172, 140], true);
        assert!(
            skin.iter()
                .zip([224, 172, 140])
                .all(|(a, b)| (*a as i32 - b).abs() <= 2),
            "{skin:?}"
        );
        // Gamut warning greys pure blue.
        assert_eq!(lut.apply([0, 0, 255], true), [128, 128, 128]);
        let blue = lut.apply([0, 0, 255], false);
        assert!(
            (blue[0] as i32 - 35).abs() <= 1 && (blue[2] as i32 - 165).abs() <= 1,
            "{blue:?}"
        );
    }
}
