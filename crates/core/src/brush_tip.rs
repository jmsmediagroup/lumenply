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
        let (fx, fy) = (x - 0.5, y - 0.5);
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

impl BrushTip {
    /// A tip from coverage values in `[0, 1]`, row-major. Values are
    /// clamped; NaN reads as no paint.
    pub fn new(name: impl Into<String>, width: u32, height: u32, coverage: Vec<f32>) -> Result<Self, String> {
        if width == 0 || height == 0 || width > MAX_TIP_SIDE || height > MAX_TIP_SIDE {
            return Err(format!(
                "a brush tip must be 1–{MAX_TIP_SIDE} px on a side, not {width}×{height}"
            ));
        }
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

    /// Number of mip levels (the tip itself counts as one).
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// Coverage at `(x, y)` in tip pixels (centres at `i + 0.5`) for a
    /// sample that spans `footprint` tip pixels: above 1 the sample reads
    /// the matching mip levels, so detail finer than a canvas pixel
    /// averages out instead of aliasing.
    pub fn sample(&self, x: f32, y: f32, footprint: f32) -> f32 {
        let last = self.levels.len() - 1;
        let lod = if footprint > 1.0 {
            footprint.log2().min(last as f32)
        } else {
            0.0
        };
        let l0 = lod.floor() as usize;
        let t = lod - l0 as f32;
        let s0 = 1.0 / (1u64 << l0) as f32;
        let v0 = self.levels[l0].bilinear(x * s0, y * s0);
        if t <= 0.0 || l0 >= last {
            return v0;
        }
        let s1 = s0 * 0.5;
        let v1 = self.levels[l0 + 1].bilinear(x * s1, y * s1);
        v0 + (v1 - v0) * t
    }

    /// The farthest a covered pixel can lie from the dab centre, as a
    /// multiple of the dab radius, at any angle: half the diagonal of the
    /// tip grown by half a texel each side (the bilinear fade) over half
    /// its longer side.
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
/// default varies nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// the amount is [`crate::commands::Brush::jitter`].
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
        }
    }
}

impl BrushDynamics {
    /// True when no setting varies the dabs.
    pub fn is_static(&self) -> bool {
        let d = BrushDynamics {
            min_roundness: self.min_roundness,
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
}

/// Deterministic hash of `(n, salt)` onto `[0, 1)`.
pub(crate) fn hash01(n: u32, salt: u32) -> f32 {
    let mut h = n.wrapping_mul(0x27D4_EB2F) ^ salt;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h >> 8) as f32 / (1u32 << 24) as f32
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
pub(crate) fn size_curve(min_diameter: f32, pressure: f32) -> f32 {
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
pub(crate) struct Station {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub opacity: f32,
    pub direction: f32,
}

/// Turn stroke stations into dabs: count, scatter, size, angle,
/// roundness, flips and transfer, each hashed from the dab's index.
pub(crate) fn apply_dynamics(
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
            });
            k += 1;
        }
    }
    dabs
}

/// Call `f(x, y, coverage)` for every canvas pixel under a dab of a
/// sampled tip, or of the computed round tip (`tip == None`, with
/// `hardness`) squashed and turned. Opacity and selection are the
/// caller's.
pub(crate) fn stamp(
    tip: Option<&BrushTip>,
    hardness: f32,
    d: &Dab,
    canvas: Rect,
    mut f: impl FnMut(i32, i32, f32),
) {
    let r = d.radius;
    if r <= 0.0 {
        return;
    }
    let rho = d.roundness.clamp(0.01, 1.0);
    let (sin, cos) = d.angle.sin_cos();
    // Half extents of the unturned footprint, and canvas px per tip px.
    // A sampled tip reaches half a texel past its edge, where bilinear
    // filtering fades it out (see [`BrushTip::reach`]).
    let (hw, hh, scale) = match tip {
        None => (r, r * rho, 1.0),
        Some(t) => {
            let scale = 2.0 * r / t.width.max(t.height) as f32;
            let (w, h) = (t.width as f32 + 1.0, t.height as f32 + 1.0);
            (w * scale * 0.5, h * scale * 0.5 * rho, scale)
        }
    };
    let ex = (hw * cos).abs() + (hh * sin).abs();
    let ey = (hw * sin).abs() + (hh * cos).abs();
    let x0 = (d.x - ex).floor() as i32;
    let y0 = (d.y - ey).floor() as i32;
    let x1 = (d.x + ex).ceil() as i32;
    let y1 = (d.y + ey).ceil() as i32;
    let area = Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32).intersect(&canvas);
    let hard = hardness.clamp(0.0, 0.999);
    let footprint = 1.0 / (scale * rho);
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
                    t.sample(tx, ty, footprint)
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

