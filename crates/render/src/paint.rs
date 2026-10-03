//! Painting a stroke into pixels: the brush, the dabs it stamps along a
//! stroke, and every mode of `lumenply-core`'s `PaintStroke` (paint,
//! erase, dodge, burn, smudge, the sponge, blur and sharpen). It lives
//! here rather than in `core` so the edit graph (ADR 0025) can replay a
//! stroke as an operation; `core` re-exports these types under their old
//! paths.
//!
//! A stroke is painted in two steps: [`interpolate_dabs`] turns its points
//! into dabs (smoothing, spacing, dynamics; deterministic), and
//! [`paint_dabs`] stamps them in order. [`dab_area`] and [`dabs_reach`]
//! say which pixels that writes and reads, so a caller can paint part of
//! a canvas and know exactly what it may skip.

use std::fmt;

use lumenply_doc::adjust::{srgb_decode, srgb_encode};
use lumenply_doc::Selection;
use lumenply_tiles::{Rect, Rgba, TileStore};
use serde::{Deserialize, Serialize};

pub use crate::brush_tip::{BrushDynamics, BrushTip, Dab};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrushMode {
    /// Lay colour down over what is there.
    #[default]
    Paint,
    /// Remove coverage (the eraser); `color[3]` acts as strength.
    Erase,
    /// Lighten what is there (gamma-domain, like the classic tool);
    /// `color[3]` acts as strength.
    Dodge,
    /// Darken what is there; `color[3]` acts as strength.
    Burn,
    /// Drag pixels along the stroke; `color[3]` acts as strength.
    Smudge,
    /// Boost saturation (the sponge); `color[3]` acts as strength.
    Saturate,
    /// Drain saturation; `color[3]` acts as strength.
    Desaturate,
    /// Soften what is there (a small box blur per dab); `color[3]` acts as
    /// strength.
    Blur,
    /// Crisp up what is there (unsharp mask per dab); `color[3]` acts as
    /// strength.
    Sharpen,
    /// Paint from a past state (the history brush). A plain stroke cannot
    /// carry the state, so [`paint_stroke`] refuses this mode: use
    /// `HistoryStroke` in `lumenply-core`.
    History,
}

impl BrushMode {
    /// True for the modes where a pixel's result depends only on that
    /// pixel and the dabs over it (paint, erase, dodge, burn, the sponge).
    /// A stroke in these modes can be painted one tile at a time; smudge,
    /// blur and sharpen read around each dab, so their dabs interact.
    pub fn is_local(self) -> bool {
        matches!(
            self,
            BrushMode::Paint
                | BrushMode::Erase
                | BrushMode::Dodge
                | BrushMode::Burn
                | BrushMode::Saturate
                | BrushMode::Desaturate
        )
    }
}

/// A brush: the computed round tip or a sampled one, with Photoshop's
/// tip shape (angle, roundness) and per-dab dynamics. Pressure scales the
/// radius; `hardness` 1.0 is a crisp (antialiased) edge, 0.0 a fully soft
/// falloff (round tip only). Not `Copy`: a sampled tip is shared image
/// data, so clone it (cheap, the tip is an `Arc`).
#[derive(Clone, Debug)]
pub struct Brush {
    pub radius: f32,
    pub hardness: f32,
    /// Straight linear RGBA.
    pub color: [f32; 4],
    /// Distance between dabs as a fraction of the radius.
    pub spacing: f32,
    /// Scatter: each dab lands up to `jitter × radius` off the stroke, in
    /// a direction and distance hashed from its index, so replays are
    /// deterministic. 0 keeps dabs on the line; at most
    /// [`crate::brush_tip::MAX_SCATTER`].
    pub jitter: f32,
    pub mode: BrushMode,
    /// A sampled tip (grayscale coverage), or `None` for the computed
    /// round tip.
    pub tip: Option<std::sync::Arc<BrushTip>>,
    /// Tip angle in degrees, counter-clockwise.
    pub angle: f32,
    /// Tip roundness in `(0, 1]`: 1 keeps its proportions, less squashes
    /// it across its angle.
    pub roundness: f32,
    /// Shape dynamics, count and transfer jitter.
    pub dynamics: BrushDynamics,
}

