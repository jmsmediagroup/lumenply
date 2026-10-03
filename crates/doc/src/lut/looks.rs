//! Built-in looks for the Color Lookup adjustment, generated in code (no
//! third-party LUT files, so no licensing questions) and baked into 33³
//! tables.
//!
//! Every look is a function of one gamma-encoded sRGB colour in `[0, 1]`,
//! written with a few shared building blocks:
//!
//! - `Y(c) = 0.2126 r + 0.7152 g + 0.0722 b` — Rec. 709 weights on the
//!   encoded values, as Threshold and Gradient Map use;
//! - `S(x, k) = x + k·(x²(3 − 2x) − x)` — an S-curve pinned at 0, ½ and 1
//!   that mixes towards smoothstep by `k` (monotone for `0 ≤ k ≤ 1`);
//! - `sat(c, k) = Y + (c − Y)·k` — saturation around the colour's own
//!   luminance;
//! - `pow(c, γ)` per channel — a white-balance shift that keeps black and
//!   white where they are.
//!
//! Results are clamped to `[0, 1]`.

use super::Lut3D;

/// Lattice size of the baked looks (the size Photoshop's own LUTs use).
pub const LOOK_SIZE: usize = 33;

/// A built-in look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Warm,
    Cool,
    TealOrange,
    BleachBypass,
    FadedFilm,
    MonochromeContrast,
    Crisp,
}

impl Look {
    pub const ALL: [Look; 7] = [
        Look::Warm,
        Look::Cool,
        Look::TealOrange,
        Look::BleachBypass,
        Look::FadedFilm,
        Look::MonochromeContrast,
        Look::Crisp,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Look::Warm => "Warm",
            Look::Cool => "Cool",
            Look::TealOrange => "Teal & Orange",
            Look::BleachBypass => "Bleach Bypass",
            Look::FadedFilm => "Faded Film",
            Look::MonochromeContrast => "Monochrome Contrast",
            Look::Crisp => "Crisp",
        }
    }

    /// Stable lower-case id (`teal-orange`), for scripts and debug tokens.
    pub fn id(self) -> &'static str {
        match self {
            Look::Warm => "warm",
            Look::Cool => "cool",
            Look::TealOrange => "teal-orange",
            Look::BleachBypass => "bleach-bypass",
            Look::FadedFilm => "faded-film",
            Look::MonochromeContrast => "mono-contrast",
            Look::Crisp => "crisp",
        }
    }

    pub fn from_id(id: &str) -> Option<Look> {
        Look::ALL.into_iter().find(|l| l.id() == id)
    }

    /// The look as a [`LOOK_SIZE`]³ table titled with its name.
    pub fn table(self) -> Lut3D {
        Lut3D::from_fn(LOOK_SIZE, |c| self.eval(c)).with_title(self.name())
    }

    /// The look's formula on one gamma-encoded colour.
    pub fn eval(self, c: [f32; 3]) -> [f32; 3] {
        let out = match self {
            // Warm: midtones toward amber with black and white pinned —
            // pow(c, (0.90, 0.97, 1.12)) — then 5% more saturation.
            Look::Warm => sat(gamma(c, [0.90, 0.97, 1.12]), 1.05),
            // Cool: the mirror image, pow(c, (1.10, 1.00, 0.90)), 3% less
            // saturation, as daylight-blue grades tend to be.
            Look::Cool => sat(gamma(c, [1.10, 1.00, 0.90]), 0.97),
            // Teal & Orange: split toning by luminance — shadows pushed
            // toward teal by (−0.06, +0.01, +0.05)·(1 − Y)², highlights
            // toward orange by (+0.06, +0.01, −0.06)·Y² — over an
            // S(·, 0.25) contrast curve, then saturation × 1.15 so skin and
            // sky separate.
            Look::TealOrange => {
                let y = luma(c);
                let s = c.map(|v| s_curve(v, 0.25));
                let lo = (1.0 - y) * (1.0 - y);
                let hi = y * y;
                let teal = [-0.06, 0.01, 0.05];
                let orange = [0.06, 0.01, -0.06];
                let t: [f32; 3] = std::array::from_fn(|i| s[i] + teal[i] * lo + orange[i] * hi);
                sat(t, 1.15)
            }
            // Bleach Bypass: the classic shader — overlay the colour's
            // own luminance onto it (multiply-style `2·c·Y` below Y = 0.45,
            // screen-style `1 − 2(1 − c)(1 − Y)` above, crossfading over
            // Y ∈ [0.45, 0.55]), mixed in at 80%, then saturation × 0.55.
            Look::BleachBypass => {
                let y = luma(c);
                let k = ((y - 0.45) * 10.0).clamp(0.0, 1.0);
                let o: [f32; 3] = std::array::from_fn(|i| {
                    let m = 2.0 * c[i] * y;
                    let s = 1.0 - 2.0 * (1.0 - c[i]) * (1.0 - y);
                    let ov = m + (s - m) * k;
                    c[i] + (ov - c[i]) * 0.8
                });
                sat(o, 0.55)
            }
            // Faded Film: S(·, 0.2), then the range squeezed to
            // [0.07, 0.93] (milky blacks, soft whites), saturation × 0.8,
            // a cool cast in the shadows (+0.025 blue)·(1 − Y) and a warm
            // one in the highlights (+0.02 red, −0.02 blue)·Y.
            Look::FadedFilm => {
                let y = luma(c);
                let f = c.map(|v| 0.07 + 0.86 * s_curve(v, 0.2));
                let mut o = sat(f, 0.8);
                o[0] += 0.02 * y;
                o[2] += 0.025 * (1.0 - y) - 0.02 * y;
                o
            }
            // Monochrome Contrast: grey from a red-filter mix
            // 0.40 r + 0.50 g + 0.10 b, through S(·, 0.6).
            Look::MonochromeContrast => {
                let g = s_curve(0.40 * c[0] + 0.50 * c[1] + 0.10 * c[2], 0.6);
                [g; 3]
            }
            // Crisp: S(·, 0.3) contrast, then vibrance — saturation
            // × (1 + 0.35·(1 − s)), where s = max − min is the colour's
            // own saturation, so muted colours gain most.
            Look::Crisp => {
                let s = c.map(|v| s_curve(v, 0.3));
                let chroma = s[0].max(s[1]).max(s[2]) - s[0].min(s[1]).min(s[2]);
                sat(s, 1.0 + 0.35 * (1.0 - chroma))
            }
        };
        out.map(|v| v.clamp(0.0, 1.0))
    }
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn s_curve(x: f32, k: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x + k * (x * x * (3.0 - 2.0 * x) - x)
}

