//! Brush tips and shape dynamics: Photoshop's Brush Settings for the
//! engine.
//!
//! A [`BrushTip`] is a grayscale coverage image (1 = full paint) stamped
//! once per dab, scaled so its longer side spans the brush diameter,
//! squashed by the roundness and turned by the angle. Sampling is bilinear
//! on a box-filtered mip chain, blended between the two nearest levels,
//! so a large tip stamped small stays anti-aliased instead of aliasing.
//!
//! [`BrushDynamics`] vary each dab: size, angle, roundness, flips,
//! scattering, count, opacity and flow. Every random choice is a hash of
//! the dab's index in the stroke, never a global generator, so replaying a
//! stroke (the live preview, undo, redo) stamps identical pixels.

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use lumenply_tiles::Rect;
use serde::{Deserialize, Serialize};

/// A sampled brush tip: grayscale coverage with its mip chain.
#[derive(Debug)]
pub struct BrushTip {
    name: String,
    width: u32,
    height: u32,
    /// `levels[0]` is the tip itself; each next level is a 2×2 box
    /// average of the one before, down to a single pixel.
    levels: Vec<Level>,
}

#[derive(Debug)]
struct Level {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

impl Level {
    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        if x < 0 || y < 0 || x >= self.w as isize || y >= self.h as isize {
            0.0
        } else {
            self.data[y as usize * self.w + x as usize]
        }
    }

    /// Bilinear sample at `(x, y)` in this level's pixel units (pixel
    /// centres at `i + 0.5`); outside the image the tip is empty.
    #[inline]
    fn bilinear(&self, x: f32, y: f32) -> f32 {
        // Clamped well outside the image (where it reads as empty), so a
        // wild coordinate can't overflow the integer neighbours.
        let fx = (x - 0.5).clamp(-2.0, self.w as f32 + 1.0);
        let fy = (y - 0.5).clamp(-2.0, self.h as f32 + 1.0);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let top = self.at(x0, y0) + (self.at(x0 + 1, y0) - self.at(x0, y0)) * tx;
        let bottom = self.at(x0, y0 + 1) + (self.at(x0 + 1, y0 + 1) - self.at(x0, y0 + 1)) * tx;
        top + (bottom - top) * ty
    }

    fn half(&self) -> Level {
        let (w, h) = (self.w.div_ceil(2), self.h.div_ceil(2));
        let mut data = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (2 * x as isize, 2 * y as isize);
                let sum =
                    self.at(sx, sy) + self.at(sx + 1, sy) + self.at(sx, sy + 1) + self.at(sx + 1, sy + 1);
                data[y * w + x] = sum * 0.25;
            }
        }
        Level { w, h, data }
    }
}

/// Largest tip side accepted (Photoshop's limit is 5000).
pub const MAX_TIP_SIDE: u32 = 8192;

/// A tip's size is checked before its pixels are converted, so a bogus
/// size can't allocate first and fail later.
fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_TIP_SIDE || height > MAX_TIP_SIDE {
        return Err(format!(
            "a brush tip must be 1–{MAX_TIP_SIDE} px on a side, not {width}×{height}"
        ));
    }
    Ok(())
}

/// Canvas pixels a minified sampled tip can reach beyond
/// `radius × reach()`: with the footprint capped at two canvas pixels
/// (see [`stamp`]), the coarsest level's fade adds at most 2 px and its
/// zero padding at most 4 px per axis — under 9 px once turned.
pub const TIP_MARGIN: f32 = 9.0;

