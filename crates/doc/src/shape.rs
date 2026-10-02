//! Shape layers: an editable vector outline with a fill and a stroke,
//! composited like a pixel layer (mask, blend, opacity, effects, clipping)
//! from a derived raster cache.
//!
//! The outline is a parametric [`ShapeGeometry`] (rectangle with a corner
//! radius, ellipse, polygon, line, a built-in custom shape, or a free
//! bezier path) followed by the layer's `transform`. Free transform, image
//! rotation and crops compose into `transform`, so the shape is redrawn
//! crisply from the vector outline every time; the pixels are never
//! resampled. [`ShapeLayer::cache`] is rendered by `lumenply-render`, never
//! saved, and rebuilt on load and after every edit.

use lumenply_tiles::{Affine, Rect, TileStore};
use serde::{Deserialize, Serialize};

use crate::fill::Fill;
use crate::{PathNode, SubPath, VectorPath};

/// Cubic-bezier handle length for a quarter circle of radius 1.
pub const KAPPA: f32 = 0.552_284_8;

/// The shape tool's kinds (Photoshop's grouped shape tools).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShapeKind {
    #[default]
    Rectangle,
    RoundedRectangle,
    Ellipse,
    Polygon,
    Line,
    Custom,
}

impl ShapeKind {
    pub const ALL: [ShapeKind; 6] = [
        ShapeKind::Rectangle,
        ShapeKind::RoundedRectangle,
        ShapeKind::Ellipse,
        ShapeKind::Polygon,
        ShapeKind::Line,
        ShapeKind::Custom,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::RoundedRectangle => "Rounded Rectangle",
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Polygon => "Polygon",
            ShapeKind::Line => "Line",
            ShapeKind::Custom => "Custom Shape",
        }
    }
}

/// The built-in custom shapes, drawn into the dragged box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CustomShape {
    #[default]
    Star,
    Arrow,
    Heart,
    Bubble,
}

impl CustomShape {
    pub const ALL: [CustomShape; 4] = [
        CustomShape::Star,
        CustomShape::Arrow,
        CustomShape::Heart,
        CustomShape::Bubble,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CustomShape::Star => "Star",
            CustomShape::Arrow => "Arrow",
            CustomShape::Heart => "Heart",
            CustomShape::Bubble => "Speech Bubble",
        }
    }

    /// The outline in the unit square `[0, 1]²` (y down).
    fn unit_path(self) -> SubPath {
        match self {
            CustomShape::Star => {
                // Five points; the inner radius gives the classic
                // pentagram proportions.
                let inner = 0.5 * 0.381_966;
                corners(
                    (0..10)
                        .map(|i| {
                            let r = if i % 2 == 0 { 0.5 } else { inner };
                            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                            (0.5 + r * a.cos(), 0.5 + r * a.sin())
                        })
                        .collect::<Vec<_>>(),
                )
            }
            CustomShape::Arrow => corners(vec![
                (0.0, 0.3),
                (0.55, 0.3),
                (0.55, 0.0),
                (1.0, 0.5),
                (0.55, 1.0),
                (0.55, 0.7),
                (0.0, 0.7),
            ]),
            CustomShape::Heart => {
                let n = |p: (f32, f32), i: (f32, f32), o: (f32, f32)| PathNode {
                    point: p,
                    handle_in: i,
                    handle_out: o,
                };
                SubPath {
                    nodes: vec![
                        n((0.5, 0.28), (0.5, 0.12), (0.5, 0.12)),
                        n((0.25, 0.0), (0.40, 0.0), (0.10, 0.0)),
                        n((0.0, 0.32), (0.0, 0.14), (0.0, 0.55)),
                        n((0.5, 1.0), (0.30, 0.75), (0.70, 0.75)),
                        n((1.0, 0.32), (1.0, 0.55), (1.0, 0.14)),
                        n((0.75, 0.0), (0.90, 0.0), (0.60, 0.0)),
                    ],
                    closed: true,
                }
            }
            CustomShape::Bubble => {
                // A rounded body over the top 78% with a tail at the
                // bottom left.
                let (b, r) = (0.78, 0.18);
                let k = r * KAPPA;
                let n = |p: (f32, f32), i: (f32, f32), o: (f32, f32)| PathNode {
                    point: p,
                    handle_in: i,
                    handle_out: o,
                };
                let c = |x: f32, y: f32| PathNode::corner(x, y);
                SubPath {
                    nodes: vec![
                        n((r, 0.0), (r - k, 0.0), (r, 0.0)),
                        n((1.0 - r, 0.0), (1.0 - r, 0.0), (1.0 - r + k, 0.0)),
                        n((1.0, r), (1.0, r - k), (1.0, r)),
                        n((1.0, b - r), (1.0, b - r), (1.0, b - r + k)),
                        n((1.0 - r, b), (1.0 - r + k, b), (1.0 - r, b)),
                        c(0.42, b),
                        c(0.16, 1.0),
                        c(0.24, b),
                        n((r, b), (r, b), (r - k, b)),
                        n((0.0, b - r), (0.0, b - r + k), (0.0, b - r)),
                        n((0.0, r), (0.0, r), (0.0, r - k)),
                    ],
                    closed: true,
                }
            }
        }
    }
}

