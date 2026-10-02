//! Rendering shape layers into their pixel cache.
//!
//! The outline is flattened with a flatness tolerance (straight segments
//! stay single chords), then:
//! - the fill is an anti-aliased scanline fill — 16 sub-scanlines per
//!   pixel row with exact horizontal coverage, even-odd across subpaths
//!   (so a subpath inside another cuts a hole, as with the pen's paths);
//! - the stroke is a distance field around the chords: a pixel's coverage
//!   ramps over one pixel at the stroke's edge. Inside and outside strokes
//!   are the band clipped by the fill's coverage, so their outline edge is
//!   exactly the fill's edge. Joins and dash ends are round.
//!
//! Work runs per 256-row band in parallel; nothing is rendered outside the
//! canvas (the cache is re-rendered whenever the shape or canvas changes).

use std::sync::Arc;

use lumenply_doc::shape::{ShapeLayer, StrokeAlign};
use lumenply_doc::{Document, Layer, LayerContent, VectorPath};
use lumenply_tiles::{Rect, Rgba, Tile, TileCoord, TileStore, TILE_SIZE};
use rayon::prelude::*;

/// Sub-scanlines per pixel row in the fill.
const SUB: usize = 16;
/// Flattening tolerance in pixels.
const TOL: f32 = 0.05;

/// Flatten every subpath into a polyline, within `tol` pixels of the
/// curve. Returned with each subpath's `closed` flag; closed polylines do
/// not repeat their first point.
pub fn flatten(path: &VectorPath, tol: f32) -> Vec<(Vec<(f32, f32)>, bool)> {
    let tol = tol.max(1e-3);
    let mut out = Vec::new();
    for sp in &path.subpaths {
        if sp.nodes.len() < 2 {
            continue;
        }
        let mut pts = vec![sp.nodes[0].point];
        let segs = sp.nodes.len() - usize::from(!sp.closed);
        for i in 0..segs {
            let a = &sp.nodes[i];
            let b = &sp.nodes[(i + 1) % sp.nodes.len()];
            flatten_cubic(a.point, a.handle_out, b.handle_in, b.point, tol, &mut pts);
        }
        if sp.closed && pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        out.push((pts, sp.closed));
    }
    out
}

fn dist_to_line(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len
}

/// Append a cubic segment (its start point is already in `out`).
fn flatten_cubic(
    p0: (f32, f32),
    c0: (f32, f32),
    c1: (f32, f32),
    p1: (f32, f32),
    tol: f32,
    out: &mut Vec<(f32, f32)>,
) {
    // Handles on the chord (corner nodes): a straight segment.
    if dist_to_line(c0, p0, p1) < 1e-3 && dist_to_line(c1, p0, p1) < 1e-3 {
        out.push(p1);
        return;
    }
    // The second differences bound the deviation from the chords.
    let dd = ((p0.0 - 2.0 * c0.0 + c1.0).hypot(p0.1 - 2.0 * c0.1 + c1.1))
        .max((c0.0 - 2.0 * c1.0 + p1.0).hypot(c0.1 - 2.0 * c1.1 + p1.1));
    let n = ((0.75 * dd / tol).sqrt().ceil() as usize).clamp(1, 1024);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        out.push((
            a * p0.0 + b * c0.0 + c * c1.0 + d * p1.0,
            a * p0.1 + b * c0.1 + c * c1.1 + d * p1.1,
        ));
    }
}

/// A non-horizontal polygon edge, top to bottom.
#[derive(Clone, Copy)]
struct Edge {
    y0: f32,
    y1: f32,
    x0: f32,
    /// dx/dy.
    slope: f32,
}

fn fill_edges(polys: &[(Vec<(f32, f32)>, bool)]) -> Vec<Edge> {
    let mut out = Vec::new();
    for (pts, _) in polys {
        if pts.len() < 3 {
            continue;
        }
        // Open subpaths fill as if closed, as in Photoshop.
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            if a.1 == b.1 || !(a.0.is_finite() && a.1.is_finite() && b.0.is_finite() && b.1.is_finite()) {
                continue;
            }
            let (t, u) = if a.1 < b.1 { (a, b) } else { (b, a) };
            out.push(Edge {
                y0: t.1,
                y1: u.1,
                x0: t.0,
                slope: (u.0 - t.0) / (u.1 - t.1),
            });
        }
    }
    out
}