impl BrushTip {
    /// A tip from coverage values in `[0, 1]`, row-major. Values are
    /// clamped; NaN reads as no paint.
    pub fn new(name: impl Into<String>, width: u32, height: u32, coverage: Vec<f32>) -> Result<Self, String> {
        check_size(width, height)?;
        if coverage.len() != width as usize * height as usize {
            return Err(format!(
                "a {width}×{height} brush tip needs {} values, got {}",
                width as usize * height as usize,
                coverage.len()
            ));
        }
        let data = coverage
            .into_iter()
            .map(|v| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) })
            .collect();
        let mut levels = vec![Level {
            w: width as usize,
            h: height as usize,
            data,
        }];
        while levels.last().is_some_and(|l| l.w > 1 || l.h > 1) {
            let next = levels.last().expect("non-empty").half();
            levels.push(next);
        }
        Ok(BrushTip {
            name: name.into(),
            width,
            height,
            levels,
        })
    }

    /// A tip from 8-bit gray, 255 = full paint (Photoshop's sampled tips).
    pub fn from_gray8(name: impl Into<String>, width: u32, height: u32, gray: &[u8]) -> Result<Self, String> {
        check_size(width, height)?;
        Self::new(
            name,
            width,
            height,
            gray.iter().map(|&v| v as f32 / 255.0).collect(),
        )
    }

    /// A tip from 16-bit gray, 65535 = full paint.
    pub fn from_gray16(
        name: impl Into<String>,
        width: u32,
        height: u32,
        gray: &[u16],
    ) -> Result<Self, String> {
        check_size(width, height)?;
        Self::new(
            name,
            width,
            height,
            gray.iter().map(|&v| v as f32 / 65535.0).collect(),
        )
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Coverage of tip pixel `(x, y)`; 0 outside.
    pub fn coverage(&self, x: u32, y: u32) -> f32 {
        self.levels[0].at(x as isize, y as isize)
    }

    /// Every coverage value, row-major: exactly what [`BrushTip::new`]
    /// rebuilds this tip from.
    pub fn coverage_values(&self) -> &[f32] {
        &self.levels[0].data
    }

    /// Number of mip levels (the tip itself counts as one).
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// Coverage at `(x, y)` in tip pixels (centres at `i + 0.5`) for a
    /// sample that spans `footprint` tip pixels: above 1 the sample reads
    /// the matching mip levels, so detail finer than a canvas pixel
    /// averages out instead of aliasing.
    pub fn sample(&self, x: f32, y: f32, footprint: f32) -> f32 {
        self.sampler(footprint).at(x, y)
    }

    /// A sampler for one footprint, so a dab (whose footprint is the same
    /// for every pixel) picks its mip levels once.
    pub fn sampler(&self, footprint: f32) -> TipSampler<'_> {
        let last = self.levels.len() - 1;
        let lod = if footprint > 1.0 {
            footprint.log2().min(last as f32)
        } else {
            0.0
        };
        let l0 = lod.floor() as usize;
        let t = if l0 >= last { 0.0 } else { lod - l0 as f32 };
        TipSampler {
            tip: self,
            l0,
            t,
            s0: 1.0 / (1u64 << l0) as f32,
        }
    }
}

/// Samples a tip at one footprint (see [`BrushTip::sampler`]).
pub struct TipSampler<'a> {
    tip: &'a BrushTip,
    l0: usize,
    /// Blend toward level `l0 + 1`.
    t: f32,
    /// Level-`l0` pixels per tip pixel.
    s0: f32,
}

impl TipSampler<'_> {
    /// How far past the tip's right and bottom edges (in tip pixels) this
    /// sampler can still read coverage: half a texel of bilinear fade at
    /// the coarsest level it reads, plus that level's zero padding (odd
    /// sizes round up when halved). The left and top edges get the fade
    /// only, so this bounds both sides.
    pub fn margin(&self) -> (f32, f32) {
        let top = (self.l0 + (self.t > 0.0) as usize).min(self.tip.levels.len() - 1);
        let texel = (1u64 << top) as f32;
        let level = &self.tip.levels[top];
        let pad_x = level.w as f32 * texel - self.tip.width as f32;
        let pad_y = level.h as f32 * texel - self.tip.height as f32;
        (0.5 * texel + pad_x, 0.5 * texel + pad_y)
    }

    /// Coverage at `(x, y)` in tip pixels.
    #[inline]
    pub fn at(&self, x: f32, y: f32) -> f32 {
        let v0 = self.tip.levels[self.l0].bilinear(x * self.s0, y * self.s0);
        if self.t <= 0.0 {
            return v0;
        }
        let s1 = self.s0 * 0.5;
        let v1 = self.tip.levels[self.l0 + 1].bilinear(x * s1, y * s1);
        v0 + (v1 - v0) * self.t
    }
}

impl BrushTip {
    /// The farthest a covered pixel can lie from the dab centre, as a
    /// multiple of the dab radius, at any angle: half the diagonal of the
    /// tip grown by half a texel each side (the bilinear fade) over half
    /// its longer side. A minified dab reads coarser levels that spread a
    /// little further; that is bounded in canvas pixels by [`TIP_MARGIN`].
    pub fn reach(&self) -> f32 {
        let (w, h) = (self.width as f32, self.height as f32);
        (w + 1.0).hypot(h + 1.0) / w.max(h)
    }

    /// A `size`×`size` grayscale preview (0–255, 255 = paint), the tip
    /// fitted and centred, anti-aliased through the mip chain.
    pub fn thumbnail(&self, size: u32) -> Vec<u8> {
        let size = size.max(1);
        let m = self.width.max(self.height) as f32;
        let scale = size as f32 / m; // thumbnail px per tip px
        let (ox, oy) = (
            (size as f32 - self.width as f32 * scale) * 0.5,
            (size as f32 - self.height as f32 * scale) * 0.5,
        );
        let mut out = vec![0u8; (size * size) as usize];
        for y in 0..size {
            for x in 0..size {
                let tx = (x as f32 + 0.5 - ox) / scale;
                let ty = (y as f32 + 0.5 - oy) / scale;
                let v = self.sample(tx, ty, 1.0 / scale);
                out[(y * size + x) as usize] = (v * 255.0 + 0.5) as u8;
            }
        }
        out
    }
}