impl Default for Brush {
    fn default() -> Self {
        Brush {
            radius: 8.0,
            hardness: 0.8,
            color: [0.0, 0.0, 0.0, 1.0],
            spacing: 0.2,
            jitter: 0.0,
            mode: BrushMode::Paint,
            tip: None,
            angle: 0.0,
            roundness: 1.0,
            dynamics: BrushDynamics::default(),
        }
    }
}

impl Brush {
    /// How far past the dab radius a dab can reach at any angle, as a
    /// multiple of it: 1 for the round tip, half the diagonal over half
    /// the longer side for a sampled one.
    pub fn reach(&self) -> f32 {
        self.tip.as_ref().map_or(1.0, |t| t.reach())
    }

    /// Canvas pixels a sampled tip can reach beyond `radius × reach()`
    /// when stamped small ([`crate::brush_tip::TIP_MARGIN`]); 0 for the
    /// round tip.
    pub fn margin(&self) -> f32 {
        if self.tip.is_some() {
            crate::brush_tip::TIP_MARGIN
        } else {
            0.0
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    /// Scales the dab radius (0..=1).
    pub pressure: f32,
    /// Scales the dab's coverage (0..=1): pen pressure mapped to opacity.
    pub opacity: f32,
}

impl StrokePoint {
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        StrokePoint {
            x,
            y,
            pressure,
            opacity: 1.0,
        }
    }

    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }
}

/// Bounding box of a stroke's dabs, grown by the brush radius.
pub fn stroke_bounds(brush: &Brush, points: &[StrokePoint], canvas: Rect) -> Rect {
    let scatter = brush.jitter.clamp(0.0, crate::brush_tip::MAX_SCATTER);
    let r = (brush.radius * (brush.reach() + scatter) + brush.margin()).ceil() as i32 + 2;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for p in points {
        x0 = x0.min(p.x.floor() as i32 - r);
        y0 = y0.min(p.y.floor() as i32 - r);
        x1 = x1.max(p.x.ceil() as i32 + r);
        y1 = y1.max(p.y.ceil() as i32 + r);
    }
    if x1 < x0 {
        return Rect::default();
    }
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&canvas)
}

/// Smooth sparse input points with a Catmull-Rom spline so strokes drawn
/// with a mouse (few events per frame) don't look like polylines. Input with
/// fewer than three points is returned unchanged.
pub fn smooth_stroke(points: &[StrokePoint]) -> Vec<StrokePoint> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(points.len() * 6);
    let get = |i: isize| points[i.clamp(0, points.len() as isize - 1) as usize];
    for i in 0..points.len() - 1 {
        let (p0, p1, p2, p3) = (
            get(i as isize - 1),
            get(i as isize),
            get(i as isize + 1),
            get(i as isize + 2),
        );
        let seg_len = ((p2.x - p1.x).powi(2) + (p2.y - p1.y).powi(2)).sqrt();
        let steps = (seg_len / 3.0).ceil().clamp(1.0, 24.0) as usize;
        for k in 0..steps {
            let t = k as f32 / steps as f32;
            let (t2, t3) = (t * t, t * t * t);
            let cr = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * ((2.0 * b)
                    + (-a + c) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (-a + 3.0 * b - 3.0 * c + d) * t3)
            };
            out.push(
                StrokePoint::new(
                    cr(p0.x, p1.x, p2.x, p3.x),
                    cr(p0.y, p1.y, p2.y, p3.y),
                    p1.pressure + (p2.pressure - p1.pressure) * t,
                )
                .with_opacity(p1.opacity + (p2.opacity - p1.opacity) * t),
            );
        }
    }
    out.push(*points.last().expect("non-empty"));
    out
}