/// Even-odd coverage of `rows` pixel rows starting at `y0`, over the
/// columns `x0 .. x0 + w`, row-major.
fn fill_coverage(edges: &[Edge], x0: i32, w: usize, y0: i32, rows: usize) -> Vec<f32> {
    let mut cov = vec![0f32; w * rows];
    let (top, bottom) = (y0 as f32, (y0 + rows as i32) as f32);
    let band: Vec<Edge> = edges
        .iter()
        .filter(|e| e.y1 > top && e.y0 < bottom)
        .copied()
        .collect();
    if band.is_empty() || w == 0 {
        return cov;
    }
    let k = 1.0 / SUB as f32;
    let wf = w as f32;
    let mut diff = vec![0f32; w + 1];
    let mut xs: Vec<f32> = Vec::new();
    for r in 0..rows {
        let row_y = (y0 + r as i32) as f32;
        diff.iter_mut().for_each(|d| *d = 0.0);
        let row = &mut cov[r * w..(r + 1) * w];
        let mut any = false;
        for s in 0..SUB {
            let sy = row_y + (s as f32 + 0.5) * k;
            xs.clear();
            xs.extend(
                band.iter()
                    .filter(|e| e.y0 <= sy && e.y1 > sy)
                    .map(|e| e.x0 + (sy - e.y0) * e.slope - x0 as f32),
            );
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.chunks_exact(2) {
                let (xa, xb) = (pair[0].max(0.0), pair[1].min(wf));
                if xb <= xa {
                    continue;
                }
                any = true;
                let (ia, ib) = (xa as usize, xb as usize);
                if ia == ib {
                    row[ia] += (xb - xa) * k;
                } else {
                    row[ia] += ((ia + 1) as f32 - xa) * k;
                    diff[ia + 1] += k;
                    diff[ib] -= k;
                    if ib < w {
                        row[ib] += (xb - ib as f32) * k;
                    }
                }
            }
        }
        if any {
            let mut acc = 0.0;
            for (c, d) in row.iter_mut().zip(&diff) {
                acc += d;
                *c = (*c + acc).clamp(0.0, 1.0);
            }
        }
    }
    cov
}

/// A stroke chord with its bounding box.
#[derive(Clone, Copy)]
struct Seg {
    a: (f32, f32),
    b: (f32, f32),
    min: (f32, f32),
    max: (f32, f32),
}

impl Seg {
    fn new(a: (f32, f32), b: (f32, f32)) -> Seg {
        Seg {
            a,
            b,
            min: (a.0.min(b.0), a.1.min(b.1)),
            max: (a.0.max(b.0), a.1.max(b.1)),
        }
    }

