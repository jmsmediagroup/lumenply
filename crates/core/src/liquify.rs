//! The Liquify command: bakes a painted displacement field into a pixel
//! layer (see `lumenply_render::liquify`).

use crate::{Command, EditError, EditResult};
use lumenply_doc::{Document, LayerId};
use lumenply_render::{liquify_store, Displacement};
use lumenply_tiles::{Rect, Rgba};
use std::sync::Arc;

/// Warp a pixel layer by `field`. With a selection, the warp is blended in
/// by the selection's coverage, as filters are.
pub struct LiquifyLayer {
    pub layer: LayerId,
    pub field: Arc<Displacement>,
}

impl Command for LiquifyLayer {
    fn label(&self) -> String {
        "Liquify".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(self.field.rect)
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let sel = doc.selection.clone();
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let store = l.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        let warped = liquify_store(store, &self.field);
        match sel {
            None => *store = warped,
            Some(sel) => {
                let r = self.field.rect;
                let mut out = store.clone();
                for y in r.y..r.bottom() {
                    for x in r.x..r.right() {
                        let k = sel.value(x, y);
                        if k <= 0.0 {
                            continue;
                        }
                        let o = store.get_pixel(x, y);
                        let w = warped.get_pixel(x, y);
                        out.set_pixel(
                            x,
                            y,
                            Rgba::new(
                                o.r + (w.r - o.r) * k,
                                o.g + (w.g - o.g) * k,
                                o.b + (w.b - o.b) * k,
                                o.a + (w.a - o.a) * k,
                            ),
                        );
                    }
                }
                out.prune_blank();
                *store = out;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;
    use lumenply_doc::Selection;

    fn doc_with_dot() -> (Document, LayerId) {
        let mut doc = Document::new(100, 80);
        let id = doc.add_pixel_layer("dot");
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(40, 30, Rgba::new(1.0, 0.0, 0.0, 1.0));
        (doc, id)
    }

    fn shifted(dx: f32) -> Arc<Displacement> {
        let mut f = Displacement::new(Rect::new(0, 0, 100, 80), 2);
        for v in &mut f.d {
            *v = [-dx, 0.0];
        }
        Arc::new(f)
    }

    #[test]
    fn liquify_moves_pixels_and_undoes_in_one_step() {
        let (doc, id) = doc_with_dot();
        let mut ed = Editor::new(doc);
        ed.execute(&LiquifyLayer {
            layer: id,
            field: shifted(7.0),
        })
        .unwrap();
        let px = |ed: &Editor, x, y| ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(x, y);
        assert_eq!(px(&ed, 47, 30), Rgba::new(1.0, 0.0, 0.0, 1.0));
        assert_eq!(px(&ed, 40, 30).a, 0.0);
        assert_eq!(ed.history().last(), Some(&"Liquify"));
        ed.undo();
        assert_eq!(px(&ed, 40, 30), Rgba::new(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn a_selection_limits_the_warp() {
        let (mut doc, id) = doc_with_dot();
        // Only the left half is selected: the dot's destination (47, 30)
        // is selected, so the warp shows there; outside it nothing moves.
        doc.selection = Some(Selection::rect(Rect::new(0, 0, 50, 80)));
        doc.layer_mut(id)
            .unwrap()
            .pixels_mut()
            .unwrap()
            .set_pixel(70, 30, Rgba::new(0.0, 0.0, 1.0, 1.0));
        let mut ed = Editor::new(doc);
        ed.execute(&LiquifyLayer {
            layer: id,
            field: shifted(7.0),
        })
        .unwrap();
        let px = |x, y| ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(x, y);
        assert_eq!(px(47, 30), Rgba::new(1.0, 0.0, 0.0, 1.0));
        assert_eq!(
            px(70, 30),
            Rgba::new(0.0, 0.0, 1.0, 1.0),
            "unselected pixels stay"
        );
        assert_eq!(px(77, 30).a, 0.0);
    }

    #[test]
    fn liquify_refuses_non_pixel_layers() {
        let mut doc = Document::new(10, 10);
        let id = doc.add_filter(lumenply_doc::Filter::GaussianBlur { radius: 2.0 });
        let mut ed = Editor::new(doc);
        let err = ed.execute(&LiquifyLayer {
            layer: id,
            field: shifted(1.0),
        });
        assert!(matches!(err, Err(EditError::NotPixel(_))));
    }
}