/// A closed subpath of straight segments through `pts`.
fn corners(pts: Vec<(f32, f32)>) -> SubPath {
    SubPath {
        nodes: pts.into_iter().map(|(x, y)| PathNode::corner(x, y)).collect(),
        closed: true,
    }
}

/// Map a subpath through `f` (points and handles alike).
fn map_subpath(sp: &SubPath, f: impl Fn((f32, f32)) -> (f32, f32)) -> SubPath {
    SubPath {
        nodes: sp
            .nodes
            .iter()
            .map(|n| PathNode {
                point: f(n.point),
                handle_in: f(n.handle_in),
                handle_out: f(n.handle_out),
            })
            .collect(),
        closed: sp.closed,
    }
}

impl VectorPath {
    /// The path mapped through an affine transform (exact for beziers).
    pub fn transformed(&self, t: &Affine) -> VectorPath {
        VectorPath {
            subpaths: self
                .subpaths
                .iter()
                .map(|sp| map_subpath(sp, |(x, y)| t.apply(x, y)))
                .collect(),
        }
    }
}

/// The parametric outline of a shape, before the layer's transform.
/// Boxes are `[x, y, width, height]` in canvas pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ShapeGeometry {
    /// A rectangle; `radius` > 0 rounds its corners (in pixels).
    Rectangle {
        rect: [f32; 4],
        radius: f32,
    },
    Ellipse {
        rect: [f32; 4],
    },
    /// A regular polygon inscribed in the box's ellipse, first corner at
    /// the top.
    Polygon {
        rect: [f32; 4],
        sides: u32,
    },
    /// A straight bar `weight` px thick, with optional arrowheads.
    Line {
        from: [f32; 2],
        to: [f32; 2],
        weight: f32,
        #[serde(default)]
        arrow_start: bool,
        #[serde(default)]
        arrow_end: bool,
    },
    Custom {
        rect: [f32; 4],
        shape: CustomShape,
    },
    /// A free bezier outline (e.g. converted from the pen's work path).
    Path {
        path: VectorPath,
    },
}

/// Polygon side count limits.
pub const MIN_SIDES: u32 = 3;
pub const MAX_SIDES: u32 = 100;

