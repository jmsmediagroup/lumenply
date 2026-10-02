//! Smart object contents: Edit Contents saves back and Replace Contents
//! loads a new image through this command. The smart object keeps its
//! transform; only the untouched source pixels change.

use crate::{Command, EditError, EditResult};
use lumenply_doc::{Document, LayerContent, LayerId};
use lumenply_tiles::{Rect, TileStore};

/// Replace a smart object's source pixels and re-render it through its
/// transform.
pub struct ReplaceSmartSource {
    pub layer: LayerId,
    pub source: TileStore,
}

impl Command for ReplaceSmartSource {
    fn label(&self) -> String {
        "Update smart object".into()
    }

    fn target_layer(&self) -> Option<LayerId> {
        Some(self.layer)
    }

    fn affected(&self, doc: &Document) -> Option<Rect> {
        let l = doc.layer(self.layer)?;
        let sm = l.smart_layer()?;
        let old = sm.cache.as_ref().and_then(|c| c.bounds()).unwrap_or_default();
        let new = lumenply_render::transform_store(&self.source, &sm.transform)
            .bounds()
            .unwrap_or_default();
        Some(old.union(&new))
    }

    fn apply(&self, doc: &mut Document) -> EditResult {
        let l = doc.layer_mut(self.layer).ok_or(EditError::NoLayer(self.layer))?;
        let LayerContent::Smart(sm) = &mut l.content else {
            return Err(EditError::Invalid("the layer is not a smart object".into()));
        };
        sm.source = self.source.clone();
        sm.cache = Some(lumenply_render::transform_store(&sm.source, &sm.transform));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{ConvertToSmartObject, TransformLayer};
    use crate::Editor;
    use lumenply_tiles::{Affine, Raster, Rgba};

    #[test]
    fn new_contents_render_through_the_existing_transform() {
        let mut doc = Document::new(100, 100);
        let id = doc.add_pixel_layer("photo");
        for y in 0..10 {
            for x in 0..10 {
                doc.layer_mut(id).unwrap().pixels_mut().unwrap().set_pixel(
                    x,
                    y,
                    Rgba::new(1.0, 0.0, 0.0, 1.0),
                );
            }
        }
        let mut ed = Editor::new(doc);
        ed.execute(&ConvertToSmartObject { layer: id }).unwrap();
        // Moved 50 px right and doubled: the 10 px square covers 50..70.
        ed.execute(&TransformLayer {
            layer: id,
            transform: Affine::translate(50.0, 0.0).then(&Affine::around(50.0, 0.0, 2.0, 2.0, 0.0)),
        })
        .unwrap();
        let px = |ed: &Editor, x, y| {
            ed.doc()
                .layer(id)
                .unwrap()
                .raster_store()
                .unwrap()
                .get_pixel(x, y)
        };
        assert_eq!(px(&ed, 60, 10), Rgba::new(1.0, 0.0, 0.0, 1.0));
        // New contents: a blue square of the same size.
        let mut blue = Raster::new(10, 10);
        for p in &mut blue.pixels {
            *p = Rgba::new(0.0, 0.0, 1.0, 1.0);
        }
        ed.execute(&ReplaceSmartSource {
            layer: id,
            source: TileStore::from_raster(&blue, 0, 0),
        })
        .unwrap();
        assert_eq!(
            px(&ed, 60, 10),
            Rgba::new(0.0, 0.0, 1.0, 1.0),
            "same place, same scale"
        );
        assert_eq!(px(&ed, 5, 5).a, 0.0, "nothing left at the old spot");
        assert_eq!(ed.history().last(), Some(&"Update smart object"));
        ed.undo();
        assert_eq!(px(&ed, 60, 10), Rgba::new(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn only_smart_objects_take_new_contents() {
        let mut doc = Document::new(10, 10);
        let id = doc.add_pixel_layer("plain");
        let mut ed = Editor::new(doc);
        let r = ed.execute(&ReplaceSmartSource {
            layer: id,
            source: TileStore::new(),
        });
        assert!(r.is_err());
    }
}
