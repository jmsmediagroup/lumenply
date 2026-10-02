//! Snapping maths for the canvas tools. Given candidate lines (guides,
//! grid, canvas edges and centre, other layers' bounds) and the edges of
//! whatever is being dragged, find the smallest nudge that puts one of
//! those edges onto a line, within a tolerance. Everything is in document
//! pixels: the UI converts its screen-pixel snap distance by the zoom.

use lumenply_doc::{Document, LayerId};

/// What a snap line came from. When two lines are equally close, the
/// earlier kind wins (a guide beats the canvas edge beats a layer edge
/// beats the grid).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SnapSource {
    Guide,
    Canvas,
    Layer,
    Grid,
}

/// One axis's result: move by `delta` to land on `line`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snap {
    pub delta: f32,
    pub line: f32,
    pub source: SnapSource,
}

/// The lines things can snap to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapLines {
    /// Vertical lines, by x position.
    pub xs: Vec<(f32, SnapSource)>,
    /// Horizontal lines, by y position.
    pub ys: Vec<(f32, SnapSource)>,
    /// Grid spacing in document pixels: a line at every multiple, both axes.
    pub grid: Option<f32>,
}

/// Which kinds of line [`doc_lines`] collects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapOptions {
    pub guides: bool,
    pub canvas: bool,
    pub layers: bool,
    /// Grid spacing in document pixels, when snapping to the grid.
    pub grid: Option<f32>,
}

impl SnapLines {
    /// The smallest nudge along x that puts any of `edges` on a vertical
    /// line, if one is within `tol`.
    pub fn snap_x(&self, edges: &[f32], tol: f32) -> Option<Snap> {
        best(&self.xs, self.grid, edges, tol)
    }

    /// [`SnapLines::snap_x`] for horizontal lines.
    pub fn snap_y(&self, edges: &[f32], tol: f32) -> Option<Snap> {
        best(&self.ys, self.grid, edges, tol)
    }

    /// Snap a free point (a marquee corner, a crop edge): each axis on its own.
    pub fn snap_point(&self, x: f32, y: f32, tol: f32) -> ((f32, f32), [Option<Snap>; 2]) {
        let sx = self.snap_x(&[x], tol);
        let sy = self.snap_y(&[y], tol);
        let x = sx.map_or(x, |s| x + s.delta);
        let y = sy.map_or(y, |s| y + s.delta);
        ((x, y), [sx, sy])
    }

    /// Snap a rectangle that moves as a whole: its left, centre and right
    /// edges compete for one x nudge, top, middle and bottom for one y.
    pub fn snap_rect(&self, x0: f32, y0: f32, x1: f32, y1: f32, tol: f32) -> [Option<Snap>; 2] {
        [
            self.snap_x(&[x0, (x0 + x1) / 2.0, x1], tol),
            self.snap_y(&[y0, (y0 + y1) / 2.0, y1], tol),
        ]
    }
}

fn best(lines: &[(f32, SnapSource)], grid: Option<f32>, edges: &[f32], tol: f32) -> Option<Snap> {
    let mut found: Option<Snap> = None;
    let mut consider = |delta: f32, line: f32, source: SnapSource| {
        if !delta.is_finite() || delta.abs() > tol {
            return;
        }
        let better = match found {
            None => true,
            Some(f) => delta.abs() < f.delta.abs() || (delta.abs() == f.delta.abs() && source < f.source),
        };
        if better {
            found = Some(Snap { delta, line, source });
        }
    };
    for &e in edges {
        for &(line, source) in lines {
            consider(line - e, line, source);
        }
        if let Some(s) = grid.filter(|s| *s > 0.0 && s.is_finite()) {
            let line = (e / s).round() * s;
            consider(line - e, line, SnapSource::Grid);
        }
    }
    found
}

