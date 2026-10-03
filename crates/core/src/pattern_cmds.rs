//! Pattern commands (ADR 0020): Edit ▸ Define Pattern, adding library
//! patterns to the document and removing unused ones. Pattern fill layers
//! are ordinary fill layers ([`crate::fill_cmds::AddFillLayer`] with a
//! `Fill::Pattern`); the Pattern Overlay effect goes through
//! [`crate::commands::SetLayerEffects`]. A reference that carries pixels
//! the document lacks is adopted into `Document::patterns` by the fill
//! refresh after every command, so documents stay self-contained.

use lumenply_doc::pattern::MAX_PATTERN_SIDE;
use lumenply_doc::{Document, LayerId, Pattern};
use lumenply_tiles::{Raster, Rect};

use crate::{Command, EditError, EditResult};

/// The pixels Define Pattern captures: the selection's bounds (the canvas
/// without a selection) of the visible image, or of one layer when
/// `layer` names it. Pixels outside a soft or shaped selection fade to
/// transparent by their selection value. Capped at 1024 px a side (the
/// top-left part is kept).
pub fn capture_pattern(doc: &Document, layer: Option<LayerId>) -> Result<Raster, EditError> {
    let canvas = doc.canvas();
    let rect = match &doc.selection {
        Some(s) => s.tight_bounds(canvas),
        None => canvas,
    };
    if rect.w == 0 || rect.h == 0 {
        return Err(EditError::Invalid("the selection is empty".into()));
    }
    let rect = Rect::new(
        rect.x,
        rect.y,
        rect.w.min(MAX_PATTERN_SIDE),
        rect.h.min(MAX_PATTERN_SIDE),
    );
    let mut r = match layer {
        Some(id) => {
            let l = doc.layer(id).ok_or(EditError::NoLayer(id))?;
            let store = l.raster_store().ok_or_else(|| {
                EditError::Invalid(format!("layer {id} has no pixels to define a pattern from"))
            })?;
            store.to_raster(rect)
        }
        None => lumenply_render::composite_rect(doc, rect).to_raster(rect),
    };
    if let Some(sel) = &doc.selection {
        for y in 0..rect.h {
            for x in 0..rect.w {
                let v = sel.value(rect.x + x as i32, rect.y + y as i32).clamp(0.0, 1.0);
                if v < 1.0 {
                    let p = r.get(x, y);
                    r.set(
                        x,
                        y,
                        lumenply_tiles::Rgba::new(p.r * v, p.g * v, p.b * v, p.a * v),
                    );
                }
            }
        }
    }
    Ok(r)
}

/// Add a pattern to the document, or replace the one with its id
/// (Edit ▸ Define Pattern, or a library pattern the document now uses).
pub struct DefinePattern {
    pub pattern: Pattern,
}

impl Command for DefinePattern {
    fn label(&self) -> String {
        "Define Pattern".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        // Layers using a replaced pattern redraw; nothing else moves.
        None
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        doc.upsert_pattern(self.pattern.clone());
        Ok(())
    }
}

/// Drop the document's patterns no layer uses (they are not saved
/// either; this tidies the document's list in the picker).
pub struct RemoveUnusedPatterns;

impl Command for RemoveUnusedPatterns {
    fn label(&self) -> String {
        "Remove unused patterns".into()
    }