/// Per-dab variation: Photoshop's Shape Dynamics, Scattering and Transfer
/// panels. Every amount is a fraction in `[0, 1]` unless noted; the
/// default varies nothing. Serialises with every field; missing fields
/// read as their defaults.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushDynamics {
    /// Each dab shrinks by up to this fraction of the size.
    pub size_jitter: f32,
    /// Floor for pressure and size jitter, as a fraction of the size.
    pub min_diameter: f32,
    /// Each dab turns by up to this fraction of ±180°.
    pub angle_jitter: f32,
    /// The tip angle is measured from the stroke direction
    /// (Photoshop's Angle Control: Direction).
    pub follow_direction: bool,
    /// Each dab's roundness drops by up to this fraction...
    pub roundness_jitter: f32,
    /// ...but never below this.
    pub min_roundness: f32,
    /// Each dab is mirrored left-right at random.
    pub flip_x_jitter: bool,
    /// Each dab is mirrored top-bottom at random.
    pub flip_y_jitter: bool,
    /// Scatter only across the stroke (Photoshop's "Both Axes" off);
    /// the amount is [`crate::paint::Brush::jitter`].
    pub scatter_across: bool,
    /// Dabs stamped at each spacing step, 1–16.
    pub count: u32,
    /// Each step stamps up to this fraction fewer dabs (at least one).
    pub count_jitter: f32,
    /// Each dab's opacity drops by up to this fraction.
    pub opacity_jitter: f32,
    /// Each dab's paint amount drops by up to this fraction. Dabs here
    /// build up like Photoshop's flow, so this compounds with
    /// `opacity_jitter`.
    pub flow_jitter: f32,
    /// Paper grain (Photoshop's Texture, subtract mode): each dab loses
    /// up to this much coverage where the canvas-anchored grain is low, so
    /// the tooth survives dabs building up. 0 is off.
    pub texture_depth: f32,
    /// Grain size: 1 is the default ~6 px tooth.
    pub texture_scale: f32,
    /// Colour dynamics (painting only): each dab mixes up to this far
    /// from the brush colour toward `background`...
    pub fg_bg_jitter: f32,
    /// ...turns its hue by up to ± this half of the colour wheel...
    pub hue_jitter: f32,
    /// ...and shifts saturation and brightness by up to ± this much
    /// (gamma-encoded HSL, as Photoshop does).
    pub saturation_jitter: f32,
    pub brightness_jitter: f32,
    /// The second colour for `fg_bg_jitter`: straight linear RGB (the
    /// app passes its background colour).
    pub background: [f32; 3],
}

impl Default for BrushDynamics {
    fn default() -> Self {
        BrushDynamics {
            size_jitter: 0.0,
            min_diameter: 0.0,
            angle_jitter: 0.0,
            follow_direction: false,
            roundness_jitter: 0.0,
            min_roundness: 0.25,
            flip_x_jitter: false,
            flip_y_jitter: false,
            scatter_across: false,
            count: 1,
            count_jitter: 0.0,
            opacity_jitter: 0.0,
            flow_jitter: 0.0,
            texture_depth: 0.0,
            texture_scale: 1.0,
            fg_bg_jitter: 0.0,
            hue_jitter: 0.0,
            saturation_jitter: 0.0,
            brightness_jitter: 0.0,
            background: [1.0, 1.0, 1.0],
        }
    }
}

impl BrushDynamics {
    /// True when no setting varies the dabs.
    pub fn is_static(&self) -> bool {
        let d = BrushDynamics {
            min_roundness: self.min_roundness,
            texture_scale: self.texture_scale,
            background: self.background,
            ..BrushDynamics::default()
        };
        *self == d
    }
}

/// Largest scatter, as a multiple of the brush radius.
pub const MAX_SCATTER: f32 = 4.0;

/// One stamp of the tip, after dynamics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    /// Radius in canvas pixels (half the tip's longer side).
    pub radius: f32,
    /// Coverage multiplier in `[0, 1]`.
    pub opacity: f32,
    /// Counter-clockwise angle in radians.
    pub angle: f32,
    /// `(0, 1]`; 1 is the tip's own proportions.
    pub roundness: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// This dab's paint colour (straight linear RGB) when colour
    /// dynamics vary it; `None` paints the brush colour.
    pub color: Option<[f32; 3]>,
}