fn finite(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

impl ShapeGeometry {
    pub fn name(&self) -> &'static str {
        match self {
            ShapeGeometry::Rectangle { radius, .. } if *radius > 0.0 => "Rounded Rectangle",
            ShapeGeometry::Rectangle { .. } => "Rectangle",
            ShapeGeometry::Ellipse { .. } => "Ellipse",
            ShapeGeometry::Polygon { .. } => "Polygon",
            ShapeGeometry::Line { .. } => "Line",
            ShapeGeometry::Custom { shape, .. } => shape.name(),
            ShapeGeometry::Path { .. } => "Shape",
        }
    }

    /// Whether every number is finite (files are checked with this).
    pub fn is_finite(&self) -> bool {
        let all = |v: &[f32]| v.iter().all(|x| x.is_finite());
        match self {
            ShapeGeometry::Rectangle { rect, radius } => all(rect) && radius.is_finite(),
            ShapeGeometry::Ellipse { rect }
            | ShapeGeometry::Polygon { rect, .. }
            | ShapeGeometry::Custom { rect, .. } => all(rect),
            ShapeGeometry::Line { from, to, weight, .. } => all(from) && all(to) && weight.is_finite(),
            ShapeGeometry::Path { path } => path.subpaths.iter().all(|sp| {
                sp.nodes.iter().all(|n| {
                    all(&[
                        n.point.0,
                        n.point.1,
                        n.handle_in.0,
                        n.handle_in.1,
                        n.handle_out.0,
                        n.handle_out.1,
                    ])
                })
            }),
        }
    }

    /// The outline as a bezier path (untransformed).
    pub fn path(&self) -> VectorPath {
        let one = |sp: SubPath| VectorPath { subpaths: vec![sp] };
        match self {
            ShapeGeometry::Rectangle { rect, radius } => one(rounded_rect(*rect, finite(*radius))),
            ShapeGeometry::Ellipse { rect } => one(ellipse(*rect)),
            ShapeGeometry::Polygon { rect, sides } => {
                let n = (*sides).clamp(MIN_SIDES, MAX_SIDES);
                let [x, y, w, h] = rect.map(finite);
                let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                one(corners(
                    (0..n)
                        .map(|i| {
                            let a =
                                -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::TAU / n as f32;
                            (cx + w / 2.0 * a.cos(), cy + h / 2.0 * a.sin())
                        })
                        .collect(),
                ))
            }
            ShapeGeometry::Line {
                from,
                to,
                weight,
                arrow_start,
                arrow_end,
            } => one(line_outline(
                (finite(from[0]), finite(from[1])),
                (finite(to[0]), finite(to[1])),
                finite(*weight),
                *arrow_start,
                *arrow_end,
            )),
            ShapeGeometry::Custom { rect, shape } => {
                let [x, y, w, h] = rect.map(finite);
                one(map_subpath(&shape.unit_path(), |(u, v)| (x + u * w, y + v * h)))
            }
            ShapeGeometry::Path { path } => path.clone(),
        }
    }

    /// Build the geometry a shape-tool drag describes. `a` and `b` are the
    /// (already constrained, see [`drag_box`]) drag ends; boxes use them
    /// as opposite corners, a line runs from `a` to `b`.
    pub fn from_drag(params: &ShapeParams, a: (f32, f32), b: (f32, f32)) -> ShapeGeometry {
        let rect = [a.0.min(b.0), a.1.min(b.1), (b.0 - a.0).abs(), (b.1 - a.1).abs()];
        match params.kind {
            ShapeKind::Rectangle => ShapeGeometry::Rectangle { rect, radius: 0.0 },
            ShapeKind::RoundedRectangle => ShapeGeometry::Rectangle {
                rect,
                radius: params.radius.max(0.0),
            },
            ShapeKind::Ellipse => ShapeGeometry::Ellipse { rect },
            ShapeKind::Polygon => ShapeGeometry::Polygon {
                rect,
                sides: params.sides.clamp(MIN_SIDES, MAX_SIDES),
            },
            ShapeKind::Line => ShapeGeometry::Line {
                from: [a.0, a.1],
                to: [b.0, b.1],
                weight: params.weight.max(0.5),
                arrow_start: params.arrow_start,
                arrow_end: params.arrow_end,
            },
            ShapeKind::Custom => ShapeGeometry::Custom {
                rect,
                shape: params.custom,
            },
        }
    }
}

