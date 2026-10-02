//! Edit > Content-Aware Fill. Re-exported from [`crate::commands`].

use lumenply_doc::{Document, LayerId};
use lumenply_tiles::{Rect, Rgba};

use crate::{Command, EditError, EditResult};

/// Replace the selected part of a pixel layer with content synthesised from
/// the rest of it (PatchMatch inpainting, see `lumenply_render::inpaint`).
/// Sampling is limited to the selection's bounds grown by `margin` pixels
/// (0 picks one: the hole's larger side, at least 64 px). A soft selection
/// blends the fill in by its coverage. One undo step.
pub struct ContentAwareFill {
    pub layer: LayerId,
    pub margin: u32,
}

impl ContentAwareFill {
    /// The area the fill reads from and writes into: the selection's
    /// bounds plus the sampling margin, inside the canvas.
    pub fn work_area(&self, doc: &Document) -> Option<Rect> {
        let canvas = doc.canvas();
        let hole = doc.selection.as_ref()?.tight_bounds(canvas);
        if hole.is_empty() {
            return None;
        }
        let m = if self.margin == 0 {
            hole.w.max(hole.h).max(64)
        } else {
            self.margin
        } as i32;
        Some(
            Rect::new(
                hole.x - m,
                hole.y - m,
                hole.w + 2 * m as u32,
                hole.h + 2 * m as u32,
            )
            .intersect(&canvas),
        )
    }
}

impl Command for ContentAwareFill {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Content-Aware Fill".into()
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let canvas = doc.canvas();
        doc.selection.as_ref().map(|s| s.tight_bounds(canvas))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc
            .selection
            .clone()
            .ok_or_else(|| EditError::Invalid("select the area to fill first".into()))?;
        let area = self
            .work_area(doc)
            .ok_or_else(|| EditError::Invalid("select the area to fill first".into()))?;
        let layer = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = layer.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let src = store.to_raster(area);
        let cov = sel.coverage.to_dense(area);
        let hole: Vec<bool> = cov.iter().map(|&c| c > 0.0).collect();
        let filled = lumenply_render::inpaint::inpaint(&src, &hole, 0x5EED_0001)
            .map_err(|e| EditError::Invalid(e.to_string()))?;
        let w = area.w as usize;
        for (i, &k) in cov.iter().enumerate() {
            if k <= 0.0 {
                continue;
            }
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            let o = src.pixels[i];
            let f = filled.pixels[i];
            let k = k.min(1.0);
            store.set_pixel(
                area.x + x,
                area.y + y,
                Rgba::new(
                    o.r + (f.r - o.r) * k,
                    o.g + (f.g - o.g) * k,
                    o.b + (f.b - o.b) * k,
                    o.a + (f.a - o.a) * k,
                ),
            );
        }
        store.prune_blank();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddPixelLayer, SetSelection};
    use crate::Editor;
    use lumenply_doc::Selection;
    use lumenply_tiles::Raster;

    #[test]
    fn fills_a_striped_hole_in_one_undo_step() {
        let (w, h) = (300u32, 200u32);
        let stripe = |x: u32| if x % 10 < 5 { 0.8 } else { 0.1 };
        let mut r = Raster::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // A red blob to remove, over vertical stripes.
                let blob = (x as i32 - 150).pow(2) + (y as i32 - 100).pow(2) < 15 * 15;
                let v = stripe(x);
                r.set(
                    x,
                    y,
                    if blob {
                        Rgba::new(1.0, 0.0, 0.0, 1.0)
                    } else {
                        Rgba::new(v, v, v, 1.0)
                    },
                );
            }
        }
        let mut ed = Editor::new(Document::new(w, h));
        ed.execute(&AddPixelLayer::from_raster("bg", r, 0, 0)).unwrap();
        let id = ed.doc().layers()[0].id;
        let fill = ContentAwareFill { layer: id, margin: 0 };
        assert!(ed.execute(&fill).is_err(), "needs a selection");
        ed.execute(&SetSelection {
            selection: Some(Selection::ellipse(Rect::new(131, 81, 38, 38))),
        })
        .unwrap();
        assert_eq!(fill.work_area(ed.doc()), Some(Rect::new(67, 17, 166, 166)));
        let t = std::time::Instant::now();
        ed.execute(&fill).unwrap();
        let ms = t.elapsed().as_millis();
        assert!(ms < 5000, "fill took {ms} ms");
        let px = ed.doc().layer(id).unwrap().pixels().unwrap().clone();
        let mut worst = 0.0f32;
        for y in 85..115 {
            for x in 135..165 {
                let p = px.get_pixel(x, y);
                let v = stripe(x as u32);
                worst = worst
                    .max((p.r - v).abs())
                    .max((p.g - v).abs())
                    .max((p.a - 1.0).abs());
            }
        }
        assert!(
            worst < 0.02,
            "the stripes run through the filled hole (worst {worst})"
        );
        assert_eq!(ed.history().last().copied(), Some("Content-Aware Fill"));
        ed.undo();
        let back = ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(150, 100);
        assert!(
            (back.r - 1.0).abs() < 1e-3 && back.g < 1e-3,
            "undo restores the blob"
        );
    }

    #[test]
    fn refuses_non_pixel_layers_and_empty_selections() {
        let mut ed = Editor::new(Document::new(64, 64));
        ed.execute(&SetSelection {
            selection: Some(Selection::rect(Rect::new(10, 10, 5, 5))),
        })
        .unwrap();
        assert!(ed.execute(&ContentAwareFill { layer: 99, margin: 0 }).is_err());
        let fill = ContentAwareFill { layer: 1, margin: 8 };
        let mut doc = Document::new(64, 64);
        doc.selection = Some(Selection::rect(Rect::new(10, 10, 5, 5)));
        assert_eq!(fill.work_area(&doc), Some(Rect::new(2, 2, 21, 21)));
        doc.selection = Some(Selection::rect(Rect::new(200, 200, 5, 5)));
        assert_eq!(fill.work_area(&doc), None, "selection off the canvas");
    }
}