/// Deterministic hash of `(n, salt)` onto `[0, 1)`.
pub fn hash01(n: u32, salt: u32) -> f32 {
    let mut h = n.wrapping_mul(0x27D4_EB2F) ^ salt;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Lattice hash of a 2-D integer point onto `[0, 1)`.
fn hash2(x: i32, y: i32, salt: u32) -> f32 {
    hash01(
        (x as u32).wrapping_mul(0x8DA6_B343) ^ (y as u32).wrapping_mul(0xD816_3841),
        salt,
    )
}

/// Smoothly interpolated value noise in `[0, 1)`.
fn value_noise(x: f32, y: f32, salt: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (tx, ty) = (s(x - x0), s(y - y0));
    let (ix, iy) = (x0 as i32, y0 as i32);
    let a = hash2(ix, iy, salt);
    let b = hash2(ix + 1, iy, salt);
    let c = hash2(ix, iy + 1, salt);
    let d = hash2(ix + 1, iy + 1, salt);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    top + (bottom - top) * ty
}

/// Paper grain at canvas pixel `(x, y)` in `[0, 1]`: two octaves of value
/// noise over a fine per-pixel tooth, anchored to the canvas so every dab
/// (and every stroke) meets the same paper. `scale` 1 makes ~6 px cells.
pub fn grain(x: i32, y: i32, scale: f32) -> f32 {
    let cell = 6.0 * scale.clamp(0.1, 10.0);
    let (u, v) = ((x as f32 + 0.5) / cell, (y as f32 + 0.5) / cell);
    let coarse = value_noise(u, v, 0x6EED_0001);
    let mid = value_noise(u * 2.7 + 17.0, v * 2.7 + 5.0, 0x6EED_0002);
    let fine = hash2(x, y, 0x6EED_0003);
    // Stretch the contrast so the tooth has real highs and lows.
    let g = 0.55 * coarse + 0.3 * mid + 0.15 * fine;
    ((g - 0.5) * 1.8 + 0.5).clamp(0.0, 1.0)
}

/// How much of a dab the paper takes at grain `g` for texture `depth`:
/// grain below the depth stays bare (a soft threshold 0.24 wide), so the
/// tooth survives any number of dabs building up — deeper texture, more
/// bare paper.
pub fn grain_mask(g: f32, depth: f32) -> f32 {
    let t = ((g - (depth - 0.12)) / 0.24).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// Salts for each dynamic, so they vary independently. The scatter salts
// predate the dynamics and must not change (old strokes replay as before).
const SALT_SCATTER_ANGLE: u32 = 0x9E37_79B9;
const SALT_SCATTER_DIST: u32 = 0x85EB_CA6B;
const SALT_SIZE: u32 = 0xC2B2_AE35;
const SALT_ANGLE: u32 = 0x2545_F491;
const SALT_ROUND: u32 = 0x1656_67B1;
const SALT_FLIP_X: u32 = 0xD3A2_646C;
const SALT_FLIP_Y: u32 = 0xFD70_46C5;
const SALT_COUNT: u32 = 0xB55A_4F09;
const SALT_OPACITY: u32 = 0x5BD1_E995;
const SALT_FLOW: u32 = 0x68E3_1DA4;

/// Pressure → size factor with the minimum diameter as the floor.
pub fn size_curve(min_diameter: f32, pressure: f32) -> f32 {
    let p = pressure.clamp(0.0, 1.0);
    let m = min_diameter.clamp(0.0, 1.0);
    if m <= 0.0 {
        p
    } else {
        m + (1.0 - m) * p
    }
}

/// A point on the stroke where dabs are stamped, before dynamics: its
/// position, pressure, pen opacity and the stroke direction there
/// (counter-clockwise radians, 0 = rightwards).
#[derive(Clone, Copy, Debug)]
pub struct Station {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub opacity: f32,
    pub direction: f32,
}

/// Turn stroke stations into dabs: count, scatter, size, angle,
/// roundness, flips and transfer, each hashed from the dab's index.
pub fn apply_dynamics(
    radius: f32,
    jitter: f32,
    angle_deg: f32,
    roundness: f32,
    d: &BrushDynamics,
    stations: &[Station],
) -> Vec<Dab> {
    let jitter = jitter.clamp(0.0, MAX_SCATTER);
    let count = d.count.clamp(1, 16);
    let base_angle = angle_deg.to_radians();
    let base_round = roundness.clamp(0.01, 1.0);
    let mut dabs = Vec::with_capacity(stations.len() * count as usize);
    let mut k = 0u32;
    for (i, s) in stations.iter().enumerate() {
        let n = if count > 1 && d.count_jitter > 0.0 {
            let drop = (d.count_jitter.clamp(0.0, 1.0) * hash01(i as u32, SALT_COUNT) * count as f32) as u32;
            count.saturating_sub(drop).max(1)
        } else {
            count
        };
        for _ in 0..n {
            let (mut x, mut y) = (s.x, s.y);
            if jitter > 0.0 {
                if d.scatter_across {
                    let off = (hash01(k, SALT_SCATTER_DIST) * 2.0 - 1.0) * jitter * radius;
                    // The normal of the direction (cos a, −sin a) on screen.
                    x += s.direction.sin() * off;
                    y += s.direction.cos() * off;
                } else {
                    let a = hash01(k, SALT_SCATTER_ANGLE) * TAU;
                    let dist = hash01(k, SALT_SCATTER_DIST).sqrt() * jitter * radius;
                    x += a.cos() * dist;
                    y += a.sin() * dist;
                }
            }
            let mut f = s.pressure.clamp(0.0, 1.0);
            if d.size_jitter > 0.0 {
                f *= 1.0 - d.size_jitter.clamp(0.0, 1.0) * hash01(k, SALT_SIZE);
            }
            if d.min_diameter > 0.0 {
                f = size_curve(d.min_diameter, f);
            }
            let mut angle = base_angle;
            if d.follow_direction {
                angle += s.direction;
            }
            if d.angle_jitter > 0.0 {
                angle += (hash01(k, SALT_ANGLE) * 2.0 - 1.0) * d.angle_jitter.clamp(0.0, 1.0) * PI;
            }
            let mut round = base_round;
            if d.roundness_jitter > 0.0 {
                let floor = d.min_roundness.clamp(0.01, 1.0).min(base_round);
                round =
                    (round * (1.0 - d.roundness_jitter.clamp(0.0, 1.0) * hash01(k, SALT_ROUND))).max(floor);
            }
            let mut opacity = s.opacity.clamp(0.0, 1.0);
            if d.opacity_jitter > 0.0 {
                opacity *= 1.0 - d.opacity_jitter.clamp(0.0, 1.0) * hash01(k, SALT_OPACITY);
            }
            if d.flow_jitter > 0.0 {
                opacity *= 1.0 - d.flow_jitter.clamp(0.0, 1.0) * hash01(k, SALT_FLOW);
            }
            dabs.push(Dab {
                x,
                y,
                radius: radius * f,
                opacity,
                angle,
                roundness: round,
                flip_x: d.flip_x_jitter && hash01(k, SALT_FLIP_X) < 0.5,
                flip_y: d.flip_y_jitter && hash01(k, SALT_FLIP_Y) < 0.5,
                color: None,
            });
            k += 1;
        }
    }
    dabs
}

/// Public only so `lumenply-core`'s stroke tests can predict a dab's colour.
#[doc(hidden)]
pub const SALT_FG_BG: u32 = 0x3C6E_F372;
const SALT_HUE: u32 = 0xA54F_F53A;
const SALT_SAT: u32 = 0x510E_527F;
const SALT_BRI: u32 = 0x9B05_688C;

/// Give each dab its own colour from the colour dynamics: a mix toward
/// the background colour, then hue, saturation and brightness shifts in
/// gamma-encoded HSL. Hashed from the dab index like every dynamic; a
/// no-op (dabs keep `color: None`) when no colour dynamic is on.
pub fn apply_color_dynamics(dabs: &mut [Dab], base: [f32; 3], d: &BrushDynamics) {
    use lumenply_doc::adjust::{hsl_to_rgb, rgb_to_hsl, srgb_decode, srgb_encode};
    let unit = |v: f32| v.clamp(0.0, 1.0);
    let (fg_bg, hue, sat, bri) = (
        unit(d.fg_bg_jitter),
        unit(d.hue_jitter),
        unit(d.saturation_jitter),
        unit(d.brightness_jitter),
    );
    if fg_bg + hue + sat + bri <= 0.0 {
        return;
    }
    let enc = |c: [f32; 3]| c.map(|v| srgb_encode(v.clamp(0.0, 1.0)));
    let (fg, bg) = (enc(base), enc(d.background));
    for (k, dab) in dabs.iter_mut().enumerate() {
        let k = k as u32;
        let t = fg_bg * hash01(k, SALT_FG_BG);
        let mut c = [0.0; 3];
        for i in 0..3 {
            c[i] = fg[i] + (bg[i] - fg[i]) * t;
        }
        if hue + sat + bri > 0.0 {
            let swing = |salt: u32| hash01(k, salt) * 2.0 - 1.0;
            let (h, s, l) = rgb_to_hsl(c[0], c[1], c[2]);
            let h = h + swing(SALT_HUE) * hue * 0.5;
            let s = (s + swing(SALT_SAT) * sat).clamp(0.0, 1.0);
            let l = (l + swing(SALT_BRI) * bri * 0.5).clamp(0.0, 1.0);
            c = hsl_to_rgb(h, s, l);
        }
        dab.color = Some(c.map(srgb_decode));
    }
}

/// Where and how a dab lands: what [`stamp`] needs before visiting pixels.
struct Footprint<'a> {
    rho: f32,
    sin: f32,
    cos: f32,
    /// Canvas px per tip px.
    scale: f32,
    sampler: Option<TipSampler<'a>>,
    /// The canvas pixels visited, already clipped to the canvas.
    area: Rect,
}

/// `None` for a dab of no size.
fn footprint<'a>(tip: Option<&'a BrushTip>, d: &Dab, canvas: Rect) -> Option<Footprint<'a>> {
    let r = d.radius;
    if r <= 0.0 {
        return None;
    }
    let rho = d.roundness.clamp(0.01, 1.0);
    let (sin, cos) = d.angle.sin_cos();
    // Canvas px per tip px, and the sampler: the mip level follows the
    // squashed axis's footprint, but no coarser than two canvas pixels
    // along the other, so the blur (and how far it spreads) stays small.
    let scale = tip.map_or(1.0, |t| 2.0 * r / t.width.max(t.height) as f32);
    let sampler = tip.map(|t| t.sampler((1.0 / (scale * rho)).min(2.0 / scale)));
    // Half extents of the unturned footprint. A sampled tip reaches past
    // its edge by its levels' bilinear fade and zero padding (`margin`).
    let (hw, hh) = match (tip, &sampler) {
        (Some(t), Some(s)) => {
            let (mx, my) = s.margin();
            (
                (t.width as f32 * 0.5 + mx) * scale,
                (t.height as f32 * 0.5 + my) * scale * rho,
            )
        }
        _ => (r, r * rho),
    };
    let ex = (hw * cos).abs() + (hh * sin).abs();
    let ey = (hw * sin).abs() + (hh * cos).abs();
    let x0 = (d.x - ex).floor() as i32;
    let y0 = (d.y - ey).floor() as i32;
    let x1 = (d.x + ex).ceil() as i32;
    let y1 = (d.y + ey).ceil() as i32;
    let area = Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32).intersect(&canvas);
    Some(Footprint {
        rho,
        sin,
        cos,
        scale,
        sampler,
        area,
    })
}