/// A rectangle with corners rounded by `radius` (clamped to half the
/// shorter side), as one closed subpath running clockwise on screen.
fn rounded_rect([x, y, w, h]: [f32; 4], radius: f32) -> SubPath {
    let (x, y, w, h) = (finite(x), finite(y), finite(w), finite(h));
    let r = radius.clamp(0.0, (w.abs().min(h.abs())) / 2.0);
    if r <= 0.0 {
        return corners(vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]);
    }
    let k = r * KAPPA;
    let n = |p: (f32, f32), i: (f32, f32), o: (f32, f32)| PathNode {
        point: p,
        handle_in: i,
        handle_out: o,
    };
    let (r0, r1, b0, b1) = (x + w - r, x + r, y + h - r, y + r);
    SubPath {
        nodes: vec![
            n((r1, y), (r1 - k, y), (r1, y)),
            n((r0, y), (r0, y), (r0 + k, y)),
            n((x + w, b1), (x + w, b1 - k), (x + w, b1)),
            n((x + w, b0), (x + w, b0), (x + w, b0 + k)),
            n((r0, y + h), (r0 + k, y + h), (r0, y + h)),
            n((r1, y + h), (r1, y + h), (r1 - k, y + h)),
            n((x, b0), (x, b0 + k), (x, b0)),
            n((x, b1), (x, b1), (x, b1 - k)),
        ],
        closed: true,
    }
}

/// The ellipse inscribed in a box: four cubic quarter arcs.
fn ellipse([x, y, w, h]: [f32; 4]) -> SubPath {
    let (rx, ry) = (finite(w) / 2.0, finite(h) / 2.0);
    let (cx, cy) = (finite(x) + rx, finite(y) + ry);
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    let n = |p: (f32, f32), i: (f32, f32), o: (f32, f32)| PathNode {
        point: p,
        handle_in: i,
        handle_out: o,
    };
    SubPath {
        nodes: vec![
            n((cx, cy - ry), (cx - kx, cy - ry), (cx + kx, cy - ry)),
            n((cx + rx, cy), (cx + rx, cy - ky), (cx + rx, cy + ky)),
            n((cx, cy + ry), (cx + kx, cy + ry), (cx - kx, cy + ry)),
            n((cx - rx, cy), (cx - rx, cy + ky), (cx - rx, cy - ky)),
        ],
        closed: true,
    }
}

/// Arrowhead width and length as multiples of the line weight.
pub const ARROW_WIDTH: f32 = 4.0;
pub const ARROW_LENGTH: f32 = 4.0;

/// A bar from `a` to `b`, `weight` thick, with optional arrowheads, as one
/// closed polygon (so it fills like any other shape).
fn line_outline(a: (f32, f32), b: (f32, f32), weight: f32, arrow_a: bool, arrow_b: bool) -> SubPath {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let half = weight.max(0.0) / 2.0;
    if len < 1e-4 {
        return corners(vec![
            (a.0 - half, a.1 - half),
            (a.0 + half, a.1 - half),
            (a.0 + half, a.1 + half),
            (a.0 - half, a.1 + half),
        ]);
    }
    let d = (dx / len, dy / len);
    let nrm = (-d.1, d.0);
    let at = |p: (f32, f32), along: f32, across: f32| {
        (
            p.0 + d.0 * along + nrm.0 * across,
            p.1 + d.1 * along + nrm.1 * across,
        )
    };
    let heads = usize::from(arrow_a) + usize::from(arrow_b);
    let head_len = if heads > 0 {
        (weight * ARROW_LENGTH).min(len / heads as f32)
    } else {
        0.0
    };
    let head_half = (weight * ARROW_WIDTH / 2.0).max(half);
    let mut pts = Vec::new();
    // Up the +normal side from a to b, round the tip, back down the other.
    if arrow_a {
        pts.push(a);
        pts.push(at(a, head_len, head_half));
        pts.push(at(a, head_len, half));
    } else {
        pts.push(at(a, 0.0, half));
    }
    if arrow_b {
        pts.push(at(b, -head_len, half));
        pts.push(at(b, -head_len, head_half));
        pts.push(b);
        pts.push(at(b, -head_len, -head_half));
        pts.push(at(b, -head_len, -half));
    } else {
        pts.push(at(b, 0.0, half));
        pts.push(at(b, 0.0, -half));
    }
    if arrow_a {
        pts.push(at(a, head_len, -half));
        pts.push(at(a, head_len, -head_half));
    } else {
        pts.push(at(a, 0.0, -half));
    }
    corners(pts)
}

