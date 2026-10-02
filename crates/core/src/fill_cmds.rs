//! Fill layer commands: add a Solid Color or Gradient fill layer, edit its
//! settings. Re-exported from [`crate::commands`].

use lumenply_doc::{Document, Fill, Layer, LayerId};
use lumenply_tiles::Rect;

use crate::{Command, EditError, EditResult};

/// Add a fill layer above `above` (or on top of the stack). With
/// `mask_selection` and an active selection, the selection becomes the
/// layer's mask, as Photoshop does.
pub struct AddFillLayer {
    pub fill: Fill,
    pub above: Option<LayerId>,
    pub mask_selection: bool,
}

impl AddFillLayer {
    pub fn new(fill: Fill) -> Self {
        AddFillLayer {
            fill,
            above: None,
            mask_selection: true,
        }
    }
}

impl Command for AddFillLayer {
    fn label(&self) -> String {
        format!("Add {} layer", self.fill.name())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let id = doc.alloc_id();
        let mut layer = Layer::fill(id, self.fill.clone());
        if let Some(f) = layer.fill_layer_mut() {
            lumenply_render::fill::refresh_cache(f, doc.width, doc.height, doc.float_mode);
        }
        if self.mask_selection {
            layer.mask = doc.selection.as_ref().map(|s| s.to_mask());
        }
        match self.above.and_then(|a| doc.siblings_mut(a).map(|list| (a, list))) {
            Some((a, list)) => {
                let i = list.iter().position(|l| l.id == a).expect("in siblings");
                list.insert(i + 1, layer);
            }
            None => {
                doc.add_layer(layer);
            }
        }
        Ok(())
    }
}

/// Replace a fill layer's settings (re-renders its pixels).
pub struct SetFill {
    pub layer: LayerId,
    pub fill: Fill,
}

impl Command for SetFill {
    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn label(&self) -> String {
        format!("Edit {}", self.fill.name())
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        Some(doc.canvas())
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let (w, h, float) = (doc.width, doc.height, doc.float_mode);
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let old = l
            .fill_layer()
            .ok_or_else(|| EditError::Invalid(format!("layer {} is not a fill layer", self.layer)))?
            .fill
            .name();
        // Kind changes rename a layer that still carries the default name.
        if old != self.fill.name() && l.name == old {
            l.name = self.fill.name().to_string();
        }
        let f = l.fill_layer_mut().expect("checked above");
        f.fill = self.fill.clone();
        lumenply_render::fill::refresh_cache(f, w, h, float);
        Ok(())
    }
}