fn sat(c: [f32; 3], k: f32) -> [f32; 3] {
    let y = luma(c);
    c.map(|v| y + (v - y) * k)
}

fn gamma(c: [f32; 3], g: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| c[i].clamp(0.0, 1.0).powf(g[i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-4)
    }

    #[test]
    fn the_formulas_give_their_documented_values() {
        // Warm on mid grey: pow(0.5, (0.90, 0.97, 1.12)) = (0.535887,
        // 0.510506, 0.460094); Y = 0.512262; ×1.05 saturation around Y.
        let w = Look::Warm.eval([0.5; 3]);
        assert!(near(w, [0.537068, 0.510418, 0.457485]), "{w:?}");
        // Cool mirrors it: red pulled down, blue up.
        let c = Look::Cool.eval([0.5; 3]);
        assert!(near(c, [0.467385, 0.499864, 0.534674]), "{c:?}");
        // Monochrome: 0.4·1 + 0.5·0.5 + 0.1·0 = 0.65 through S(·, 0.6):
        // smoothstep(0.65) = 0.71825, so 0.65 + 0.6·0.06825 = 0.69095.
        let m = Look::MonochromeContrast.eval([1.0, 0.5, 0.0]);
        assert!(near(m, [0.69095; 3]), "{m:?}");
        // Faded Film lifts black to (0.07, 0.07, 0.095) and drops white
        // to (0.95, 0.93, 0.91).
        assert!(near(Look::FadedFilm.eval([0.0; 3]), [0.07, 0.07, 0.095]));
        assert!(near(Look::FadedFilm.eval([1.0; 3]), [0.95, 0.93, 0.91]));
        // Bleach Bypass keeps black and white and desaturates pure red:
        // Y = 0.2126 (multiply side), overlay (0.4252, 0, 0) at 80% →
        // (0.54016, 0, 0); Y' = 0.11484; ×0.55 saturation.
        let b = Look::BleachBypass.eval([1.0, 0.0, 0.0]);
        assert!(near(b, [0.348766, 0.051678, 0.051678]), "{b:?}");
        assert!(near(Look::BleachBypass.eval([1.0; 3]), [1.0; 3]));
        assert!(near(Look::BleachBypass.eval([0.0; 3]), [0.0; 3]));
        // Teal & Orange: shadows lean teal (blue above red), highlights
        // orange (red above blue); neutral mid grey stays near neutral.
        let dark = Look::TealOrange.eval([0.2; 3]);
        let light = Look::TealOrange.eval([0.8; 3]);
        assert!(dark[2] > dark[0] && light[0] > light[2], "{dark:?} {light:?}");
        // Crisp pins black, white and mid grey.
        for v in [0.0, 0.5, 1.0] {
            assert!(near(Look::Crisp.eval([v; 3]), [v; 3]));
        }
    }

    #[test]
    fn baked_tables_match_their_formula_at_lattice_points() {
        for look in Look::ALL {
            let t = look.table();
            assert_eq!(t.size, LOOK_SIZE);
            assert_eq!(t.title, look.name());
            assert!(t.validate().is_ok());
            assert_eq!(Look::from_id(look.id()), Some(look));
            let p = [8.0 / 32.0, 20.0 / 32.0, 27.0 / 32.0];
            assert!(near(t.apply(p), look.eval(p)), "{}", look.name());
        }
    }
}