/// The shape tool's settings for the next shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeParams {
    pub kind: ShapeKind,
    /// Rounded rectangle corner radius in pixels.
    pub radius: f32,
    /// Polygon side count.
    pub sides: u32,
    /// Line thickness in pixels.
    pub weight: f32,
    pub arrow_start: bool,
    pub arrow_end: bool,
    pub custom: CustomShape,
}

impl Default for ShapeParams {
    fn default() -> Self {
        ShapeParams {
            kind: ShapeKind::Rectangle,
            radius: 20.0,
            sides: 5,
            weight: 4.0,
            arrow_start: false,
            arrow_end: false,
            custom: CustomShape::Star,
        }
    }
}

/// The drag ends a shape-tool drag from `a` to `b` describes. `shift`
/// constrains boxes to squares (circles, regular polygons) and lines to
/// multiples of 45°; `alt` draws from the centre: `a` becomes the middle.
/// Boxes come back as (top-left, bottom-right); lines as (start, end).
pub fn drag_box(
    kind: ShapeKind,
    a: (f32, f32),
    b: (f32, f32),
    shift: bool,
    alt: bool,
) -> ((f32, f32), (f32, f32)) {
    let (mut dx, mut dy) = (b.0 - a.0, b.1 - a.1);
    if kind == ShapeKind::Line {
        if shift {
            let len = (dx * dx + dy * dy).sqrt();
            let step = std::f32::consts::FRAC_PI_4;
            let ang = (dy.atan2(dx) / step).round() * step;
            (dx, dy) = (len * ang.cos(), len * ang.sin());
            // Keep exact axes exact.
            if dx.abs() < 1e-4 {
                dx = 0.0;
            }
            if dy.abs() < 1e-4 {
                dy = 0.0;
            }
        }
        return if alt {
            ((a.0 - dx, a.1 - dy), (a.0 + dx, a.1 + dy))
        } else {
            (a, (a.0 + dx, a.1 + dy))
        };
    }
    if shift {
        let s = dx.abs().max(dy.abs());
        dx = s.copysign(dx);
        dy = s.copysign(dy);
    }
    let (p, q) = if alt {
        ((a.0 - dx, a.1 - dy), (a.0 + dx, a.1 + dy))
    } else {
        (a, (a.0 + dx, a.1 + dy))
    };
    ((p.0.min(q.0), p.1.min(q.1)), (p.0.max(q.0), p.1.max(q.1)))
}

/// Where a stroke sits relative to the outline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StrokeAlign {
    /// Entirely inside the outline (Photoshop's default for shapes).
    #[default]
    Inside,
    Center,
    Outside,
}

impl StrokeAlign {
    pub const ALL: [StrokeAlign; 3] = [StrokeAlign::Inside, StrokeAlign::Center, StrokeAlign::Outside];

    pub fn name(self) -> &'static str {
        match self {
            StrokeAlign::Inside => "Inside",
            StrokeAlign::Center => "Center",
            StrokeAlign::Outside => "Outside",
        }
    }
}

/// A shape's outline stroke.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeStroke {
    /// Straight linear RGB.
    pub color: [f32; 3],
    /// Width in canvas pixels (not scaled by transforms).
    pub width: f32,
    #[serde(default)]
    pub align: StrokeAlign,
    /// Dash and gap lengths as multiples of the width; `None` is solid.
    #[serde(default)]
    pub dash: Option<[f32; 2]>,
}