/// The canvas pixels [`stamp`] visits for this dab (clipped to `canvas`):
/// no pixel outside it gets coverage.
pub fn stamp_area(tip: Option<&BrushTip>, d: &Dab, canvas: Rect) -> Rect {
    footprint(tip, d, canvas).map_or(Rect::default(), |f| f.area)
}

/// Call `f(x, y, coverage)` for every canvas pixel under a dab of a
/// sampled tip, or of the computed round tip (`tip == None`, with
/// `hardness`) squashed and turned. Opacity and selection are the
/// caller's.
pub fn stamp(tip: Option<&BrushTip>, hardness: f32, d: &Dab, canvas: Rect, mut f: impl FnMut(i32, i32, f32)) {
    let r = d.radius;
    let Some(Footprint {
        rho,
        sin,
        cos,
        scale,
        sampler,
        area,
    }) = footprint(tip, d, canvas)
    else {
        return;
    };
    let hard = hardness.clamp(0.0, 0.999);
    for py in area.y..area.bottom() {
        for px in area.x..area.right() {
            let dx = px as f32 + 0.5 - d.x;
            let dy = py as f32 + 0.5 - d.y;
            // Into tip space: undo the counter-clockwise turn, then the
            // squash, then the flips.
            let mut lx = dx * cos - dy * sin;
            let mut ly = (dx * sin + dy * cos) / rho;
            if d.flip_x {
                lx = -lx;
            }
            if d.flip_y {
                ly = -ly;
            }
            let cover = match tip {
                None => {
                    let len = (lx * lx + ly * ly).sqrt();
                    let dist = len / r;
                    // Distance to the ellipse's edge in canvas pixels, for
                    // a one-pixel antialiased rim on both axes.
                    let across = (lx * lx + (ly / rho) * (ly / rho)).sqrt();
                    let k = if across > 0.0 { len / across } else { rho };
                    let edge = ((1.0 - dist) * r * k + 0.5).clamp(0.0, 1.0);
                    let soft = if dist <= hard {
                        1.0
                    } else {
                        1.0 - (dist - hard) / (1.0 - hard)
                    };
                    edge * soft.clamp(0.0, 1.0)
                }
                Some(t) => {
                    let tx = lx / scale + t.width as f32 * 0.5;
                    let ty = ly / scale + t.height as f32 * 0.5;
                    sampler.as_ref().map_or(0.0, |s| s.at(tx, ty))
                }
            };
            if cover > 0.0 {
                f(px, py, cover);
            }
        }
    }
}

