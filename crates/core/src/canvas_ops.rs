//! Image ▸ Trim, Reveal All and arbitrary canvas rotation: each works out a
//! frame and hands it to [`CropCanvas`], so every layer kind, mask, guide
//! and path follows exactly as in a crop.

use crate::crop::CropCanvas;
use crate::{Command, EditError, EditResult};
use lumenply_doc::{Document, LayerContent};
use lumenply_tiles::Rect;

/// What Trim cuts away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimBasis {
    /// Fully transparent borders.
    Transparent,
    /// Borders the colour of the top-left pixel.
    TopLeftColor,
}

/// Image ▸ Trim: crop to the part of the composite that isn't border.
pub struct Trim {
    pub basis: TrimBasis,
}

impl Trim {
    /// The frame Trim would crop to, or `None` when nothing would change.
    pub fn frame(&self, doc: &Document) -> Option<Rect> {
        let flat = lumenply_render::composite_raster(doc);
        let (w, h) = (flat.width, flat.height);
        if w == 0 || h == 0 {
            return None;
        }
        let corner = flat.get(0, 0);
        let border = |x: u32, y: u32| {
            let p = flat.get(x, y);
            match self.basis {
                TrimBasis::Transparent => p.a <= 1e-4,
                TrimBasis::TopLeftColor => {
                    (p.r - corner.r).abs() < 1e-3
                        && (p.g - corner.g).abs() < 1e-3
                        && (p.b - corner.b).abs() < 1e-3
                        && (p.a - corner.a).abs() < 1e-3
                }
            }
        };
        let row_is_border = |y: u32| (0..w).all(|x| border(x, y));
        let col_is_border = |x: u32, y0: u32, y1: u32| (y0..y1).all(|y| border(x, y));
        let top = (0..h).find(|&y| !row_is_border(y))?;
        let bottom = (0..h).rev().find(|&y| !row_is_border(y))? + 1;
        let left = (0..w).find(|&x| !col_is_border(x, top, bottom))?;
        let right = (0..w).rev().find(|&x| !col_is_border(x, top, bottom))? + 1;
        let r = Rect::new(left as i32, top as i32, right - left, bottom - top);
        (r != doc.canvas()).then_some(r)
    }
}

impl Command for Trim {
    fn label(&self) -> String {
        "Trim".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let rect = self
            .frame(doc)
            .ok_or_else(|| EditError::Invalid("nothing to trim".into()))?;
        CropCanvas {
            rect,
            angle: 0.0,
            delete_cropped: false,
        }
        .apply(doc)
    }
}

/// Image ▸ Reveal All: grow the canvas to show every layer's pixels,
/// including those a crop left outside.
pub struct RevealAll;

impl RevealAll {
    pub fn frame(doc: &Document) -> Rect {
        let mut r = doc.canvas();
        doc.for_each_layer(|l| {
            let store = match &l.content {
                LayerContent::Pixel(s) => Some(s),
                LayerContent::Smart(sm) => sm.cache.as_ref(),
                _ => None,
            };
            if let Some(b) = store.and_then(crate::snap::painted_bounds) {
                r = r.union(&b);
            }
        });
        r
    }
}

impl Command for RevealAll {
    fn label(&self) -> String {
        "Reveal all".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let rect = RevealAll::frame(doc);
        if rect == doc.canvas() {
            return Err(EditError::Invalid("everything is already on the canvas".into()));
        }
        CropCanvas {
            rect,
            angle: 0.0,
            delete_cropped: false,
        }
        .apply(doc)
    }
}

/// Image ▸ Image rotation ▸ Arbitrary: turn the whole image by `degrees`
/// (clockwise) about its centre; the canvas grows to hold the turned
/// corners, which are transparent.
pub struct RotateCanvas {
    pub degrees: f32,
}

impl RotateCanvas {
    /// The crop that performs the rotation.
    pub fn crop(&self, doc: &Document) -> CropCanvas {
        let a = self.degrees.to_radians();
        let (w, h) = (doc.width as f32, doc.height as f32);
        let (s, c) = (a.sin().abs(), a.cos().abs());
        let (nw, nh) = ((w * c + h * s).round(), (w * s + h * c).round());
        let (cx, cy) = (w / 2.0, h / 2.0);
        CropCanvas {
            rect: Rect::new(
                (cx - nw / 2.0).round() as i32,
                (cy - nh / 2.0).round() as i32,
                nw as u32,
                nh as u32,
            ),
            // Turning the frame one way turns the image the other.
            angle: -a,
            delete_cropped: false,
        }
    }
}