#[cfg(test)]
mod stroke_tests {
    use super::*;
    use crate::commands::{Brush, BrushMode, PaintStroke, StrokePoint};
    use crate::Command;
    use lumenply_doc::{Document, LayerId};
    use lumenply_tiles::Rgba;

    /// The round-brush painter as it was before tips and dynamics, kept
    /// verbatim so the default brush is proven to render bit-identically.
    mod legacy {
        use super::super::hash01;
        use crate::commands::{smooth_stroke, Brush, StrokePoint};
        use lumenply_doc::Selection;
        use lumenply_tiles::Rect;

        pub fn interpolate_dabs(brush: &Brush, points: &[StrokePoint]) -> Vec<StrokePoint> {
            let smoothed = smooth_stroke(points);
            let points = &smoothed[..];
            let mut dabs = vec![points[0]];
            let mut carry = 0.0f32;
            let jitter = brush.jitter.clamp(0.0, 1.0);
            for pair in points.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                if len <= 0.0 {
                    continue;
                }
                let eff = brush.radius * (0.5 * (a.pressure + b.pressure)).clamp(0.0, 1.0);
                let step = (brush.spacing * eff).max(0.5);
                let mut t = step - carry;
                while t <= len {
                    let f = t / len;
                    dabs.push(
                        StrokePoint::new(
                            a.x + (b.x - a.x) * f,
                            a.y + (b.y - a.y) * f,
                            a.pressure + (b.pressure - a.pressure) * f,
                        )
                        .with_opacity(a.opacity + (b.opacity - a.opacity) * f),
                    );
                    t += step;
                }
                carry = len - (t - step);
            }
            if jitter > 0.0 {
                for (i, d) in dabs.iter_mut().enumerate() {
                    let angle = hash01(i as u32, 0x9E37_79B9) * std::f32::consts::TAU;
                    let dist = hash01(i as u32, 0x85EB_CA6B).sqrt() * jitter * brush.radius;
                    d.x += angle.cos() * dist;
                    d.y += angle.sin() * dist;
                }
            }
            dabs
        }