// ---- built-in tips -------------------------------------------------------------

/// Names of the generated tips that ship with the app.
pub const BUILTIN_TIPS: [&str; 5] = ["Chalk", "Spatter", "Grass", "Dry brush", "Leaf"];

/// Coverage of a generated tip at normalised `(u, v)` for pixel index.
type TipShape = fn(f32, f32, u32) -> f32;

/// A generated built-in tip by name (see [`BUILTIN_TIPS`]). They are
/// small and deterministic, so they cost nothing to ship.
pub fn builtin_tip(name: &str) -> Option<Arc<BrushTip>> {
    let (w, h, f): (u32, u32, TipShape) = match name {
        "Chalk" => (64, 64, chalk),
        "Spatter" => (96, 96, spatter),
        "Grass" => (96, 96, grass),
        "Dry brush" => (96, 40, dry_brush),
        "Leaf" => (64, 64, leaf),
        _ => return None,
    };
    let mut cov = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            // 4×4 supersampling for smooth generated edges; coordinates
            // are normalised to [-1, 1] across the longer side.
            let mut s = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let m = w.max(h) as f32;
                    let u = ((x as f32 + (sx as f32 + 0.5) / 4.0) - w as f32 * 0.5) / (m * 0.5);
                    let v = ((y as f32 + (sy as f32 + 0.5) / 4.0) - h as f32 * 0.5) / (m * 0.5);
                    s += f(u, v, y * w + x);
                }
            }
            cov.push(s / 16.0);
        }
    }
    BrushTip::new(name, w, h, cov).ok().map(Arc::new)
}

fn disc(u: f32, v: f32, cx: f32, cy: f32, r: f32) -> f32 {
    if (u - cx).powi(2) + (v - cy).powi(2) <= r * r {
        1.0
    } else {
        0.0
    }
}

