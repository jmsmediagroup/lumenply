//! Crop ▸ Perspective: crop the canvas to a four-cornered area and
//! rectify it, so a document shot at an angle (or a building leaning back)
//! comes out square-on. One command, one undo step.
//!
//! Every pixel layer and every mask is resampled through the homography
//! that maps the output rectangle onto the quad (bilinear, premultiplied
//! linear). Text, shapes and smart objects cannot follow a perspective
//! change as live layers, so they are rasterized first, inside the same
//! undo step; the app says so before the crop (Photoshop asks the same).
//! Fill and adjustment layers simply cover the new canvas. Guides are
//! dropped (they would no longer be straight); paths follow the mapping.

use lumenply_doc::{Document, LayerContent, LayerId, Mask, VectorPath};
use lumenply_render::Homography;
use lumenply_tiles::{Raster, Rect, Rgba, TileStore};
use rayon::prelude::*;

use crate::commands::RasterizeLayer;
use crate::crop::MAX_CROP_SIDE;
use crate::{Command, EditError, EditResult};

/// Crop to `quad` (top-left, top-right, bottom-right, bottom-left, in
/// canvas pixels) and stretch it to a `width × height` canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct PerspectiveCrop {
    pub quad: [(f32, f32); 4],
    pub width: u32,
    pub height: u32,
}

impl PerspectiveCrop {
    /// The size the quad suggests: the average lengths of its opposite
    /// edges, rounded, at least 1.
    pub fn natural_size(quad: [(f32, f32); 4]) -> (u32, u32) {
        let d = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1);
        let w = (d(quad[0], quad[1]) + d(quad[3], quad[2])) / 2.0;
        let h = (d(quad[0], quad[3]) + d(quad[1], quad[2])) / 2.0;
        (w.round().max(1.0) as u32, h.round().max(1.0) as u32)
    }

    /// Whether the corners make a convex quad, in order (either winding).
    pub fn is_convex(quad: [(f32, f32); 4]) -> bool {
        if quad.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
            return false;
        }
        let mut sign = 0.0f32;
        for i in 0..4 {
            let (a, b, c) = (quad[i], quad[(i + 1) % 4], quad[(i + 2) % 4]);
            let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
            if cross.abs() < 1e-3 || (sign != 0.0 && cross.signum() != sign) {
                return false;
            }
            sign = cross.signum();
        }
        true
    }

    /// Output pixel space → current canvas.
    pub fn homography(&self) -> Option<Homography> {
        Homography::rect_to_quad(Rect::new(0, 0, self.width, self.height), self.quad)
    }

    /// The layers the crop rasterizes: text, shapes and smart objects.
    pub fn rasterized_layers(doc: &Document) -> Vec<LayerId> {
        let mut ids = Vec::new();
        doc.for_each_layer(|l| {
            if matches!(
                l.content,
                LayerContent::Text(_) | LayerContent::Shape(_) | LayerContent::Smart(_)
            ) {
                ids.push(l.id);
            }
        });
        ids
    }

    /// The quad's bounding box grown by a pixel: all a sample can read.
    fn reach(&self) -> Rect {
        let xs = self.quad.map(|p| p.0);
        let ys = self.quad.map(|p| p.1);
        let x0 = xs.iter().copied().fold(f32::INFINITY, f32::min).floor() as i32 - 1;
        let y0 = ys.iter().copied().fold(f32::INFINITY, f32::min).floor() as i32 - 1;
        let x1 = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 2;
        let y1 = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 2;
        Rect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32)
    }
}

impl Command for PerspectiveCrop {
    fn label(&self) -> String {
        "Perspective Crop".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if self.width == 0 || self.height == 0 {
            return Err(EditError::Invalid(
                "the cropped size must be at least 1 px".into(),
            ));
        }
        if self.width > MAX_CROP_SIDE || self.height > MAX_CROP_SIDE {
            return Err(EditError::Invalid(format!(
                "the cropped size is larger than {MAX_CROP_SIDE} px a side"
            )));
        }
        if !Self::is_convex(self.quad) {
            return Err(EditError::Invalid(
                "the four corners must make a convex shape".into(),
            ));
        }
        let h = self
            .homography()
            .ok_or_else(|| EditError::Invalid("the corners form a degenerate quad".into()))?;
        let inv = h
            .inverse()
            .ok_or_else(|| EditError::Invalid("the corners form a degenerate quad".into()))?;
        for id in Self::rasterized_layers(doc) {
            RasterizeLayer { layer: id }.apply(doc)?;
        }
        let reach = self.reach();
        let (w, hh) = (self.width, self.height);
        doc.for_each_layer_mut(|l| {
            if let LayerContent::Pixel(store) = &mut l.content {
                *store = warp_pixels(store, &h, reach, w, hh);
            }
            if let Some(m) = l.mask.as_mut() {
                *m = warp_mask(m, &h, reach, w, hh);
            }
            if let Some(m) = l.smart_filters.mask.as_mut() {
                *m = warp_mask(m, &h, reach, w, hh);
            }
        });
        let map_path = |p: &mut VectorPath| {
            for sp in &mut p.subpaths {
                for n in &mut sp.nodes {
                    for pt in [&mut n.point, &mut n.handle_in, &mut n.handle_out] {
                        if let Some(q) = inv.apply(pt.0, pt.1) {
                            *pt = q;
                        }
                    }
                }
            }
        };
        if let Some(p) = doc.work_path.as_mut() {
            map_path(p);
        }
        for n in &mut doc.saved_paths {
            map_path(&mut n.path);
        }
        doc.guides.clear();
        doc.width = w;
        doc.height = hh;
        doc.selection = None;
        Ok(())
    }
}