    fn affected(&self, _doc: &Document) -> Option<Rect> {
        Some(Rect::default())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let used = doc.used_pattern_ids();
        let before = doc.patterns.len();
        doc.patterns.retain(|p| used.contains(&p.id));
        if doc.patterns.len() == before {
            return Err(EditError::Invalid("every pattern is in use".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AddFillLayer, SetLayerEffects};
    use crate::Editor;
    use lumenply_doc::{Fill, Layer, PatternOverlayFx, Selection};
    use lumenply_tiles::Rgba;

    /// 4×4 document whose one layer's red is x/4 and green y/4.
    fn doc() -> Document {
        let mut d = Document::new(4, 4);
        let id = d.alloc_id();
        let mut l = Layer::pixel(id, "ramp");
        for y in 0..4 {
            for x in 0..4 {
                l.pixels_mut()
                    .unwrap()
                    .set_pixel(x, y, Rgba::new(x as f32 / 4.0, y as f32 / 4.0, 0.0, 1.0));
            }
        }
        d.add_layer(l);
        d
    }

    fn close(a: Rgba, b: Rgba) -> bool {
        (a.r - b.r).abs() < 1e-3
            && (a.g - b.g).abs() < 1e-3
            && (a.b - b.b).abs() < 1e-3
            && (a.a - b.a).abs() < 1e-3
    }

    #[test]
    fn define_pattern_captures_the_selection_bounds_of_the_image() {
        let mut d = doc();
        d.selection = Some(Selection::rect(Rect::new(1, 2, 2, 2)));
        let r = capture_pattern(&d, None).unwrap();
        assert_eq!((r.width, r.height), (2, 2));
        assert!(
            close(r.get(0, 0), Rgba::new(0.25, 0.5, 0.0, 1.0)),
            "{:?}",
            r.get(0, 0)
        );
        assert!(
            close(r.get(1, 1), Rgba::new(0.5, 0.75, 0.0, 1.0)),
            "{:?}",
            r.get(1, 1)
        );
        // Without a selection: the whole canvas.
        d.selection = None;
        let all = capture_pattern(&d, Some(d.layers()[0].id)).unwrap();
        assert_eq!((all.width, all.height), (4, 4));
        assert!(close(all.get(3, 0), Rgba::new(0.75, 0.0, 0.0, 1.0)));
    }

    #[test]
    fn define_pattern_adds_and_undo_removes_it() {
        let mut ed = Editor::new(doc());
        let p = Pattern::new("id-1", "Ramp", capture_pattern(ed.doc(), None).unwrap());
        ed.execute(&DefinePattern { pattern: p.clone() }).unwrap();
        assert_eq!(ed.doc().patterns.len(), 1);
        assert_eq!(ed.doc().patterns[0].name, "Ramp");
        // Same id again replaces rather than duplicates.
        let renamed = Pattern {
            name: "Ramp 2".into(),
            ..p.clone()
        };
        ed.execute(&DefinePattern { pattern: renamed }).unwrap();
        assert_eq!(ed.doc().patterns.len(), 1);
        assert_eq!(ed.doc().patterns[0].name, "Ramp 2");
        ed.undo();
        ed.undo();
        assert!(ed.doc().patterns.is_empty());
    }

    #[test]
    fn pattern_fill_layers_tile_the_pattern_and_adopt_it() {
        let mut ed = Editor::new(Document::new(6, 2));
        // 3×1: red, green, blue.
        let mut r = Raster::new(3, 1);
        r.set(0, 0, Rgba::new(1.0, 0.0, 0.0, 1.0));
        r.set(1, 0, Rgba::new(0.0, 1.0, 0.0, 1.0));
        r.set(2, 0, Rgba::new(0.0, 0.0, 1.0, 1.0));
        let p = Pattern::new("rgb", "RGB", r);
        ed.execute(&AddFillLayer::new(Fill::pattern(p.reference())))
            .unwrap();
        let d = ed.doc();
        assert_eq!(d.patterns.len(), 1, "the layer's pattern joined the document");
        let l = &d.layers()[0];
        assert_eq!(l.name, "Pattern Fill");
        let cache = l.fill_layer().unwrap().cache.as_ref().unwrap();
        // Period 3 along x, every row alike; 16-bit storage rounds.
        assert!(close(cache.get_pixel(0, 0), Rgba::new(1.0, 0.0, 0.0, 1.0)));
        assert!(close(cache.get_pixel(4, 1), Rgba::new(0.0, 1.0, 0.0, 1.0)));
        assert!(close(cache.get_pixel(5, 0), Rgba::new(0.0, 0.0, 1.0, 1.0)));
        // Unused patterns go; used ones stay.
        assert!(ed.execute(&RemoveUnusedPatterns).is_err());
    }

    #[test]
    fn pattern_overlay_through_set_layer_effects() {
        let mut ed = Editor::new(doc());
        let id = ed.doc().layers()[0].id;
        let p = Pattern::new("w", "White", Raster::filled(1, 1, Rgba::WHITE));
        let mut fx = ed.doc().layers()[0].effects.clone();
        let mut po = PatternOverlayFx::new(p.reference());
        po.opacity = 0.5;
        fx.pattern_overlay = Some(po);
        ed.execute(&SetLayerEffects {
            layer: id,
            effects: fx,
        })
        .unwrap();
        assert_eq!(ed.doc().patterns.len(), 1);
        let out = lumenply_render::composite_raster(ed.doc());
        // (0, 0) was black: half white over it is 0.5.
        assert!(
            close(out.get(0, 0), Rgba::new(0.5, 0.5, 0.5, 1.0)),
            "{:?}",
            out.get(0, 0)
        );
        // (2, 0) red 0.5: 0.5·0.5 + 0.5 = 0.75.
        assert!((out.get(2, 0).r - 0.75).abs() < 1e-3);
    }
}