/// A rough disc with a grainy, broken fill.
fn chalk(u: f32, v: f32, pixel: u32) -> f32 {
    let a = v.atan2(u);
    // A ragged rim: a few low-frequency bumps.
    let rim = 0.86 + 0.06 * (a * 5.0).sin() + 0.04 * (a * 11.0 + 1.3).sin();
    if u.hypot(v) > rim {
        return 0.0;
    }
    let g = hash01(pixel, 0xC4A1_7E55);
    if g < 0.3 {
        0.0
    } else {
        0.45 + 0.55 * g
    }
}

/// A burst of droplets of varied size.
fn spatter(u: f32, v: f32, _: u32) -> f32 {
    let mut c: f32 = 0.0;
    for i in 0..42u32 {
        let a = hash01(i, 0x51A7_7E21) * TAU;
        let d = hash01(i, 0x0D15_7A11).powf(0.7) * 0.85;
        let r = 0.03 + 0.11 * hash01(i, 0x0005_12E5).powi(3);
        c = c.max(disc(u, v, a.cos() * d, a.sin() * d, r));
    }
    c
}

/// Blades of grass rising from the bottom edge.
fn grass(u: f32, v: f32, _: u32) -> f32 {
    let mut c: f32 = 0.0;
    for i in 0..9u32 {
        let base = -0.7 + 1.4 * (i as f32 + 0.5) / 9.0 + (hash01(i, 0x6A55_0001) - 0.5) * 0.12;
        let top = -0.95 + 0.5 * hash01(i, 0x6A55_0002);
        let lean = (hash01(i, 0x6A55_0003) - 0.5) * 0.7;
        if v > 1.0 || v < top {
            continue;
        }
        // 0 at the root, 1 at the tip.
        let t = (1.0 - v) / (1.0 - top);
        let cx = base + lean * t * t;
        let half = 0.07 * (1.0 - t);
        c = c.max(if (u - cx).abs() <= half { 1.0 } else { 0.0 });
    }
    c
}

/// A flat row of bristles of uneven size and pressure.
fn dry_brush(u: f32, v: f32, _: u32) -> f32 {
    let mut c: f32 = 0.0;
    for i in 0..16u32 {
        let cx = -0.92 + 1.84 * (i as f32 + 0.5) / 16.0;
        let cy = (hash01(i, 0xB415_0001) - 0.5) * 0.25;
        let r = 0.04 + 0.05 * hash01(i, 0xB415_0002);
        let ink = 0.55 + 0.45 * hash01(i, 0xB415_0003);
        c = c.max(disc(u, v, cx, cy, r) * ink);
    }
    c
}