    #[inline]
    fn dist2(&self, p: (f32, f32)) -> f32 {
        let (dx, dy) = (self.b.0 - self.a.0, self.b.1 - self.a.1);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 {
            (((p.0 - self.a.0) * dx + (p.1 - self.a.1) * dy) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (qx, qy) = (self.a.0 + t * dx - p.0, self.a.1 + t * dy - p.1);
        qx * qx + qy * qy
    }
}

/// The stroke's chords; with a dash pattern, only the "on" stretches,
/// each shortened by half the width at both ends so the round caps land
/// where a butt-capped dash would end.
fn stroke_segs(polys: &[(Vec<(f32, f32)>, bool)], width: f32, dash: Option<[f32; 2]>) -> Vec<Seg> {
    let mut out = Vec::new();
    let dash = dash.and_then(|[on, off]| {
        let (on, off) = (on.max(0.0) * width, off.max(0.0) * width);
        (on.is_finite() && off.is_finite() && on + off >= 1.0).then_some((on, off))
    });
    for (pts, closed) in polys {
        if pts.len() < 2 {
            continue;
        }
        let n = if *closed { pts.len() } else { pts.len() - 1 };
        let chords = (0..n).map(|i| (pts[i], pts[(i + 1) % pts.len()]));
        let Some((on, off)) = dash else {
            out.extend(chords.map(|(a, b)| Seg::new(a, b)));
            continue;
        };
        // Walk the polyline by arc length through the pattern.
        let period = on + off;
        let cap = width / 2.0;
        let mut pos = 0.0f32; // arc length at the chord's start
        for (a, b) in chords {
            let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            if len <= 0.0 {
                continue;
            }
            let at = |s: f32| {
                let t = ((s - pos) / len).clamp(0.0, 1.0);
                (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
            };
            let mut k = (pos / period).floor();
            loop {
                let start = k * period + cap.min(on / 2.0);
                let end = k * period + on - cap.min(on / 2.0);
                if start > pos + len {
                    break;
                }
                let (s0, s1) = (start.max(pos), end.min(pos + len));
                if s1 >= s0 {
                    out.push(Seg::new(at(s0), at(s1)));
                }
                k += 1.0;
            }
            pos += len;
        }
    }
    out
}

/// Render a shape over `canvas` (nothing outside it), compacted to 16-bit
/// unless `float`.
pub fn render_shape(shape: &ShapeLayer, canvas: Rect, float: bool) -> TileStore {
    let mut store = TileStore::new();
    let stroke = shape.stroke.filter(|s| s.sane_width() > 0.0);
    if shape.fill.is_none() && stroke.is_none() {
        return store;
    }
    let polys = flatten(&shape.outline(), TOL);
    let mut bb: Option<[f32; 4]> = None;
    for (x, y) in polys.iter().flat_map(|(p, _)| p.iter().copied()) {
        if !(x.is_finite() && y.is_finite()) {
            continue;
        }
        bb = Some(match bb {
            None => [x, y, x, y],
            Some([a, b, c, d]) => [a.min(x), b.min(y), c.max(x), d.max(y)],
        });
    }
    let Some([bx0, by0, bx1, by1]) = bb else {
        return store;
    };
    let any_open = polys.iter().any(|(_, closed)| !closed);
    let align = match stroke {
        Some(_) if any_open => StrokeAlign::Center,
        Some(s) => s.align,
        None => StrokeAlign::Inside,
    };
    let width = stroke.map_or(0.0, |s| s.sane_width());
    // Distance at which the stroke's coverage reaches zero.
    let reach = match align {
        StrokeAlign::Center => width / 2.0 + 0.5,
        _ => width + 0.5,
    };
    let grow = if stroke.is_some() { reach + 1.0 } else { 1.0 };
    let (x0, y0) = ((bx0 - grow).floor() as i32, (by0 - grow).floor() as i32);
    let (x1, y1) = ((bx1 + grow).ceil() as i32, (by1 + grow).ceil() as i32);
    if x1 <= x0 || y1 <= y0 {
        return store;
    }
    let area = Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&canvas);
    if area.is_empty() {
        return store;
    }

    let need_cov = shape.fill.is_some() || (stroke.is_some() && align != StrokeAlign::Center);
    let edges = if need_cov { fill_edges(&polys) } else { Vec::new() };
    let sampler = shape.fill.as_ref().map(|f| f.sampler(shape.fill_box()));
    let segs = match stroke {
        Some(s) => stroke_segs(&polys, width, s.dash),
        None => Vec::new(),
    };
    let ink = stroke.map(|s| {
        let c = s
            .color
            .map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
        Rgba::new(c[0], c[1], c[2], 1.0)
    });
    let ts = TILE_SIZE as i32;
    let rows: Vec<i32> = (area.y.div_euclid(ts)..=(area.bottom() - 1).div_euclid(ts)).collect();
    let cols: Vec<i32> = (area.x.div_euclid(ts)..=(area.right() - 1).div_euclid(ts)).collect();
    let tiles: Vec<(TileCoord, Tile)> = rows
        .par_iter()
        .flat_map_iter(|&ty| {
            let band_y0 = (ty * ts).max(area.y);
            let band_y1 = (ty * ts + ts).min(area.bottom());
            let band_h = (band_y1 - band_y0) as usize;
            let aw = area.w as usize;
            let cov = if need_cov {
                fill_coverage(&edges, area.x, aw, band_y0, band_h)
            } else {
                Vec::new()
            };
            let mut out = Vec::new();
            for &tx in &cols {
                let tx0 = (tx * ts).max(area.x);
                let tx1 = (tx * ts + ts).min(area.right());
                // Stroke distances for this tile's part of the area.
                let (tw, th) = ((tx1 - tx0) as usize, band_h);
                let mut d2 = Vec::new();
                if ink.is_some() {
                    let r = reach + 0.5;
                    let near: Vec<&Seg> = segs
                        .iter()
                        .filter(|s| {
                            s.max.0 + r >= tx0 as f32
                                && s.min.0 - r <= tx1 as f32
                                && s.max.1 + r >= band_y0 as f32
                                && s.min.1 - r <= band_y1 as f32
                        })
                        .collect();
                    if !near.is_empty() {
                        d2 = vec![f32::INFINITY; tw * th];
                        for s in near {
                            let px0 = ((s.min.0 - r).floor() as i32).max(tx0);
                            let px1 = ((s.max.0 + r).ceil() as i32).min(tx1);
                            let py0 = ((s.min.1 - r).floor() as i32).max(band_y0);
                            let py1 = ((s.max.1 + r).ceil() as i32).min(band_y1);
                            for py in py0..py1 {
                                let row = (py - band_y0) as usize * tw;
                                for px in px0..px1 {
                                    let i = row + (px - tx0) as usize;
                                    let d = s.dist2((px as f32 + 0.5, py as f32 + 0.5));
                                    if d < d2[i] {
                                        d2[i] = d;
                                    }
                                }
                            }
                        }
                    }
                }
                let mut tile = Tile::new();
                let mut blank = true;
                {
                    let px = tile.pixels_mut();
                    let (ox, oy) = (tx * ts, ty * ts);
                    for py in band_y0..band_y1 {
                        for x in tx0..tx1 {
                            let icov = if need_cov {
                                cov[(py - band_y0) as usize * aw + (x - area.x) as usize]
                            } else {
                                0.0
                            };
                            let f = match &sampler {
                                Some(smp) if icov > 0.0 => smp.sample(x, py).scale(icov),
                                _ => Rgba::TRANSPARENT,
                            };
                            let s = match ink {
                                Some(c) if !d2.is_empty() => {
                                    let d = d2[(py - band_y0) as usize * tw + (x - tx0) as usize].sqrt();
                                    let scov = match align {
                                        StrokeAlign::Center => (width / 2.0 + 0.5 - d).clamp(0.0, 1.0),
                                        StrokeAlign::Inside => (width + 0.5 - d).clamp(0.0, 1.0).min(icov),
                                        StrokeAlign::Outside => {
                                            (width + 0.5 - d).clamp(0.0, 1.0).min(1.0 - icov)
                                        }
                                    };
                                    c.scale(scov)
                                }
                                _ => Rgba::TRANSPARENT,
                            };
                            let p = if align == StrokeAlign::Outside {
                                // Stroke and fill are complementary at the edge:
                                // add them, or the seam would show.
                                Rgba::new(s.r + f.r, s.g + f.g, s.b + f.b, (s.a + f.a).min(1.0))
                            } else {
                                s.over(f)
                            };
                            if p.a > 0.0 {
                                blank = false;
                                px[(py - oy) as usize * TILE_SIZE + (x - ox) as usize] = p;
                            }
                        }
                    }
                }
                if !blank {
                    if !float {
                        tile.compact();
                    }
                    out.push((TileCoord::new(tx, ty), tile));
                }
            }
            out
        })
        .collect();
    for (c, t) in tiles {
        store.insert(c, Arc::new(t));
    }
    store
}

/// Re-render a shape layer's cache for a `width × height` canvas.
pub fn refresh_cache(s: &mut ShapeLayer, width: u32, height: u32, float: bool) {
    s.cache = Some(render_shape(s, Rect::new(0, 0, width, height), float));
    s.cache_canvas = (width, height);
}

/// Bring every shape layer's cache up to date with the document's canvas.
pub fn refresh_stale(doc: &mut Document) {
    let (w, h, float) = (doc.width, doc.height, doc.float_mode);
    doc.for_each_layer_mut(|l: &mut Layer| {
        if let LayerContent::Shape(s) = &mut l.content {
            if s.is_stale(w, h) {
                refresh_cache(s, w, h, float);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::adjust::srgb_encode;
    use lumenply_doc::shape::{ShapeGeometry, ShapeStroke};
    use lumenply_doc::{Fill, Gradient, GradientStyle};
    use lumenply_tiles::Affine;

    fn red() -> Option<Fill> {
        Some(Fill::Solid {
            color: [1.0, 0.0, 0.0],
        })
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> ShapeGeometry {
        ShapeGeometry::Rectangle {
            rect: [x, y, w, h],
            radius: 0.0,
        }
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn straight_edges_flatten_to_single_chords() {
        let p = rect(0.0, 0.0, 100.0, 50.0).path();
        let f = flatten(&p, TOL);
        assert_eq!(
            f,
            vec![(vec![(0.0, 0.0), (100.0, 0.0), (100.0, 50.0), (0.0, 50.0)], true)]
        );
        // A circle of radius 100 stays within the tolerance of the arc.
        let c = ShapeGeometry::Ellipse {
            rect: [0.0, 0.0, 200.0, 200.0],
        }
        .path();
        let pts = &flatten(&c, TOL)[0].0;
        assert!(pts.len() > 40 && pts.len() < 400, "{}", pts.len());
        for &(x, y) in pts {
            let r = ((x - 100.0).powi(2) + (y - 100.0).powi(2)).sqrt();
            assert!((r - 100.0).abs() < 0.03 + TOL, "cubic arc error + chord: r = {r}");
        }
    }

    #[test]
    fn rectangles_fill_with_exact_fractional_edges() {
        let s = ShapeLayer::new(rect(10.25, 20.0, 30.5, 10.0), red(), None);
        let out = render_shape(&s, Rect::new(0, 0, 100, 100), true);
        // Fully inside, fully outside, and a quarter / three-quarter pixel.
        assert_eq!(out.get_pixel(20, 25).a, 1.0);
        assert_eq!(out.get_pixel(9, 25).a, 0.0);
        assert!(
            close(out.get_pixel(10, 25).a, 0.75, 1e-5),
            "{:?}",
            out.get_pixel(10, 25)
        );
        assert!(
            close(out.get_pixel(40, 25).a, 0.75, 1e-5),
            "{:?}",
            out.get_pixel(40, 25)
        );
        assert_eq!(out.get_pixel(20, 19).a, 0.0);
        assert_eq!(out.get_pixel(20, 29).a, 1.0);
        assert_eq!(out.get_pixel(20, 30).a, 0.0);
        // Premultiplied red.
        let p = out.get_pixel(10, 25);
        assert!(close(p.r, 0.75, 1e-5) && p.g == 0.0);
    }

    #[test]
    fn ellipse_coverage_matches_its_area() {
        let s = ShapeLayer::new(
            ShapeGeometry::Ellipse {
                rect: [100.0, 50.0, 300.0, 200.0],
            },
            red(),
            None,
        );
        let out = render_shape(&s, Rect::new(0, 0, 600, 600), true);
        let mut area = 0.0f64;
        for c in out.coords().collect::<Vec<_>>() {
            for p in out.tile(c).unwrap().pixels().iter() {
                area += p.a as f64;
            }
        }
        let exact = std::f64::consts::PI * 150.0 * 100.0;
        assert!((area - exact).abs() / exact < 2e-4, "{area} vs {exact}");
        // Spans two tile columns, one tile row.
        assert_eq!(out.len(), 2);
        assert_eq!(out.get_pixel(250, 150).a, 1.0);
        assert_eq!(out.get_pixel(101, 52).a, 0.0, "outside the curve in the corner");
    }

    #[test]
    fn strokes_align_inside_centre_and_outside() {
        let canvas = Rect::new(0, 0, 64, 64);
        let mk = |align| {
            let s = ShapeLayer::new(
                rect(10.0, 10.0, 40.0, 40.0),
                None,
                Some(ShapeStroke {
                    color: [0.0, 0.0, 1.0],
                    width: 4.0,
                    align,
                    dash: None,
                }),
            );
            render_shape(&s, canvas, true)
        };
        // Pixel x covers [x, x+1); the left edge sits at x = 10.
        let alpha = |st: &TileStore, x: i32| st.get_pixel(x, 30).a;
        let inside = mk(StrokeAlign::Inside);
        assert_eq!(
            (5..17).map(|x| alpha(&inside, x)).collect::<Vec<_>>(),
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0]
        );
        let centre = mk(StrokeAlign::Center);
        assert_eq!(
            (5..17).map(|x| alpha(&centre, x)).collect::<Vec<_>>(),
            vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        let outside = mk(StrokeAlign::Outside);
        assert_eq!(
            (5..17).map(|x| alpha(&outside, x)).collect::<Vec<_>>(),
            vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        // The inside stroke keeps the outer corner square.
        assert_eq!(inside.get_pixel(10, 10).a, 1.0);
        assert_eq!(inside.get_pixel(30, 30).a, 0.0, "hollow middle");
        assert_eq!(inside.get_pixel(13, 30).b, 1.0, "blue ink");
    }

    #[test]
    fn outside_strokes_meet_the_fill_without_a_seam() {
        // An edge through pixel centres: fill and stroke each cover half.
        let s = ShapeLayer::new(
            rect(10.5, 10.0, 20.0, 20.0),
            red(),
            Some(ShapeStroke {
                color: [0.0, 0.0, 1.0],
                width: 3.0,
                align: StrokeAlign::Outside,
                dash: None,
            }),
        );
        let out = render_shape(&s, Rect::new(0, 0, 64, 64), true);
        let p = out.get_pixel(10, 20);
        assert!(close(p.a, 1.0, 1e-5), "{p:?}");
        assert!(close(p.r, 0.5, 1e-5) && close(p.b, 0.5, 1e-5), "{p:?}");
    }

    #[test]
    fn dashes_leave_gaps() {
        // An open path (from the pen) strokes centred whatever the
        // alignment says.
        let s = ShapeLayer::new(
            ShapeGeometry::Path {
                path: VectorPath {
                    subpaths: vec![lumenply_doc::SubPath {
                        nodes: vec![
                            lumenply_doc::PathNode::corner(2.0, 10.5),
                            lumenply_doc::PathNode::corner(98.0, 10.5),
                        ],
                        closed: false,
                    }],
                },
            },
            None,
            Some(ShapeStroke {
                color: [0.0, 0.0, 0.0],
                width: 2.0,
                align: StrokeAlign::Inside,
                dash: Some([4.0, 4.0]),
            }),
        );
        let out = render_shape(&s, Rect::new(0, 0, 128, 32), true);
        // Period 16 px: "on" for 8 (round caps inside), "off" for 8.
        let row: Vec<f32> = (0..40).map(|x| out.get_pixel(x, 10).a).collect();
        assert_eq!(row[4], 1.0, "{row:?}");
        assert_eq!(row[14], 0.0, "{row:?}");
        assert_eq!(row[20], 1.0, "{row:?}");
        assert_eq!(row[30], 0.0, "{row:?}");
        // Centred on y = 10.5, 2 px wide: rows 9.5..11.5.
        assert_eq!(out.get_pixel(4, 9).a, 0.5);
        assert_eq!(out.get_pixel(4, 12).a, 0.0);
    }

    #[test]
    fn transforms_re_render_crisply_and_gradients_span_the_shape() {
        // Scaling the vector 4× keeps the edge a one-pixel ramp.
        let mut s = ShapeLayer::new(rect(2.0, 2.0, 10.25, 10.0), red(), None);
        s.transform_by(&Affine::scale(4.0, 4.0));
        let out = render_shape(&s, Rect::new(0, 0, 64, 64), true);
        assert_eq!(out.get_pixel(48, 20).a, 1.0);
        assert_eq!(out.get_pixel(49, 20).a, 0.0);
        // Rotated 90° about the origin: x → −y, y → x, then shifted back.
        let mut r = ShapeLayer::new(rect(0.0, 0.0, 20.0, 10.0), red(), None);
        r.transform_by(&Affine::rotate(std::f32::consts::FRAC_PI_2).then(&Affine::translate(30.0, 0.0)));
        let out = render_shape(&r, Rect::new(0, 0, 64, 64), true);
        assert_eq!(out.get_pixel(25, 15).a, 1.0, "now 10 wide, 20 tall");
        assert_eq!(out.get_pixel(25, 21).a, 0.0);
        assert_eq!(out.get_pixel(19, 5).a, 0.0);
        // A left-to-right gradient spans the shape's own box.
        let g = ShapeLayer::new(
            rect(100.0, 0.0, 100.0, 10.0),
            Some(Fill::Gradient {
                gradient: Gradient::default(),
                style: GradientStyle::Linear,
                angle: 0.0,
                scale: 1.0,
                reverse: false,
                offset: [0.0, 0.0],
            }),
            None,
        );
        let out = render_shape(&g, Rect::new(0, 0, 256, 16), true);
        let enc = |x: i32| srgb_encode(out.get_pixel(x, 5).r);
        assert!(enc(100) < 0.01, "{}", enc(100));
        assert!(close(enc(149), 0.495, 3e-3), "{}", enc(149));
        assert!(enc(199) > 0.99, "{}", enc(199));
    }

    #[test]
    fn caches_clip_to_the_canvas_and_follow_its_size() {
        let mut d = Document::new(100, 100);
        let id = d.alloc_id();
        d.add_layer(Layer::shape(
            id,
            ShapeLayer::new(rect(50.0, 50.0, 500.0, 500.0), red(), None),
        ));
        refresh_stale(&mut d);
        let c = d.layer(id).unwrap().raster_store().unwrap();
        assert_eq!(c.content_bounds(), Some(Rect::new(50, 50, 50, 50)));
        assert!(c.tile(TileCoord::new(0, 0)).unwrap().is_compact());
        d.width = 300;
        refresh_stale(&mut d);
        let c = d.layer(id).unwrap().raster_store().unwrap();
        assert_eq!(c.content_bounds(), Some(Rect::new(50, 50, 250, 50)));
    }
}
