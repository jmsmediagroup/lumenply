//! Photoshop's Layer ▸ Layer Mask commands: a new mask that reveals or
//! hides all, or reveals or hides the selection (which is then used up:
//! deselected, as in Photoshop), and Apply, which bakes a pixel layer's
//! mask into its own transparency.

use std::sync::Arc;

use lumenply_doc::{Document, LayerId, Mask};
use lumenply_tiles::TILE_SIZE;

use crate::{Command, EditError, EditResult};

/// What a new layer mask starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskFrom {
    RevealAll,
    HideAll,
    RevealSelection,
    HideSelection,
}

impl MaskFrom {
    /// The menu and history wording.
    pub fn label(self) -> &'static str {
        match self {
            MaskFrom::RevealAll => "Reveal all",
            MaskFrom::HideAll => "Hide all",
            MaskFrom::RevealSelection => "Reveal selection",
            MaskFrom::HideSelection => "Hide selection",
        }
    }

    pub fn uses_selection(self) -> bool {
        matches!(self, MaskFrom::RevealSelection | MaskFrom::HideSelection)
    }
}

/// Give a layer a new mask (replacing none: the layer must not have one).
/// The selection kinds need a selection and deselect it, keeping it for
/// Select ▸ Reselect.
pub struct AddLayerMask {
    pub layer: LayerId,
    pub from: MaskFrom,
}

impl Command for AddLayerMask {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        format!("Layer mask: {}", self.from.label().to_lowercase())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let has_mask = doc
            .layer(self.layer)
            .ok_or(EditError::NoLayer(self.layer))?
            .mask
            .is_some();
        if has_mask {
            return Err(EditError::Invalid("the layer already has a mask".into()));
        }
        let mask = match self.from {
            MaskFrom::RevealAll => Mask::reveal_all(),
            MaskFrom::HideAll => Mask::hide_all(),
            MaskFrom::RevealSelection | MaskFrom::HideSelection => {
                let mut sel = doc
                    .selection
                    .take()
                    .ok_or_else(|| EditError::Invalid("nothing is selected".into()))?;
                doc.last_selection = Some(sel.clone());
                if self.from == MaskFrom::HideSelection {
                    sel.invert();
                }
                sel.to_mask()
            }
        };
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        l.mask = Some(mask);
        Ok(())
    }
}

/// Why `ApplyLayerMask` can't run on `layer`, if it can't.
pub fn apply_mask_block(doc: &Document, layer: LayerId) -> Option<&'static str> {
    let Some(l) = doc.layer(layer) else {
        return Some("Select a layer first");
    };
    match &l.mask {
        None => Some("This layer has no mask"),
        Some(_) if l.pixels().is_none() => Some("Masks apply to pixel layers: rasterize it first"),
        Some(m) if !m.enabled => Some("The mask is disabled: enable it first"),
        Some(_) => None,
    }
}

/// Layer ▸ Layer Mask ▸ Apply: multiply the layer's pixels by its mask and
/// drop the mask. The picture doesn't change.
pub struct ApplyLayerMask {
    pub layer: LayerId,
}

impl Command for ApplyLayerMask {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        "Apply layer mask".into()
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        if let Some(why) = apply_mask_block(doc, self.layer) {
            return Err(EditError::Invalid(why.to_lowercase()));
        }
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let mask = l.mask.take().expect("checked above");
        let store = l.pixels_mut().ok_or(EditError::NotPixel(self.layer))?;
        for c in store.coords().collect::<Vec<_>>() {
            let Some(tile) = store.tile(c) else { continue };
            match mask.tiles.tile(c) {
                // Untouched mask area: the mask's default all over.
                None if mask.default >= 1.0 => {}
                None if mask.default <= 0.0 => {
                    store.remove(c);
                }
                None => {
                    let mut t = tile.clone();
                    for p in t.pixels_mut().iter_mut() {
                        *p = p.scale(mask.default);
                    }
                    store.insert(c, Arc::new(t));
                }
                Some(m) => {
                    // Hoisted: a compact tile converts on every call.
                    let cover: Vec<f32> = m.pixels().iter().map(|p| p.a).collect();
                    let mut t = tile.clone();
                    for (p, v) in t.pixels_mut().iter_mut().zip(cover) {
                        if v < 1.0 {
                            *p = p.scale(v.max(0.0));
                        }
                    }
                    store.insert(c, Arc::new(t));
                }
            }
        }
        debug_assert_eq!(TILE_SIZE, 256);
        store.prune_blank();
        store.compact();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::SetSelection;
    use crate::Editor;
    use lumenply_doc::Selection;
    use lumenply_tiles::{Rect, Rgba};

    fn doc_with_layer() -> (Document, LayerId) {
        let mut doc = Document::new(300, 300);
        let id = doc.add_pixel_layer("a");
        let red = Rgba::new(1.0, 0.0, 0.0, 1.0);
        let px = doc.layer_mut(id).unwrap().pixels_mut().unwrap();
        for y in 0..300 {
            for x in 0..300 {
                px.set_pixel(x, y, red);
            }
        }
        (doc, id)
    }

    fn rect_selection(r: Rect) -> Selection {
        Selection::rect(r)
    }