impl Command for RotateCanvas {
    fn label(&self) -> String {
        format!("Rotate canvas {:.1}°", self.degrees)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if !self.degrees.is_finite() || self.degrees % 360.0 == 0.0 {
            return Err(EditError::Invalid("no rotation".into()));
        }
        self.crop(doc).apply(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;
    use lumenply_tiles::Rgba;

    fn doc_with_box(w: u32, h: u32, b: Rect, background: Option<Rgba>) -> Document {
        let mut doc = Document::new(w, h);
        if let Some(bg) = background {
            let id = doc.add_pixel_layer("bg");
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    doc.layer_mut(id)
                        .unwrap()
                        .pixels_mut()
                        .unwrap()
                        .set_pixel(x, y, bg);
                }
            }
        }
        let id = doc.add_pixel_layer("box");
        for y in b.y..b.bottom() {
            for x in b.x..b.right() {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        doc
    }

    #[test]
    fn trim_cuts_transparent_or_flat_borders() {
        let doc = doc_with_box(100, 80, Rect::new(20, 10, 30, 40), None);
        let mut ed = Editor::new(doc);
        ed.execute(&Trim {
            basis: TrimBasis::Transparent,
        })
        .unwrap();
        assert_eq!((ed.doc().width, ed.doc().height), (30, 40));
        // On an opaque white background, trim by the corner colour.
        let doc = doc_with_box(100, 80, Rect::new(20, 10, 30, 40), Some(Rgba::WHITE));
        let t = Trim {
            basis: TrimBasis::Transparent,
        };
        assert_eq!(t.frame(&doc), None, "nothing transparent to trim");
        let t = Trim {
            basis: TrimBasis::TopLeftColor,
        };
        assert_eq!(t.frame(&doc), Some(Rect::new(20, 10, 30, 40)));
    }

    #[test]
    fn reveal_all_brings_back_what_a_crop_left_outside() {
        let doc = doc_with_box(100, 80, Rect::new(60, 50, 30, 20), None);
        let mut ed = Editor::new(doc);
        ed.execute(&CropCanvas {
            rect: Rect::new(0, 0, 50, 40),
            angle: 0.0,
            delete_cropped: false,
        })
        .unwrap();
        assert_eq!((ed.doc().width, ed.doc().height), (50, 40));
        ed.execute(&RevealAll).unwrap();
        // Canvas 0..50 ∪ the box at 60..90 × 50..70.
        assert_eq!((ed.doc().width, ed.doc().height), (90, 70));
        assert!(ed.execute(&RevealAll).is_err(), "nothing left to reveal");
    }

    #[test]
    fn rotation_grows_the_canvas_to_the_turned_bounds() {
        let doc = doc_with_box(100, 50, Rect::new(0, 0, 100, 50), None);
        let mut ed = Editor::new(doc);
        ed.execute(&RotateCanvas { degrees: 90.0 }).unwrap();
        assert_eq!((ed.doc().width, ed.doc().height), (50, 100));
        let mut ed = Editor::new(doc_with_box(100, 50, Rect::new(0, 0, 100, 50), None));
        ed.execute(&RotateCanvas { degrees: 30.0 }).unwrap();
        // 100·cos30 + 50·sin30 = 111.6, 100·sin30 + 50·cos30 = 93.3.
        assert_eq!((ed.doc().width, ed.doc().height), (112, 93));
        // The centre stays red, the new corners are transparent.
        let l = ed.doc().layers()[0].id;
        let px = |x, y| ed.doc().layer(l).unwrap().pixels().unwrap().get_pixel(x, y);
        assert!(px(56, 46).r > 0.95);
        assert_eq!(px(1, 1).a, 0.0);
        assert!(ed.execute(&RotateCanvas { degrees: 0.0 }).is_err());
    }

    #[test]
    fn positive_degrees_turn_clockwise() {
        // A marker in the top-left corner of a wide image ends up in the
        // top-right corner after a quarter turn clockwise.
        let mut ed = Editor::new(doc_with_box(100, 50, Rect::new(0, 0, 10, 10), None));
        ed.execute(&RotateCanvas { degrees: 90.0 }).unwrap();
        let l = ed.doc().layers()[0].id;
        let px = |x, y| ed.doc().layer(l).unwrap().pixels().unwrap().get_pixel(x, y);
        assert!(px(45, 5).r > 0.95, "top-right: {:?}", px(45, 5));
        assert_eq!(px(5, 5).a, 0.0, "top-left is empty now");
    }
}