/// Dabs along a polyline, spaced by the pressure-scaled radius so thin
/// stroke ends stay continuous, then varied by the brush's dynamics
/// (scatter, count, size, angle, roundness, transfer). Each variation is
/// a hash of the dab's index: deterministic, so undo previews and replays
/// stamp identical pixels. `points` must not be empty.
pub fn interpolate_dabs(brush: &Brush, points: &[StrokePoint]) -> Vec<Dab> {
    use crate::brush_tip::{size_curve, Station};
    let smoothed = smooth_stroke(points);
    let points = &smoothed[..];
    // Direction of travel, counter-clockwise from rightwards (screen y
    // points down); a lone dab faces right.
    let heading = |a: StrokePoint, b: StrokePoint| (a.y - b.y).atan2(b.x - a.x);
    let first_dir = points
        .windows(2)
        .find(|w| w[0].x != w[1].x || w[0].y != w[1].y)
        .map_or(0.0, |w| heading(w[0], w[1]));
    let station = |p: StrokePoint, direction: f32| Station {
        x: p.x,
        y: p.y,
        pressure: p.pressure,
        opacity: p.opacity,
        direction,
    };
    let mut stations = vec![station(points[0], first_dir)];
    let mut carry = 0.0f32;
    let min_d = brush.dynamics.min_diameter;
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        if len <= 0.0 {
            continue;
        }
        let dir = heading(a, b);
        let eff = brush.radius * size_curve(min_d, 0.5 * (a.pressure + b.pressure));
        let step = (brush.spacing * eff).max(0.5);
        let mut t = step - carry;
        while t <= len {
            let f = t / len;
            let p = StrokePoint::new(
                a.x + (b.x - a.x) * f,
                a.y + (b.y - a.y) * f,
                a.pressure + (b.pressure - a.pressure) * f,
            )
            .with_opacity(a.opacity + (b.opacity - a.opacity) * f);
            stations.push(station(p, dir));
            t += step;
        }
        carry = len - (t - step);
    }
    let mut dabs = crate::brush_tip::apply_dynamics(
        brush.radius,
        brush.jitter,
        brush.angle,
        brush.roundness,
        &brush.dynamics,
        &stations,
    );
    let [r, g, b, _] = brush.color;
    crate::brush_tip::apply_color_dynamics(&mut dabs, [r, g, b], &brush.dynamics);
    dabs
}

/// Whether a dab goes through [`crate::brush_tip::stamp`] (sampled tip,
/// squashed or textured) rather than the plain round-tip loop.
fn stamped(brush: &Brush, p: &Dab) -> bool {
    let depth = brush.dynamics.texture_depth.clamp(0.0, 1.0);
    brush.tip.is_some() || p.roundness < 1.0 || depth > 0.0
}

/// The pixels the plain round tip visits for a dab, clipped to `canvas`.
fn round_area(p: &Dab, canvas: Rect) -> Rect {
    let r = p.radius;
    let x0 = (p.x - r).floor() as i32;
    let y0 = (p.y - r).floor() as i32;
    let x1 = (p.x + r).ceil() as i32;
    let y1 = (p.y + r).ceil() as i32;
    Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32).intersect(&canvas)
}

/// The canvas pixels [`dab_coverage`] visits for a dab, clipped to
/// `canvas`: every pixel it gives coverage lies inside.
pub fn dab_area(brush: &Brush, p: &Dab, canvas: Rect) -> Rect {
    if p.radius <= 0.0 {
        return Rect::default();
    }
    if stamped(brush, p) {
        crate::brush_tip::stamp_area(brush.tip.as_deref(), p, canvas)
    } else {
        round_area(p, canvas)
    }
}

/// Call `f(x, y, coverage)` for every canvas pixel a dab touches, with the
/// selection and the dab's opacity already applied to the coverage.
pub fn dab_coverage(
    brush: &Brush,
    p: impl std::borrow::Borrow<Dab>,
    canvas: Rect,
    sel: Option<&Selection>,
    mut f: impl FnMut(i32, i32, f32),
) {
    let p: &Dab = p.borrow();
    let r = p.radius;
    if r <= 0.0 {
        return;
    }
    let depth = brush.dynamics.texture_depth.clamp(0.0, 1.0);
    if stamped(brush, p) {
        let scale = brush.dynamics.texture_scale;
        crate::brush_tip::stamp(
            brush.tip.as_deref(),
            brush.hardness,
            p,
            canvas,
            |px, py, mut c| {
                if depth > 0.0 {
                    c *= crate::brush_tip::grain_mask(crate::brush_tip::grain(px, py, scale), depth);
                }
                let cover = c * sel.map_or(1.0, |s| s.value(px, py)) * p.opacity;
                if cover > 0.0 {
                    f(px, py, cover);
                }
            },
        );
        return;
    }
    // The computed round tip (angle can't change a circle), exactly as it
    // has always rendered.
    let area = round_area(p, canvas);
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

/// Why a stroke can't be painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintError {
    /// A stroke needs at least one point.
    NoPoints,
    /// History mode paints from a past state a plain stroke doesn't carry.
    NeedsSource,
}

impl fmt::Display for PaintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PaintError::NoPoints => "stroke has no points",
            PaintError::NeedsSource => "the history brush needs a source state",
        })
    }
}