impl Default for ShapeStroke {
    fn default() -> Self {
        ShapeStroke {
            color: [0.0, 0.0, 0.0],
            width: 3.0,
            align: StrokeAlign::Inside,
            dash: None,
        }
    }
}

impl ShapeStroke {
    /// Width clamped to something renderable: finite, 0..=1000 px.
    pub fn sane_width(&self) -> f32 {
        crate::sane_radius(self.width)
    }

    /// How far (px) the stroke reaches outside the outline.
    pub fn reach_outside(&self) -> f32 {
        match self.align {
            StrokeAlign::Inside => 0.0,
            StrokeAlign::Center => self.sane_width() / 2.0,
            StrokeAlign::Outside => self.sane_width(),
        }
    }
}

mod affine_coeffs {
    use lumenply_tiles::Affine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(t: &Affine, s: S) -> Result<S::Ok, S::Error> {
        t.coeffs().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Affine, D::Error> {
        Ok(Affine::from_coeffs(<[f32; 6]>::deserialize(d)?))
    }
}

fn identity() -> Affine {
    Affine::IDENTITY
}

/// A shape layer: geometry, placement, fill and stroke, plus its rendered
/// pixels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShapeLayer {
    pub geometry: ShapeGeometry,
    /// Applied to the geometry's outline; transforms compose here.
    #[serde(with = "affine_coeffs", default = "identity")]
    pub transform: Affine,
    /// The inside's paint (solid or gradient); `None` leaves it empty.
    /// Gradients span the shape's bounds.
    pub fill: Option<Fill>,
    pub stroke: Option<ShapeStroke>,
    /// Derived: the shape rendered over the canvas recorded in
    /// `cache_canvas` (clipped to it). Never saved.
    #[serde(skip)]
    pub cache: Option<TileStore>,
    #[serde(skip)]
    pub cache_canvas: (u32, u32),
}

impl PartialEq for ShapeLayer {
    /// Compares the settings; the cache is derived state.
    fn eq(&self, o: &Self) -> bool {
        self.geometry == o.geometry
            && self.transform == o.transform
            && self.fill == o.fill
            && self.stroke == o.stroke
    }
}

impl ShapeLayer {
    pub fn new(geometry: ShapeGeometry, fill: Option<Fill>, stroke: Option<ShapeStroke>) -> Self {
        ShapeLayer {
            geometry,
            transform: Affine::IDENTITY,
            fill,
            stroke,
            cache: None,
            cache_canvas: (0, 0),
        }
    }

    /// The outline in canvas coordinates.
    pub fn outline(&self) -> VectorPath {
        let p = self.geometry.path();
        if self.transform == Affine::IDENTITY {
            p
        } else {
            p.transformed(&self.transform)
        }
    }

