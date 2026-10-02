//! Adjustment layers: non-destructive colour operations on straight
//! (unpremultiplied) linear RGB in `[0, 1]`.
//!
//! Per-channel adjustments (Levels, Curves) are compiled to a lookup table
//! once per tile by the renderer; see [`Adjustment::compile`].

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Adjustment {
    Invert,
    BrightnessContrast {
        /// -1.0 to 1.0, added to every channel.
        brightness: f32,
        /// -1.0 to 1.0; 0 leaves contrast unchanged.
        contrast: f32,
    },
    HueSaturation {
        /// Hue rotation in degrees.
        hue: f32,
        /// -1.0 (grey) to 1.0 (double saturation).
        saturation: f32,
        /// -1.0 (black) to 1.0 (white).
        lightness: f32,
    },
    /// Remap input range `[in_black, in_white]` through `gamma` onto
    /// `[out_black, out_white]`.
    Levels {
        in_black: f32,
        in_white: f32,
        /// 1.0 is linear; >1 brightens midtones.
        gamma: f32,
        out_black: f32,
        out_white: f32,
    },
    /// A master curve through control points `[x, y]` in `[0, 1]`, sorted by
    /// `x`, interpolated with a monotone cubic so it never overshoots.
    Curves {
        points: Vec<[f32; 2]>,
    },
    /// Weighted desaturation; weights are normalised, so any positive values
    /// work. `(0.2126, 0.7152, 0.0722)` is linear luminance.
    BlackWhite {
        red: f32,
        green: f32,
        blue: f32,
    },
    /// Photographic exposure: `(c · 2^exposure + offset) ^ (1 / gamma)`.
    Exposure {
        exposure: f32,
        offset: f32,
        gamma: f32,
    },
    /// Shift colour in shadows, midtones and highlights separately; each
    /// component is -1.0 to 1.0 (cyan↔red, magenta↔green, yellow↔blue).
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        preserve_luminosity: bool,
    },
    /// `saturation` scales all colours; `vibrance` boosts muted colours more
    /// than already-saturated ones. Both -1.0 to 1.0.
    Vibrance {
        vibrance: f32,
        saturation: f32,
    },
    /// Pixels with luminance at or above `level` become white, others black.
    Threshold {
        level: f32,
    },
    /// Quantise each channel to `levels` steps (2–256).
    Posterize {
        levels: u32,
    },
}

impl Adjustment {
    pub fn name(&self) -> &'static str {
        match self {
            Adjustment::Invert => "Invert",
            Adjustment::BrightnessContrast { .. } => "Brightness/Contrast",
            Adjustment::HueSaturation { .. } => "Hue/Saturation",
            Adjustment::Levels { .. } => "Levels",
            Adjustment::Curves { .. } => "Curves",
            Adjustment::BlackWhite { .. } => "Black & White",
            Adjustment::Exposure { .. } => "Exposure",
            Adjustment::ColorBalance { .. } => "Color Balance",
            Adjustment::Vibrance { .. } => "Vibrance",
            Adjustment::Threshold { .. } => "Threshold",
            Adjustment::Posterize { .. } => "Posterize",
        }
    }

    pub fn color_balance_default() -> Self {
        Adjustment::ColorBalance {
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
            preserve_luminosity: true,
        }
    }

    pub fn levels_default() -> Self {
        Adjustment::Levels {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        }
    }

    pub fn black_white_default() -> Self {
        Adjustment::BlackWhite {
            red: 0.2126,
            green: 0.7152,
            blue: 0.0722,
        }
    }

    /// Apply to one pixel. Convenient for tests; the renderer uses
    /// [`Adjustment::compile`] so per-channel work is done once per tile.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        self.compile().apply(rgb)
    }

    /// Precompute whatever makes per-pixel application fast.
    pub fn compile(&self) -> CompiledAdjustment {
        match self {
            Adjustment::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            } => {
                let span = (in_white - in_black).max(1e-4);
                let g = 1.0 / gamma.max(1e-3);
                CompiledAdjustment::Lut(build_lut(|x| {
                    let t = ((x - in_black) / span).clamp(0.0, 1.0).powf(g);
                    out_black + (out_white - out_black) * t
                }))
            }
            Adjustment::Curves { points } => {
                let spline = MonotoneCubic::new(points);
                CompiledAdjustment::Lut(build_lut(|x| spline.eval(x)))
            }
            other => CompiledAdjustment::Direct(other.clone()),
        }
    }
}