impl std::error::Error for PaintError {}

/// Paint a stroke into a pixel layer's tiles, in any mode but History.
/// Writes stay inside `canvas`; `sel` (if any) scales every dab's
/// coverage.
pub fn paint_stroke(
    store: &mut TileStore,
    brush: &Brush,
    points: &[StrokePoint],
    canvas: Rect,
    sel: Option<&Selection>,
) -> Result<(), PaintError> {
    if points.is_empty() {
        return Err(PaintError::NoPoints);
    }
    if brush.mode == BrushMode::History {
        return Err(PaintError::NeedsSource);
    }
    paint_dabs(store, brush, &interpolate_dabs(brush, points), canvas, sel)
}

/// Stamp `dabs` (from [`interpolate_dabs`]) in order, in the brush's mode.
///
/// For the local modes ([`BrushMode::is_local`]) `canvas` only clips: painting
/// with it narrowed to one tile, and only the dabs whose [`dab_area`]
/// reaches that tile, paints exactly that tile's pixels of the whole
/// stroke. Smudge, blur and sharpen read around each dab (and clamp at the
/// canvas edge), so they need every dab and the real canvas.
pub fn paint_dabs(
    store: &mut TileStore,
    brush: &Brush,
    dabs: &[Dab],
    canvas: Rect,
    sel: Option<&Selection>,
) -> Result<(), PaintError> {
    match brush.mode {
        BrushMode::History => return Err(PaintError::NeedsSource),
        BrushMode::Blur | BrushMode::Sharpen => filter_dabs(store, brush, dabs, canvas, sel),
        BrushMode::Smudge => smudge_dabs(store, brush, dabs, canvas, sel),
        _ => local_dabs(store, brush, dabs, canvas, sel),
    }
    Ok(())
}

/// Paint, erase, dodge, burn and the sponge: each pixel changes on its own.
fn local_dabs(store: &mut TileStore, brush: &Brush, dabs: &[Dab], canvas: Rect, sel: Option<&Selection>) {
    let [cr, cg, cb, ca] = brush.color;
    let mode = brush.mode;
    for d in dabs {
        // Colour dynamics give a dab its own colour.
        let [cr, cg, cb] = d.color.unwrap_or([cr, cg, cb]);
        dab_coverage(brush, d, canvas, sel, |px, py, cover| {
            let dst = store.get_pixel(px, py);
            let out = match mode {
                BrushMode::Paint => Rgba::from_straight(cr, cg, cb, ca * cover).over(dst),
                BrushMode::Erase => dst.scale(1.0 - (ca * cover).clamp(0.0, 1.0)),
                BrushMode::Dodge | BrushMode::Burn => {
                    if dst.a <= 0.0 {
                        return;
                    }
                    let k = (ca * cover).clamp(0.0, 1.0);
                    let [r, g, b, a] = dst.to_straight();
                    let tone = |c: f32| {
                        let e = srgb_encode(c);
                        let e = if mode == BrushMode::Dodge {
                            e + (1.0 - e) * k
                        } else {
                            e * (1.0 - k)
                        };
                        srgb_decode(e)
                    };
                    Rgba::from_straight(tone(r), tone(g), tone(b), a)
                }
                BrushMode::Saturate | BrushMode::Desaturate => {
                    if dst.a <= 0.0 {
                        return;
                    }
                    let k = (ca * cover).clamp(0.0, 1.0);
                    // Scale chroma around the gamma-domain luminance,
                    // like the sponge tool's perceptual behaviour.
                    let scale = if mode == BrushMode::Saturate {
                        1.0 + k
                    } else {
                        1.0 - k
                    };
                    let [r, g, b, a] = dst.to_straight();
                    let enc = srgb_encode;
                    let dec = srgb_decode;
                    let (er, eg, eb) = (enc(r), enc(g), enc(b));
                    let y = 0.2126 * er + 0.7152 * eg + 0.0722 * eb;
                    let sat = |c: f32| dec((y + (c - y) * scale).clamp(0.0, 1.0));
                    Rgba::from_straight(sat(er), sat(eg), sat(eb), a)
                }
                BrushMode::Smudge | BrushMode::Blur | BrushMode::Sharpen | BrushMode::History => {
                    unreachable!("handled by paint_dabs")
                }
            };
            store.set_pixel(px, py, out);
        });
    }
}