    /// The outline's bounding box `[x0, y0, x1, y1]` (from the flattened
    /// curve, so handles don't inflate it), or `None` when empty.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let mut b: Option<[f32; 4]> = None;
        for (pts, _) in self.outline().flatten() {
            for (x, y) in pts {
                b = Some(match b {
                    None => [x, y, x, y],
                    Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                });
            }
        }
        b
    }

    /// The whole-pixel box a gradient fill spans: the outline's bounds.
    pub fn fill_box(&self) -> Rect {
        match self.bounds() {
            Some([x0, y0, x1, y1]) => {
                // Flattening wobbles in the last float bits; don't let that
                // grow the box by a pixel.
                let (x, y) = ((x0 + 1e-3).floor() as i32, (y0 + 1e-3).floor() as i32);
                Rect::new(
                    x,
                    y,
                    (((x1 - 1e-3).ceil() as i32 - x).max(1)) as u32,
                    (((y1 - 1e-3).ceil() as i32 - y).max(1)) as u32,
                )
            }
            None => Rect::new(0, 0, 1, 1),
        }
    }

    /// Compose `t` after the current transform (the cache goes stale).
    pub fn transform_by(&mut self, t: &Affine) {
        self.transform = self.transform.then(t);
        self.cache = None;
    }

    /// Whether the cache is missing or was rendered for another canvas.
    pub fn is_stale(&self, width: u32, height: u32) -> bool {
        self.cache.is_none() || self.cache_canvas != (width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn rectangles_and_rounded_corners() {
        let sq = ShapeGeometry::Rectangle {
            rect: [10.0, 20.0, 30.0, 40.0],
            radius: 0.0,
        }
        .path();
        let pts: Vec<_> = sq.subpaths[0].nodes.iter().map(|n| n.point).collect();
        assert_eq!(pts, vec![(10.0, 20.0), (40.0, 20.0), (40.0, 60.0), (10.0, 60.0)]);
        assert!(sq.subpaths[0].closed);
        // A radius past half the short side clamps: 30 wide → 15.
        let r = ShapeGeometry::Rectangle {
            rect: [0.0, 0.0, 30.0, 40.0],
            radius: 100.0,
        }
        .path();
        let n = &r.subpaths[0].nodes;
        assert_eq!(n.len(), 8);
        assert_eq!(n[0].point, (15.0, 0.0));
        assert_eq!(n[1].point, (15.0, 0.0), "top edge collapses to a point");
        assert_eq!(n[2].point, (30.0, 15.0));
        assert!(close(n[1].handle_out.0, 15.0 + 15.0 * KAPPA));
    }

    #[test]
    fn ellipses_polygons_and_custom_shapes_fill_their_box() {
        let e = ShapeLayer::new(
            ShapeGeometry::Ellipse {
                rect: [0.0, 0.0, 100.0, 50.0],
            },
            None,
            None,
        );
        let [x0, y0, x1, y1] = e.bounds().unwrap();
        assert!(close(x0, 0.0) && close(y0, 0.0) && close(x1, 100.0) && close(y1, 50.0));
        let hex = ShapeGeometry::Polygon {
            rect: [0.0, 0.0, 100.0, 100.0],
            sides: 6,
        }
        .path();
        let p = &hex.subpaths[0].nodes;
        assert_eq!(p.len(), 6);
        assert!(
            close(p[0].point.0, 50.0) && close(p[0].point.1, 0.0),
            "first corner on top"
        );
        assert!(close(p[1].point.0, 50.0 + 50.0 * 30f32.to_radians().cos()));
        assert!(close(p[1].point.1, 50.0 - 50.0 * 30f32.to_radians().sin()));
        // Sides clamp to 3..=100.
        let tri = ShapeGeometry::Polygon {
            rect: [0.0, 0.0, 10.0, 10.0],
            sides: 1,
        };
        assert_eq!(tri.path().subpaths[0].nodes.len(), 3);
        for c in CustomShape::ALL {
            let s = ShapeLayer::new(
                ShapeGeometry::Custom {
                    rect: [100.0, 100.0, 200.0, 100.0],
                    shape: c,
                },
                None,
                None,
            );
            let [x0, y0, x1, y1] = s.bounds().unwrap();
            assert!(x0 >= 99.9 && y0 >= 99.9 && x1 <= 300.1 && y1 <= 200.1, "{c:?}");
            assert!(x1 - x0 > 150.0 && y1 - y0 > 80.0, "{c:?} spans its box");
        }
    }

    #[test]
    fn lines_are_bars_with_optional_arrowheads() {
        let bar = ShapeGeometry::Line {
            from: [0.0, 10.0],
            to: [100.0, 10.0],
            weight: 4.0,
            arrow_start: false,
            arrow_end: false,
        }
        .path();
        let pts: Vec<_> = bar.subpaths[0].nodes.iter().map(|n| n.point).collect();
        assert_eq!(pts, vec![(0.0, 12.0), (100.0, 12.0), (100.0, 8.0), (0.0, 8.0)]);
        let arrow = ShapeGeometry::Line {
            from: [0.0, 10.0],
            to: [100.0, 10.0],
            weight: 4.0,
            arrow_start: false,
            arrow_end: true,
        }
        .path();
        let pts: Vec<_> = arrow.subpaths[0].nodes.iter().map(|n| n.point).collect();
        // Head 16 long and 16 wide (4× the weight); the tip is the end.
        assert_eq!(
            pts,
            vec![
                (0.0, 12.0),
                (84.0, 12.0),
                (84.0, 18.0),
                (100.0, 10.0),
                (84.0, 2.0),
                (84.0, 8.0),
                (0.0, 8.0)
            ]
        );
    }

    #[test]
    fn drags_constrain_with_shift_and_centre_with_alt() {
        let k = ShapeKind::Rectangle;
        assert_eq!(
            drag_box(k, (10.0, 10.0), (4.0, 30.0), false, false),
            ((4.0, 10.0), (10.0, 30.0))
        );
        // Shift: the longer side wins, keeping the drag's direction.
        assert_eq!(
            drag_box(k, (10.0, 10.0), (4.0, 30.0), true, false),
            ((-10.0, 10.0), (10.0, 30.0))
        );
        // Alt: the press point is the centre.
        assert_eq!(
            drag_box(k, (50.0, 50.0), (60.0, 55.0), false, true),
            ((40.0, 45.0), (60.0, 55.0))
        );
        assert_eq!(
            drag_box(k, (50.0, 50.0), (60.0, 55.0), true, true),
            ((40.0, 40.0), (60.0, 60.0))
        );
        // Lines snap to 45° steps and keep their length.
        let (a, b) = drag_box(ShapeKind::Line, (0.0, 0.0), (10.0, 1.0), true, false);
        assert_eq!(a, (0.0, 0.0));
        assert!(close(b.0, 101f32.sqrt()) && b.1 == 0.0, "{b:?}");
        let (_, b) = drag_box(ShapeKind::Line, (0.0, 0.0), (10.0, 9.0), true, false);
        assert!(
            close(b.0, b.1) && close(b.0, 181f32.sqrt() / 2f32.sqrt()),
            "{b:?}"
        );
        let (a, b) = drag_box(ShapeKind::Line, (5.0, 5.0), (8.0, 9.0), false, true);
        assert_eq!((a, b), ((2.0, 1.0), (8.0, 9.0)));
    }

    #[test]
    fn transforms_map_the_outline_and_serialize_as_coefficients() {
        let mut s = ShapeLayer::new(
            ShapeGeometry::Rectangle {
                rect: [0.0, 0.0, 10.0, 10.0],
                radius: 0.0,
            },
            Some(Fill::Solid {
                color: [1.0, 0.0, 0.0],
            }),
            Some(ShapeStroke::default()),
        );
        s.transform_by(&Affine::scale(2.0, 3.0));
        s.transform_by(&Affine::translate(5.0, 0.0));
        let b = s.bounds().unwrap();
        assert!(
            close(b[0], 5.0) && close(b[1], 0.0) && close(b[2], 25.0) && close(b[3], 30.0),
            "{b:?}"
        );
        assert_eq!(s.fill_box(), Rect::new(5, 0, 20, 30));
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(r#""transform":[2.0,0.0,0.0,3.0,5.0,0.0]"#),
            "{json}"
        );
        assert!(json.contains(r#""geometry":{"type":"rectangle","rect":[0.0,0.0,10.0,10.0],"radius":0.0}"#));
        let back: ShapeLayer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        // Transform and stroke details may be absent.
        let old = r#"{"geometry":{"type":"ellipse","rect":[0,0,4,4]},"fill":null,"stroke":{"color":[0,0,0],"width":2}}"#;
        let s: ShapeLayer = serde_json::from_str(old).unwrap();
        assert_eq!(s.transform, Affine::IDENTITY);
        assert_eq!(s.stroke.unwrap().align, StrokeAlign::Inside);
    }
}
