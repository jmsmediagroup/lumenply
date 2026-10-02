//! The Crop tool's command: reframe the canvas to a rectangle that may
//! reach past the old edges (the new area is transparent) and may be
//! rotated (straighten). One command, so one undo step.

use std::sync::Arc;

use lumenply_doc::{Document, LayerContent, VectorPath};
use lumenply_tiles::{Affine, Rect, Rgba, TileStore, TILE_SIZE};

use crate::{Command, EditError, EditResult};

/// The largest canvas side a crop may produce. A crop box dragged far past
/// the canvas would otherwise allocate a gigantic composite.
pub const MAX_CROP_SIDE: u32 = 32_768;

/// Crop (or extend) the canvas to `rect`, optionally rotated.
///
/// - `rect` is in current canvas pixels; parts outside the canvas become
///   transparent canvas.
/// - `angle` turns the crop frame clockwise (radians, screen coordinates)
///   about its centre; the image is turned the other way so the frame's
///   content ends up upright. 0 is a plain crop: every layer moves by whole
///   pixels, exactly. Any other angle resamples pixel layers bilinearly;
///   smart objects compose the rotation (re-rendered from their source);
///   masks follow their layer. Text layers move their anchor but stay
///   upright and editable (text cannot rotate without rasterizing); guides
///   keep their offset from the frame's centre.
/// - `delete_cropped` removes pixel-layer pixels outside the new canvas.
///   Off, they stay as hidden overflow that a later canvas enlargement or
///   move reveals again (Photoshop's "Delete Cropped Pixels" unticked).
///   Smart-object sources are never cut.
pub struct CropCanvas {
    pub rect: Rect,
    pub angle: f32,
    pub delete_cropped: bool,
}

impl CropCanvas {
    /// Where a point of the current canvas lands on the cropped canvas.
    pub fn mapping(&self) -> Affine {
        if self.angle == 0.0 {
            return Affine::translate(-self.rect.x as f32, -self.rect.y as f32);
        }
        let (w, h) = (self.rect.w as f32, self.rect.h as f32);
        let (cx, cy) = (self.rect.x as f32 + w / 2.0, self.rect.y as f32 + h / 2.0);
        Affine::translate(-cx, -cy)
            .then(&Affine::rotate(-self.angle))
            .then(&Affine::translate(w / 2.0, h / 2.0))
    }
}