/// Where a smudge dab `d` takes its paint from: the offset back to the
/// previous dab and the square of pixels it snapshots there. `None` when
/// the dab doesn't move (it smudges nothing).
fn smudge_source(brush: &Brush, prev: &Dab, d: &Dab) -> Option<(i32, i32, Rect)> {
    let (ox, oy) = ((d.x - prev.x).round() as i32, (d.y - prev.y).round() as i32);
    if ox == 0 && oy == 0 {
        return None;
    }
    let reach = brush.radius * brush.reach() + brush.margin();
    let r = (reach.ceil() as i32 + 2).max(1);
    let (sx, sy) = (d.x.round() as i32 - ox - r, d.y.round() as i32 - oy - r);
    let side = (2 * r + 1) as u32;
    Some((ox, oy, Rect::new(sx, sy, side, side)))
}

/// Smudge: each dab stamps the pixels from under the previous dab,
/// sampled from a snapshot so a dab never reads its own writes.
fn smudge_dabs(store: &mut TileStore, brush: &Brush, dabs: &[Dab], canvas: Rect, sel: Option<&Selection>) {
    let ca = brush.color[3];
    let mut prev: Option<Dab> = None;
    for &d in dabs {
        if let Some(p) = prev {
            if let Some((ox, oy, src)) = smudge_source(brush, &p, &d) {
                let (sx, sy) = (src.x, src.y);
                let side = src.w as usize;
                let mut snap = vec![Rgba::TRANSPARENT; side * side];
                for (i, q) in snap.iter_mut().enumerate() {
                    let (gx, gy) = (sx + (i % side) as i32, sy + (i / side) as i32);
                    *q = store.get_pixel(gx, gy);
                }
                dab_coverage(brush, d, canvas, sel, |px, py, cover| {
                    let (lx, ly) = (px - ox - sx, py - oy - sy);
                    if lx < 0 || ly < 0 || lx >= side as i32 || ly >= side as i32 {
                        return;
                    }
                    let src = snap[ly as usize * side + lx as usize];
                    let dst = store.get_pixel(px, py);
                    let k = (ca * cover).clamp(0.0, 1.0);
                    // Premultiplied pixels lerp component-wise.
                    let mix = Rgba::new(
                        dst.r + (src.r - dst.r) * k,
                        dst.g + (src.g - dst.g) * k,
                        dst.b + (src.b - dst.b) * k,
                        dst.a + (src.a - dst.a) * k,
                    );
                    store.set_pixel(px, py, mix);
                });
            }
        }
        prev = Some(d);
    }
}

/// Unsharp gain of a Sharpen dab at full strength and spacing 1 (see
/// [`paint_dabs`]'s blur and sharpen for how spacing scales it).
pub const SHARPEN_GAIN: f32 = 0.6;

/// Kernel radius: 3×3 for small brushes, 5×5 from a 24 px diameter up.
pub fn kernel_radius(brush: &Brush) -> i32 {
    if brush.radius >= 12.0 {
        2
    } else {
        1
    }
}

/// The pixels a blur or sharpen dab snapshots (clipped to the canvas), or
/// `None` when it does nothing.
fn filter_region(brush: &Brush, d: &Dab, canvas: Rect) -> Option<Rect> {
    let r = d.radius;
    if r <= 0.0 {
        return None;
    }
    let pad = r.ceil() as i32 + 1 + kernel_radius(brush);
    let region = Rect::new(
        d.x.floor() as i32 - pad,
        d.y.floor() as i32 - pad,
        (2 * pad + 1) as u32,
        (2 * pad + 1) as u32,
    )
    .intersect(&canvas);
    (!region.is_empty()).then_some(region)
}