/// Number of entries in a compiled lookup table.
pub const LUT_SIZE: usize = 1024;

/// An adjustment ready for per-pixel application.
#[derive(Clone, Debug)]
pub enum CompiledAdjustment {
    Direct(Adjustment),
    Lut(Box<[f32; LUT_SIZE]>),
}

impl CompiledAdjustment {
    #[inline]
    pub fn apply(&self, [r, g, b]: [f32; 3]) -> [f32; 3] {
        match self {
            CompiledAdjustment::Lut(lut) => [lut_lookup(lut, r), lut_lookup(lut, g), lut_lookup(lut, b)],
            CompiledAdjustment::Direct(adj) => match *adj {
                Adjustment::Invert => [1.0 - r, 1.0 - g, 1.0 - b],
                Adjustment::BrightnessContrast { brightness, contrast } => {
                    let k = ((contrast.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4).tan();
                    let f = |c: f32| ((c + brightness - 0.5) * k + 0.5).clamp(0.0, 1.0);
                    [f(r), f(g), f(b)]
                }
                Adjustment::HueSaturation {
                    hue,
                    saturation,
                    lightness,
                } => {
                    let (h, s, l) = rgb_to_hsl(r, g, b);
                    let h = (h + hue / 360.0).rem_euclid(1.0);
                    let s = (s * (1.0 + saturation)).clamp(0.0, 1.0);
                    let l = if lightness >= 0.0 {
                        l + (1.0 - l) * lightness
                    } else {
                        l * (1.0 + lightness)
                    };
                    hsl_to_rgb(h, s, l.clamp(0.0, 1.0))
                }
                Adjustment::BlackWhite { red, green, blue } => {
                    let sum = (red + green + blue).max(1e-6);
                    let y = ((r * red + g * green + b * blue) / sum).clamp(0.0, 1.0);
                    [y, y, y]
                }
                Adjustment::Exposure {
                    exposure,
                    offset,
                    gamma,
                } => {
                    let k = 2f32.powf(exposure);
                    let inv_g = 1.0 / gamma.max(1e-3);
                    let f = |c: f32| (c * k + offset).clamp(0.0, 1.0).powf(inv_g);
                    [f(r), f(g), f(b)]
                }
                Adjustment::ColorBalance {
                    shadows,
                    midtones,
                    highlights,
                    preserve_luminosity,
                } => {
                    let l = luminance(r, g, b);
                    let ws = ((0.5 - l) / 0.5).clamp(0.0, 1.0);
                    let wh = ((l - 0.5) / 0.5).clamp(0.0, 1.0);
                    let wm = (1.0 - (l - 0.5).abs() * 2.0).clamp(0.0, 1.0);
                    let shift = |i: usize| 0.3 * (ws * shadows[i] + wm * midtones[i] + wh * highlights[i]);
                    let mut out = [
                        (r + shift(0)).clamp(0.0, 1.0),
                        (g + shift(1)).clamp(0.0, 1.0),
                        (b + shift(2)).clamp(0.0, 1.0),
                    ];
                    if preserve_luminosity {
                        let l2 = luminance(out[0], out[1], out[2]);
                        if l2 > 1e-6 {
                            let k = l / l2;
                            out = [
                                (out[0] * k).min(1.0),
                                (out[1] * k).min(1.0),
                                (out[2] * k).min(1.0),
                            ];
                        }
                    }
                    out
                }
                Adjustment::Vibrance { vibrance, saturation } => {
                    let (h, s, l) = rgb_to_hsl(r, g, b);
                    let s = s * (1.0 + saturation);
                    let s = s + vibrance * s * (1.0 - s);
                    hsl_to_rgb(h, s.clamp(0.0, 1.0), l)
                }
                Adjustment::Threshold { level } => {
                    let v = if luminance(r, g, b) >= level { 1.0 } else { 0.0 };
                    [v, v, v]
                }
                Adjustment::Posterize { levels } => {
                    let n = levels.clamp(2, 256) as f32;
                    let f = |c: f32| ((c * n).floor().min(n - 1.0)) / (n - 1.0);
                    [f(r), f(g), f(b)]
                }
                Adjustment::Levels { .. } | Adjustment::Curves { .. } => {
                    unreachable!("per-channel adjustments compile to a LUT")
                }
            },
        }
    }
}

fn build_lut(f: impl Fn(f32) -> f32) -> Box<[f32; LUT_SIZE]> {
    let mut lut = Box::new([0f32; LUT_SIZE]);
    for (i, v) in lut.iter_mut().enumerate() {
        *v = f(i as f32 / (LUT_SIZE - 1) as f32).clamp(0.0, 1.0);
    }
    lut
}

#[inline]
fn lut_lookup(lut: &[f32; LUT_SIZE], x: f32) -> f32 {
    let p = x.clamp(0.0, 1.0) * (LUT_SIZE - 1) as f32;
    let i = p as usize;
    if i >= LUT_SIZE - 1 {
        return lut[LUT_SIZE - 1];
    }
    let t = p - i as f32;
    lut[i] + (lut[i + 1] - lut[i]) * t
}

/// Fritsch–Carlson monotone cubic interpolation through sorted points.
struct MonotoneCubic {
    xs: Vec<f32>,
    ys: Vec<f32>,
    tangents: Vec<f32>,
}

impl MonotoneCubic {
    fn new(points: &[[f32; 2]]) -> Self {
        let mut pts: Vec<[f32; 2]> = points.to_vec();
        pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
        pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
        if pts.is_empty() {
            pts = vec![[0.0, 0.0], [1.0, 1.0]];
        } else if pts.len() == 1 {
            let y = pts[0][1];
            pts = vec![[0.0, y], [1.0, y]];
        }
        let n = pts.len();
        let xs: Vec<f32> = pts.iter().map(|p| p[0]).collect();
        let ys: Vec<f32> = pts.iter().map(|p| p[1]).collect();
        let deltas: Vec<f32> = (0..n - 1)
            .map(|i| (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i]))
            .collect();
        let mut tangents = vec![0f32; n];
        tangents[0] = deltas[0];
        tangents[n - 1] = deltas[n - 2];
        for i in 1..n - 1 {
            tangents[i] = if deltas[i - 1] * deltas[i] <= 0.0 {
                0.0
            } else {
                (deltas[i - 1] + deltas[i]) / 2.0
            };
        }
        for i in 0..n - 1 {
            if deltas[i] == 0.0 {
                tangents[i] = 0.0;
                tangents[i + 1] = 0.0;
                continue;
            }
            let a = tangents[i] / deltas[i];
            let b = tangents[i + 1] / deltas[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let tau = 3.0 / s.sqrt();
                tangents[i] = tau * a * deltas[i];
                tangents[i + 1] = tau * b * deltas[i];
            }
        }
        MonotoneCubic { xs, ys, tangents }
    }