/// A pointed leaf with a midrib, pointing up.
fn leaf(u: f32, v: f32, _: u32) -> f32 {
    // Two arcs meeting at the tips (v = ±0.9): half-width 0.45 at the
    // middle, tapering to zero.
    let t = v / 0.9;
    if t.abs() > 1.0 {
        return 0.0;
    }
    let half = 0.45 * (1.0 - t * t);
    if u.abs() > half {
        return 0.0;
    }
    if u.abs() < 0.025 && v > -0.8 {
        0.55 // the midrib reads lighter
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray_tips_normalise_to_coverage() {
        let t = BrushTip::from_gray8("t", 3, 1, &[0, 128, 255]).unwrap();
        assert_eq!(t.coverage(0, 0), 0.0);
        assert_eq!(t.coverage(1, 0), 128.0 / 255.0);
        assert_eq!(t.coverage(2, 0), 1.0);
        let t = BrushTip::from_gray16("t", 2, 1, &[0, 65535]).unwrap();
        assert_eq!(t.coverage(1, 0), 1.0);
        assert!(BrushTip::new("bad", 2, 2, vec![1.0; 3]).is_err());
        assert!(BrushTip::new("bad", 0, 2, vec![]).is_err());
    }

    #[test]
    fn mips_box_average_down_to_one_pixel() {
        // 4×2: a left column pair of ones, the rest zero.
        let t = BrushTip::new("t", 4, 2, vec![1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0]).unwrap();
        assert_eq!(t.level_count(), 3); // 4×2, 2×1, 1×1
        assert_eq!(t.levels[1].data, vec![1.0, 0.0]);
        assert_eq!(t.levels[2].data, vec![0.25]); // (1 + 0 + 0 + 0) / 4: the pad is empty
                                                  // Sampling a footprint of 2 tip px reads level 1 exactly.
        assert_eq!(t.sample(1.0, 1.0, 2.0), 1.0);
        // Halfway between levels 0 and 1 (footprint √2) blends them.
        let at0 = t.levels[0].bilinear(2.0, 1.0);
        let at1 = t.levels[1].bilinear(1.0, 0.5);
        assert!((t.sample(2.0, 1.0, 2f32.sqrt()) - (at0 + (at1 - at0) * 0.5)).abs() < 1e-6);
    }

    #[test]
    fn dynamics_are_deterministic_and_bounded() {
        let stations: Vec<Station> = (0..40)
            .map(|i| Station {
                x: i as f32 * 2.0,
                y: 10.0,
                pressure: 1.0,
                opacity: 1.0,
                direction: 0.0,
            })
            .collect();
        let d = BrushDynamics {
            size_jitter: 1.0,
            min_diameter: 0.25,
            angle_jitter: 0.5,
            roundness_jitter: 1.0,
            min_roundness: 0.4,
            opacity_jitter: 1.0,
            flow_jitter: 0.5,
            count: 3,
            count_jitter: 0.5,
            flip_x_jitter: true,
            ..BrushDynamics::default()
        };
        let a = apply_dynamics(10.0, 0.0, 30.0, 1.0, &d, &stations);
        let b = apply_dynamics(10.0, 0.0, 30.0, 1.0, &d, &stations);
        assert_eq!(a, b, "replays stamp the same dabs");
        // Count 3 with up to half dropped: between 2 and 3 dabs a station.
        assert!(a.len() >= 80 && a.len() <= 120 && a.len() < 120, "{}", a.len());
        for dab in &a {
            assert!(
                dab.radius >= 2.5 - 1e-5 && dab.radius <= 10.0,
                "min diameter 25 % of 10"
            );
            let turn = dab.angle - 30f32.to_radians();
            assert!(turn.abs() <= PI * 0.5 + 1e-5, "±90° at 50 % jitter");
            assert!(dab.roundness >= 0.4 && dab.roundness <= 1.0);
            assert!((0.0..=1.0).contains(&dab.opacity));
        }
        assert!(a.iter().any(|d| d.radius < 6.0) && a.iter().any(|d| d.radius > 8.0));
        assert!(a.iter().any(|d| d.flip_x) && a.iter().any(|d| !d.flip_x));
        assert!(a.iter().all(|d| !d.flip_y));
    }

    #[test]
    fn static_dynamics_pass_stations_through() {
        // Floors, grain size and the background colour vary nothing alone.
        let calm = BrushDynamics {
            min_roundness: 0.9,
            texture_scale: 3.0,
            background: [0.0; 3],
            ..BrushDynamics::default()
        };
        assert!(calm.is_static());
        let busy = BrushDynamics {
            count: 2,
            ..BrushDynamics::default()
        };
        assert!(!busy.is_static());
        let s = Station {
            x: 3.5,
            y: 4.5,
            pressure: 0.5,
            opacity: 0.25,
            direction: 1.0,
        };
        let dabs = apply_dynamics(8.0, 0.0, 0.0, 1.0, &BrushDynamics::default(), &[s]);
        assert_eq!(
            dabs,
            vec![Dab {
                x: 3.5,
                y: 4.5,
                radius: 4.0,
                opacity: 0.25,
                angle: 0.0,
                roundness: 1.0,
                flip_x: false,
                flip_y: false,
                color: None,
            }]
        );
        // Minimum diameter lifts pressure 0 to the floor: 8 × 0.5 = 4.
        let floor = BrushDynamics {
            min_diameter: 0.5,
            ..BrushDynamics::default()
        };
        let zero = Station { pressure: 0.0, ..s };
        assert_eq!(apply_dynamics(8.0, 0.0, 0.0, 1.0, &floor, &[zero])[0].radius, 4.0);
        // Follow direction adds the stroke direction to the tip angle.
        let follow = BrushDynamics {
            follow_direction: true,
            ..BrushDynamics::default()
        };
        assert_eq!(
            apply_dynamics(8.0, 0.0, 90.0, 1.0, &follow, &[s])[0].angle,
            PI / 2.0 + 1.0
        );
    }

    #[test]
    fn scatter_across_moves_dabs_only_along_the_normal() {
        let stations: Vec<Station> = (0..20)
            .map(|i| Station {
                x: i as f32,
                y: 50.0,
                pressure: 1.0,
                opacity: 1.0,
                direction: 0.0, // rightwards: the normal is vertical
            })
            .collect();
        let d = BrushDynamics {
            scatter_across: true,
            ..BrushDynamics::default()
        };
        let dabs = apply_dynamics(10.0, 2.0, 0.0, 1.0, &d, &stations);
        for (dab, s) in dabs.iter().zip(&stations) {
            assert_eq!(dab.x, s.x);
            assert!((dab.y - 50.0).abs() <= 20.0); // 2 × radius
        }
        assert!(dabs.iter().any(|d| (d.y - 50.0).abs() > 5.0));
    }

    #[test]
    fn builtin_tips_are_generated_identically() {
        for name in BUILTIN_TIPS {
            let a = builtin_tip(name).unwrap();
            let b = builtin_tip(name).unwrap();
            assert_eq!(a.name(), name);
            assert_eq!(a.levels[0].data, b.levels[0].data, "{name} is deterministic");
            let ink: f32 = a.levels[0].data.iter().sum();
            let n = (a.width() * a.height()) as f32;
            assert!(ink > n * 0.05 && ink < n * 0.9, "{name} covers {:.2}", ink / n);
        }
        assert!(builtin_tip("Nope").is_none());
        let chalk = builtin_tip("Chalk").unwrap();
        assert_eq!((chalk.width(), chalk.height()), (64, 64));
        // Corners are outside the chalk disc.
        assert_eq!(chalk.coverage(0, 0), 0.0);
        assert_eq!(chalk.thumbnail(16).len(), 256);
    }
}