impl Command for CropCanvas {
    fn label(&self) -> String {
        if self.angle == 0.0 {
            "Crop".into()
        } else {
            "Straighten and crop".into()
        }
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let r = self.rect;
        if r.is_empty() {
            return Err(EditError::Invalid("crop area is empty".into()));
        }
        if r.w > MAX_CROP_SIDE || r.h > MAX_CROP_SIDE {
            return Err(EditError::Invalid(format!(
                "crop area {}×{} is larger than {MAX_CROP_SIDE} px a side",
                r.w, r.h
            )));
        }
        if !self.angle.is_finite() {
            return Err(EditError::Invalid("crop angle is not a number".into()));
        }
        let t = self.mapping();
        let exact = t.integer_translation();
        let keep = Rect::new(0, 0, r.w, r.h);
        doc.for_each_layer_mut(|l| {
            match &mut l.content {
                LayerContent::Pixel(store) => {
                    let moved = match exact {
                        Some((dx, dy)) => store.translated(dx, dy),
                        None => lumenply_render::transform_store(store, &t),
                    };
                    *store = if self.delete_cropped {
                        clip_store(&moved, keep)
                    } else {
                        moved
                    };
                }
                LayerContent::Smart(sm) => {
                    sm.transform = sm.transform.then(&t);
                    sm.cache = Some(lumenply_render::transform_store(&sm.source, &sm.transform));
                }
                LayerContent::Shape(sh) => sh.transform_by(&t),
                LayerContent::Text(text) => {
                    // Whole-pixel crops keep the anchor exact.
                    match exact {
                        Some((dx, dy)) => (text.x, text.y) = (text.x + dx as f32, text.y + dy as f32),
                        None => text.map_position(|x, y| t.apply(x, y)),
                    }
                    lumenply_render::text::refresh_cache(text);
                }
                _ => {}
            }
            if let Some(m) = l.mask.as_mut() {
                *m = lumenply_render::transform_mask(m, &t);
            }
        });
        let map_path = |p: &mut VectorPath| {
            for sp in &mut p.subpaths {
                for n in &mut sp.nodes {
                    for pt in [&mut n.point, &mut n.handle_in, &mut n.handle_out] {
                        *pt = t.apply(pt.0, pt.1);
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
        // Guides keep their offset from the frame's centre (for a plain
        // crop: from its top-left corner).
        let (gx, gy) = if self.angle == 0.0 {
            (-r.x as f32, -r.y as f32)
        } else {
            (
                r.w as f32 / 2.0 - (r.x as f32 + r.w as f32 / 2.0),
                r.h as f32 / 2.0 - (r.y as f32 + r.h as f32 / 2.0),
            )
        };
        for g in &mut doc.guides {
            *g = g.shifted(gx, gy);
        }
        doc.width = r.w;
        doc.height = r.h;
        doc.selection = None;
        Ok(())
    }
}

/// Keep only the pixels of `store` inside `keep`. Tiles wholly inside stay
/// shared (no copy), tiles wholly outside are dropped, and only the tiles
/// straddling the edge are copied and trimmed.
pub fn clip_store(store: &TileStore, keep: Rect) -> TileStore {
    let mut out = TileStore::new();
    for c in store.coords() {
        let tr = c.rect();
        let inside = tr.intersect(&keep);
        let Some(tile) = store.tile_arc(c) else { continue };
        if inside.is_empty() {
            continue;
        }
        if inside == tr {
            out.insert(c, tile.clone());
            continue;
        }
        let mut t = (**tile).clone();
        let px = t.pixels_mut();
        let (ox, oy) = (tr.x, tr.y);
        for (i, p) in px.iter_mut().enumerate() {
            let (x, y) = (ox + (i % TILE_SIZE) as i32, oy + (i / TILE_SIZE) as i32);
            if !inside.contains(x, y) {
                *p = Rgba::TRANSPARENT;
            }
        }
        out.insert(c, Arc::new(t));
    }
    out.prune_blank();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;
    use lumenply_doc::{Guide, Layer, PathNode, SubPath, TextLayer};

    fn doc_with_dots(w: u32, h: u32, dots: &[(i32, i32)]) -> (Document, u64) {
        let mut doc = Document::new(w, h);
        let id = doc.add_pixel_layer("p");
        let px = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for &(x, y) in dots {
            px.set_pixel(x, y, Rgba::WHITE);
        }
        (doc, id)
    }

    fn px(doc: &Document, id: u64, x: i32, y: i32) -> f32 {
        doc.layer(id).unwrap().pixels().unwrap().get_pixel(x, y).a
    }

    #[test]
    fn a_plain_crop_moves_everything_by_whole_pixels_and_keeps_the_overflow() {
        let (mut doc, id) = doc_with_dots(100, 80, &[(40, 30), (5, 5)]);
        let tid = doc.alloc_id();
        let mut text = TextLayer::new("Hi", 60.0, 50.0, 12.0, [1.0; 4]);
        lumenply_render::text::refresh_cache(&mut text);
        doc.add_layer(Layer::text(tid, text));
        doc.guides = vec![Guide::vertical(35.0), Guide::horizontal(70.0)];
        doc.work_path = Some(VectorPath {
            subpaths: vec![SubPath {
                nodes: vec![PathNode::corner(30.0, 20.0), PathNode::corner(80.0, 60.0)],
                closed: false,
            }],
        });
        doc.selection = Some(lumenply_doc::Selection::all());

        let mut ed = Editor::new(doc);
        ed.execute(&CropCanvas {
            rect: Rect::new(30, 20, 50, 40),
            angle: 0.0,
            delete_cropped: false,
        })
        .unwrap();
        let d = ed.doc();
        assert_eq!((d.width, d.height), (50, 40));
        assert_eq!(px(d, id, 10, 10), 1.0, "(40, 30) lands at (10, 10)");
        assert_eq!(px(d, id, -25, -15), 1.0, "the cropped-off dot is kept off-canvas");
        let t = d.layer(tid).unwrap().text_layer().unwrap();
        assert_eq!((t.x, t.y), (30.0, 30.0), "the text anchor moves with the image");
        assert!(t.cache.is_some(), "the text raster is rebuilt");
        assert_eq!(d.guides, vec![Guide::vertical(5.0), Guide::horizontal(50.0)]);
        let nodes = &d.work_path.as_ref().unwrap().subpaths[0].nodes;
        assert_eq!((nodes[0].point, nodes[1].point), ((0.0, 0.0), (50.0, 40.0)));
        assert!(d.selection.is_none(), "the selection is dropped");
        assert_eq!(ed.history().last().copied(), Some("Crop"));

        ed.undo();
        assert_eq!((ed.doc().width, ed.doc().height), (100, 80));
        assert_eq!(px(ed.doc(), id, 40, 30), 1.0);
    }

    #[test]
    fn deleting_cropped_pixels_trims_layers_to_the_new_canvas() {
        // Dots inside, just outside and far outside the crop, on several tiles.
        let (mut doc, id) = doc_with_dots(600, 400, &[(300, 200), (299, 200), (590, 390), (12, 12)]);
        CropCanvas {
            rect: Rect::new(300, 100, 280, 290),
            angle: 0.0,
            delete_cropped: true,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!((doc.width, doc.height), (280, 290));
        assert_eq!(px(&doc, id, 0, 100), 1.0, "(300, 200) survives at (0, 100)");
        assert_eq!(px(&doc, id, -1, 100), 0.0, "(299, 200) was cropped off");
        assert_eq!(px(&doc, id, 290, 290), 0.0, "(590, 390) was cropped off");
        let b = doc.layer(id).unwrap().pixels().unwrap().content_bounds().unwrap();
        assert_eq!(b, Rect::new(0, 100, 1, 1), "nothing is left outside the canvas");
    }

    #[test]
    fn a_crop_box_past_the_edges_enlarges_the_canvas_with_transparency() {
        let (mut doc, id) = doc_with_dots(100, 80, &[(40, 30), (0, 0)]);
        CropCanvas {
            rect: Rect::new(-10, -5, 120, 90),
            angle: 0.0,
            delete_cropped: true,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!((doc.width, doc.height), (120, 90));
        assert_eq!(px(&doc, id, 50, 35), 1.0);
        assert_eq!(px(&doc, id, 10, 5), 1.0, "the old origin is at (10, 5)");
        assert_eq!(px(&doc, id, 0, 0), 0.0, "the new margin is transparent");
    }

    #[test]
    fn a_rotated_crop_straightens_a_tilted_band() {
        // A 6 px wide white band tilted 10° clockwise through (100, 100).
        let theta = 10f32.to_radians();
        let (dx, dy) = (theta.cos(), theta.sin());
        let mut doc = Document::new(200, 200);
        let id = doc.add_pixel_layer("p");
        let store = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for y in 0..200 {
            for x in 0..200 {
                let (vx, vy) = (x as f32 + 0.5 - 100.0, y as f32 + 0.5 - 100.0);
                let dist = (vx * -dy + vy * dx).abs();
                if dist <= 3.0 {
                    store.set_pixel(x, y, Rgba::WHITE);
                }
            }
        }
        let mut ed = Editor::new(doc);
        ed.execute(&CropCanvas {
            rect: Rect::new(50, 50, 100, 100),
            angle: theta,
            delete_cropped: true,
        })
        .unwrap();
        let d = ed.doc();
        assert_eq!((d.width, d.height), (100, 100));
        assert_eq!(ed.history().last().copied(), Some("Straighten and crop"));
        // The band now runs level through the centre row.
        for x in [10, 30, 50, 70, 90] {
            assert!(
                px(d, id, x, 50) > 0.95,
                "on the band at x={x}: {}",
                px(d, id, x, 50)
            );
            assert!(
                px(d, id, x, 40) < 0.01,
                "above the band at x={x}: {}",
                px(d, id, x, 40)
            );
            assert!(
                px(d, id, x, 60) < 0.01,
                "below the band at x={x}: {}",
                px(d, id, x, 60)
            );
        }
    }

    #[test]
    fn a_rotated_crop_keeps_text_upright_and_maps_its_anchor() {
        let mut doc = Document::new(200, 200);
        let tid = doc.alloc_id();
        let mut text = TextLayer::new("Title", 100.0, 60.0, 20.0, [1.0; 4]);
        lumenply_render::text::refresh_cache(&mut text);
        doc.add_layer(Layer::text(tid, text));
        // A quarter turn: the frame's centre (100, 100) stays put; a point
        // 40 px above it ends up 40 px to its left.
        CropCanvas {
            rect: Rect::new(50, 50, 100, 100),
            angle: std::f32::consts::FRAC_PI_2,
            delete_cropped: false,
        }
        .apply(&mut doc)
        .unwrap();
        let t = doc.layer(tid).unwrap().text_layer().unwrap();
        assert!(
            (t.x - 10.0).abs() < 1e-3 && (t.y - 50.0).abs() < 1e-3,
            "{:?}",
            (t.x, t.y)
        );
    }

    #[test]
    fn empty_huge_and_nan_crops_are_refused() {
        let (mut doc, _) = doc_with_dots(10, 10, &[]);
        let bad = |rect: Rect, angle: f32| CropCanvas {
            rect,
            angle,
            delete_cropped: false,
        };
        assert!(bad(Rect::new(0, 0, 0, 5), 0.0).apply(&mut doc).is_err());
        assert!(bad(Rect::new(0, 0, MAX_CROP_SIDE + 1, 5), 0.0)
            .apply(&mut doc)
            .is_err());
        assert!(bad(Rect::new(0, 0, 5, 5), f32::NAN).apply(&mut doc).is_err());
        assert_eq!((doc.width, doc.height), (10, 10));
    }

    #[test]
    fn clipping_shares_tiles_that_are_wholly_inside() {
        let mut s = TileStore::new();
        s.set_pixel(10, 10, Rgba::WHITE); // tile (0, 0), wholly inside
        s.set_pixel(300, 10, Rgba::WHITE); // tile (1, 0), straddles
        s.set_pixel(600, 10, Rgba::WHITE); // tile (2, 0), wholly outside
        let c = clip_store(&s, Rect::new(0, 0, 400, 256));
        let first = lumenply_tiles::TileCoord::new(0, 0);
        assert!(Arc::ptr_eq(
            s.tile_arc(first).unwrap(),
            c.tile_arc(first).unwrap()
        ));
        assert_eq!(c.get_pixel(300, 10), Rgba::WHITE);
        assert_eq!(c.get_pixel(600, 10), Rgba::TRANSPARENT);
        assert_eq!(c.len(), 2);
    }
}
