//! Adjustment layers: non-destructive colour operations on straight
//! (unpremultiplied) linear RGB in `[0, 1]`.
//!
//! Per-channel adjustments (Levels, Curves) are compiled to a lookup table
//! once per tile by the renderer; see [`Adjustment::compile`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::gradient::Gradient;
use crate::lut::Lut3D;

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
        /// Hue rotation in degrees; with `colorize`, the hue (0-360).
        hue: f32,
        /// -1.0 (grey) to 1.0 (double saturation); with `colorize`, the
        /// saturation (0-1).
        saturation: f32,
        /// -1.0 (black) to 1.0 (white).
        lightness: f32,
        /// Photoshop's Colorize: one hue and saturation for every pixel,
        /// keeping its lightness.
        #[serde(default)]
        colorize: bool,
    },
    /// Remap input range `[in_black, in_white]` through `gamma` onto
    /// `[out_black, out_white]`, then each channel through its own remap.
    Levels {
        in_black: f32,
        in_white: f32,
        /// 1.0 is linear; >1 brightens midtones.
        gamma: f32,
        out_black: f32,
        out_white: f32,
        /// Per-channel (R, G, B) remaps applied after the master one.
        #[serde(default, skip_serializing_if = "channels_identity")]
        channels: [LevelsChannel; 3],
    },
    /// A master curve through control points `[x, y]` in `[0, 1]`, sorted by
    /// `x`, interpolated with a monotone cubic so it never overshoots; then
    /// a curve per channel (R, G, B; empty = straight).
    Curves {
        points: Vec<[f32; 2]>,
        #[serde(default, skip_serializing_if = "curves_identity")]
        channels: [Vec<[f32; 2]>; 3],
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
    /// Map each pixel's (gamma) luminance onto a colour ramp: dark tones
    /// take the gradient's start, light tones its end.
    GradientMap {
        gradient: Gradient,
        #[serde(default)]
        reverse: bool,
    },
    /// Each output channel is a weighted mix of the input R, G and B plus
    /// a constant: rows are `[r, g, b, constant]`, 1.0 = 100%. With
    /// `monochrome` every channel takes the `gray` row instead.
    ChannelMixer {
        red: [f32; 4],
        green: [f32; 4],
        blue: [f32; 4],
        monochrome: bool,
        gray: [f32; 4],
    },
    /// A coloured lens filter: multiply by `color` (straight linear RGB)
    /// at `density` (0..1), optionally restoring each pixel's luminance.
    PhotoFilter {
        color: [f32; 3],
        density: f32,
        preserve_luminosity: bool,
    },
    /// CMYK shifts (each -1..1) per colour family, in the order of
    /// [`SELECTIVE_FAMILIES`]. `absolute` adds ink outright; relative
    /// (the default) scales the ink a pixel already has.
    SelectiveColor {
        colors: [[f32; 4]; 9],
        #[serde(default)]
        absolute: bool,
    },
    /// Photoshop's Color Lookup: a 3D LUT (optionally with a 1D shaper)
    /// run on gamma-encoded RGB with tetrahedral interpolation. The table
    /// is shared, so cloning the adjustment (undo, compile) is cheap.
    ColorLookup {
        #[serde(with = "crate::lut::shared")]
        lut: Arc<Lut3D>,
        /// What the table is called in the UI: the file or look it came
        /// from; empty while no table is chosen (the identity).
        #[serde(default)]
        name: String,
    },
}

/// The colour families of [`Adjustment::SelectiveColor`], in storage order.
pub const SELECTIVE_FAMILIES: [&str; 9] = [
    "Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas", "Whites", "Neutrals", "Blacks",
];

/// One channel's levels remap (same parameters as the master set).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LevelsChannel {
    pub in_black: f32,
    pub in_white: f32,
    pub gamma: f32,
    pub out_black: f32,
    pub out_white: f32,
}

impl Default for LevelsChannel {
    fn default() -> Self {
        LevelsChannel {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
        }
    }
}

impl LevelsChannel {
    pub fn is_identity(&self) -> bool {
        *self == LevelsChannel::default()
    }

    /// The remap itself, in the (gamma) domain levels work in.
    pub fn map(&self, x: f32) -> f32 {
        let span = (self.in_white - self.in_black).max(1e-4);
        let g = 1.0 / self.gamma.max(1e-3);
        let t = ((x - self.in_black) / span).clamp(0.0, 1.0).powf(g);
        self.out_black + (self.out_white - self.out_black) * t
    }
}

/// A curve that leaves every value as it is: no points, or the diagonal.
pub fn curve_is_identity(points: &[[f32; 2]]) -> bool {
    points.len() < 2 || points == [[0.0, 0.0], [1.0, 1.0]]
}

fn curves_identity(c: &[Vec<[f32; 2]>; 3]) -> bool {
    c.iter().all(|p| curve_is_identity(p))
}