        pub fn dab_coverage(
            brush: &Brush,
            p: StrokePoint,
            canvas: Rect,
            sel: Option<&Selection>,
            mut f: impl FnMut(i32, i32, f32),
        ) {
            let r = brush.radius * p.pressure.clamp(0.0, 1.0);
            if r <= 0.0 {
                return;
            }
            let x0 = (p.x - r).floor() as i32;
            let y0 = (p.y - r).floor() as i32;
            let x1 = (p.x + r).ceil() as i32;
            let y1 = (p.y + r).ceil() as i32;
            let area = Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32).intersect(&canvas);
            let hard = brush.hardness.clamp(0.0, 0.999);
            for py in area.y..area.bottom() {
                for px in area.x..area.right() {
                    let dx = px as f32 + 0.5 - p.x;
                    let dy = py as f32 + 0.5 - p.y;
                    let d = (dx * dx + dy * dy).sqrt() / r;
                    let edge = ((1.0 - d) * r + 0.5).clamp(0.0, 1.0);
                    let soft = if d <= hard {
                        1.0
                    } else {
                        1.0 - (d - hard) / (1.0 - hard)
                    };
                    let cover = edge
                        * soft.clamp(0.0, 1.0)
                        * sel.map_or(1.0, |s| s.value(px, py))
                        * p.opacity.clamp(0.0, 1.0);
                    if cover > 0.0 {
                        f(px, py, cover);
                    }
                }
            }
        }
    }

    fn doc() -> (Document, LayerId) {
        let mut doc = Document::new(96, 64);
        let id = doc.add_pixel_layer("p");
        (doc, id)
    }

    fn paint(brush: Brush, points: Vec<StrokePoint>) -> Document {
        let (mut d, id) = doc();
        PaintStroke {
            layer: id,
            brush,
            points,
        }
        .apply(&mut d)
        .unwrap();
        d
    }

    fn px(d: &Document, x: i32, y: i32) -> Rgba {
        d.layers()[0].pixels().unwrap().get_pixel(x, y)
    }

    #[test]
    fn the_round_tip_renders_exactly_as_before() {
        let wavy: Vec<StrokePoint> = (0..30)
            .map(|i| {
                let t = i as f32 / 29.0;
                StrokePoint::new(8.0 + 80.0 * t, 32.0 + 14.0 * (t * 6.0).sin(), 0.3 + 0.7 * t)
                    .with_opacity(1.0 - 0.6 * t)
            })
            .collect();
        let brushes = [
            Brush::default(),
            Brush {
                radius: 11.5,
                hardness: 0.0,
                color: [0.9, 0.2, 0.1, 0.7],
                spacing: 0.1,
                ..Brush::default()
            },
            Brush {
                radius: 6.0,
                hardness: 1.0,
                jitter: 0.8,
                // A circle has no angle: turning it changes nothing.
                angle: 37.0,
                ..Brush::default()
            },
        ];
        for (n, brush) in brushes.into_iter().enumerate() {
            let new = paint(brush.clone(), wavy.clone());
            let (mut old, id) = doc();
            let canvas = old.canvas();
            let store = old.layer_mut(id).unwrap().pixels_mut().unwrap();
            let [cr, cg, cb, ca] = brush.color;
            for d in legacy::interpolate_dabs(&brush, &wavy) {
                legacy::dab_coverage(&brush, d, canvas, None, |x, y, cover| {
                    let dst = store.get_pixel(x, y);
                    store.set_pixel(x, y, Rgba::from_straight(cr, cg, cb, ca * cover).over(dst));
                });
            }
            let mut painted = 0;
            for y in 0..64 {
                for x in 0..96 {
                    let (a, b) = (px(&new, x, y), px(&old, x, y));
                    assert_eq!(
                        (a.r.to_bits(), a.g.to_bits(), a.b.to_bits(), a.a.to_bits()),
                        (b.r.to_bits(), b.g.to_bits(), b.b.to_bits(), b.a.to_bits()),
                        "brush {n} differs at ({x}, {y})"
                    );
                    painted += (a.a > 0.0) as u32;
                }
            }
            assert!(painted > 300, "brush {n} painted {painted} px");
        }
        // And one explicit value: the default brush's centre is solid black.
        let d = paint(Brush::default(), vec![StrokePoint::new(20.5, 20.5, 1.0)]);
        assert_eq!(px(&d, 20, 20).a, 1.0);
        assert_eq!(px(&d, 20 + 9, 20).a, 0.0);
    }

    /// 4×4, left half full paint, right half empty.
    fn half_tip() -> Arc<BrushTip> {
        let cov = (0..16).map(|i| if i % 4 < 2 { 1.0 } else { 0.0 }).collect();
        Arc::new(BrushTip::new("half", 4, 4, cov).unwrap())
    }

    #[test]
    fn a_sampled_tip_stamps_scaled_with_bilinear_edges() {
        // Radius 4 = an 8 px dab: each tip pixel covers 2×2 canvas pixels.
        let brush = Brush {
            radius: 4.0,
            tip: Some(half_tip()),
            ..Brush::default()
        };
        let d = paint(brush, vec![StrokePoint::new(10.0, 10.0, 1.0)]);
        // The edge fades across one texel (two pixels), centred on it.
        let row: Vec<f32> = (4..12).map(|x| px(&d, x, 10).a).collect();
        assert_eq!(row, vec![0.0, 0.25, 0.75, 1.0, 1.0, 0.75, 0.25, 0.0]);
        // The top and bottom edges fade in the same way.
        let col: Vec<f32> = [4, 5, 6, 13, 14, 15].iter().map(|&y| px(&d, 7, y).a).collect();
        assert_eq!(col, vec![0.0, 0.25, 0.75, 0.75, 0.25, 0.0]);
    }

    #[test]
    fn tips_turn_counter_clockwise_and_squash() {
        // An 8×2 bar → 16×4 px at radius 8; at 90° it stands upright.
        let bar = Arc::new(BrushTip::new("bar", 8, 2, vec![1.0; 16]).unwrap());
        let flat = Brush {
            radius: 8.0,
            tip: Some(bar.clone()),
            ..Brush::default()
        };
        let d = paint(flat.clone(), vec![StrokePoint::new(30.0, 30.0, 1.0)]);
        assert_eq!((px(&d, 36, 30).a, px(&d, 30, 36).a), (1.0, 0.0));
        let upright = Brush { angle: 90.0, ..flat };
        let d = paint(upright, vec![StrokePoint::new(30.0, 30.0, 1.0)]);
        assert_eq!((px(&d, 36, 30).a, px(&d, 30, 36).a), (0.0, 1.0));
        assert_eq!(px(&d, 30, 23).a, 1.0);
        // The round tip at 50 % roundness is a 20×10 ellipse.
        let oval = Brush {
            radius: 10.0,
            hardness: 1.0,
            roundness: 0.5,
            ..Brush::default()
        };
        let d = paint(oval.clone(), vec![StrokePoint::new(40.5, 30.5, 1.0)]);
        assert_eq!((px(&d, 47, 30).a, px(&d, 40, 33).a), (1.0, 1.0));
        assert_eq!((px(&d, 40, 37).a, px(&d, 52, 30).a), (0.0, 0.0));
        // Turned 90°: now tall.
        let d = paint(
            Brush { angle: 90.0, ..oval },
            vec![StrokePoint::new(40.5, 30.5, 1.0)],
        );
        assert_eq!((px(&d, 40, 37).a, px(&d, 47, 30).a), (1.0, 0.0));
    }

    #[test]
    fn small_dabs_of_a_detailed_tip_average_instead_of_aliasing() {
        // A 64×64 one-pixel checkerboard stamped 4 px wide: without mips
        // each canvas pixel would land on a single 0 or 1 texel.
        let cov = (0..64 * 64).map(|i| ((i % 64 + i / 64) % 2) as f32).collect();
        let checker = Arc::new(BrushTip::new("checker", 64, 64, cov).unwrap());
        let brush = Brush {
            radius: 2.0,
            tip: Some(checker),
            ..Brush::default()
        };
        let d = paint(brush, vec![StrokePoint::new(10.0, 10.0, 1.0)]);
        for (x, y) in [(9, 9), (10, 9), (9, 10), (10, 10)] {
            assert!(
                (px(&d, x, y).a - 0.5).abs() < 0.02,
                "({x}, {y}) = {}",
                px(&d, x, y).a
            );
        }
    }

    #[test]
    fn dynamic_strokes_replay_identically_and_stay_in_bounds() {
        let brush = Brush {
            radius: 6.0,
            jitter: 2.0,
            tip: builtin_tip("Spatter"),
            dynamics: BrushDynamics {
                size_jitter: 0.8,
                angle_jitter: 1.0,
                roundness_jitter: 0.5,
                count: 3,
                count_jitter: 0.5,
                opacity_jitter: 0.6,
                flip_x_jitter: true,
                follow_direction: true,
                ..BrushDynamics::default()
            },
            ..Brush::default()
        };
        let points: Vec<StrokePoint> = (0..12)
            .map(|i| StrokePoint::new(20.0 + i as f32 * 5.0, 32.0, 1.0))
            .collect();
        let a = paint(brush.clone(), points.clone());
        let b = paint(brush.clone(), points.clone());
        let bounds = crate::commands::stroke_bounds(&brush, &points, a.canvas());
        let mut painted = 0;
        for y in 0..64 {
            for x in 0..96 {
                let (p, q) = (px(&a, x, y), px(&b, x, y));
                assert_eq!(p.a.to_bits(), q.a.to_bits(), "({x}, {y}) replays identically");
                if p.a > 0.0 {
                    painted += 1;
                    assert!(bounds.contains(x, y), "({x}, {y}) outside {bounds:?}");
                }
            }
        }
        assert!(painted > 200, "{painted}");
        // The mode still applies to sampled tips: an erase lifts paint.
        let erase = Brush {
            mode: BrushMode::Erase,
            tip: Some(half_tip()),
            radius: 4.0,
            dynamics: BrushDynamics::default(),
            jitter: 0.0,
            ..brush
        };
        let (mut d, id) = doc();
        crate::commands::Fill {
            layer: id,
            color: [1.0, 1.0, 1.0, 1.0],
        }
        .apply(&mut d)
        .unwrap();
        PaintStroke {
            layer: id,
            brush: erase,
            points: vec![StrokePoint::new(10.0, 10.0, 1.0)],
        }
        .apply(&mut d)
        .unwrap();
        assert_eq!(
            (px(&d, 7, 10).a, px(&d, 10, 10).a, px(&d, 11, 10).a),
            (0.0, 0.75, 1.0)
        );
    }
}