/// Gather the snap lines a document offers. `skip` lists layers that are
/// being moved (they must not snap to themselves). Layer edges come from
/// visible layers' painted bounds, which costs a pixel scan per layer:
/// collect once when a drag starts, not every frame.
pub fn doc_lines(doc: &Document, opts: &SnapOptions, skip: &[LayerId]) -> SnapLines {
    let mut out = SnapLines {
        grid: opts.grid,
        ..Default::default()
    };
    if opts.canvas {
        let (w, h) = (doc.width as f32, doc.height as f32);
        for x in [0.0, w / 2.0, w] {
            out.xs.push((x, SnapSource::Canvas));
        }
        for y in [0.0, h / 2.0, h] {
            out.ys.push((y, SnapSource::Canvas));
        }
    }
    if opts.guides {
        for g in &doc.guides {
            if g.is_vertical() {
                out.xs.push((g.pos, SnapSource::Guide));
            } else {
                out.ys.push((g.pos, SnapSource::Guide));
            }
        }
    }
    if opts.layers {
        fn walk(layers: &[lumenply_doc::Layer], skip: &[LayerId], out: &mut SnapLines) {
            for l in layers {
                if !l.visible || skip.contains(&l.id) {
                    continue;
                }
                if let Some(children) = l.children() {
                    walk(children, skip, out);
                    continue;
                }
                if let Some(b) = l.raster_store().and_then(|s| s.content_bounds()) {
                    out.xs.push((b.x as f32, SnapSource::Layer));
                    out.xs.push((b.right() as f32, SnapSource::Layer));
                    out.ys.push((b.y as f32, SnapSource::Layer));
                    out.ys.push((b.bottom() as f32, SnapSource::Layer));
                }
            }
        }
        walk(doc.layers(), skip, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::Guide;
    use lumenply_tiles::Rgba;

    fn lines(xs: &[(f32, SnapSource)]) -> SnapLines {
        SnapLines {
            xs: xs.to_vec(),
            ys: xs.to_vec(),
            grid: None,
        }
    }

    #[test]
    fn the_nearest_edge_within_tolerance_snaps() {
        let l = lines(&[(100.0, SnapSource::Guide)]);
        let s = l.snap_x(&[97.0, 150.0], 6.0).unwrap();
        assert_eq!(
            s,
            Snap {
                delta: 3.0,
                line: 100.0,
                source: SnapSource::Guide
            }
        );
        assert_eq!(l.snap_x(&[93.5], 6.0), None, "6.5 px away is out of reach");
        assert_eq!(
            l.snap_x(&[106.0], 6.0).unwrap().delta,
            -6.0,
            "exactly at the tolerance snaps"
        );
    }

    #[test]
    fn closer_lines_win_and_ties_go_to_guides() {
        let l = lines(&[(103.0, SnapSource::Canvas), (100.0, SnapSource::Guide)]);
        assert_eq!(l.snap_x(&[102.0], 6.0).unwrap().line, 103.0, "1 px beats 2 px");
        let s = l.snap_x(&[101.5], 6.0).unwrap();
        assert_eq!(
            (s.line, s.source),
            (100.0, SnapSource::Guide),
            "a tie goes to the guide"
        );
    }

    #[test]
    fn grid_lines_sit_at_every_multiple() {
        let l = SnapLines {
            grid: Some(50.0),
            ..Default::default()
        };
        assert_eq!(
            l.snap_x(&[148.0], 6.0),
            Some(Snap {
                delta: 2.0,
                line: 150.0,
                source: SnapSource::Grid
            })
        );
        assert_eq!(l.snap_y(&[-52.0], 6.0).unwrap().line, -50.0);
        assert_eq!(l.snap_x(&[124.0], 6.0), None);
        // A point snaps per axis.
        let ((x, y), [sx, sy]) = l.snap_point(203.0, 177.0, 4.0);
        assert_eq!((x, y), (200.0, 177.0));
        assert!(sx.is_some() && sy.is_none());
    }

    #[test]
    fn a_moving_rectangle_snaps_by_its_edges_or_its_centre() {
        let l = lines(&[(100.0, SnapSource::Canvas), (150.0, SnapSource::Guide)]);
        // 97..147 (centre 122): the left edge is 3 from the centre line and
        // the right edge 3 from the guide; the guide wins the tie.
        let [sx, _] = l.snap_rect(97.0, 0.0, 147.0, 10.0, 6.0);
        assert_eq!(sx.map(|s| (s.delta, s.line)), Some((3.0, 150.0)));
        // 70..128 has its centre at 99: one pixel off the centre line.
        let [sx, _] = l.snap_rect(70.0, 0.0, 128.0, 10.0, 6.0);
        assert_eq!(sx.map(|s| (s.delta, s.line)), Some((1.0, 100.0)));
    }

    #[test]
    fn a_document_offers_its_guides_canvas_and_other_layers() {
        let mut doc = Document::new(200, 100);
        doc.guides.push(Guide::vertical(30.0));
        doc.guides.push(Guide::horizontal(80.0));
        let a = doc.add_pixel_layer("a");
        let b = doc.add_pixel_layer("b");
        for (id, (x, y)) in [(a, (10, 40)), (b, (150, 5))] {
            let px = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
            for dy in 0..10 {
                for dx in 0..10 {
                    px.set_pixel(x + dx, y + dy, Rgba::WHITE);
                }
            }
        }
        let all = SnapOptions {
            guides: true,
            canvas: true,
            layers: true,
            grid: Some(25.0),
        };
        let l = doc_lines(&doc, &all, &[b]);
        let xs: Vec<f32> = l.xs.iter().map(|p| p.0).collect();
        let ys: Vec<f32> = l.ys.iter().map(|p| p.0).collect();
        assert_eq!(xs, vec![0.0, 100.0, 200.0, 30.0, 10.0, 20.0], "b is being moved");
        assert_eq!(ys, vec![0.0, 50.0, 100.0, 80.0, 40.0, 50.0]);
        assert_eq!(l.grid, Some(25.0));
        let none = SnapOptions {
            guides: false,
            canvas: false,
            layers: false,
            grid: None,
        };
        assert_eq!(doc_lines(&doc, &none, &[]), SnapLines::default());
    }
}