fn channels_identity(c: &[LevelsChannel; 3]) -> bool {
    c.iter().all(LevelsChannel::is_identity)
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
            Adjustment::GradientMap { .. } => "Gradient Map",
            Adjustment::ChannelMixer { .. } => "Channel Mixer",
            Adjustment::PhotoFilter { .. } => "Photo Filter",
            Adjustment::SelectiveColor { .. } => "Selective Color",
            Adjustment::ColorLookup { .. } => "Color Lookup",
        }
    }

    /// Photoshop's fresh Color Lookup: no table chosen yet (the identity).
    pub fn color_lookup_default() -> Self {
        Adjustment::ColorLookup {
            lut: Arc::new(Lut3D::identity(2)),
            name: String::new(),
        }
    }

    pub fn gradient_map_default() -> Self {
        Adjustment::GradientMap {
            gradient: Gradient::default(),
            reverse: false,
        }
    }

    /// The identity mix (Photoshop's 40/40/20 waits in the gray row).
    pub fn channel_mixer_default() -> Self {
        Adjustment::ChannelMixer {
            red: [1.0, 0.0, 0.0, 0.0],
            green: [0.0, 1.0, 0.0, 0.0],
            blue: [0.0, 0.0, 1.0, 0.0],
            monochrome: false,
            gray: [0.4, 0.4, 0.2, 0.0],
        }
    }

    /// Warming Filter (85) at 25%, luminosity preserved — Photoshop's default.
    pub fn photo_filter_default() -> Self {
        Adjustment::PhotoFilter {
            color: [236u8, 138, 0].map(|c| srgb_decode(c as f32 / 255.0)),
            density: 0.25,
            preserve_luminosity: true,
        }
    }

    pub fn selective_color_default() -> Self {
        Adjustment::SelectiveColor {
            colors: [[0.0; 4]; 9],
            absolute: false,
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
            channels: [LevelsChannel::default(); 3],
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

    /// Whether the adjustment's maths runs on gamma-encoded (sRGB) values.
    ///
    /// Pixels rest in linear light, but the familiar behaviour of tonal
    /// tools — where Photoshop's mid grey sits at 0.5, a curve's diagonal
    /// bends around perceptual tones, levels endpoints match the histogram
    /// people know — comes from working on gamma values. Everything does,
    /// except Exposure, which is a linear-light tool by definition.
    pub fn gamma_space(&self) -> bool {
        !matches!(self, Adjustment::Exposure { .. })
    }

    /// Precompute whatever makes per-pixel application fast. Gamma-space
    /// adjustments bake the sRGB transfer into the LUT, so applying them is
    /// no dearer than linear ones.
    pub fn compile(&self) -> CompiledAdjustment {
        let wrap = |f: &dyn Fn(f32) -> f32, gamma: bool| {
            if gamma {
                CompiledAdjustment::Lut(build_lut(|x| srgb_decode(f(srgb_encode(x)))))
            } else {
                CompiledAdjustment::Lut(build_lut(f))
            }
        };
        match self {
            Adjustment::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
                channels,
            } => {
                let master = LevelsChannel {
                    in_black: *in_black,
                    in_white: *in_white,
                    gamma: *gamma,
                    out_black: *out_black,
                    out_white: *out_white,
                };
                if channels_identity(channels) {
                    wrap(&|x| master.map(x), self.gamma_space())
                } else {
                    // One LUT per channel: the master remap, then the
                    // channel's own, gamma-wrapped like every levels step.
                    let g = self.gamma_space();
                    let per = |ch: &LevelsChannel| {
                        build_lut(|x| {
                            if g {
                                srgb_decode(ch.map(master.map(srgb_encode(x))))
                            } else {
                                ch.map(master.map(x))
                            }
                        })
                    };
                    CompiledAdjustment::LutRgb(Box::new([
                        *per(&channels[0]),
                        *per(&channels[1]),
                        *per(&channels[2]),
                    ]))
                }
            }
            Adjustment::Curves { points, channels } => {
                let master = MonotoneCubic::new(points);
                if curves_identity(channels) {
                    wrap(&|x| master.eval(x), self.gamma_space())
                } else {
                    // One table per channel: the master curve, then the
                    // channel's own, both on gamma values.
                    let g = self.gamma_space();
                    let per = |pts: &Vec<[f32; 2]>| {
                        let ch = MonotoneCubic::new(pts);
                        build_lut(|x| {
                            if g {
                                srgb_decode(ch.eval(master.eval(srgb_encode(x))))
                            } else {
                                ch.eval(master.eval(x))
                            }
                        })
                    };
                    CompiledAdjustment::LutRgb(Box::new([
                        *per(&channels[0]),
                        *per(&channels[1]),
                        *per(&channels[2]),
                    ]))
                }
            }
            Adjustment::Invert => wrap(&|x| 1.0 - x, self.gamma_space()),
            Adjustment::BrightnessContrast { brightness, contrast } => {
                let k = ((contrast.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4).tan();
                let (brightness, k) = (*brightness, k);
                wrap(
                    &|c| ((c + brightness - 0.5) * k + 0.5).clamp(0.0, 1.0),
                    self.gamma_space(),
                )
            }
            Adjustment::Posterize { levels } => {
                let n = (*levels).clamp(2, 256) as f32;
                wrap(
                    &|c| ((c * n).floor().min(n - 1.0)) / (n - 1.0),
                    self.gamma_space(),
                )
            }
            Adjustment::GradientMap { gradient, reverse } => {
                // Gamma luminance → the ramp's colour, decoded to linear
                // once here so the per-pixel cost is a lookup.
                let g = if *reverse {
                    gradient.reversed()
                } else {
                    gradient.clone()
                };
                let mut map = Box::new([[0f32; 3]; LUT_SIZE]);
                for (out, c) in map.iter_mut().zip(g.sample_gamma(LUT_SIZE)) {
                    *out = [srgb_decode(c[0]), srgb_decode(c[1]), srgb_decode(c[2])];
                }
                CompiledAdjustment::Map(map)
            }
            Adjustment::ColorLookup { lut, .. } => CompiledAdjustment::Cube(lut.clone()),
            other => CompiledAdjustment::Direct(other.clone(), other.gamma_space()),
        }
    }
}

/// The sRGB transfer function (linear → gamma), exact.
pub fn srgb_encode(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Inverse of [`srgb_encode`] (gamma → linear), exact.
pub fn srgb_decode(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Fast transfer for the per-pixel (non-LUT) adjustments: 4096 entries with
/// linear interpolation, accurate to well under half an 8-bit step.
fn fast_encode(v: f32) -> f32 {
    use std::sync::OnceLock;
    static LUT: OnceLock<Box<[f32; 4096]>> = OnceLock::new();
    let lut = LUT.get_or_init(|| {
        let mut l = Box::new([0f32; 4096]);
        for (i, v) in l.iter_mut().enumerate() {
            *v = srgb_encode(i as f32 / 4095.0);
        }
        l
    });
    interp(lut, v)
}

fn fast_decode(v: f32) -> f32 {
    use std::sync::OnceLock;
    static LUT: OnceLock<Box<[f32; 4096]>> = OnceLock::new();
    let lut = LUT.get_or_init(|| {
        let mut l = Box::new([0f32; 4096]);
        for (i, v) in l.iter_mut().enumerate() {
            *v = srgb_decode(i as f32 / 4095.0);
        }
        l
    });
    interp(lut, v)
}

#[inline]
fn interp(lut: &[f32; 4096], x: f32) -> f32 {
    let p = x.clamp(0.0, 1.0) * 4095.0;
    let i = p as usize;
    if i >= 4095 {
        return lut[4095];
    }
    let t = p - i as f32;
    lut[i] + (lut[i + 1] - lut[i]) * t
}

/// Number of entries in a compiled lookup table.
pub const LUT_SIZE: usize = 1024;

/// An adjustment ready for per-pixel application.
#[derive(Clone, Debug)]
pub enum CompiledAdjustment {
    /// Per-pixel maths; the flag says whether it runs on gamma values.
    Direct(Adjustment, bool),
    Lut(Box<[f32; LUT_SIZE]>),
    /// Separate tables for R, G and B (per-channel Levels).
    LutRgb(Box<[[f32; LUT_SIZE]; 3]>),
    /// Gamma luminance → linear RGB (Gradient Map).
    Map(Box<[[f32; 3]; LUT_SIZE]>),
    /// A 3D table on gamma RGB (Color Lookup); shared, never copied.
    Cube(Arc<Lut3D>),
}

impl CompiledAdjustment {
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            CompiledAdjustment::Lut(lut) => [
                lut_lookup(lut, rgb[0]),
                lut_lookup(lut, rgb[1]),
                lut_lookup(lut, rgb[2]),
            ],
            CompiledAdjustment::LutRgb(luts) => [
                lut_lookup(&luts[0], rgb[0]),
                lut_lookup(&luts[1], rgb[1]),
                lut_lookup(&luts[2], rgb[2]),
            ],
            CompiledAdjustment::Map(map) => {
                let y = luminance(fast_encode(rgb[0]), fast_encode(rgb[1]), fast_encode(rgb[2]));
                let p = y.clamp(0.0, 1.0) * (LUT_SIZE - 1) as f32;
                let i = p as usize;
                if i >= LUT_SIZE - 1 {
                    return map[LUT_SIZE - 1];
                }
                let t = p - i as f32;
                let (a, b) = (map[i], map[i + 1]);
                [
                    a[0] + (b[0] - a[0]) * t,
                    a[1] + (b[1] - a[1]) * t,
                    a[2] + (b[2] - a[2]) * t,
                ]
            }
            CompiledAdjustment::Cube(lut) => {
                let out = lut.apply([fast_encode(rgb[0]), fast_encode(rgb[1]), fast_encode(rgb[2])]);
                [fast_decode(out[0]), fast_decode(out[1]), fast_decode(out[2])]
            }
            CompiledAdjustment::Direct(adj, gamma) => {
                let [r, g, b] = if *gamma {
                    [fast_encode(rgb[0]), fast_encode(rgb[1]), fast_encode(rgb[2])]
                } else {
                    rgb
                };
                let out = Self::direct(adj, [r, g, b]);
                if *gamma {
                    [fast_decode(out[0]), fast_decode(out[1]), fast_decode(out[2])]
                } else {
                    out
                }
            }
        }
    }

    #[inline]
    fn direct(adj: &Adjustment, [r, g, b]: [f32; 3]) -> [f32; 3] {
        {
            match *adj {
                Adjustment::HueSaturation {
                    hue,
                    saturation,
                    lightness,
                    colorize,
                } => {
                    let (h, s, l) = rgb_to_hsl(r, g, b);
                    let (h, s) = if colorize {
                        ((hue / 360.0).rem_euclid(1.0), saturation.clamp(0.0, 1.0))
                    } else {
                        (
                            (h + hue / 360.0).rem_euclid(1.0),
                            (s * (1.0 + saturation)).clamp(0.0, 1.0),
                        )
                    };
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
                Adjustment::ChannelMixer {
                    red,
                    green,
                    blue,
                    monochrome,
                    gray,
                } => {
                    let mix = |w: [f32; 4]| (r * w[0] + g * w[1] + b * w[2] + w[3]).clamp(0.0, 1.0);
                    if monochrome {
                        let v = mix(gray);
                        [v, v, v]
                    } else {
                        [mix(red), mix(green), mix(blue)]
                    }
                }
                Adjustment::PhotoFilter {
                    color,
                    density,
                    preserve_luminosity,
                } => {
                    let f = [
                        fast_encode(color[0]),
                        fast_encode(color[1]),
                        fast_encode(color[2]),
                    ];
                    let d = density.clamp(0.0, 1.0);
                    let c = [r, g, b];
                    let out: [f32; 3] = std::array::from_fn(|i| c[i] + (c[i] * f[i] - c[i]) * d);
                    if preserve_luminosity {
                        set_lum(out, luminance(r, g, b))
                    } else {
                        out
                    }
                }
                Adjustment::SelectiveColor { colors, absolute } => {
                    selective_color([r, g, b], &colors, absolute)
                }
                Adjustment::Invert
                | Adjustment::BrightnessContrast { .. }
                | Adjustment::Posterize { .. }
                | Adjustment::Levels { .. }
                | Adjustment::Curves { .. }
                | Adjustment::GradientMap { .. }
                | Adjustment::ColorLookup { .. } => {
                    unreachable!("per-channel adjustments compile to a LUT")
                }
            }
        }
    }
}

/// Set a colour's luminance to `l`, clipping back into gamut while keeping
/// the luminance (the W3C "SetLum" of the luminosity blend mode).
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - luminance(c[0], c[1], c[2]);
    let c = c.map(|v| v + d);
    let l = luminance(c[0], c[1], c[2]);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 && l - n > 1e-6 {
        out = out.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 && x - l > 1e-6 {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    out.map(|v| v.clamp(0.0, 1.0))
}

/// How much a gamma colour belongs to each selective-colour family, in
/// [`SELECTIVE_FAMILIES`] order. Hue families take the gap between the
/// two strongest (primaries) or two weakest (secondaries) channels, as
/// Photoshop does; whites, blacks and neutrals take how far the colour
/// sits above, below and around mid grey.
pub fn selective_weights([r, g, b]: [f32; 3]) -> [f32; 9] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let mid = r + g + b - max - min;
    let mut w = [0f32; 9];
    // Primaries: the top channel, by its lead over the middle one.
    if r >= g && r >= b {
        w[0] = max - mid;
    } else if g >= b {
        w[2] = max - mid;
    } else {
        w[4] = max - mid;
    }
    // Secondaries: the bottom channel, by the middle one's lead over it.
    if b <= r && b <= g {
        w[1] = mid - min;
    } else if r <= g {
        w[3] = mid - min;
    } else {
        w[5] = mid - min;
    }
    w[6] = ((min - 0.5) * 2.0).max(0.0);
    w[7] = (1.0 - ((max - 0.5).abs() + (min - 0.5).abs())).max(0.0);
    w[8] = ((0.5 - max) * 2.0).max(0.0);
    w
}

/// Selective Color on gamma RGB. Each channel's ink is its complement
/// (cyan = 1 − red, …); a family's C/M/Y shift adds to that ink outright
/// (absolute) or in proportion to it (relative), and its black shift
/// scales the result by `1 − k` (absolute) or `1 − k·K` with
/// `K = 1 − max(r, g, b)`, the pixel's own black (relative). Each
/// family's change is weighted by [`selective_weights`]; changes add up.
fn selective_color(c: [f32; 3], colors: &[[f32; 4]; 9], absolute: bool) -> [f32; 3] {
    let w = selective_weights(c);
    let k0 = 1.0 - c[0].max(c[1]).max(c[2]);
    let mut out = c;
    for (f, adj) in colors.iter().enumerate() {
        if w[f] <= 0.0 || adj.iter().all(|v| *v == 0.0) {
            continue;
        }
        let k = adj[3].clamp(-1.0, 1.0);
        let dark = if absolute { 1.0 - k } else { 1.0 - k * k0 };
        for i in 0..3 {
            let a = adj[i].clamp(-1.0, 1.0);
            let ink = 1.0 - c[i];
            let ink2 = if absolute { ink + a } else { ink * (1.0 + a) }.clamp(0.0, 1.0);
            let v = ((1.0 - ink2) * dark).clamp(0.0, 1.0);
            out[i] += w[f] * (v - c[i]);
        }
    }
    out.map(|v| v.clamp(0.0, 1.0))
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

    /// Run an adjustment on gamma-domain values and read the result back in
    /// the gamma domain — the numbers a user sees in any picker. Tonal
    /// adjustments do their maths there (see [`Adjustment::gamma_space`]),
    /// while pixels rest in linear light.
    fn in_gamma(adj: &Adjustment, rgb: [f32; 3]) -> [f32; 3] {
        let lin = adj.apply([srgb_decode(rgb[0]), srgb_decode(rgb[1]), srgb_decode(rgb[2])]);
        [srgb_encode(lin[0]), srgb_encode(lin[1]), srgb_encode(lin[2])]
    }

    #[test]
    fn colorize_gives_every_pixel_one_hue_and_keeps_its_lightness() {
        let red = Adjustment::HueSaturation {
            hue: 0.0,
            saturation: 0.5,
            lightness: 0.0,
            colorize: true,
        };
        // Mid grey: HSL(0°, 50%, 50%) = (0.75, 0.25, 0.25) in gamma values.
        let out = in_gamma(&red, [0.5; 3]);
        assert!(
            close(out[0], 0.75) && close(out[1], 0.25) && close(out[2], 0.25),
            "{out:?}"
        );
        // A blue pixel of the same lightness turns the same red.
        let out = in_gamma(&red, [0.25, 0.25, 0.75]);
        assert!(close(out[0], 0.75) && close(out[1], 0.25), "{out:?}");
        // White and black keep their lightness.
        assert!(close(in_gamma(&red, [1.0; 3])[1], 1.0) && close(in_gamma(&red, [0.0; 3])[0], 0.0));
        // Hue 120° is green; lightness still applies on top.
        let green = Adjustment::HueSaturation {
            hue: 120.0,
            saturation: 1.0,
            lightness: -0.5,
            colorize: true,
        };
        let out = in_gamma(&green, [0.5; 3]);
        assert!(
            close(out[0], 0.0) && close(out[1], 0.5) && close(out[2], 0.0),
            "{out:?}"
        );
    }

    #[test]
    fn curves_have_a_curve_per_channel_after_the_master() {
        // Red pulled down at the midpoint; green and blue straight.
        let c = Adjustment::Curves {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            channels: [
                vec![[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]],
                vec![],
                vec![[0.0, 0.0], [1.0, 1.0]],
            ],
        };
        let out = in_gamma(&c, [0.5; 3]);
        assert!(
            close(out[0], 0.25) && close(out[1], 0.5) && close(out[2], 0.5),
            "{out:?}"
        );
        // The master runs first: lifting 0.25 to 0.5, then red's curve.
        let c = Adjustment::Curves {
            points: vec![[0.0, 0.0], [0.25, 0.5], [1.0, 1.0]],
            channels: [vec![[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]], vec![], vec![]],
        };
        let out = in_gamma(&c, [0.25; 3]);
        assert!(close(out[0], 0.25) && close(out[1], 0.5), "{out:?}");
        // Straight channels compile to the single master table.
        assert!(curve_is_identity(&[]) && curve_is_identity(&[[0.0, 0.0], [1.0, 1.0]]));
        let plain = Adjustment::Curves {
            points: vec![[0.0, 0.1], [1.0, 1.0]],
            channels: [vec![], vec![[0.0, 0.0], [1.0, 1.0]], vec![]],
        };
        assert!(matches!(plain.compile(), CompiledAdjustment::Lut(_)));
    }

    #[test]
    fn transfer_round_trips_and_has_the_srgb_anchor_points() {
        for i in 0..=100 {
            let v = i as f32 / 100.0;
            assert!(close(srgb_decode(srgb_encode(v)), v));
            assert!(close(fast_encode(v), srgb_encode(v)));
            assert!(close(fast_decode(v), srgb_decode(v)));
        }
        assert!(close(srgb_decode(0.5), 0.2140), "sRGB mid grey is ~21.4% linear");
    }

    #[test]
    fn invert_brightness_hue() {
        // Invert works on gamma values, like Photoshop: an sRGB 0.2 becomes
        // an sRGB 0.8.
        let inv = in_gamma(&Adjustment::Invert, [0.2, 0.5, 1.0]);
        assert!(close(inv[0], 0.8) && close(inv[2], 0.0), "{inv:?}");

        let same = Adjustment::BrightnessContrast {
            brightness: 0.0,
            contrast: 0.0,
        }
        .apply([0.3, 0.6, 0.9]);
        assert!(close(same[0], 0.3) && close(same[2], 0.9), "identity is exact");

        // Contrast pivots around gamma 0.5 (linear ~0.214), so mid grey is a
        // fixed point of any contrast change.
        let mid = srgb_decode(0.5);
        let c = Adjustment::BrightnessContrast {
            brightness: 0.0,
            contrast: 0.5,
        };
        assert!(close(c.apply([mid; 3])[0], mid), "mid grey pinned");

        let hs = Adjustment::HueSaturation {
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0,
            colorize: false,
        };
        let g = hs.apply([1.0, 0.0, 0.0]);
        assert!(close(g[0], 0.0) && close(g[1], 1.0) && close(g[2], 0.0), "{g:?}");
    }

    #[test]
    fn levels_remaps_range_and_gamma() {
        let id = Adjustment::levels_default();
        assert!(close(id.apply([0.37; 3])[0], 0.37));

        // Levels endpoints are gamma-domain values, matching the histogram.
        let crush = Adjustment::Levels {
            in_black: 0.25,
            in_white: 0.75,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
            channels: Default::default(),
        };
        assert!(close(in_gamma(&crush, [0.25; 3])[0], 0.0));
        assert!(close(in_gamma(&crush, [0.5; 3])[0], 0.5));
        assert!(close(in_gamma(&crush, [0.9; 3])[0], 1.0));

        let bright = Adjustment::Levels {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 2.0,
            out_black: 0.0,
            out_white: 1.0,
            channels: Default::default(),
        };
        assert!(close(in_gamma(&bright, [0.25; 3])[0], 0.5)); // 0.25^(1/2)
    }

    #[test]
    fn per_channel_levels_remap_each_channel_alone() {
        // Red: crush [0.25, 0.75]; green: gamma 2; blue: identity. The
        // master set stays identity, so each channel shows only its own map.
        let adj = Adjustment::Levels {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 1.0,
            out_black: 0.0,
            out_white: 1.0,
            channels: [
                LevelsChannel {
                    in_black: 0.25,
                    in_white: 0.75,
                    ..LevelsChannel::default()
                },
                LevelsChannel {
                    gamma: 2.0,
                    ..LevelsChannel::default()
                },
                LevelsChannel::default(),
            ],
        };
        assert!(matches!(adj.compile(), CompiledAdjustment::LutRgb(_)));
        let out = in_gamma(&adj, [0.5, 0.25, 0.4]);
        assert!(close(out[0], 0.5), "red midpoint of [0.25,0.75]: {out:?}");
        assert!(close(out[1], 0.5), "green 0.25^(1/2): {out:?}");
        assert!(close(out[2], 0.4), "blue untouched: {out:?}");

        // Master and channel compose: master gamma 2 lifts 0.25 to 0.5,
        // then the red crush maps 0.5 to its midpoint 0.5.
        let both = Adjustment::Levels {
            in_black: 0.0,
            in_white: 1.0,
            gamma: 2.0,
            out_black: 0.0,
            out_white: 1.0,
            channels: [
                LevelsChannel {
                    in_black: 0.25,
                    in_white: 0.75,
                    ..LevelsChannel::default()
                },
                LevelsChannel::default(),
                LevelsChannel::default(),
            ],
        };
        let out = in_gamma(&both, [0.25; 3]);
        assert!(close(out[0], 0.5), "master then channel: {out:?}");
        assert!(close(out[1], 0.5), "master alone on green: {out:?}");
    }

    #[test]
    fn curves_pass_through_points_and_stay_monotone() {
        // Curve points live in the gamma domain, like the curves dialog.
        let c = Adjustment::Curves {
            points: vec![[0.0, 0.0], [0.25, 0.1], [0.75, 0.9], [1.0, 1.0]],
            channels: Default::default(),
        };
        assert!(close(in_gamma(&c, [0.25; 3])[0], 0.1));
        assert!(close(in_gamma(&c, [0.75; 3])[0], 0.9));
        assert!(close(in_gamma(&c, [0.0; 3])[0], 0.0) && close(in_gamma(&c, [1.0; 3])[0], 1.0));
        let mut prev = -1.0;
        for i in 0..=100 {
            let y = c.apply([i as f32 / 100.0; 3])[0];
            assert!(y >= prev - 1e-6, "curve dipped at {i}");
            prev = y;
        }
        // An empty point list is the identity.
        let e = Adjustment::Curves {
            points: vec![],
            channels: Default::default(),
        };
        assert!(close(e.apply([0.4; 3])[0], 0.4));
    }

    #[test]
    fn newer_adjustments_behave() {
        // Exposure is the one linear-light tool: +1 EV doubles linear values.
        let e = Adjustment::Exposure {
            exposure: 1.0,
            offset: 0.0,
            gamma: 1.0,
        };
        assert!(close(e.apply([0.25; 3])[0], 0.5));

        // Threshold levels compare gamma-domain luminance.
        let t = Adjustment::Threshold { level: 0.5 };
        let hi = in_gamma(&t, [0.9, 0.9, 0.9]);
        let lo = in_gamma(&t, [0.1, 0.1, 0.1]);
        assert!(hi.iter().all(|v| close(*v, 1.0)), "{hi:?}");
        assert!(lo.iter().all(|v| close(*v, 0.0)), "{lo:?}");

        // Posterize steps are even in the gamma domain.
        let p = Adjustment::Posterize { levels: 2 };
        let o = in_gamma(&p, [0.3, 0.6, 0.99]);
        assert!(close(o[0], 0.0) && close(o[1], 1.0) && close(o[2], 1.0), "{o:?}");

        let cb = Adjustment::ColorBalance {
            shadows: [0.0; 3],
            midtones: [1.0, 0.0, 0.0],
            highlights: [0.0; 3],
            preserve_luminosity: false,
        };
        let o = in_gamma(&cb, [0.5; 3]);
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
        let y = in_gamma(&bw, [1.0, 0.0, 0.0]);
        // Weights mix gamma-domain channels, so pure red maps to its weight.
        assert!(
            close(y[0], 0.2126) && close(y[0], y[1]) && close(y[1], y[2]),
            "{y:?}"
        );
        let red_only = Adjustment::BlackWhite {
            red: 1.0,
            green: 0.0,
            blue: 0.0,
        };
        assert!(close(in_gamma(&red_only, [0.3, 1.0, 1.0])[0], 0.3));
    }

    #[test]
    fn gradient_map_follows_gamma_luminance() {
        // Black → red: mid grey (sRGB 0.5, linear 0.2140) lands halfway,
        // on sRGB (0.5, 0, 0) — linear (0.2140, 0, 0).
        let gm = Adjustment::GradientMap {
            gradient: Gradient::two([0.0; 3], [1.0, 0.0, 0.0]),
            reverse: false,
        };
        assert!(matches!(gm.compile(), CompiledAdjustment::Map(_)));
        let out = gm.apply([srgb_decode(0.5); 3]);
        assert!(close(out[0], 0.2140) && out[1] == 0.0 && out[2] == 0.0, "{out:?}");
        assert!(close(in_gamma(&gm, [0.0; 3])[0], 0.0) && close(in_gamma(&gm, [1.0; 3])[0], 1.0));
        // Pure green's gamma luminance is 0.7152, so it lands there.
        let g = in_gamma(&gm, [0.0, 1.0, 0.0]);
        assert!(close(g[0], 0.7152) && close(g[1], 0.0), "{g:?}");
        // Reverse runs the ramp the other way: black becomes red.
        let rev = Adjustment::GradientMap {
            gradient: Gradient::two([0.0; 3], [1.0, 0.0, 0.0]),
            reverse: true,
        };
        let o = in_gamma(&rev, [0.0; 3]);
        assert!(close(o[0], 1.0) && close(o[1], 0.0), "{o:?}");
        assert!(close(in_gamma(&rev, [0.25; 3])[0], 0.75));
    }

    #[test]
    fn channel_mixer_mixes_gamma_channels() {
        let id = Adjustment::channel_mixer_default();
        let o = in_gamma(&id, [0.2, 0.6, 0.9]);
        assert!(close(o[0], 0.2) && close(o[1], 0.6) && close(o[2], 0.9), "{o:?}");
        // Swap red and blue, add 10% to green.
        let swap = Adjustment::ChannelMixer {
            red: [0.0, 0.0, 1.0, 0.0],
            green: [0.0, 1.0, 0.0, 0.1],
            blue: [1.0, 0.0, 0.0, 0.0],
            monochrome: false,
            gray: [0.4, 0.4, 0.2, 0.0],
        };
        let o = in_gamma(&swap, [0.2, 0.6, 0.9]);
        assert!(close(o[0], 0.9) && close(o[1], 0.7) && close(o[2], 0.2), "{o:?}");
        // Monochrome: 50/50/0 + 10% constant on (0.2, 0.6, 0.9) gives 0.5.
        let mono = Adjustment::ChannelMixer {
            red: [1.0, 0.0, 0.0, 0.0],
            green: [0.0, 1.0, 0.0, 0.0],
            blue: [0.0, 0.0, 1.0, 0.0],
            monochrome: true,
            gray: [0.5, 0.5, 0.0, 0.1],
        };
        let o = in_gamma(&mono, [0.2, 0.6, 0.9]);
        assert!(o.iter().all(|v| close(*v, 0.5)), "{o:?}");
        // Sums past 100% clip at white.
        let hot = Adjustment::ChannelMixer {
            red: [2.0, 0.0, 0.0, 0.0],
            green: [0.0, 1.0, 0.0, 0.0],
            blue: [0.0, 0.0, 1.0, 0.0],
            monochrome: false,
            gray: [0.4, 0.4, 0.2, 0.0],
        };
        assert!(close(in_gamma(&hot, [0.7, 0.0, 0.0])[0], 1.0));
    }

    #[test]
    fn photo_filter_multiplies_then_restores_luminance() {
        // Filter sRGB (1, 0.5, 0) at 50% on mid grey: halfway to the product.
        let f = |preserve| Adjustment::PhotoFilter {
            color: [1.0, srgb_decode(0.5), 0.0],
            density: 0.5,
            preserve_luminosity: preserve,
        };
        let o = in_gamma(&f(false), [0.5; 3]);
        assert!(
            close(o[0], 0.5) && close(o[1], 0.375) && close(o[2], 0.25),
            "{o:?}"
        );
        // Preserving luminosity shifts it back up to luminance 0.5:
        // (0.5, 0.375, 0.25) has 0.39255, so +0.10745 per channel.
        let o = in_gamma(&f(true), [0.5; 3]);
        assert!(
            close(o[0], 0.60745) && close(o[1], 0.48245) && close(o[2], 0.35745),
            "{o:?}"
        );
        assert!(close(luminance(o[0], o[1], o[2]), 0.5));
        // Zero density is the identity.
        let none = Adjustment::PhotoFilter {
            color: [1.0, 0.0, 0.0],
            density: 0.0,
            preserve_luminosity: false,
        };
        assert!(close(in_gamma(&none, [0.3, 0.6, 0.9])[1], 0.6));
    }

    #[test]
    fn selective_color_targets_one_family() {
        let w = selective_weights([1.0, 0.5, 0.0]);
        assert!(
            close(w[0], 0.5) && close(w[1], 0.5),
            "orange is half red, half yellow: {w:?}"
        );
        assert_eq!(selective_weights([0.5; 3])[7], 1.0, "mid grey is all neutral");
        assert_eq!(selective_weights([1.0; 3])[6], 1.0, "white is all white");
        assert_eq!(selective_weights([0.0; 3])[8], 1.0, "black is all black");

        let with = |family: usize, cmyk: [f32; 4], absolute: bool| {
            let mut colors = [[0.0; 4]; 9];
            colors[family] = cmyk;
            Adjustment::SelectiveColor { colors, absolute }
        };
        // Reds +100% cyan: absolute fills the red channel's ink (pure red
        // goes black); relative scales an ink of zero, so nothing happens.
        let o = in_gamma(&with(0, [1.0, 0.0, 0.0, 0.0], true), [1.0, 0.0, 0.0]);
        assert!(o.iter().all(|v| close(*v, 0.0)), "{o:?}");
        let o = in_gamma(&with(0, [1.0, 0.0, 0.0, 0.0], false), [1.0, 0.0, 0.0]);
        assert!(close(o[0], 1.0), "{o:?}");
        // ... and the reds setting leaves yellow alone.
        let o = in_gamma(&with(0, [1.0, 0.0, 0.0, 0.0], true), [1.0, 1.0, 0.0]);
        assert!(close(o[0], 1.0) && close(o[1], 1.0), "{o:?}");
        // Neutrals +50% black on mid grey: absolute halves it (0.25);
        // relative scales by the grey's own black 0.5 → × 0.75 = 0.375.
        let o = in_gamma(&with(7, [0.0, 0.0, 0.0, 0.5], true), [0.5; 3]);
        assert!(o.iter().all(|v| close(*v, 0.25)), "{o:?}");
        let o = in_gamma(&with(7, [0.0, 0.0, 0.0, 0.5], false), [0.5; 3]);
        assert!(o.iter().all(|v| close(*v, 0.375)), "{o:?}");
        // Yellows −100% yellow turns pure yellow white.
        let o = in_gamma(&with(1, [0.0, 0.0, -1.0, 0.0], true), [1.0, 1.0, 0.0]);
        assert!(o.iter().all(|v| close(*v, 1.0)), "{o:?}");
    }

    #[test]
    fn new_adjustments_round_trip_through_json() {
        for adj in [
            Adjustment::gradient_map_default(),
            Adjustment::channel_mixer_default(),
            Adjustment::photo_filter_default(),
            Adjustment::selective_color_default(),
        ] {
            let json = serde_json::to_string(&adj).unwrap();
            let back: Adjustment = serde_json::from_str(&json).unwrap();
            assert_eq!(back, adj, "{json}");
        }
        // Optional flags may be missing.
        let gm: Adjustment = serde_json::from_str(
            r#"{"type":"gradient-map","gradient":{"stops":[{"pos":0,"color":[0,0,0]},{"pos":1,"color":[1,1,1]}]}}"#,
        )
        .unwrap();
        assert!(matches!(gm, Adjustment::GradientMap { reverse: false, .. }));
    }

    #[test]
    fn color_lookup_runs_its_table_on_gamma_values() {
        let lookup = |lut: Lut3D| Adjustment::ColorLookup {
            lut: Arc::new(lut),
            name: "test".into(),
        };
        // The identity leaves linear pixels alone (only the 4096-entry
        // transfer tables stand between input and output).
        let id = lookup(Lut3D::identity(33));
        assert!(matches!(id.compile(), CompiledAdjustment::Cube(_)));
        for rgb in [[0.0, 0.5, 1.0], [0.0031, 0.2140, 0.9], [0.05, 0.6, 0.33]] {
            let out = id.apply(rgb);
            assert!(
                (0..3).all(|c| (out[c] - rgb[c]).abs() < 1e-4),
                "{rgb:?} -> {out:?}"
            );
        }
        assert!(Adjustment::color_lookup_default().apply([0.3, 0.6, 0.9])[1] - 0.6 < 1e-4);
        // Swapping red and blue swaps them.
        let swap = lookup(Lut3D::from_fn(17, |[r, g, b]| [b, g, r]));
        let o = swap.apply([0.8, 0.4, 0.1]);
        assert!(close(o[0], 0.1) && close(o[1], 0.4) && close(o[2], 0.8), "{o:?}");
        // The table sees gamma values: one halving every channel turns
        // sRGB 0.5 (linear 0.2140) into sRGB 0.25 (linear 0.0508).
        let half = lookup(Lut3D::from_fn(2, |c| c.map(|v| v * 0.5)));
        let o = in_gamma(&half, [0.5, 1.0, 0.0]);
        assert!(close(o[0], 0.25) && close(o[1], 0.5) && close(o[2], 0.0), "{o:?}");
        assert!(close(half.apply([srgb_decode(0.5); 3])[0], 0.0508));
        // Serde keeps the table and the name.
        let json = serde_json::to_string(&swap).unwrap();
        let back: Adjustment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, swap);
        assert_eq!(swap.name(), "Color Lookup");
    }
}