/// Blur or sharpen along a stroke. Every dab filters a snapshot of the
/// pixels around it taken just before it lands, so a dab never reads its
/// own writes (as Smudge does). Blur averages premultiplied pixels (a box
/// of [`kernel_radius`]) and moves toward it by a = strength × coverage ×
/// spacing; Sharpen adds `SHARPEN_GAIN` × a of the difference from that
/// box average, on straight gamma-encoded colour, alpha kept. Dabs overlap
/// about 2 / spacing times along a stroke, so scaling by the spacing makes
/// a stroke at full strength lay about two full passes on each pixel
/// however densely it is dabbed. Reads beyond the canvas repeat its edge.
fn filter_dabs(store: &mut TileStore, brush: &Brush, dabs: &[Dab], canvas: Rect, sel: Option<&Selection>) {
    let strength = brush.color[3].clamp(0.0, 1.0) * brush.spacing.clamp(0.02, 1.0);
    let kr = kernel_radius(brush);
    let sharpen = brush.mode == BrushMode::Sharpen;
    for &d in dabs {
        let Some(region) = filter_region(brush, &d, canvas) else {
            continue;
        };
        let (w, h) = (region.w as i32, region.h as i32);
        let snap = store.to_raster(region);
        // Sharpen compares gamma-encoded straight colour.
        let enc: Vec<[f32; 4]> = if sharpen {
            snap.pixels
                .iter()
                .map(|p| {
                    let [r, g, b, a] = p.to_straight();
                    [srgb_encode(r), srgb_encode(g), srgb_encode(b), a]
                })
                .collect()
        } else {
            Vec::new()
        };
        let at = |x: i32, y: i32| ((y.clamp(0, h - 1) * w) + x.clamp(0, w - 1)) as usize;
        let n = ((2 * kr + 1) * (2 * kr + 1)) as f32;
        dab_coverage(brush, d, canvas, sel, |px, py, cover| {
            let k = (strength * cover).clamp(0.0, 1.0);
            let (lx, ly) = (px - region.x, py - region.y);
            let me = at(lx, ly);
            if sharpen {
                let c = enc[me];
                if c[3] <= 0.0 {
                    return;
                }
                let mut avg = [0f32; 3];
                for dy in -kr..=kr {
                    for dx in -kr..=kr {
                        let q = enc[at(lx + dx, ly + dy)];
                        for i in 0..3 {
                            avg[i] += q[i];
                        }
                    }
                }
                let g = SHARPEN_GAIN * k;
                let out = |i: usize| srgb_decode((c[i] + (c[i] - avg[i] / n) * g).clamp(0.0, 1.0));
                store.set_pixel(px, py, Rgba::from_straight(out(0), out(1), out(2), c[3]));
            } else {
                let mut acc = [0f32; 4];
                for dy in -kr..=kr {
                    for dx in -kr..=kr {
                        let q = snap.pixels[at(lx + dx, ly + dy)];
                        acc[0] += q.r;
                        acc[1] += q.g;
                        acc[2] += q.b;
                        acc[3] += q.a;
                    }
                }
                let dst = snap.pixels[me];
                let lerp = |a: f32, b: f32| a + (b / n - a) * k;
                store.set_pixel(
                    px,
                    py,
                    Rgba::new(
                        lerp(dst.r, acc[0]),
                        lerp(dst.g, acc[1]),
                        lerp(dst.b, acc[2]),
                        lerp(dst.a, acc[3]),
                    ),
                );
            }
        });
    }
}

/// Which pixels painting a run of dabs can touch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StrokeReach {
    /// Every pixel [`paint_dabs`] may write (inside the canvas).
    pub write: Rect,
    /// Every pixel whose value can affect the result: the written ones and,
    /// for smudge, blur and sharpen, the pixels they sample around each
    /// dab (smudge's may lie outside the canvas).
    pub read: Rect,
}

/// What [`paint_dabs`] reads and writes for these dabs. Painting a store
/// that holds the input over `read` gives the same pixels over `write` as
/// painting the whole layer; everything outside `write` is unchanged.
pub fn dabs_reach(brush: &Brush, dabs: &[Dab], canvas: Rect) -> StrokeReach {
    let mut out = StrokeReach::default();
    if brush.mode == BrushMode::History {
        return out;
    }
    let mut prev: Option<&Dab> = None;
    for d in dabs {
        let area = dab_area(brush, d, canvas);
        match brush.mode {
            BrushMode::Smudge => {
                if let Some((_, _, src)) = prev.and_then(|p| smudge_source(brush, p, d)) {
                    out.write = out.write.union(&area);
                    out.read = out.read.union(&src);
                }
            }
            BrushMode::Blur | BrushMode::Sharpen => {
                if let Some(region) = filter_region(brush, d, canvas) {
                    out.write = out.write.union(&area);
                    out.read = out.read.union(&region);
                }
            }
            _ => out.write = out.write.union(&area),
        }
        prev = Some(d);
    }
    out.read = out.read.union(&out.write);
    out
}