/// Bilinear sample of a dense buffer (pixel centres at integers), `edge`
/// outside it.
#[inline]
fn bilinear<T: Copy>(
    buf: &[T],
    w: usize,
    h: usize,
    x: f32,
    y: f32,
    edge: T,
    mix: impl Fn([T; 4], [f32; 4]) -> T,
) -> T {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (ix, iy) = (x0 as i64, y0 as i64);
    let at = |cx: i64, cy: i64| {
        if cx < 0 || cy < 0 || cx >= w as i64 || cy >= h as i64 {
            edge
        } else {
            buf[cy as usize * w + cx as usize]
        }
    };
    mix(
        [at(ix, iy), at(ix + 1, iy), at(ix, iy + 1), at(ix + 1, iy + 1)],
        [(1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy],
    )
}

fn warp_pixels(src: &TileStore, h: &Homography, reach: Rect, w: u32, hh: u32) -> TileStore {
    let Some(bounds) = src.content_bounds() else {
        return TileStore::new();
    };
    let reach = reach.intersect(&bounds);
    if reach.is_empty() {
        return TileStore::new();
    }
    let r = src.to_raster(reach);
    let (rw, rh) = (r.width as usize, r.height as usize);
    let mut out = Raster::new(w, hh);
    out.pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let Some((sx, sy)) = h.apply(x as f32 + 0.5, y as f32 + 0.5) else {
                    continue;
                };
                let (lx, ly) = (sx - 0.5 - reach.x as f32, sy - 0.5 - reach.y as f32);
                *o = bilinear(&r.pixels, rw, rh, lx, ly, Rgba::TRANSPARENT, |p, k| {
                    let c = |f: fn(&Rgba) -> f32| {
                        f(&p[0]) * k[0] + f(&p[1]) * k[1] + f(&p[2]) * k[2] + f(&p[3]) * k[3]
                    };
                    Rgba::new(c(|p| p.r), c(|p| p.g), c(|p| p.b), c(|p| p.a))
                });
            }
        });
    TileStore::from_raster(&out, 0, 0)
}