    #[test]
    fn reveal_and_hide_selection_mask_and_deselect() {
        let (mut doc, id) = doc_with_layer();
        doc.selection = Some(rect_selection(Rect::new(100, 100, 50, 50)));
        let mut ed = Editor::new(doc);
        ed.execute(&AddLayerMask {
            layer: id,
            from: MaskFrom::RevealSelection,
        })
        .unwrap();
        let m = ed.doc().layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(
            (m.value(120, 120), m.value(10, 10), m.value(260, 120)),
            (1.0, 0.0, 0.0)
        );
        assert!(ed.doc().selection.is_none(), "the selection is used up");
        assert!(ed.doc().last_selection.is_some(), "and kept for Reselect");
        assert_eq!(ed.history().last().copied(), Some("Layer mask: reveal selection"));
        ed.undo();
        assert!(ed.doc().layer(id).unwrap().mask.is_none());
        assert!(ed.doc().selection.is_some(), "undo brings the selection back");

        ed.execute(&AddLayerMask {
            layer: id,
            from: MaskFrom::HideSelection,
        })
        .unwrap();
        let m = ed.doc().layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(
            (m.value(120, 120), m.value(10, 10), m.value(260, 120)),
            (0.0, 1.0, 1.0)
        );
        // A second mask is refused; so is a selection mask without one.
        assert!(ed
            .execute(&AddLayerMask {
                layer: id,
                from: MaskFrom::HideAll,
            })
            .is_err());
    }

    #[test]
    fn reveal_and_hide_all_ignore_the_selection() {
        let (mut doc, id) = doc_with_layer();
        doc.selection = Some(rect_selection(Rect::new(0, 0, 10, 10)));
        let mut ed = Editor::new(doc);
        ed.execute(&AddLayerMask {
            layer: id,
            from: MaskFrom::HideAll,
        })
        .unwrap();
        let m = ed.doc().layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!((m.value(5, 5), m.value(200, 200)), (0.0, 0.0));
        assert!(ed.doc().selection.is_some(), "the selection stays");
        ed.undo();
        ed.execute(&SetSelection { selection: None }).unwrap();
        assert!(ed
            .execute(&AddLayerMask {
                layer: id,
                from: MaskFrom::RevealSelection,
            })
            .is_err());
    }

    #[test]
    fn apply_bakes_the_mask_into_the_pixels() {
        let (mut doc, id) = doc_with_layer();
        let mut mask = Mask::reveal_all();
        mask.set_value(5, 5, 0.0);
        mask.set_value(6, 5, 0.25);
        // A whole hidden tile, and a half-revealed one, by default.
        let mut half = Mask::hide_all();
        half.default = 0.5;
        doc.layer_mut(id).unwrap().mask = Some(mask);
        let mut ed = Editor::new(doc);
        assert_eq!(apply_mask_block(ed.doc(), id), None);
        ed.execute(&ApplyLayerMask { layer: id }).unwrap();
        let l = ed.doc().layer(id).unwrap();
        assert!(l.mask.is_none());
        let px = l.pixels().unwrap();
        assert_eq!(px.get_pixel(5, 5).a, 0.0);
        let p = px.get_pixel(6, 5);
        assert!((p.a - 0.25).abs() < 0.002 && (p.r - 0.25).abs() < 0.002, "{p:?}");
        assert_eq!(px.get_pixel(200, 200).a, 1.0);
        ed.undo();
        assert!(ed.doc().layer(id).unwrap().mask.is_some());
        assert_eq!(
            ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(5, 5).a,
            1.0
        );

        // Default-only masks: a half mask halves everything; hide-all
        // empties the layer.
        let (mut doc, id) = doc_with_layer();
        doc.layer_mut(id).unwrap().mask = Some(half);
        let mut ed = Editor::new(doc);
        ed.execute(&ApplyLayerMask { layer: id }).unwrap();
        let p = ed.doc().layer(id).unwrap().pixels().unwrap().get_pixel(150, 150);
        assert!((p.a - 0.5).abs() < 0.002, "{p:?}");
        let (mut doc, id) = doc_with_layer();
        doc.layer_mut(id).unwrap().mask = Some(Mask::hide_all());
        let mut ed = Editor::new(doc);
        ed.execute(&ApplyLayerMask { layer: id }).unwrap();
        assert!(ed.doc().layer(id).unwrap().pixels().unwrap().is_empty());
    }

    #[test]
    fn apply_says_why_it_cant_run() {
        let (mut doc, id) = doc_with_layer();
        assert_eq!(apply_mask_block(&doc, id), Some("This layer has no mask"));
        let mut m = Mask::reveal_all();
        m.enabled = false;
        doc.layer_mut(id).unwrap().mask = Some(m);
        assert_eq!(
            apply_mask_block(&doc, id),
            Some("The mask is disabled: enable it first")
        );
        let g = doc.add_group("g");
        doc.layer_mut(g).unwrap().mask = Some(Mask::reveal_all());
        assert_eq!(
            apply_mask_block(&doc, g),
            Some("Masks apply to pixel layers: rasterize it first")
        );
        assert_eq!(apply_mask_block(&doc, 999), Some("Select a layer first"));
    }
}