    fn eval(&self, x: f32) -> f32 {
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        let i = match self.xs.binary_search_by(|v| v.total_cmp(&x)) {
            Ok(i) => return self.ys[i],
            Err(i) => i - 1,
        };
        let h = self.xs[i + 1] - self.xs[i];
        let t = (x - self.xs[i]) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * self.ys[i] + h10 * h * self.tangents[i] + h01 * self.ys[i + 1] + h11 * h * self.tangents[i + 1]
    }
}

#[inline]
pub fn luminance(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

pub fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max - min < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [l, l, l];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    #[test]
    fn invert_brightness_hue() {
        let inv = Adjustment::Invert.apply([0.2, 0.5, 1.0]);
        assert!(close(inv[0], 0.8) && close(inv[2], 0.0));

        let same = Adjustment::BrightnessContrast {
            brightness: 0.0,
            contrast: 0.0,
        }
        .apply([0.3, 0.6, 0.9]);
        assert!(close(same[0], 0.3) && close(same[2], 0.9));

        let hs = Adjustment::HueSaturation {
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0,
        };
        let g = hs.apply([1.0, 0.0, 0.0]);
        assert!(close(g[0], 0.0) && close(g[1], 1.0) && close(g[2], 0.0), "{g:?}");
    }

    #[test]
    fn levels_remaps_range_and_gamma() {
        let id = Adjustment::levels_default();
        assert!(close(id.apply([0.37; 3])[0], 0.37));

        let crush = Adjustment::Levels {
            in_black: 0.25,
            in_white: 0.75,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        };
        assert!(close(crush.apply([0.25; 3])[0], 0.0));
        assert!(close(crush.apply([0.5; 3])[0], 0.5));
        assert!(close(crush.apply([0.9; 3])[0], 1.0));

        let bright = Adjustment::Levels {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 2.0,
            out_black: 0.0,
            out_white: 1.0,
        };
        assert!(close(bright.apply([0.25; 3])[0], 0.5)); // 0.25^(1/2)
    }

    #[test]
    fn curves_pass_through_points_and_stay_monotone() {
        let c = Adjustment::Curves {
            points: vec![[0.0, 0.0], [0.25, 0.1], [0.75, 0.9], [1.0, 1.0]],
        };
        assert!(close(c.apply([0.25; 3])[0], 0.1));
        assert!(close(c.apply([0.75; 3])[0], 0.9));
        assert!(close(c.apply([0.0; 3])[0], 0.0) && close(c.apply([1.0; 3])[0], 1.0));
        let mut prev = -1.0;
        for i in 0..=100 {
            let y = c.apply([i as f32 / 100.0; 3])[0];
            assert!(y >= prev - 1e-6, "curve dipped at {i}");
            prev = y;
        }
        // An empty point list is the identity.
        let e = Adjustment::Curves { points: vec![] };
        assert!(close(e.apply([0.4; 3])[0], 0.4));
    }

    #[test]
    fn newer_adjustments_behave() {
        let e = Adjustment::Exposure {
            exposure: 1.0,
            offset: 0.0,
            gamma: 1.0,
        };
        assert!(close(e.apply([0.25; 3])[0], 0.5));
        let t = Adjustment::Threshold { level: 0.5 };
        assert_eq!(t.apply([0.9, 0.9, 0.9]), [1.0; 3]);
        assert_eq!(t.apply([0.1, 0.1, 0.1]), [0.0; 3]);
        let p = Adjustment::Posterize { levels: 2 };
        assert_eq!(p.apply([0.3, 0.6, 0.99]), [0.0, 1.0, 1.0]);
        let cb = Adjustment::ColorBalance {
            shadows: [0.0; 3],
            midtones: [1.0, 0.0, 0.0],
            highlights: [0.0; 3],
            preserve_luminosity: false,
        };
        let o = cb.apply([0.5; 3]);
        assert!(o[0] > 0.5 && close(o[1], 0.5), "midtone red shift: {o:?}");
        let v = Adjustment::Vibrance {
            vibrance: 1.0,
            saturation: 0.0,
        };
        let muted = v.apply([0.5, 0.45, 0.45]);
        let (_, s0, _) = rgb_to_hsl(0.5, 0.45, 0.45);
        let (_, s1, _) = rgb_to_hsl(muted[0], muted[1], muted[2]);
        assert!(s1 > s0, "vibrance saturates muted colours");
        let vivid = v.apply([1.0, 0.0, 0.0]);
        assert!(
            close(vivid[0], 1.0) && close(vivid[1], 0.0),
            "already saturated stays"
        );
    }

    #[test]
    fn black_white_uses_weights() {
        let bw = Adjustment::black_white_default();
        let y = bw.apply([1.0, 0.0, 0.0]);
        assert!(close(y[0], 0.2126) && close(y[0], y[1]) && close(y[1], y[2]));
        let red_only = Adjustment::BlackWhite {
            red: 1.0,
            green: 0.0,
            blue: 0.0,
        };
        assert!(close(red_only.apply([0.3, 1.0, 1.0])[0], 0.3));
    }
}