fn warp_mask(m: &Mask, h: &Homography, reach: Rect, w: u32, hh: u32) -> Mask {
    let dense = m.to_dense(reach);
    let (rw, rh) = (reach.w as usize, reach.h as usize);
    let mut buf = vec![m.default; w as usize * hh as usize];
    buf.par_chunks_mut(w as usize).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let Some((sx, sy)) = h.apply(x as f32 + 0.5, y as f32 + 0.5) else {
                continue;
            };
            let (lx, ly) = (sx - 0.5 - reach.x as f32, sy - 0.5 - reach.y as f32);
            *o = bilinear(&dense, rw, rh, lx, ly, m.default, |p, k| {
                p[0] * k[0] + p[1] * k[1] + p[2] * k[2] + p[3] * k[3]
            });
        }
    });
    let mut out = Mask {
        tiles: TileStore::new(),
        default: m.default,
        enabled: m.enabled,
    };
    out.set_dense(Rect::new(0, 0, w, hh), &buf);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddMask, AddPixelLayer, AddTextLayer};
    use crate::Editor;
    use lumenply_doc::{Guide, TextLayer};

    /// The quad a 160 × 160 checkerboard of 20 px squares was warped onto.
    const QUAD: [(f32, f32); 4] = [(60.0, 40.0), (300.0, 70.0), (280.0, 250.0), (90.0, 220.0)];

    fn checker(x: u32, y: u32) -> bool {
        (x / 20 + y / 20) % 2 == 0
    }

    fn warped_checker_doc() -> (Editor, LayerId) {
        let mut r = Raster::new(160, 160);
        for y in 0..160 {
            for x in 0..160 {
                let v = if checker(x, y) { 1.0 } else { 0.0 };
                r.set(x, y, Rgba::new(v, v, v, 1.0));
            }
        }
        let flat = TileStore::from_raster(&r, 0, 0);
        let warped = lumenply_render::perspective_store(&flat, QUAD);
        let mut doc = Document::new(360, 300);
        let id = doc.add_pixel_layer("Photo");
        *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = warped;
        (Editor::new(doc), id)
    }

    #[test]
    fn a_warped_checkerboard_comes_back_as_square_cells() {
        let (mut ed, id) = warped_checker_doc();
        ed.execute(&PerspectiveCrop {
            quad: QUAD,
            width: 160,
            height: 160,
        })
        .unwrap();
        let d = ed.doc();
        assert_eq!((d.width, d.height), (160, 160));
        assert_eq!(ed.history().last().copied(), Some("Perspective Crop"));
        let px = d.layer(id).unwrap().pixels().unwrap();
        // Every cell's middle 12 × 12 is solid black or white again.
        let mut worst = 0.0f32;
        for cy in 0..8 {
            for cx in 0..8 {
                let want = if (cx + cy) % 2 == 0 { 1.0 } else { 0.0 };
                for y in cy * 20 + 4..cy * 20 + 16 {
                    for x in cx * 20 + 4..cx * 20 + 16 {
                        let p = px.get_pixel(x, y);
                        worst = worst.max((p.r - want).abs()).max((p.a - 1.0).abs());
                    }
                }
            }
        }
        assert!(worst < 0.02, "cells are square and solid (worst {worst})");
        // Cell edges sit on the 20 px grid: a mid-grey only at the boundary.
        let p = |x, y| px.get_pixel(x, y).r;
        assert!(p(17, 10) > 0.9 && p(22, 10) < 0.1, "{} {}", p(17, 10), p(22, 10));
        ed.undo();
        assert_eq!((ed.doc().width, ed.doc().height), (360, 300));
    }

    #[test]
    fn the_natural_size_averages_opposite_edges() {
        let rect = [(10.0, 10.0), (210.0, 10.0), (210.0, 110.0), (10.0, 110.0)];
        assert_eq!(PerspectiveCrop::natural_size(rect), (200, 100));
        // Top 100, bottom 140 wide; sides 50 and 50 tall-ish.
        let trap = [(20.0, 0.0), (120.0, 0.0), (140.0, 50.0), (0.0, 50.0)];
        let (w, h) = PerspectiveCrop::natural_size(trap);
        assert_eq!(w, 120);
        assert_eq!(h, (20f32.hypot(50.0)).round() as u32);
    }

    #[test]
    fn masks_follow_text_is_rasterized_and_guides_go() {
        let mut r = Raster::filled(200, 100, Rgba::WHITE);
        r.set(0, 0, Rgba::WHITE);
        let mut ed = Editor::new(Document::new(200, 100));
        ed.execute(&AddPixelLayer::from_raster("bg", r, 0, 0)).unwrap();
        let id = ed.doc().layers()[0].id;
        ed.execute(&AddMask { layer: id }).unwrap();
        ed.execute(&AddTextLayer {
            text: TextLayer::new("Hi", 120.0, 30.0, 20.0, [1.0; 4]),
            above: None,
        })
        .unwrap();
        let tid = ed.doc().layers().last().unwrap().id;
        let mut doc = ed.doc().clone();
        // Hide the right half in the mask; a guide at x = 50.
        doc.layer_mut(id)
            .unwrap()
            .mask
            .as_mut()
            .unwrap()
            .fill_rect(Rect::new(100, 0, 100, 100), 0.0);
        doc.guides = vec![Guide::vertical(50.0)];
        assert_eq!(PerspectiveCrop::rasterized_layers(&doc), vec![tid]);
        // An axis-aligned quad over the middle: a plain 2× crop-and-scale.
        let crop = PerspectiveCrop {
            quad: [(50.0, 0.0), (150.0, 0.0), (150.0, 100.0), (50.0, 100.0)],
            width: 200,
            height: 100,
        };
        crop.apply(&mut doc).unwrap();
        let m = doc.layer(id).unwrap().mask.clone().unwrap();
        let v = |x, y| m.to_dense(Rect::new(x, y, 1, 1))[0];
        assert_eq!(v(10, 50), 1.0);
        assert_eq!(v(190, 50), 0.0);
        assert!(
            (v(100, 50) - 0.5).abs() < 0.3,
            "the edge lands mid-canvas: {}",
            v(100, 50)
        );
        assert!(
            doc.layer(tid).unwrap().pixels().is_some(),
            "the text became pixels"
        );
        assert!(doc.guides.is_empty());
    }

    #[test]
    fn bad_quads_and_sizes_are_refused() {
        let (mut ed, _) = warped_checker_doc();
        let bow_tie = [(0.0, 0.0), (100.0, 100.0), (100.0, 0.0), (0.0, 100.0)];
        assert!(!PerspectiveCrop::is_convex(bow_tie));
        assert!(PerspectiveCrop::is_convex(QUAD));
        let bad = |quad, width, height| PerspectiveCrop { quad, width, height };
        assert!(ed.execute(&bad(bow_tie, 10, 10)).is_err());
        assert!(ed.execute(&bad(QUAD, 0, 10)).is_err());
        assert!(ed.execute(&bad(QUAD, MAX_CROP_SIDE + 1, 10)).is_err());
        let nan = [(f32::NAN, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        assert!(ed.execute(&bad(nan, 10, 10)).is_err());
        assert_eq!((ed.doc().width, ed.doc().height), (360, 300));
    }
}
